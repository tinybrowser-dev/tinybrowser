// https://www.rfc-editor.org/rfc/rfc6455#section-4.2.1

use std::future::Future;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use http::{HeaderValue, Method, StatusCode, header};
use http_body_util::Full;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use sha1::{Digest as _, Sha1};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

use crate::{Request, Response, text};

pub type WebSocket = WebSocketStream<TokioIo<Upgraded>>;
pub use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Clone, Copy)]
enum HandshakeRejection {
    HttpVersion,
    Method,
    NotWebsocket,
    MissingKey,
    VersionUnsupported,
}

#[must_use]
pub fn is_upgrade_request(request: &Request) -> bool {
    request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
}

pub fn upgrade<F, Fut>(request: Request, handler: F) -> Response
where
    F: FnOnce(WebSocket) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let key = match upgrade_key(&request) {
        Ok(key) => key,
        Err(rejection) => return rejection_response(rejection),
    };
    let accept = accept_key(&key);
    let upgraded = hyper::upgrade::on(request);
    tokio::spawn(async move {
        let Ok(io) = upgraded.await else { return };
        let config = WebSocketConfig::default()
            .max_message_size(Some(8 * 1024 * 1024))
            .max_frame_size(Some(1024 * 1024));
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, Some(config)).await;
        handler(socket).await;
    });
    switching_protocols(&accept)
}

fn upgrade_key(request: &Request) -> Result<String, HandshakeRejection> {
    if request.version() != http::Version::HTTP_11 {
        return Err(HandshakeRejection::HttpVersion);
    }
    if request.method() != Method::GET {
        return Err(HandshakeRejection::Method);
    }
    let headers = request.headers();
    let has_upgrade = headers
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let has_connection = headers.get(header::CONNECTION).is_some_and(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        })
    });
    if !has_upgrade || !has_connection {
        return Err(HandshakeRejection::NotWebsocket);
    }
    let version_ok = headers
        .get(header::SEC_WEBSOCKET_VERSION)
        .is_some_and(|value| value.as_bytes() == b"13");
    if !version_ok {
        return Err(HandshakeRejection::VersionUnsupported);
    }
    headers
        .get(header::SEC_WEBSOCKET_KEY)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or(HandshakeRejection::MissingKey)
}

fn rejection_response(rejection: HandshakeRejection) -> Response {
    match rejection {
        HandshakeRejection::HttpVersion => {
            text(StatusCode::UPGRADE_REQUIRED, "upgrade requires HTTP/1.1")
        }
        HandshakeRejection::Method => text(
            StatusCode::METHOD_NOT_ALLOWED,
            "websocket upgrade requires GET",
        ),
        HandshakeRejection::NotWebsocket => {
            text(StatusCode::BAD_REQUEST, "not a websocket request")
        }
        HandshakeRejection::MissingKey => text(StatusCode::BAD_REQUEST, "missing websocket key"),
        HandshakeRejection::VersionUnsupported => {
            let mut response = text(
                StatusCode::UPGRADE_REQUIRED,
                "unsupported websocket version",
            );
            response.headers_mut().insert(
                header::SEC_WEBSOCKET_VERSION,
                HeaderValue::from_static("13"),
            );
            response
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            response
        }
    }
}

fn switching_protocols(accept: &str) -> Response {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response
        .headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    match HeaderValue::from_str(accept) {
        Ok(value) => {
            response
                .headers_mut()
                .insert(header::SEC_WEBSOCKET_ACCEPT, value);
        }
        Err(_) => {
            *response.status_mut() = StatusCode::BAD_REQUEST;
        }
    }
    response
}

fn accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    BASE64_STANDARD.encode(hasher.finalize())
}
