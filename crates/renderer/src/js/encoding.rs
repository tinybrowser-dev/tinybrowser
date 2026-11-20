// https://encoding.spec.whatwg.org/#interface-textdecoder
// https://infra.spec.whatwg.org/#forgiving-base64-decode
// https://html.spec.whatwg.org/multipage/webappapis.html#atob

use base64::Engine as _;
use encoding_rs::{CoderResult, DecoderResult, Encoding};
use rquickjs::{Ctx, Exception, Object, Result, TypedArray, Value, prelude::Func};

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let host = super::bridge::object(ctx)?;
    host.set("__tbUtf8Encode", Func::from(utf8_encode))?;
    host.set("__tbEncodeInto", Func::from(encode_into))?;
    host.set("__tbDecode", Func::from(decode))?;
    host.set("__tbDecodeXmlResponse", Func::from(decode_xml_response))?;
    host.set("__tbDecoderInit", Func::from(decoder_init))?;
    host.set("__tbDecoderDecode", Func::from(decoder_decode))?;
    host.set("__tbDecoderFree", Func::from(decoder_free))?;
    host.set("__tbAtob", Func::from(atob))?;
    host.set("__tbBtoa", Func::from(btoa))?;
    host.set("__tbBase64Encode", Func::from(base64_encode))?;
    host.set("__tbEncodingName", Func::from(encoding_name))?;
    Ok(())
}

// https://encoding.spec.whatwg.org/#concept-encoding-get
fn encoding_name(label: String) -> Option<String> {
    Encoding::for_label(crate::dom::forms::trim_label(&label).as_bytes())
        .filter(|encoding| *encoding != encoding_rs::REPLACEMENT)
        .map(|encoding| encoding.name().to_owned())
}

// https://encoding.spec.whatwg.org/#utf-8-encoder
fn utf8_encode<'js>(ctx: Ctx<'js>, input: Value<'js>) -> Result<TypedArray<'js, u8>> {
    let units = super::bindings::webidl_to_units(&ctx, input)?;
    let text = String::from_utf16_lossy(&units);
    let (bytes, _, _) = encoding_rs::UTF_8.encode(&text);
    TypedArray::new(ctx, bytes.into_owned())
}

// https://encoding.spec.whatwg.org/#dom-textencoder-encodeinto
fn encode_into<'js>(ctx: Ctx<'js>, source: Value<'js>, max: usize) -> Result<Object<'js>> {
    let units = super::bindings::webidl_to_units(&ctx, source)?;
    let (read, written, bytes) = if max == 0 {
        (0, 0, Vec::new())
    } else {
        let mut dst = vec![0_u8; max];
        let mut encoder = encoding_rs::UTF_8.new_encoder();
        let (_, read, written, _) = encoder.encode_from_utf16(&units, &mut dst, true);
        dst.truncate(written);
        (read, written, dst)
    };
    let result = Object::new(ctx.clone())?;
    result.set("read", read)?;
    result.set("written", written)?;
    result.set("bytes", TypedArray::new(ctx, bytes)?)?;
    Ok(result)
}

// https://encoding.spec.whatwg.org/#concept-encoding-get
fn decode<'js>(
    ctx: Ctx<'js>,
    source: TypedArray<'js, u8>,
    label: String,
    fatal: bool,
    ignore_bom: bool,
) -> Result<String> {
    let bytes = source.to_vec();
    let encoding = resolve_label(&ctx, &label)?;
    let mut decoder = if ignore_bom {
        encoding.new_decoder_without_bom_handling()
    } else {
        encoding.new_decoder_with_bom_removal()
    };
    decode_loop(&mut decoder, &bytes, fatal, true)
        .map_err(|_| Exception::throw_type(&ctx, "The encoded data was not valid."))
}

// https://html.spec.whatwg.org/multipage/xhtml.html#parsing-xhtml-documents
// https://xhr.spec.whatwg.org/#response-xml
fn decode_xml_response<'js>(
    ctx: Ctx<'js>,
    source: TypedArray<'js, u8>,
    content_type: String,
) -> Result<String> {
    let bytes = source.to_vec();
    let header = (!content_type.trim().is_empty()).then_some(content_type.as_str());
    let (encoding, bom_len) =
        crate::sniff::sniff_encoding(&bytes, header, true).unwrap_or((encoding_rs::UTF_8, 0));
    let mut decoder = encoding.new_decoder_without_bom_handling();
    decode_loop(
        &mut decoder,
        bytes.get(bom_len..).unwrap_or(&[]),
        true,
        true,
    )
    .map_err(|_| Exception::throw_type(&ctx, "The encoded data was not valid."))
}

// https://encoding.spec.whatwg.org/#dom-textdecoder-decode
fn decoder_init(ctx: Ctx<'_>, label: String, ignore_bom: bool) -> Result<u64> {
    let encoding = resolve_label(&ctx, &label)?;
    let world_rc = super::bindings::world(&ctx)?;
    let id = world_rc
        .borrow()
        .runtime
        .registry
        .borrow_mut()
        .allocate_decoder();
    world_rc.borrow_mut().state.decoders.insert(
        id,
        super::world::DecoderSession::fresh(encoding, ignore_bom),
    );
    Ok(id)
}

// https://encoding.spec.whatwg.org/#dom-textdecoder-decode
fn decoder_decode<'js>(
    ctx: Ctx<'js>,
    id: u64,
    source: TypedArray<'js, u8>,
    fatal: bool,
    stream: bool,
) -> Result<String> {
    let bytes = source.to_vec();
    let world_rc = super::bindings::world(&ctx)?;
    let mut world = world_rc.borrow_mut();
    let Some(session) = world.state.decoders.get_mut(&id) else {
        return Err(Exception::throw_type(&ctx, "The decoder is closed."));
    };
    let mut input = std::mem::take(&mut session.carry);
    input.extend_from_slice(&bytes);
    let result = decode_loop(&mut session.decoder, &input, fatal, !stream);
    match result {
        Ok(text) => {
            if !stream {
                *session =
                    super::world::DecoderSession::fresh(session.encoding, session.ignore_bom);
            }
            Ok(text)
        }
        Err(failure) => {
            if stream {
                session.carry = input[failure.consumed..].to_vec();
            } else {
                *session =
                    super::world::DecoderSession::fresh(session.encoding, session.ignore_bom);
            }
            Err(Exception::throw_type(
                &ctx,
                "The encoded data was not valid.",
            ))
        }
    }
}

fn decoder_free(ctx: Ctx<'_>, id: u64) {
    if let Ok(world) = super::bindings::world(&ctx) {
        world.borrow_mut().state.decoders.remove(&id);
    }
}

// https://encoding.spec.whatwg.org/#concept-encoding-get
fn resolve_label(ctx: &Ctx<'_>, label: &str) -> Result<&'static Encoding> {
    Encoding::for_label(crate::dom::forms::trim_label(label).as_bytes())
        .filter(|encoding| *encoding != encoding_rs::REPLACEMENT)
        .ok_or_else(|| Exception::throw_range(ctx, "The encoding label is not supported"))
}

// https://infra.spec.whatwg.org/#forgiving-base64-decode
fn atob<'js>(ctx: Ctx<'js>, input: Value<'js>) -> Result<Option<String>> {
    let Some(bytes) = latin1_bytes(&ctx, input)? else {
        return Ok(None);
    };
    let decoded = data_url::forgiving_base64::decode_to_vec(&bytes).ok();
    Ok(decoded.map(|bytes| net::isomorphic_decode(&bytes)))
}

// https://html.spec.whatwg.org/multipage/webappapis.html#atob
fn btoa<'js>(ctx: Ctx<'js>, input: Value<'js>) -> Result<Option<String>> {
    let Some(bytes) = latin1_bytes(&ctx, input)? else {
        return Ok(None);
    };
    Ok(Some(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

fn base64_encode<'js>(ctx: Ctx<'js>, source: Value<'js>) -> Result<Option<String>> {
    let Some(bytes) = latin1_bytes(&ctx, source)? else {
        return Ok(None);
    };
    Ok(Some(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

fn latin1_bytes<'js>(ctx: &Ctx<'js>, input: Value<'js>) -> Result<Option<Vec<u8>>> {
    let units = super::bindings::webidl_to_units(ctx, input)?;
    let mut bytes = Vec::with_capacity(units.len());
    for unit in units {
        let Ok(byte) = u8::try_from(unit) else {
            return Ok(None);
        };
        bytes.push(byte);
    }
    Ok(Some(bytes))
}

struct DecodeFailure {
    consumed: usize,
}

fn decode_loop(
    decoder: &mut encoding_rs::Decoder,
    input: &[u8],
    fatal: bool,
    last: bool,
) -> core::result::Result<String, DecodeFailure> {
    let mut output = String::with_capacity(
        decoder
            .max_utf8_buffer_length(input.len())
            .unwrap_or(input.len().saturating_mul(3))
            .max(4),
    );
    let mut read = 0;
    loop {
        if fatal {
            let (result, consumed) =
                decoder.decode_to_string_without_replacement(&input[read..], &mut output, last);
            read += consumed;
            match result {
                DecoderResult::InputEmpty => break,
                DecoderResult::Malformed(_, _) => {
                    return Err(DecodeFailure { consumed: read });
                }
                DecoderResult::OutputFull => {
                    reserve_output(decoder, &mut output, input.len().saturating_sub(read));
                }
            }
        } else {
            let (result, consumed, _) = decoder.decode_to_string(&input[read..], &mut output, last);
            read += consumed;
            match result {
                CoderResult::InputEmpty => break,
                CoderResult::OutputFull => {
                    reserve_output(decoder, &mut output, input.len().saturating_sub(read));
                }
            }
        }
    }
    Ok(output)
}

fn reserve_output(decoder: &mut encoding_rs::Decoder, output: &mut String, remaining: usize) {
    output.reserve(
        decoder
            .max_utf8_buffer_length(remaining)
            .unwrap_or(remaining.saturating_mul(3))
            .max(4),
    );
}
