//! One page dial, from the renderer's request to a buffered outcome.
//!
//! This is the component's whole network policy: [`dial`] shapes the request,
//! [`net::Agent`] runs the fetch algorithm over [`crate::HostTransport`], and
//! the body is read under the renderer's cap. The native browser carrier runs
//! the same three pieces, so a page sees the same redirects, cookies, CORS, and
//! referrer here as it does on a socket
//! (<https://fetch.spec.whatwg.org/#concept-main-fetch>).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use dial::DialHead;
use dial::body::{BodyEvent, BodyReader};
use net::{Agent, InitiatorKind, Method, Request};
use renderer::{DialFailure, DialOutcome, DialRequest, Mount, ResponseBody, TabState};
use url::Url;

/// Fetches a top-level document through the shared fetch core and turns it
/// into a [`Mount`] ready for the renderer.
pub(crate) async fn navigate(agent: Agent, url: Url) -> Result<Mount, String> {
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
    let response = agent
        .send(request)
        .await
        .map_err(|error| format!("{:?}", dial::dial_failure(&error)))?;
    let head = DialHead::internal(&response).into_document_head();
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

/// Runs one dial to completion, including its body.
///
/// The component's transport answers with whole bodies, so this buffers: a
/// page that asked for a streamed body receives the same bytes in one piece.
pub(crate) async fn run(
    agent: Agent,
    request: DialRequest,
    initiator: url::Origin,
) -> Result<DialOutcome, DialFailure> {
    let deadline = Instant::now() + dial::budget(&request);
    // This engine keeps no style-sheet bookkeeping, so the explicit-referrer
    // authority reduces to same-origin referrers.
    let sheets: HashMap<url::Origin, HashSet<Url>> = HashMap::new();
    let mut outbound = dial::outbound_request(&request, &initiator, deadline, &sheets)?;
    let blocked = dial::watch_redirects(&request, &initiator, &mut outbound);
    let response = match agent.send(outbound).await {
        Ok(response) => response,
        Err(error) => {
            // A redirect the policy refused is an outcome, not a failure: the
            // renderer reports the violation and drops the body.
            return dial::refused_redirect(&blocked).ok_or_else(|| dial::dial_failure(&error));
        }
    };
    // An opaque response's body is not readable, so a request that named
    // integrity metadata cannot be shown the bytes it would cover
    // (<https://fetch.spec.whatwg.org/#concept-main-fetch>).
    if request.integrity.is_some() && response.response_type().is_opaque() {
        return Err(DialFailure::Connect);
    }
    let head = DialHead::read(&request, &response);
    let body = if head.null_body() {
        ResponseBody::Bytes(Vec::new())
    } else {
        let mut reader = BodyReader::new(response, request.integrity.clone(), true).await;
        loop {
            match reader.next().await {
                BodyEvent::Chunk(_) => {}
                BodyEvent::Done(bytes) => break ResponseBody::Bytes(bytes),
                BodyEvent::DecodeError => break ResponseBody::Undecodable,
                BodyEvent::Failed(failure) => return Err(failure),
            }
        }
    };
    Ok(head.into_outcome(body))
}
