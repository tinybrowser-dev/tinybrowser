// https://drafts.csswg.org/css-font-loading/#fontface-interface

use std::sync::Arc;

use parley::fontique::{Blob, Collection, CollectionOptions};
use rquickjs::{
    Ctx, Result, TypedArray,
    prelude::{Func, Opt},
};

use crate::js::bindings::world;
use crate::js::world::{FontFamilyEntry, FontRegistration};

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    ctx.globals()
        .set("__tbRegisterFont", Func::from(register_font))?;
    ctx.globals()
        .set("__tbUnregisterFont", Func::from(unregister_font))?;
    ctx.globals()
        .set("__tbValidateFont", Func::from(validate_font))?;
    let bridge = crate::js::bridge::object(ctx)?;
    bridge.set("__tbFontFamilyAvailable", Func::from(family_available))?;
    Ok(())
}

const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn decode_font(data: Vec<u8>) -> Vec<u8> {
    match data.get(..4) {
        Some(b"wOFF") => {
            if let Some(size) = data.get(16..20) {
                let size = u32::from_be_bytes([size[0], size[1], size[2], size[3]]);
                if usize::try_from(size).is_ok_and(|size| size > MAX_FONT_BYTES) {
                    return Vec::new();
                }
            }
            wuff::decompress_woff1(&data).unwrap_or(data)
        }
        Some(b"wOF2") => wuff::decompress_woff2(&data).unwrap_or(data),
        _ if data.len() > MAX_FONT_BYTES => Vec::new(),
        _ => data,
    }
}

fn validate_font(data: TypedArray<'_, u8>) -> bool {
    if data.as_raw().is_none() {
        return false;
    }
    let data = decode_font(data.to_vec());
    if data.is_empty() {
        return false;
    }
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    !collection
        .register_fonts(Blob::new(Arc::new(data)), None)
        .is_empty()
}

fn family_available(ctx: Ctx<'_>, family: String) -> Result<bool> {
    let world = world(&ctx)?;
    let mut world = world.borrow_mut();
    if let Some(parsed) = world.main_document_mut() {
        let font_ctx = parsed.document.base.font_context();
        if let Ok(mut font_ctx) = font_ctx.lock() {
            return Ok(font_ctx.collection.family_by_name(&family).is_some());
        }
    }
    Ok(world
        .runtime
        .font_ctx
        .collection
        .family_by_name(&family)
        .is_some())
}

type Registered = Vec<FontFamilyEntry>;

fn descriptors(
    added: Vec<(parley::fontique::FamilyId, Vec<parley::fontique::FontInfo>)>,
) -> Registered {
    added
        .into_iter()
        .map(|(family, fonts)| {
            (
                family,
                fonts
                    .into_iter()
                    .map(|font| (font.width(), font.style(), font.weight()))
                    .collect(),
            )
        })
        .collect()
}

fn register_font(
    ctx: Ctx<'_>,
    family: String,
    data: TypedArray<'_, u8>,
    weight: Opt<String>,
    style: Opt<String>,
    stretch: Opt<String>,
) -> Result<u32> {
    if data.as_raw().is_none() {
        return Ok(0);
    }
    let data = decode_font(data.to_vec());
    if data.is_empty() {
        return Ok(0);
    }
    let world = world(&ctx)?;
    let mut world = world.borrow_mut();
    let override_font = parley::fontique::FontInfoOverride {
        family_name: Some(&family),
        weight: weight
            .0
            .as_deref()
            .and_then(parley::fontique::FontWeight::parse_css),
        style: style
            .0
            .as_deref()
            .and_then(parley::fontique::FontStyle::parse_css),
        width: stretch
            .0
            .as_deref()
            .and_then(parley::fontique::FontWidth::parse_css),
        ..Default::default()
    };
    let mut document_fonts = Registered::new();
    let document = {
        let Some(parsed) = world.main_document_mut() else {
            return Ok(0);
        };
        let document = parsed.id;
        let font_ctx = parsed.document.base.font_context();
        if let Ok(mut font_ctx) = font_ctx.lock() {
            document_fonts.extend(descriptors(font_ctx.collection.register_fonts(
                parley::fontique::Blob::new(Arc::new(data.clone())),
                Some(override_font),
            )));
        }
        Some(document)
    };
    let runtime_fonts = descriptors(world.runtime.font_ctx.collection.register_fonts(
        parley::fontique::Blob::new(Arc::new(data)),
        Some(override_font),
    ));
    let token = world.next_font_registration;
    world.next_font_registration = world.next_font_registration.wrapping_add(1);
    world.font_registrations.insert(
        token,
        FontRegistration {
            document,
            document_fonts,
            runtime_fonts,
        },
    );
    Ok(token)
}

fn unregister_font(ctx: Ctx<'_>, token: u32) -> Result<()> {
    let world = world(&ctx)?;
    let mut world = world.borrow_mut();
    let Some(registration) = world.font_registrations.remove(&token) else {
        return Ok(());
    };
    if let Some(document) = registration.document
        && let Some(parsed) = world.document_mut_by_id(document)
    {
        let font_ctx = parsed.document.base.font_context();
        if let Ok(mut font_ctx) = font_ctx.lock() {
            unregister(&mut font_ctx.collection, &registration.document_fonts);
        }
    }
    unregister(
        &mut world.runtime.font_ctx.collection,
        &registration.runtime_fonts,
    );
    Ok(())
}

fn unregister(collection: &mut parley::fontique::Collection, fonts: &[FontFamilyEntry]) {
    for (family, entries) in fonts {
        for (width, style, weight) in entries {
            collection.unregister_font(*family, *width, *style, *weight);
        }
    }
}
