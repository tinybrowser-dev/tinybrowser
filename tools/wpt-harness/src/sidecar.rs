use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use net::{NetError, TransportError, WireRequest, WireResponse};

const SIDECAR_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub struct Sidecar {
    address: SocketAddr,
}

impl Sidecar {
    #[must_use]
    pub fn new(address: SocketAddr) -> Self {
        Self { address }
    }

    pub fn exchange(&self, request: &WireRequest) -> Result<WireResponse, NetError> {
        let mut stream = TcpStream::connect_timeout(&self.address, SIDECAR_TIMEOUT)
            .map_err(|error| transport(&format!("sidecar connect: {error}")))?;
        stream
            .set_read_timeout(Some(SIDECAR_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(SIDECAR_TIMEOUT)))
            .map_err(|error| transport(&format!("sidecar timeout: {error}")))?;
        let head = request_head(request);
        stream
            .write_all(&head)
            .map_err(|error| transport(&format!("sidecar write: {error}")))?;
        if let Some(body) = &request.body {
            stream
                .write_all(body)
                .map_err(|error| transport(&format!("sidecar write: {error}")))?;
        }
        stream
            .flush()
            .map_err(|error| transport(&format!("sidecar flush: {error}")))?;
        read_response(stream)
    }
}

fn request_head(request: &WireRequest) -> Vec<u8> {
    let mut target = request.url.path().to_owned();
    if let Some(query) = request.url.query() {
        target.push('?');
        target.push_str(query);
    }
    let host = request.url.host_str().unwrap_or("web-platform.test");
    let host = match request.url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let mut out = Vec::new();
    let _ = write!(out, "{} {target} HTTP/1.1\r\n", request.method);
    let _ = write!(out, "Host: {host}\r\n");
    let _ = write!(out, "X-WPT-Scheme: {}\r\n", request.url.scheme());
    for (name, value) in request.headers.iter() {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding"
        ) {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    let _ = write!(out, "Connection: close\r\n");
    match &request.body {
        Some(body) => {
            let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len());
            out.extend_from_slice(body);
        }
        None => out.extend_from_slice(b"\r\n"),
    }
    out
}

fn read_response(stream: TcpStream) -> Result<WireResponse, NetError> {
    let mut reader = BufReader::new(stream);
    let status_line = read_line(&mut reader)?;
    let status = status_line
        .as_bytes()
        .split(|byte| *byte == b' ')
        .nth(1)
        .and_then(|code| std::str::from_utf8(code).ok())
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| transport("sidecar sent no status line"))?;
    let mut headers = net::HeaderMap::new();
    let mut chunked = false;
    let mut content_length = None;
    loop {
        let line = read_line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = split_header(line.as_bytes()) else {
            return Err(transport("sidecar sent a malformed header"));
        };
        let lower = name.to_ascii_lowercase();
        if lower == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if lower == "content-length" {
            content_length = value.trim().parse::<usize>().ok();
        }
        headers
            .insert(&name, value.as_bytes().to_vec())
            .map_err(|_| transport("sidecar sent an invalid header"))?;
    }
    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(length) = content_length {
        let mut body = vec![0; length];
        reader
            .read_exact(&mut body)
            .map_err(|error| transport(&format!("sidecar body: {error}")))?;
        body
    } else {
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|error| transport(&format!("sidecar body: {error}")))?;
        body
    };
    let body = http_body_util::Full::new(Bytes::from(body))
        .map_err(|infallible| match infallible {})
        .boxed_unsync();
    Ok(WireResponse {
        status,
        headers,
        body,
        http2: false,
    })
}

fn read_line(reader: &mut impl BufRead) -> Result<String, NetError> {
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .map_err(|error| transport(&format!("sidecar read: {error}")))?;
    while line
        .last()
        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
    {
        let _ = line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn split_header(line: &[u8]) -> Option<(String, String)> {
    let colon = line.iter().position(|byte| *byte == b':')?;
    let name = String::from_utf8_lossy(&line[..colon]).trim().to_owned();
    let value = String::from_utf8_lossy(&line[colon + 1..])
        .trim()
        .to_owned();
    (!name.is_empty()).then_some((name, value))
}

// https://httpwg.org/specs/rfc9112.html#chunked.encoding
fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, NetError> {
    let mut body = Vec::new();
    loop {
        let size = read_line(reader)?;
        let size = size.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| transport("sidecar sent a bad chunk size"))?;
        if size == 0 {
            while !read_line(reader)?.is_empty() {}
            return Ok(body);
        }
        let mut chunk = vec![0; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| transport(&format!("sidecar chunk: {error}")))?;
        body.extend_from_slice(&chunk);
        let _end = read_line(reader)?;
    }
}

fn transport(reason: &str) -> NetError {
    NetError::Transport(TransportError::Connect(reason.into()))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::{read_chunked, split_header};

    #[test]
    fn chunks_are_decoded_and_trailers_skipped() {
        let raw = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\nX-Trace: 1\r\n\r\n";
        let mut reader = BufReader::new(Cursor::new(raw));
        assert_eq!(
            read_chunked(&mut reader).expect("chunked body"),
            b"Wikipedia"
        );
    }

    #[test]
    fn a_header_splits_at_its_first_colon() {
        assert_eq!(
            split_header(b"Content-Type: text/html; charset=utf-8"),
            Some((
                "Content-Type".to_owned(),
                "text/html; charset=utf-8".to_owned()
            ))
        );
        assert_eq!(split_header(b"no-colon"), None);
    }
}
