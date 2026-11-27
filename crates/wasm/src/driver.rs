// https://fetch.spec.whatwg.org/#concept-main-fetch

use std::time::Instant;

use dial::DialHead;
use dial::body::{BodyEvent, BodyReader};
use net::{Agent, InitiatorKind, Method, Request};
use renderer::{Mount, TabState};
use url::Url;

pub(crate) async fn navigate(
    agent: Agent,
    url: Url,
    overrides: net::RequestOverrides,
) -> Result<Mount, String> {
    let deadline = Instant::now() + dial::PAGE_FETCH_TIMEOUT;
    let mut request = Request::new(Method::GET, url.clone());
    dial::apply_navigation_headers(&mut request, &url);
    request.initiator_kind = InitiatorKind::Navigation;
    request.cors = net::cors::CorsSettings {
        mode: net::cors::RequestMode::Navigate,
        credentials: net::cors::CredentialsMode::Include,
        redirect: net::cors::RedirectMode::Follow,
        origin: url::Origin::new_opaque(),
    };
    request.deadline = Some(deadline);
    request.overrides = overrides;
    let response = agent
        .send(request)
        .await
        .map_err(|error| format!("{:?}", dial::dial_failure(&error)))?;
    let (head_status, head_url, head_content_type, head_headers) =
        DialHead::internal(&response).into_http_parts();
    let head =
        renderer::ResponseHead::http(head_status, head_url, head_content_type, &head_headers);
    let mut reader = BodyReader::new(response, None, true).await;
    let body = loop {
        match reader.next().await {
            BodyEvent::Chunk(_) => {}
            BodyEvent::Done(bytes) => break bytes,
            BodyEvent::DecodeError => return Err("body decode error".into()),
            BodyEvent::Failed(failure) => return Err(format!("{failure:?}")),
        }
    };
    Ok(Mount {
        head,
        tab: TabState::default(),
        body,
    })
}
