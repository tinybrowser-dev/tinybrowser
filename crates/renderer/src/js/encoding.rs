//! Encoding Standard codecs for the JS shims.
//!
//! `encoding_rs` already owns labels, UTF-8, UTF-16, and the legacy tables.
//! The JS `TextEncoder` / `TextDecoder` / `atob` / `btoa` surfaces stay in
//! script; these host functions are the crate calls those shims used to
//! reimplement.
//!
//! <https://encoding.spec.whatwg.org/#interface-textdecoder>
//! <https://infra.spec.whatwg.org/#forgiving-base64-decode>
//! <https://html.spec.whatwg.org/multipage/webappapis.html#atob>

use base64::Engine as _;
use encoding_rs::{CoderResult, DecoderResult, Encoding};
use rquickjs::{Ctx, Exception, Object, Result, TypedArray, Value, prelude::Func};

/// Installs the encoding hooks the JS shim calls.
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

/// Resolves a label to its encoding name, or `None` for an unknown label or
/// `replacement` (<https://encoding.spec.whatwg.org/#concept-encoding-get>).
fn encoding_name(label: String) -> Option<String> {
    Encoding::for_label(crate::dom::forms::trim_label(&label).as_bytes())
        .filter(|encoding| *encoding != encoding_rs::REPLACEMENT)
        .map(|encoding| encoding.name().to_owned())
}

/// UTF-8 encodes `input` after IDL string conversion, replacing unpaired
/// surrogates as the Encoding Standard's encoder does
/// (<https://encoding.spec.whatwg.org/#utf-8-encoder>).
fn utf8_encode<'js>(ctx: Ctx<'js>, input: Value<'js>) -> Result<TypedArray<'js, u8>> {
    let units = super::bindings::webidl_to_units(&ctx, input)?;
    let text = String::from_utf16_lossy(&units);
    let (bytes, _, _) = encoding_rs::UTF_8.encode(&text);
    TypedArray::new(ctx, bytes.into_owned())
}

/// UTF-8 encode-into: fill at most `max` bytes and report how many UTF-16
/// code units were consumed
/// (<https://encoding.spec.whatwg.org/#dom-textencoder-encodeinto>).
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

/// Decodes a complete buffer with Encoding Standard label `label`.
/// Single-shot only: every caller passes a whole buffer, so the input is
/// always final and nothing carries forward
/// (<https://encoding.spec.whatwg.org/#concept-encoding-get>).
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

/// Decodes a complete XML response body the way navigations decode one:
/// BOM, then the transport charset or the XML declaration, then UTF-8.
/// Fatal: undecodable bytes mean there is no document, so failure throws
/// instead of substituting replacement characters
/// (<https://html.spec.whatwg.org/multipage/xhtml.html#parsing-xhtml-documents>,
/// <https://xhr.spec.whatwg.org/#response-xml>).
///
/// Fatal decoding below sees the response's true bytes, not a re-encoding.
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

/// Opens a streaming decoder session for `label`, returning its platform id.
/// The session owns one `encoding_rs` decoder across `decode()` calls, which
/// is what buffers a trailing partial sequence; a fresh decoder per call
/// would silently drop it
/// (<https://encoding.spec.whatwg.org/#dom-textdecoder-decode>).
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

/// Decodes `source` through session `id`, keeping the decoder's buffered
/// tail for the next call when `stream` is true. A fatal error throws but
/// the bytes behind the error stay queued in `carry`, so later calls
/// continue after the error
/// (<https://encoding.spec.whatwg.org/#dom-textdecoder-decode>).
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
                // `last=true` ends the decoder, which must never run again;
                // the next call starts a fresh session with the same encoding
                // and BOM mode.
                *session =
                    super::world::DecoderSession::fresh(session.encoding, session.ignore_bom);
            }
            Ok(text)
        }
        Err(failure) => {
            if stream {
                // The error itself is consumed; the unprocessed queue behind
                // it carries forward.
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

/// Drops session `id`. Best-effort: after realm teardown there is no world
/// left, and the realm drop itself reclaims every session.
fn decoder_free(ctx: Ctx<'_>, id: u64) {
    if let Ok(world) = super::bindings::world(&ctx) {
        world.borrow_mut().state.decoders.remove(&id);
    }
}

/// Resolves `label` to an encoding, rejecting unknown labels and the
/// `replacement` encoding, which getting an encoding never returns
/// (<https://encoding.spec.whatwg.org/#concept-encoding-get>).
fn resolve_label(ctx: &Ctx<'_>, label: &str) -> Result<&'static Encoding> {
    Encoding::for_label(crate::dom::forms::trim_label(label).as_bytes())
        .filter(|encoding| *encoding != encoding_rs::REPLACEMENT)
        .ok_or_else(|| Exception::throw_range(ctx, "The encoding label is not supported"))
}

/// Forgiving-base64 decode of an isomorphic string
/// (<https://infra.spec.whatwg.org/#forgiving-base64-decode>).
///
/// Takes the value, not a string: a lone surrogate has no UTF-8 form for a
/// Rust `String` to cross in, so the Latin1 check runs on UTF-16 units and
/// reports `None` for the caller's `InvalidCharacterError`.
fn atob<'js>(ctx: Ctx<'js>, input: Value<'js>) -> Result<Option<String>> {
    let Some(bytes) = latin1_bytes(&ctx, input)? else {
        return Ok(None);
    };
    let decoded = data_url::forgiving_base64::decode_to_vec(&bytes).ok();
    Ok(decoded.map(|bytes| net::isomorphic_decode(&bytes)))
}

/// Latin-1-only `btoa`
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#atob>).
fn btoa<'js>(ctx: Ctx<'js>, input: Value<'js>) -> Result<Option<String>> {
    let Some(bytes) = latin1_bytes(&ctx, input)? else {
        return Ok(None);
    };
    Ok(Some(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

/// Standard base64 of isomorphic-latin1 bytes, for `FileReader` data URLs.
fn base64_encode<'js>(ctx: Ctx<'js>, source: Value<'js>) -> Result<Option<String>> {
    let Some(bytes) = latin1_bytes(&ctx, source)? else {
        return Ok(None);
    };
    Ok(Some(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

/// UTF-16 units as Latin1 bytes, or `None` when a unit exceeds `0xFF`.
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
    /// Bytes consumed when the failure surfaced, so a streaming session can
    /// retain the unprocessed queue behind the error.
    consumed: usize,
}

/// Runs `decoder` over `input`, growing the output until the input is
/// consumed. The caller owns how the decoder persists: one-shot callers pass
/// a fresh decoder with `last` true, streaming sessions pass their stored
/// decoder with `last` set from the `stream` option.
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
