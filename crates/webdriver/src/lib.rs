// https://w3c.github.io/webdriver/
// https://w3c.github.io/webdriver-bidi/

mod bidi;
mod classic;
mod session;

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use browser::BrowserHandle;
use http::{HeaderValue, Method, StatusCode, header};
use server::{Request, Response, Shutdown};
use tokio::sync::Mutex;

use crate::session::Sessions;

#[derive(Clone)]
pub struct Router {
    state: AppState,
}

impl Router {
    #[must_use]
    pub fn new(browser: BrowserHandle) -> Self {
        Self::with_endpoint(browser, None, Shutdown::new())
    }

    #[must_use]
    pub fn with_endpoint(
        browser: BrowserHandle,
        bound: impl Into<Option<SocketAddr>>,
        shutdown: Shutdown,
    ) -> Self {
        let sessions = Arc::new(Mutex::new(Sessions::new(browser, bound.into(), shutdown)));
        Self {
            state: AppState { sessions },
        }
    }

    pub async fn handle(&self, request: Request) -> Response {
        serve_request(request, self.state.clone()).await
    }
}

pub async fn serve(listener: &TcpListener, browser: &BrowserHandle) -> std::io::Result<()> {
    let bound = listener.local_addr().ok();
    let shutdown = Shutdown::new();
    let router = Router::with_endpoint(browser.clone(), bound, shutdown.clone());
    server::serve(listener, shutdown, router, |request, router| async move {
        router.handle(request).await
    })
    .await
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) sessions: Arc<Mutex<Sessions>>,
}

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

async fn serve_request(request: Request, state: AppState) -> Response {
    if !server::is_loopback_host(&request) {
        return with_no_cache(server::text(
            StatusCode::FORBIDDEN,
            "non-loopback Host header rejected",
        ));
    }
    if server::has_origin(&request) {
        return with_no_cache(server::text(
            StatusCode::FORBIDDEN,
            "Origin header rejected",
        ));
    }
    if server::has_browser_fetch_metadata(&request) {
        return with_no_cache(server::text(
            StatusCode::FORBIDDEN,
            "Sec-Fetch-* header rejected",
        ));
    }

    // https://w3c.github.io/webdriver-bidi/#transport
    if server::ws::is_upgrade_request(&request) {
        let path = request.uri().path().to_owned();
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let initial_session = match segments.as_slice() {
            ["session"] => None,
            ["session", session_id] => Some((*session_id).to_owned()),
            _ => {
                return with_no_cache(server::text(
                    StatusCode::NOT_FOUND,
                    "unknown WebDriver BiDi endpoint",
                ));
            }
        };
        if let Some(ref sid) = initial_session {
            let guard = state.sessions.lock().await;
            if !guard.open.contains_key(sid) {
                return with_no_cache(server::json(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({
                        "value": {
                            "error": "invalid session id",
                            "message": sid,
                            "stacktrace": "",
                        }
                    }),
                ));
            }
        }
        let sessions = Arc::clone(&state.sessions);
        return server::ws::upgrade(request, move |socket| async move {
            bidi::serve_websocket(socket, sessions, initial_session).await;
        });
    }

    if matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH
    ) && request.headers().contains_key(header::CONTENT_TYPE)
        && !server::is_json_content_type(&request)
    {
        return with_no_cache(server::text(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        ));
    }
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let body = match server::read_body(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(server::BodyError::TooLarge) => {
            return with_no_cache(server::text(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body too large",
            ));
        }
        Err(server::BodyError::Read) => {
            return with_no_cache(server::text(
                StatusCode::BAD_REQUEST,
                "invalid request body",
            ));
        }
    };
    let (status, value) = classic::dispatch(&method, &path, &body, &state.sessions).await;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    with_no_cache(server::json(status, &value))
}

// https://w3c.github.io/webdriver/#protocol
fn with_no_cache(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}
