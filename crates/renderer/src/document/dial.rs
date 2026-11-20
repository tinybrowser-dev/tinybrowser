use super::{CompletedDial, DialContext, QueuedDial};
use crate::protocol::DialOutcome;
use crate::sniff::sniff_encoding;

// https://html.spec.whatwg.org/multipage/parsing.html#encoding-sniffing-algorithm
pub(crate) struct ResponseDecoder {
    content_type: Option<String>,
    pending: Vec<u8>,
    decoder: Option<encoding_rs::Decoder>,
    encoding: Option<&'static encoding_rs::Encoding>,
}

impl ResponseDecoder {
    pub(crate) fn new(content_type: Option<String>) -> Self {
        Self {
            content_type,
            pending: Vec::new(),
            decoder: None,
            encoding: None,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        if let Some(decoder) = &mut self.decoder {
            return decode_chunk(decoder, bytes, false);
        }
        self.pending.extend_from_slice(bytes);
        let Some((encoding, bom_len)) =
            sniff_encoding(&self.pending, self.content_type.as_deref(), false)
        else {
            return String::new();
        };
        self.encoding = Some(encoding);
        let mut decoder = encoding.new_decoder_without_bom_handling();
        let output = decode_chunk(&mut decoder, &self.pending[bom_len..], false);
        self.pending.clear();
        self.decoder = Some(decoder);
        output
    }

    pub(crate) fn finish(mut self) -> (String, &'static str) {
        if let Some(decoder) = &mut self.decoder {
            let name = character_set_name(self.encoding.unwrap_or(encoding_rs::UTF_8));
            return (decode_chunk(decoder, &[], true), name);
        }
        let (encoding, bom_len) = sniff_encoding(&self.pending, self.content_type.as_deref(), true)
            .unwrap_or((encoding_rs::WINDOWS_1252, 0));
        let name = character_set_name(encoding);
        let mut decoder = encoding.new_decoder_without_bom_handling();
        (
            decode_chunk(&mut decoder, &self.pending[bom_len..], true),
            name,
        )
    }
}

fn character_set_name(encoding: &'static encoding_rs::Encoding) -> &'static str {
    encoding.name()
}

fn decode_chunk(decoder: &mut encoding_rs::Decoder, bytes: &[u8], last: bool) -> String {
    let mut output = String::with_capacity(
        decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len().saturating_mul(3))
            .max(4),
    );
    let mut read = 0;
    loop {
        let (result, consumed, _) = decoder.decode_to_string(&bytes[read..], &mut output, last);
        read += consumed;
        match result {
            encoding_rs::CoderResult::InputEmpty => return output,
            encoding_rs::CoderResult::OutputFull => {
                let remaining = bytes.len().saturating_sub(read);
                output.reserve(
                    decoder
                        .max_utf8_buffer_length(remaining)
                        .unwrap_or(remaining.saturating_mul(3))
                        .max(4),
                );
            }
        }
    }
}

pub(in crate::document) fn complete(
    dial: QueuedDial,
    outcome: Result<DialOutcome, crate::protocol::DialFailure>,
) -> Result<CompletedDial, DialContext> {
    let QueuedDial {
        context,
        req,
        started,
        ancestors,
        integrity,
    } = dial;
    let outcome = outcome.map_err(|_| context)?;
    Ok(CompletedDial {
        context,
        outcome,
        req,
        started,
        ancestors,
        integrity,
    })
}
