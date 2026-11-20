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
use net::Agent;
use renderer::{DialFailure, DialOutcome, DialRequest, ResponseBody};
use url::Url;

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
