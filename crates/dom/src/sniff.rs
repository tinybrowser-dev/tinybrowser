// https://html.spec.whatwg.org/multipage/parsing.html#encoding-sniffing-algorithm
// https://www.w3.org/TR/xml/#sec-guessing

// https://html.spec.whatwg.org/multipage/parsing.html#encoding-sniffing-algorithm
// https://encoding.spec.whatwg.org/#concept-encoding-get
pub fn sniff_encoding(
    bytes: &[u8],
    content_type: Option<&str>,
    eof: bool,
) -> Option<(&'static encoding_rs::Encoding, usize)> {
    if let Some(bom) = encoding_rs::Encoding::for_bom(bytes) {
        return Some(bom);
    }
    if !eof && bom_prefix(bytes) {
        return None;
    }
    if let Some(encoding) = content_type.and_then(charset_from_content_type) {
        return Some((encoding, 0));
    }
    // https://www.w3.org/TR/xml/#sec-guessing
    // https://html.spec.whatwg.org/multipage/xhtml.html#parsing-xhtml-documents
    if content_type.is_some_and(|header| crate::xml::navigated_content_type(header).is_some()) {
        return xml_encoding(bytes, eof);
    }
    if let Some(encoding) = prescan_charset(bytes) {
        return Some((html_meta_encoding(encoding), 0));
    }
    (eof || bytes.len() >= 1024).then_some((encoding_rs::WINDOWS_1252, 0))
}

// https://html.spec.whatwg.org/multipage/parsing.html#concept-encoding-get
fn html_meta_encoding(encoding: &'static encoding_rs::Encoding) -> &'static encoding_rs::Encoding {
    if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
        encoding_rs::UTF_8
    } else if encoding == encoding_rs::X_USER_DEFINED {
        encoding_rs::WINDOWS_1252
    } else {
        encoding
    }
}

// https://www.w3.org/TR/xml/#sec-guessing
fn xml_encoding(bytes: &[u8], eof: bool) -> Option<(&'static encoding_rs::Encoding, usize)> {
    const MARK: &[u8] = b"<?xml";
    // https://www.w3.org/TR/xml/#sec-guessing
    const BE: &[u8] = &[0x00, 0x3C, 0x00, 0x3F];
    const LE: &[u8] = &[0x3C, 0x00, 0x3F, 0x00];
    if bytes.len() < 4
        && !eof
        && (MARK.starts_with(bytes) || BE.starts_with(bytes) || LE.starts_with(bytes))
    {
        return None;
    }
    if bytes.starts_with(BE) {
        return Some((encoding_rs::UTF_16BE, 0));
    }
    if bytes.starts_with(LE) {
        return Some((encoding_rs::UTF_16LE, 0));
    }
    if bytes.len() < MARK.len() && MARK.starts_with(bytes) && !eof {
        return None;
    }
    if !bytes.starts_with(MARK) {
        return Some((encoding_rs::UTF_8, 0));
    }
    let window = &bytes[..bytes.len().min(1024)];
    let Some(end) = window.windows(2).position(|pair| pair == b"?>") else {
        if !eof && bytes.len() < 1024 {
            return None;
        }
        return Some((encoding_rs::UTF_8, 0));
    };
    let declaration = &window[..end];
    let declared = xml_encoding_attribute(declaration).unwrap_or(encoding_rs::UTF_8);
    // https://html.spec.whatwg.org/multipage/parsing.html#determining-the-character-encoding — remaps an ASCII-read UTF-16 XML encoding declaration to UTF-8, matching Chromium
    let encoding = if declared == encoding_rs::UTF_16LE || declared == encoding_rs::UTF_16BE {
        encoding_rs::UTF_8
    } else {
        declared
    };
    Some((encoding, 0))
}

// https://www.w3.org/TR/xml/#NT-EncodingDecl
fn xml_encoding_attribute(declaration: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let token = b"encoding";
    let mut index = 0;
    while index + token.len() <= declaration.len() {
        if declaration[index..].starts_with(token) {
            let after = index + token.len();
            let preceded = index
                .checked_sub(1)
                .is_some_and(|before| declaration[before].is_ascii_whitespace());
            let continues = declaration.get(after).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b':')
            });
            if preceded && !continues {
                return encoding_label(&declaration[after..]);
            }
        }
        index += 1;
    }
    None
}

// https://www.w3.org/TR/xml/#NT-EncodingDecl
fn encoding_label(rest: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let rest = rest.trim_ascii_start();
    let rest = rest.strip_prefix(b"=")?.trim_ascii_start();
    let quote = *rest.first()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let value = rest.get(1..)?.split(|byte| *byte == quote).next()?;
    encoding_rs::Encoding::for_label(value)
}

fn bom_prefix(bytes: &[u8]) -> bool {
    [
        [0xEF, 0xBB, 0xBF].as_slice(),
        [0xFF, 0xFE].as_slice(),
        [0xFE, 0xFF].as_slice(),
    ]
    .iter()
    .any(|bom| bytes.len() < bom.len() && bom.starts_with(bytes))
}
// https://mimesniff.spec.whatwg.org/#parse-a-mime-type
// https://encoding.spec.whatwg.org/#concept-encoding-get
fn charset_from_content_type(content_type: &str) -> Option<&'static encoding_rs::Encoding> {
    let parsed = parse_mime_type(content_type)?;
    let (_, value) = parsed
        .parameters
        .iter()
        .find(|(name, _)| name == "charset")?;
    // https://encoding.spec.whatwg.org/#concept-encoding-get
    encoding_rs::Encoding::for_label(
        value
            .trim_matches(|character: char| character.is_ascii_whitespace())
            .as_bytes(),
    )
}

fn prescan_charset(body: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let prefix = &body[..body.len().min(1024)];
    let ascii = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_start) = ascii[cursor..].find("<meta") {
        let start = cursor + relative_start;
        let Some(offset) = ascii[start..].find('>') else {
            break;
        };
        let end = start + offset;
        let tag = &ascii[start..end];
        if let Some(relative_charset) = tag.find("charset=") {
            let label = tag[relative_charset + "charset=".len()..]
                .trim_start_matches([' ', '\t', '\r', '\n', '\'', '"'])
                .split([' ', '\t', '\r', '\n', '\'', '"', ';', '>'])
                .next()?;
            if let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) {
                return Some(encoding);
            }
        }
        cursor = end.saturating_add(1);
    }
    None
}

// https://mimesniff.spec.whatwg.org/#read-the-resource-header
pub const RESOURCE_HEADER_LEN: usize = 1445;

#[derive(Clone, Copy, Default)]
pub struct MimeGuard {
    pub no_sniff: bool,
    pub http: bool,
}

// https://mimesniff.spec.whatwg.org/#mime-type-sniffing-algorithm
// https://mimesniff.spec.whatwg.org/#supplied-mime-type-detection-algorithm
#[must_use]
pub fn sniff_mime(
    header: &[u8],
    supplied: Option<&str>,
    no_sniff: bool,
    apache_bug: bool,
) -> String {
    let parsed = supplied.and_then(parse_mime_type);
    if parsed
        .as_ref()
        .is_some_and(|mime| mime.html() || mime.xml())
    {
        return supplied.unwrap_or("text/html").to_owned();
    }
    let unknown = parsed.is_none()
        || parsed.as_ref().is_some_and(|mime| {
            matches!(
                mime.essence.as_str(),
                "unknown/unknown" | "application/unknown" | "*/*"
            )
        });
    if unknown {
        return identify_unknown(header, !no_sniff);
    }
    if no_sniff {
        return supplied.unwrap_or_default().to_owned();
    }
    if apache_bug {
        return text_or_binary(header).to_owned();
    }
    if parsed
        .as_ref()
        .is_some_and(|mime| mime.image() && supported_image(&mime.essence))
        && let Some(matched) = match_image(header)
    {
        return matched.to_owned();
    }
    if parsed
        .as_ref()
        .is_some_and(|mime| mime.av() && supported_av(&mime.essence))
        && let Some(matched) = match_av(header)
    {
        return matched.to_owned();
    }
    supplied.unwrap_or_default().to_owned()
}

#[must_use]
pub fn apache_bug_content_type(value: &str) -> bool {
    matches!(
        value,
        "text/plain"
            | "text/plain; charset=ISO-8859-1"
            | "text/plain; charset=iso-8859-1"
            | "text/plain; charset=UTF-8"
    )
}

// https://fetch.spec.whatwg.org/#determine-nosniff
// https://fetch.spec.whatwg.org/#concept-header-list-get-decode-split
#[must_use]
pub fn nosniff(value: &str) -> bool {
    net::split_header_value(value)
        .first()
        .is_some_and(|first| first.eq_ignore_ascii_case("nosniff"))
}

#[must_use]
pub fn headers_nosniff(headers: &[(String, String)]) -> bool {
    net::combined_header_in(headers, "x-content-type-options").is_some_and(|value| nosniff(&value))
}

// https://fetch.spec.whatwg.org/#concept-header-extract-mime-type
// https://mimesniff.spec.whatwg.org/#supplied-mime-type-detection-algorithm
#[must_use]
pub fn extract_mime_type(value: &str) -> Option<String> {
    let mut mime_type: Option<MimeType> = None;
    let mut essence: Option<String> = None;
    let mut charset: Option<String> = None;
    for item in net::split_header_value(value) {
        let Some(mut parsed) = parse_mime_type(&item) else {
            continue;
        };
        if parsed.essence == "*/*" {
            continue;
        }
        if essence.as_deref() == Some(parsed.essence.as_str()) {
            if !parsed.parameters.iter().any(|(name, _)| name == "charset")
                && let Some(propagated) = charset.clone()
            {
                parsed.parameters.push(("charset".to_owned(), propagated));
            }
        } else {
            charset = parsed
                .parameters
                .iter()
                .find(|(name, _)| name == "charset")
                .map(|(_, value)| value.clone());
            essence = Some(parsed.essence.clone());
        }
        mime_type = Some(parsed);
    }
    mime_type.map(|mime| mime.serialize())
}

// https://mimesniff.spec.whatwg.org/#mime-type-representation
struct MimeType {
    essence: String,
    // https://mimesniff.spec.whatwg.org/#parse-a-mime-type
    parameters: Vec<(String, String)>,
}

impl MimeType {
    fn html(&self) -> bool {
        self.essence == "text/html"
    }

    fn xml(&self) -> bool {
        let subtype = self
            .essence
            .split_once('/')
            .map_or("", |(_, subtype)| subtype);
        subtype == "xml" || subtype.ends_with("+xml")
    }

    fn image(&self) -> bool {
        self.essence.starts_with("image/")
    }

    fn av(&self) -> bool {
        self.essence.starts_with("audio/")
            || self.essence.starts_with("video/")
            || self.essence == "application/ogg"
    }

    // https://mimesniff.spec.whatwg.org/#serialize-a-mime-type
    fn serialize(&self) -> String {
        let mut serialization = self.essence.clone();
        for (name, value) in &self.parameters {
            serialization.push(';');
            serialization.push_str(name);
            serialization.push('=');
            if value.is_empty() || !is_http_token(value) {
                serialization.push('"');
                for character in value.chars() {
                    if character == '"' || character == '\\' {
                        serialization.push('\\');
                    }
                    serialization.push(character);
                }
                serialization.push('"');
            } else {
                serialization.push_str(value);
            }
        }
        serialization
    }
}

// https://mimesniff.spec.whatwg.org/#parse-a-mime-type
fn parse_mime_type(input: &str) -> Option<MimeType> {
    let characters: Vec<char> = trim_http_ws(input).chars().collect();
    let mut position = 0;

    let mut kind = String::new();
    while position < characters.len() && characters[position] != '/' {
        kind.push(characters[position]);
        position += 1;
    }
    if kind.is_empty() || !is_http_token(&kind) || position == characters.len() {
        return None;
    }
    position += 1;

    let mut subtype = String::new();
    while position < characters.len() && characters[position] != ';' {
        subtype.push(characters[position]);
        position += 1;
    }
    while subtype.chars().last().is_some_and(is_http_whitespace) {
        subtype.pop();
    }
    if subtype.is_empty() || !is_http_token(&subtype) {
        return None;
    }

    let mut parameters: Vec<(String, String)> = Vec::new();
    while position < characters.len() {
        position += 1;
        while position < characters.len() && is_http_whitespace(characters[position]) {
            position += 1;
        }
        let mut name = String::new();
        while position < characters.len()
            && characters[position] != ';'
            && characters[position] != '='
        {
            name.push(characters[position]);
            position += 1;
        }
        let name = name.to_ascii_lowercase();
        if position < characters.len() {
            if characters[position] == ';' {
                continue;
            }
            position += 1;
        }
        if position == characters.len() {
            break;
        }
        let value = if characters[position] == '"' {
            let value = collect_http_quoted_string(&characters, &mut position);
            while position < characters.len() && characters[position] != ';' {
                position += 1;
            }
            value
        } else {
            let mut value = String::new();
            while position < characters.len() && characters[position] != ';' {
                value.push(characters[position]);
                position += 1;
            }
            while value.chars().last().is_some_and(is_http_whitespace) {
                value.pop();
            }
            if value.is_empty() {
                continue;
            }
            value
        };
        if !name.is_empty()
            && is_http_token(&name)
            && value.chars().all(is_http_quoted_string_token)
            && !parameters.iter().any(|(existing, _)| *existing == name)
        {
            // https://mimesniff.spec.whatwg.org/#parse-a-mime-type
            let value = if name == "charset" {
                value.to_ascii_lowercase()
            } else {
                value
            };
            parameters.push((name, value));
        }
    }

    Some(MimeType {
        essence: format!(
            "{}/{}",
            kind.to_ascii_lowercase(),
            subtype.to_ascii_lowercase()
        ),
        parameters,
    })
}

// https://xhr.spec.whatwg.org/#the-overridemimetype(
#[must_use]
pub fn parse_serialized_mime_type(input: &str) -> String {
    parse_mime_type(input).map_or_else(String::new, |mime| mime.serialize())
}

#[must_use]
pub fn mime_type_parts(input: &str) -> Option<(String, String)> {
    let mime = parse_mime_type(input)?;
    let boundary = mime
        .parameters
        .iter()
        .find(|(name, _)| name == "boundary")
        .map_or_else(String::new, |(_, value)| value.clone());
    Some((mime.essence, boundary))
}

// https://fetch.spec.whatwg.org/#collect-an-http-quoted-string
fn collect_http_quoted_string(characters: &[char], position: &mut usize) -> String {
    *position += 1;
    let mut value = String::new();
    loop {
        while *position < characters.len()
            && characters[*position] != '"'
            && characters[*position] != '\\'
        {
            value.push(characters[*position]);
            *position += 1;
        }
        if *position == characters.len() {
            return value;
        }
        let quote_or_backslash = characters[*position];
        *position += 1;
        if quote_or_backslash == '\\' {
            if *position == characters.len() {
                value.push('\\');
                return value;
            }
            value.push(characters[*position]);
            *position += 1;
            continue;
        }
        return value;
    }
}

fn identify_unknown(header: &[u8], scriptable: bool) -> String {
    if scriptable && let Some(found) = match_rows(header, SCRIPTABLE) {
        return found.to_owned();
    }
    if let Some(found) = match_rows(header, BINARY_PLAIN) {
        return found.to_owned();
    }
    if let Some(found) = match_image(header) {
        return found.to_owned();
    }
    if let Some(found) = match_av(header) {
        return found.to_owned();
    }
    if let Some(found) = match_rows(header, ARCHIVE) {
        return found.to_owned();
    }
    if header.iter().copied().any(is_binary_byte) {
        "application/octet-stream".to_owned()
    } else {
        "text/plain".to_owned()
    }
}

fn text_or_binary(header: &[u8]) -> &'static str {
    if header.len() >= 2 && (header.starts_with(&[0xFE, 0xFF]) || header.starts_with(&[0xFF, 0xFE]))
    {
        return "text/plain";
    }
    if header.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return "text/plain";
    }
    if header.iter().copied().any(is_binary_byte) {
        "application/octet-stream"
    } else {
        "text/plain"
    }
}

fn supported_image(essence: &str) -> bool {
    matches!(
        essence,
        "image/x-icon" | "image/bmp" | "image/gif" | "image/webp" | "image/png" | "image/jpeg"
    )
}

fn supported_av(essence: &str) -> bool {
    matches!(
        essence,
        "audio/aiff"
            | "audio/mpeg"
            | "application/ogg"
            | "audio/midi"
            | "video/avi"
            | "audio/wave"
            | "video/mp4"
            | "video/webm"
    )
}

struct Row {
    pattern: &'static [u8],
    mask: &'static [u8],
    whitespace: bool,
    terminal: bool,
    mime: &'static str,
}

fn match_rows(header: &[u8], rows: &[Row]) -> Option<&'static str> {
    rows.iter().find_map(|row| {
        matches_pattern(header, row.pattern, row.mask, row.whitespace, row.terminal)
            .then_some(row.mime)
    })
}

// https://mimesniff.spec.whatwg.org/#pattern-matching-algorithm
fn matches_pattern(
    input: &[u8],
    pattern: &[u8],
    mask: &[u8],
    whitespace: bool,
    terminal: bool,
) -> bool {
    let mut index = 0;
    if whitespace {
        while input
            .get(index)
            .is_some_and(|byte| is_whitespace_byte(*byte))
        {
            index += 1;
        }
    }
    for (pattern_index, pattern_byte) in pattern.iter().enumerate() {
        let Some(byte) = input.get(index) else {
            return false;
        };
        let Some(mask_byte) = mask.get(pattern_index) else {
            return false;
        };
        if byte & mask_byte != *pattern_byte {
            return false;
        }
        index += 1;
    }
    if terminal {
        return input
            .get(index)
            .is_some_and(|byte| is_tag_terminating(*byte));
    }
    true
}

fn match_image(header: &[u8]) -> Option<&'static str> {
    match_rows(header, IMAGE)
}

fn match_av(header: &[u8]) -> Option<&'static str> {
    match_rows(header, AUDIO_VIDEO)
        .or_else(|| mp4_signature(header).then_some("video/mp4"))
        .or_else(|| webm_signature(header).then_some("video/webm"))
        .or_else(|| mp3_signature(header).then_some("audio/mpeg"))
}

fn mp4_signature(sequence: &[u8]) -> bool {
    if sequence.len() < 12 {
        return false;
    }
    let box_size = u32::from_be_bytes([sequence[0], sequence[1], sequence[2], sequence[3]]);
    let box_size = usize::try_from(box_size).unwrap_or(usize::MAX);
    if sequence.len() < box_size || !box_size.is_multiple_of(4) {
        return false;
    }
    if sequence.get(4..8) != Some(b"ftyp".as_slice()) {
        return false;
    }
    if sequence.get(8..11) == Some(b"mp4".as_slice()) {
        return true;
    }
    let mut bytes_read = 16;
    while bytes_read < box_size {
        if sequence.get(bytes_read..bytes_read.saturating_add(3)) == Some(b"mp4".as_slice()) {
            return true;
        }
        bytes_read += 4;
    }
    false
}

fn webm_signature(sequence: &[u8]) -> bool {
    if sequence.get(..4) != Some([0x1A, 0x45, 0xDF, 0xA3].as_slice()) {
        return false;
    }
    let mut iter = 4;
    while iter < sequence.len() && iter < 38 {
        if sequence.get(iter..iter.saturating_add(2)) == Some([0x42, 0x82].as_slice()) {
            iter += 2;
            let Some(number_size) = parse_vint(sequence, iter) else {
                return false;
            };
            iter += number_size;
            if iter >= sequence.len().saturating_sub(4) {
                return false;
            }
            if padded_webm(sequence, iter) {
                return true;
            }
        }
        iter += 1;
    }
    false
}

// https://mimesniff.spec.whatwg.org/#parse-a-vint
fn parse_vint(sequence: &[u8], start: usize) -> Option<usize> {
    let mut mask = 0x80_u16;
    let mut number_size = 1;
    let mut index = start;
    while number_size < 8 && number_size < sequence.len() {
        let byte = u16::from(*sequence.get(index)?);
        if byte & mask != 0 {
            break;
        }
        mask >>= 1;
        number_size += 1;
        index += 1;
    }
    Some(number_size)
}

fn padded_webm(sequence: &[u8], offset: usize) -> bool {
    let Some(slice) = sequence.get(offset..) else {
        return false;
    };
    let leading = slice
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(slice.len());
    slice
        .get(leading..)
        .is_some_and(|rest| rest.starts_with(b"webm"))
}

fn mp3_signature(sequence: &[u8]) -> bool {
    if !mp3_header(sequence, 0) {
        return false;
    }
    let Some(skipped) = mp3_frame_size(sequence, 0) else {
        return false;
    };
    // https://mimesniff.spec.whatwg.org/#signature-for-mp3-without-id3
    if skipped < 4 || skipped > sequence.len() {
        return false;
    }
    mp3_header(sequence, skipped)
}

fn mp3_header(sequence: &[u8], offset: usize) -> bool {
    let Some(bytes) = sequence.get(offset..offset.saturating_add(4)) else {
        return false;
    };
    if bytes[0] != 0xFF || bytes[1] & 0xE0 != 0xE0 {
        return false;
    }
    let layer = (bytes[1] & 0x06) >> 1;
    if layer == 0 {
        return false;
    }
    let bit_rate = (bytes[2] & 0xF0) >> 4;
    if bit_rate == 15 {
        return false;
    }
    let sample_rate = (bytes[2] & 0x0C) >> 2;
    if sample_rate == 3 {
        return false;
    }
    4 - layer == 3
}

fn mp3_frame_size(sequence: &[u8], offset: usize) -> Option<usize> {
    let bytes = sequence.get(offset..offset.saturating_add(4))?;
    let version = (bytes[1] & 0x18) >> 3;
    let bitrate_index = usize::from((bytes[2] & 0xF0) >> 4);
    let bitrate = if version & 0x01 == 0 {
        *MP3_RATES.get(bitrate_index)?
    } else {
        *MP25_RATES.get(bitrate_index)?
    };
    let sample_index = usize::from((bytes[2] & 0x0C) >> 2);
    let mut sample_rate = *SAMPLE_RATES.get(sample_index)?;
    if version == 2 {
        sample_rate /= 2;
    } else if version == 0 {
        sample_rate /= 4;
    }
    if sample_rate == 0 {
        return None;
    }
    let scale = if version == 1 { 72 } else { 144 };
    let mut size = bitrate * scale / sample_rate;
    if (bytes[2] & 0x02) >> 1 != 0 {
        size += 1;
    }
    Some(size)
}

const MP3_RATES: [usize; 15] = [
    0, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000, 192_000,
    224_000, 256_000, 320_000,
];
const MP25_RATES: [usize; 15] = [
    0, 8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000,
    128_000, 144_000, 160_000,
];
const SAMPLE_RATES: [usize; 3] = [44_100, 48_000, 32_000];

fn is_binary_byte(byte: u8) -> bool {
    (0x00..=0x08).contains(&byte)
        || byte == 0x0B
        || (0x0E..=0x1A).contains(&byte)
        || (0x1C..=0x1F).contains(&byte)
}

fn is_whitespace_byte(byte: u8) -> bool {
    matches!(byte, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn is_tag_terminating(byte: u8) -> bool {
    matches!(byte, 0x20 | 0x3E)
}

// https://fetch.spec.whatwg.org/#http-whitespace
fn is_http_whitespace(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | ' ')
}

fn trim_http_ws(value: &str) -> &str {
    value.trim_matches(is_http_whitespace)
}

// https://mimesniff.spec.whatwg.org/#http-token-code-point
fn is_http_token_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty() && value.chars().all(is_http_token_char)
}

// https://mimesniff.spec.whatwg.org/#http-quoted-string-token-code-point
fn is_http_quoted_string_token(character: char) -> bool {
    character == '\t'
        || ('\u{20}'..='\u{7e}').contains(&character)
        || ('\u{80}'..='\u{ff}').contains(&character)
}

const fn row(
    pattern: &'static [u8],
    mask: &'static [u8],
    whitespace: bool,
    terminal: bool,
    mime: &'static str,
) -> Row {
    assert!(pattern.len() == mask.len());
    Row {
        pattern,
        mask,
        whitespace,
        terminal,
        mime,
    }
}

const SCRIPTABLE: &[Row] = &[
    row(
        b"<!DOCTYPE HTML",
        b"\xFF\xFF\xDF\xDF\xDF\xDF\xDF\xDF\xDF\xFF\xDF\xDF\xDF\xDF",
        true,
        true,
        "text/html",
    ),
    row(b"<HTML", b"\xFF\xDF\xDF\xDF\xDF", true, true, "text/html"),
    row(b"<HEAD", b"\xFF\xDF\xDF\xDF\xDF", true, true, "text/html"),
    row(
        b"<SCRIPT",
        b"\xFF\xDF\xDF\xDF\xDF\xDF\xDF",
        true,
        true,
        "text/html",
    ),
    row(
        b"<IFRAME",
        b"\xFF\xDF\xDF\xDF\xDF\xDF\xDF",
        true,
        true,
        "text/html",
    ),
    row(b"<H1", b"\xFF\xDF\xFF", true, true, "text/html"),
    row(b"<DIV", b"\xFF\xDF\xDF\xDF", true, true, "text/html"),
    row(b"<FONT", b"\xFF\xDF\xDF\xDF\xDF", true, true, "text/html"),
    row(
        b"<TABLE",
        b"\xFF\xDF\xDF\xDF\xDF\xDF",
        true,
        true,
        "text/html",
    ),
    row(b"<A", b"\xFF\xDF", true, true, "text/html"),
    row(
        b"<STYLE",
        b"\xFF\xDF\xDF\xDF\xDF\xDF",
        true,
        true,
        "text/html",
    ),
    row(
        b"<TITLE",
        b"\xFF\xDF\xDF\xDF\xDF\xDF",
        true,
        true,
        "text/html",
    ),
    row(b"<B", b"\xFF\xDF", true, true, "text/html"),
    row(b"<BODY", b"\xFF\xDF\xDF\xDF\xDF", true, true, "text/html"),
    row(b"<BR", b"\xFF\xDF\xDF", true, true, "text/html"),
    row(b"<P", b"\xFF\xDF", true, true, "text/html"),
    row(b"<!--", b"\xFF\xFF\xFF\xFF", true, true, "text/html"),
    row(b"<?xml", b"\xFF\xFF\xFF\xFF\xFF", true, false, "text/xml"),
    row(
        b"%PDF-",
        b"\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "application/pdf",
    ),
];

const BINARY_PLAIN: &[Row] = &[
    row(
        b"%!PS-Adobe-",
        b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "application/postscript",
    ),
    row(
        &[0xFE, 0xFF, 0x00, 0x00],
        &[0xFF, 0xFF, 0x00, 0x00],
        false,
        false,
        "text/plain",
    ),
    row(
        &[0xFF, 0xFE, 0x00, 0x00],
        &[0xFF, 0xFF, 0x00, 0x00],
        false,
        false,
        "text/plain",
    ),
    row(
        &[0xEF, 0xBB, 0xBF, 0x00],
        &[0xFF, 0xFF, 0xFF, 0x00],
        false,
        false,
        "text/plain",
    ),
];

const IMAGE: &[Row] = &[
    row(
        &[0x00, 0x00, 0x01, 0x00],
        &[0xFF, 0xFF, 0xFF, 0xFF],
        false,
        false,
        "image/x-icon",
    ),
    row(
        &[0x00, 0x00, 0x02, 0x00],
        &[0xFF, 0xFF, 0xFF, 0xFF],
        false,
        false,
        "image/x-icon",
    ),
    row(b"BM", b"\xFF\xFF", false, false, "image/bmp"),
    row(
        b"GIF87a",
        b"\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "image/gif",
    ),
    row(
        b"GIF89a",
        b"\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "image/gif",
    ),
    row(
        b"RIFF\0\0\0\0WEBPVP",
        b"\xFF\xFF\xFF\xFF\0\0\0\0\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "image/webp",
    ),
    row(
        &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        false,
        false,
        "image/png",
    ),
    row(
        &[0xFF, 0xD8, 0xFF],
        &[0xFF, 0xFF, 0xFF],
        false,
        false,
        "image/jpeg",
    ),
];

const AUDIO_VIDEO: &[Row] = &[
    row(
        b"FORM\0\0\0\0AIFF",
        b"\xFF\xFF\xFF\xFF\0\0\0\0\xFF\xFF\xFF\xFF",
        false,
        false,
        "audio/aiff",
    ),
    row(b"ID3", b"\xFF\xFF\xFF", false, false, "audio/mpeg"),
    row(
        b"OggS\0",
        b"\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "application/ogg",
    ),
    row(
        b"MThd\0\0\0\x06",
        b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "audio/midi",
    ),
    row(
        b"RIFF\0\0\0\0AVI ",
        b"\xFF\xFF\xFF\xFF\0\0\0\0\xFF\xFF\xFF\xFF",
        false,
        false,
        "video/avi",
    ),
    row(
        b"RIFF\0\0\0\0WAVE",
        b"\xFF\xFF\xFF\xFF\0\0\0\0\xFF\xFF\xFF\xFF",
        false,
        false,
        "audio/wave",
    ),
];

const ARCHIVE: &[Row] = &[
    row(
        &[0x1F, 0x8B, 0x08],
        &[0xFF, 0xFF, 0xFF],
        false,
        false,
        "application/x-gzip",
    ),
    row(
        &[0x50, 0x4B, 0x03, 0x04],
        &[0xFF, 0xFF, 0xFF, 0xFF],
        false,
        false,
        "application/zip",
    ),
    row(
        b"Rar!\x1A\x07\x00",
        b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        false,
        "application/x-rar-compressed",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> String {
        parse_serialized_mime_type(input)
    }

    #[test]
    fn case_is_normalized_and_charset_is_ascii_lowercased() {
        assert_eq!(parsed("TEXT/HTML;CHARSET=GBK"), "text/html;charset=gbk");
    }

    #[test]
    fn an_incomplete_parameter_does_not_fail_the_parse() {
        assert_eq!(parsed("text/html;"), "text/html");
        assert_eq!(parsed("text/html;charset="), "text/html");
        assert_eq!(parsed("text/html;a"), "text/html");
    }

    #[test]
    fn values_that_are_not_tokens_are_quoted_on_write() {
        assert_eq!(
            parsed("text/html;charset=gbk("),
            "text/html;charset=\"gbk(\""
        );
        assert_eq!(
            parsed("text/html;charset= gbk"),
            "text/html;charset=\" gbk\""
        );
        assert_eq!(
            parsed("text/html;charset= \"gbk\""),
            "text/html;charset=\" \\\"gbk\\\"\""
        );
        assert_eq!(
            parsed("text/html;x=(;charset=gbk"),
            "text/html;x=\"(\";charset=gbk"
        );
        assert_eq!(
            parsed("text/html;charset=();charset=GBK"),
            "text/html;charset=\"()\""
        );
    }

    #[test]
    fn quoted_values_are_unescaped_then_requoted_when_needed() {
        assert_eq!(
            parsed("text/html;charset=\"shift_jis\""),
            "text/html;charset=shift_jis"
        );
        assert_eq!(
            parsed("text/html;charset=\"a\\\"b\""),
            "text/html;charset=\"a\\\"b\""
        );
        assert_eq!(
            parsed("text/html;charset=\"shift_jis\"iso-2022-jp"),
            "text/html;charset=shift_jis"
        );
    }

    #[test]
    fn a_space_between_the_name_and_the_equals_drops_the_parameter() {
        assert_eq!(parsed("text/html;charset =gbk"), "text/html");
    }

    #[test]
    fn duplicate_parameters_keep_the_first() {
        assert_eq!(
            parsed("text/html;charset=gbk;charset=windows-1255"),
            "text/html;charset=gbk"
        );
    }

    #[test]
    fn whitespace_around_separators_follows_the_algorithm() {
        assert_eq!(parsed("text/html ;charset=gbk"), "text/html;charset=gbk");
        assert_eq!(parsed("text/html; charset=gbk"), "text/html;charset=gbk");
        assert_eq!(parsed(" text/html "), "text/html");
    }

    #[test]
    fn invalid_types_serialize_to_nothing() {
        for input in [
            "",
            "/",
            "text/",
            "/html",
            "text /html",
            "text/ html",
            "text/html\u{000c}",
        ] {
            assert_eq!(parsed(input), "", "{input:?}");
        }
        assert_eq!(parsed("*/*"), "*/*");
    }

    #[test]
    fn a_parameter_value_outside_the_quoted_string_set_is_dropped() {
        assert_eq!(parsed("text/html;charset=a\u{0100}"), "text/html");
    }

    fn extracted(input: &str) -> Option<String> {
        extract_mime_type(input)
    }

    #[test]
    fn extract_keeps_the_last_item_that_parses() {
        assert_eq!(
            extracted("text/html, text/plain").as_deref(),
            Some("text/plain")
        );
        assert_eq!(
            extracted("text/plain, cannot-parse").as_deref(),
            Some("text/plain")
        );
        assert_eq!(extracted("text/plain,").as_deref(), Some("text/plain"));
        assert_eq!(extracted("text/html, */*").as_deref(), Some("text/html"));
        assert_eq!(extracted("*/*, text/plain").as_deref(), Some("text/plain"));
        assert_eq!(extracted("*/*").as_deref(), None);
        assert_eq!(extracted("").as_deref(), None);
    }

    #[test]
    fn extract_carries_a_charset_across_a_matching_essence() {
        assert_eq!(
            extracted("text/plain;charset=gbk, text/plain").as_deref(),
            Some("text/plain;charset=gbk")
        );
        assert_eq!(
            extracted("text/plain;charset=gbk;x=foo, text/plain").as_deref(),
            Some("text/plain;charset=gbk")
        );
        assert_eq!(
            extracted("text/plain;charset=gbk, text/html").as_deref(),
            Some("text/html")
        );
        assert_eq!(
            extracted("text/plain;charset=gbk, text/plain;charset=windows-1252").as_deref(),
            Some("text/plain;charset=windows-1252")
        );
        assert_eq!(
            extracted("text/html;charset=gbk, text/html;x=\",text/plain\"").as_deref(),
            Some("text/html;x=\",text/plain\";charset=gbk")
        );
    }

    #[test]
    fn nosniff_reads_only_the_first_item() {
        assert!(nosniff("nosniff"));
        assert!(nosniff(" nosniff\t"));
        assert!(nosniff("NOSNIFF, nosniff"));
        assert!(nosniff("nosniff, nosniff"));
        assert!(!nosniff("foo, nosniff"));
        assert!(!nosniff(""));
        assert!(!nosniff("nosniff-foo"));
        assert!(!nosniff("\u{a0}nosniff"));
    }

    #[test]
    fn an_empty_first_field_is_not_nosniff() {
        let field = |value: &str| ("X-Content-Type-Options".to_owned(), value.to_owned());
        assert!(headers_nosniff(&[field("nosniff")]));
        assert!(!headers_nosniff(&[field(""), field("nosniff")]));
        assert!(!headers_nosniff(&[]));
    }
}
