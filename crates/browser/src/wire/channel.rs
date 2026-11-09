//! Length-prefixed frames for the renderer platform channel.
//!
//! One private full-duplex channel per renderer process. Every frame starts
//! with a fixed 16-byte header:
//!
//! ```text
//! version: u8 | kind: u8 | flags: u16 (zero) | request id: u64 | length: u32
//! ```
//!
//! The version byte is [`PROTOCOL_VERSION`]; a mismatch fails closed. Control
//! payloads are postcard-encoded and capped at [`MAX_CONTROL_BYTES`]. Stream
//! chunks are raw bytes and capped at [`MAX_BODY_CHUNK_BYTES`]. Readers validate
//! the header and length before they allocate or read the payload, so a corrupt
//! or hostile peer cannot force an unbounded allocation.
//!
//! The first frame in each direction is the handshake: the browser sends
//! [`HostNotice::Hello`](crate::wire::HostNotice::Hello) and the renderer replies
//! [`RendererNotice::Ready`](crate::wire::RendererNotice::Ready).

use std::io::{self, Read, Write};

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Framing and message ABI version for the renderer channel.
///
/// Version 5 added the storage service calls and the browser-broadcast
/// `storage` event. Version 6 added `window.open`/`window.close`. Version 7
/// added cross-tab messaging, session copies, and remote session reads.
/// Version 8 added `BroadcastChannel`. Version 9 replaced operation-specific
/// body messages with directional exchange stream chunks. Version 10 dropped
/// `content_language` from mount, response-start, and dial outcomes. Version 11
/// replaced JSON control payloads with postcard. Version 12 replaced
/// response-start's flat metadata fields with one shared response head.
/// Version 13 folded a dial outcome's body and stream into one
/// [`ResponseBody`](renderer::ResponseBody), made the request method a typed
/// value, and renumbered `BodyDelivery` (now `Buffer` = 0, `Stream` = 1) with
/// the removal of `Discard`. Version 14 replaced a request's kind and optional
/// CORS parameters with one [`RequestPolicy`](renderer::RequestPolicy), and a
/// page request's mode with [`CorsMode`](renderer::CorsMode), which has no
/// `navigate`. Version 15 made the handshake carry the browser identity, so
/// every page and CDP answer reports the same product as the request headers.
/// Version 16 typed the values a navigation and a window open carry (origins,
/// the window-open target and URL) and added the named-tab lookup and
/// navigation calls. Version 17 put the secure-context bit on the policy
/// container. Version 18 gave a mount and a streamed response the browsing
/// context state the browser owns (target name, opener relationship, popup
/// sandboxing flag set) and added the tab rename call. Version 19 made the
/// calling document's sandbox set part of a navigation source and dropped the
/// `window.open` flags the browser now derives from its own copy. Version 20
/// typed browsing-context names (non-empty, never a keyword) and merged the
/// name lookup and navigation into one call. Version 21 separates a
/// `window.name` assignment, which writes the active history entry, from the
/// cross-origin reset. Version 22 adds the error for a history entry a
/// traversal names but the tab no longer holds. Version 23 reports frame
/// creation and teardown so the browser owns every frame's sandbox flags, and
/// drops the renderer-supplied sandbox set from a navigation source. Version
/// 24 moved session history into one browser-owned joint list and added the
/// per-frame history commands. Version 25 puts the owning tab id on a mount so
/// a popup tab closes itself through the same path as its opener.
// Bumped for the browser-owned joint session history (child navigation-start
// events and per-frame history snapshots) and for the tab id a mount carries
// so a popup tab can close itself.
pub const PROTOCOL_VERSION: u8 = 26;

/// Fixed frame header size in bytes.
pub const HEADER_BYTES: usize = 16;

/// Maximum encoded size of one control payload.
///
/// Storage is the largest legal control payload. One origin may hold
/// [`webstorage::STORAGE_QUOTA_BYTES`] (5 MiB) of encoded keys and values, and a
/// `storage` event carries both the old and the new encoded value, so the worst
/// legal frame is roughly twice the quota plus field overhead. A cap below that
/// bound turns a legal `setItem` into a transport error, and an oversize control
/// frame stops the renderer, so the cap keeps 2.4× headroom over the bound until
/// bulk payloads move to the chunk lane.
pub const MAX_CONTROL_BYTES: usize = 24 * 1024 * 1024;

/// Maximum size of one raw response-body chunk.
pub const MAX_BODY_CHUNK_BYTES: usize = 64 * 1024;

const KIND_CONTROL: u8 = 0;
const KIND_REQUEST_CHUNK: u8 = 1;
const KIND_RESPONSE_CHUNK: u8 = 2;

/// Payload class of one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// Encoded control message.
    Control,
    /// Raw bytes uploaded with a request.
    RequestChunk,
    /// Raw bytes downloaded with a response.
    ResponseChunk,
}

impl FrameKind {
    fn wire(self) -> u8 {
        match self {
            Self::Control => KIND_CONTROL,
            Self::RequestChunk => KIND_REQUEST_CHUNK,
            Self::ResponseChunk => KIND_RESPONSE_CHUNK,
        }
    }

    fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            KIND_CONTROL => Some(Self::Control),
            KIND_REQUEST_CHUNK => Some(Self::RequestChunk),
            KIND_RESPONSE_CHUNK => Some(Self::ResponseChunk),
            _ => None,
        }
    }

    fn max_payload(self) -> usize {
        match self {
            Self::Control => MAX_CONTROL_BYTES,
            Self::RequestChunk | Self::ResponseChunk => MAX_BODY_CHUNK_BYTES,
        }
    }
}

/// One decoded frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Payload class.
    pub kind: FrameKind,
    /// Request this frame belongs to; zero for control frames.
    pub request: u64,
}

/// Writes one control message as a postcard frame.
///
/// # Errors
///
/// Serialization failure or a payload larger than [`MAX_CONTROL_BYTES`].
pub fn write_control<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let payload = postcard::to_allocvec(message).map_err(io::Error::other)?;
    write_frame(writer, FrameKind::Control, 0, &payload)
}

/// Writes one raw body chunk for `request`.
///
/// # Errors
///
/// A payload larger than [`MAX_BODY_CHUNK_BYTES`] or write failure.
#[cfg(test)]
pub fn write_body(writer: &mut impl Write, request: u64, payload: &[u8]) -> io::Result<()> {
    write_frame(writer, FrameKind::ResponseChunk, request, payload)
}

/// Writes one frame with the fixed header.
///
/// # Errors
///
/// A payload larger than the kind limit or write failure.
pub fn write_frame(
    writer: &mut impl Write,
    kind: FrameKind,
    request: u64,
    payload: &[u8],
) -> io::Result<()> {
    let header = encode_header(kind, request, payload.len())?;
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    writer.flush()
}

/// Writes one control message as a postcard frame on an async writer.
///
/// # Errors
///
/// Serialization failure or a payload larger than [`MAX_CONTROL_BYTES`].
pub async fn write_control_async<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    message: &impl Serialize,
) -> io::Result<()> {
    let payload = postcard::to_allocvec(message).map_err(io::Error::other)?;
    write_frame_async(writer, FrameKind::Control, 0, &payload).await
}

/// Writes one frame with the fixed header on an async writer.
///
/// # Errors
///
/// A payload larger than the kind limit or write failure.
pub async fn write_frame_async<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    kind: FrameKind,
    request: u64,
    payload: &[u8],
) -> io::Result<()> {
    let header = encode_header(kind, request, payload.len())?;
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

fn encode_header(kind: FrameKind, request: u64, length: usize) -> io::Result<[u8; HEADER_BYTES]> {
    if length > kind.max_payload() {
        return Err(invalid("renderer IPC payload exceeds limit"));
    }
    let length =
        u32::try_from(length).map_err(|_| invalid("renderer IPC payload exceeds limit"))?;
    let mut header = [0u8; HEADER_BYTES];
    header[0] = PROTOCOL_VERSION;
    header[1] = kind.wire();
    // header[2..4] is a zero flags field; readers reject nonzero values.
    header[4..12].copy_from_slice(&request.to_be_bytes());
    header[12..16].copy_from_slice(&length.to_be_bytes());
    Ok(header)
}

fn decode_header(header: &[u8; HEADER_BYTES]) -> io::Result<(FrameKind, u64, usize)> {
    if header[0] != PROTOCOL_VERSION {
        return Err(invalid("unsupported renderer IPC version"));
    }
    let kind = FrameKind::from_wire(header[1])
        .ok_or_else(|| invalid("unknown renderer IPC frame kind"))?;
    if header[2] != 0 || header[3] != 0 {
        return Err(invalid("nonzero renderer IPC flags"));
    }
    let request = u64::from_be_bytes([
        header[4], header[5], header[6], header[7], header[8], header[9], header[10], header[11],
    ]);
    let length = u32::from_be_bytes([header[12], header[13], header[14], header[15]]) as usize;
    if length > kind.max_payload() {
        return Err(invalid("renderer IPC frame exceeds limit"));
    }
    Ok((kind, request, length))
}

/// Reads one frame header and payload into `payload`.
///
/// `Ok(None)` is a clean peer close at a frame boundary. Every other failure is
/// deterministic: an unknown version or kind, nonzero flags, an oversized
/// length, or a truncated header or payload leaves the channel unusable.
///
/// # Errors
///
/// I/O failure or a protocol violation.
pub fn read_frame(reader: &mut impl Read, payload: &mut Vec<u8>) -> io::Result<Option<Frame>> {
    let mut header = [0u8; HEADER_BYTES];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    read_exact(reader, &mut header[1..])?;
    let (kind, request, length) = decode_header(&header)?;
    payload.clear();
    payload.resize(length, 0);
    read_exact(reader, payload)?;
    Ok(Some(Frame { kind, request }))
}

/// Reads one frame header and payload into `payload` on an async reader.
///
/// `Ok(None)` is a clean peer close at a frame boundary.
///
/// # Errors
///
/// I/O failure or a protocol violation.
pub async fn read_frame_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    payload: &mut Vec<u8>,
) -> io::Result<Option<Frame>> {
    let mut header = [0u8; HEADER_BYTES];
    if reader.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    read_exact_async(reader, &mut header[1..]).await?;
    let (kind, request, length) = decode_header(&header)?;
    payload.clear();
    payload.resize(length, 0);
    read_exact_async(reader, payload).await?;
    Ok(Some(Frame { kind, request }))
}

/// Reads one control message.
///
/// # Errors
///
/// I/O failure, a body frame, an invalid payload, or a protocol violation.
#[cfg(test)]
pub fn read_control<T: DeserializeOwned>(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> io::Result<Option<T>> {
    match read_frame(reader, buffer)? {
        None => Ok(None),
        Some(Frame {
            kind: FrameKind::Control,
            ..
        }) => decode_control(buffer).map(Some),
        Some(_) => Err(invalid("expected a control frame")),
    }
}

/// Reads one raw body chunk, returning its request id.
///
/// # Errors
///
/// I/O failure, a control frame, or a protocol violation.
#[cfg(test)]
pub fn read_body(reader: &mut impl Read, buffer: &mut Vec<u8>) -> io::Result<Option<u64>> {
    match read_frame(reader, buffer)? {
        None => Ok(None),
        Some(Frame {
            kind: FrameKind::ResponseChunk,
            request,
        }) => Ok(Some(request)),
        Some(_) => Err(invalid("expected a body frame")),
    }
}

/// Decodes one control-frame payload after its frame header has been read.
///
/// The frame length is the payload's exact extent, so anything left over after
/// the message is a framing bug, not slack. `postcard::from_bytes` would drop
/// the remainder silently.
///
/// # Errors
///
/// Invalid postcard bytes, a payload that does not match `T`, or trailing bytes.
pub fn decode_control<T: DeserializeOwned>(payload: &[u8]) -> io::Result<T> {
    let (message, trailing) = postcard::take_from_bytes(payload)
        .map_err(|error| invalid(format!("invalid renderer IPC payload: {error}")))?;
    if trailing.is_empty() {
        Ok(message)
    } else {
        Err(invalid("renderer IPC payload has trailing bytes"))
    }
}

fn read_exact(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<()> {
    reader
        .read_exact(buffer)
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => invalid("truncated renderer IPC frame"),
            _ => error,
        })
}

async fn read_exact_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    buffer: &mut [u8],
) -> io::Result<()> {
    reader
        .read_exact(buffer)
        .await
        .map(|_| ())
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => invalid("truncated renderer IPC frame"),
            _ => error,
        })
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// One message through the real frame codec and back.
    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(message: &T) -> T {
        let mut bytes = Vec::new();
        write_control(&mut bytes, message).expect("write");
        read_control(&mut Cursor::new(bytes), &mut Vec::new())
            .expect("read")
            .expect("one frame")
    }

    fn header(kind: u8, flags: u16, request: u64, length: u32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_BYTES);
        bytes.push(PROTOCOL_VERSION);
        bytes.push(kind);
        bytes.extend_from_slice(&flags.to_be_bytes());
        bytes.extend_from_slice(&request.to_be_bytes());
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes
    }

    #[test]
    fn non_finite_numbers_survive_the_wire() {
        let message: crate::wire::FromRenderer = crate::exchange::Frame::Reply {
            id: crate::exchange::RequestId::new(9),
            body: crate::wire::RendererReply {
                assignment: crate::wire::RendererAssignmentId::new(1),
                reply: crate::wire::Reply::Value(Ok(renderer::RemoteValue::Number(f64::NAN))),
            },
        };
        let back: crate::wire::FromRenderer = round_trip(&message);
        let crate::exchange::Frame::Reply {
            body: crate::wire::RendererReply { reply, .. },
            ..
        } = back
        else {
            panic!("expected a reply frame");
        };
        match reply {
            crate::wire::Reply::Value(Ok(renderer::RemoteValue::Number(number))) => {
                assert!(number.is_nan());
            }
            other => panic!("expected NaN, got {other:?}"),
        }
    }

    #[test]
    fn every_remote_value_variant_survives_the_wire() {
        // Postcard is not self-describing and rejects `deserialize_any`, so a
        // value shape it cannot decode fails only when it first crosses, not at
        // compile time. The JSON codec's `serde(with)` number shim was exactly
        // that trap; walk every variant through the real frame codec.
        let values = [
            renderer::RemoteValue::Undefined,
            renderer::RemoteValue::Null,
            renderer::RemoteValue::Bool(true),
            renderer::RemoteValue::Number(1.5),
            renderer::RemoteValue::Number(-0.0),
            renderer::RemoteValue::Number(f64::INFINITY),
            renderer::RemoteValue::String("x".into()),
            renderer::RemoteValue::List(vec![
                renderer::RemoteValue::Number(1.0),
                renderer::RemoteValue::Null,
            ]),
            renderer::RemoteValue::Map(vec![("k".into(), renderer::RemoteValue::Node(7))]),
            renderer::RemoteValue::Node(9),
        ];
        for (raw_id, value) in (1_u64..).zip(values) {
            let expected = format!("{value:?}");
            let message: crate::wire::FromRenderer = crate::exchange::Frame::Reply {
                id: crate::exchange::RequestId::new(raw_id),
                body: crate::wire::RendererReply {
                    assignment: crate::wire::RendererAssignmentId::new(1),
                    reply: crate::wire::Reply::Value(Ok(value)),
                },
            };
            let back: crate::wire::FromRenderer = round_trip(&message);
            let crate::exchange::Frame::Reply {
                body: crate::wire::RendererReply { reply, .. },
                ..
            } = back
            else {
                panic!("expected a reply frame");
            };
            let crate::wire::Reply::Value(Ok(back)) = reply else {
                panic!("expected a value reply");
            };
            // `Debug` keeps `NaN` comparable and distinguishes `-0.0`.
            assert_eq!(format!("{back:?}"), expected, "round trip");
        }
    }

    /// Every request policy survives the wire unchanged. The enum is the
    /// record of what a dial is, so a variant that cannot round-trip is a dial
    /// the browser would send with another mode.
    #[test]
    fn a_dial_request_round_trips_every_policy() {
        use renderer::{CorsMode, CorsRequest, RequestPolicy};
        let cors = CorsRequest {
            mode: CorsMode::SameOrigin,
            credentials: net::cors::CredentialsMode::Include,
            redirect: net::cors::RedirectMode::Follow,
        };
        for policy in [
            RequestPolicy::Fetch(cors),
            RequestPolicy::ClassicScript {
                crossorigin: Some(net::cors::CrossOrigin::UseCredentials),
            },
            RequestPolicy::WorkerScript,
            RequestPolicy::ModuleScript {
                crossorigin: Some(net::cors::CrossOrigin::Anonymous),
            },
            RequestPolicy::ModuleScript { crossorigin: None },
            RequestPolicy::Stylesheet { crossorigin: None },
            RequestPolicy::Image {
                crossorigin: Some(net::cors::CrossOrigin::Anonymous),
            },
            RequestPolicy::Font,
            RequestPolicy::FrameLoad,
            RequestPolicy::CspReportTo,
            RequestPolicy::CspReportUri,
        ] {
            let url = url::Url::parse("https://a.test/x").expect("URL");
            // A different origin from the request URL, so the round trip
            // proves the initiator travels on its own field
            let initiator = url::Url::parse("https://b.test/").expect("origin");
            let request = renderer::DialRequest::new(policy, &url, &initiator.origin());
            let message: crate::wire::FromRenderer = crate::exchange::Frame::Call {
                id: crate::exchange::RequestId::new(1),
                body: crate::wire::BrowserCall {
                    assignment: crate::wire::RendererAssignmentId::new(1),
                    call: crate::wire::ServiceCall::Network(crate::wire::NetworkCall::Dial(
                        Box::new(request),
                    )),
                },
            };
            let back: crate::wire::FromRenderer = round_trip(&message);
            let crate::exchange::Frame::Call {
                body: crate::wire::BrowserCall { call, .. },
                ..
            } = back
            else {
                panic!("expected a call frame");
            };
            let crate::wire::ServiceCall::Network(crate::wire::NetworkCall::Dial(request)) = call
            else {
                panic!("expected a dial");
            };
            assert_eq!(request.policy, policy, "the policy survives the wire");
            assert_eq!(
                request.initiator,
                renderer::SerializedOrigin::Tuple(initiator.origin()),
                "the initiator is the caller's origin, not the request URL"
            );
        }
    }

    /// Both response-head shapes survive the postcard wire. The origin variant
    /// decides whether the document has HTTP status and policy headers, so a
    /// decoder that could not name it would drop every navigation.
    #[test]
    fn a_response_head_round_trips_both_origins() {
        use renderer::{ResponseHead, ResponseSource};
        let url = url::Url::parse("https://a.test/").expect("URL");
        let http = ResponseHead::http(
            204,
            url.clone(),
            Some("text/html".to_owned()),
            &[("Referrer-Policy".to_owned(), "origin".to_owned())],
        );
        let local = ResponseHead::local(
            url.clone(),
            None,
            Some(renderer::PolicyContainer::default()),
        );
        let inherited = ResponseHead::local(
            url,
            None,
            Some(renderer::PolicyContainer {
                referrer_policy: Some(net::ReferrerPolicy::StrictOrigin),
                ..renderer::PolicyContainer::default()
            }),
        );
        for head in [http, local, inherited] {
            let tab = renderer::TabState {
                id: Some(renderer::WindowId::new(1)),
                name: renderer::ContextName::parse("peer"),
                auxiliary_with_opener: true,
                popup_sandbox: renderer::SandboxFlags::parse(Some("allow-scripts allow-popups")),
                secure_context: Some(false),
            };
            let message: crate::wire::ToRenderer = crate::exchange::Frame::Call {
                id: crate::exchange::RequestId::new(1),
                body: crate::wire::RendererCall::Response {
                    response: Box::new(crate::wire::ResponseStart {
                        assignment: crate::wire::RendererAssignmentId::new(1),
                        frame: renderer::FrameId::MAIN,
                        head: head.clone(),
                        tab: tab.clone(),
                    }),
                },
            };
            let back: crate::wire::ToRenderer = round_trip(&message);
            let crate::exchange::Frame::Call {
                body: crate::wire::RendererCall::Response { response },
                ..
            } = back
            else {
                panic!("expected a response frame");
            };
            assert_eq!(response.head.url, head.url);
            assert_eq!(response.head.source.is_http(), head.source.is_http());
            assert_eq!(response.head.source.status(), head.source.status());
            assert_eq!(
                response.head.content_type.as_deref(),
                head.content_type.as_deref()
            );
            assert_eq!(response.tab, tab, "the browsing-context state survives");
            match (&response.head.source, &head.source) {
                (ResponseSource::Http(back), ResponseSource::Http(sent)) => {
                    assert_eq!(back.refresh, sent.refresh);
                    assert_eq!(
                        back.policy.referrer_policy, sent.policy.referrer_policy,
                        "the parsed policy survives"
                    );
                }
                (
                    ResponseSource::Local { inherited: back },
                    ResponseSource::Local { inherited: sent },
                ) => {
                    assert_eq!(
                        back.as_ref().and_then(|policy| policy.referrer_policy),
                        sent.as_ref().and_then(|policy| policy.referrer_policy),
                        "the inherited policy survives"
                    );
                }
                _ => panic!("the response origin changed shape on the wire"),
            }
        }
    }

    /// A posted message survives the wire for every origin shape, including
    /// the opaque `"null"` sender and an exact opaque target. The target was
    /// once rejected on decode, which stopped the whole read loop.
    #[test]
    fn a_posted_message_round_trips_every_origin_shape() {
        use renderer::{PostedMessage, SerializedOrigin, TargetOrigin, WindowId};
        let tuple = url::Url::parse("https://a.test/").expect("URL");
        let opaque_target = url::Url::parse("about:blank").expect("URL");
        let cases = [
            PostedMessage {
                origin: SerializedOrigin::Opaque,
                target_origin: TargetOrigin::Any,
                payload: "v1|opaque".to_owned(),
            },
            PostedMessage {
                origin: SerializedOrigin::from(&tuple.origin()),
                target_origin: TargetOrigin::Exact(tuple.origin()),
                payload: "v1|tuple".to_owned(),
            },
            PostedMessage {
                origin: SerializedOrigin::from(&tuple.origin()),
                target_origin: TargetOrigin::Exact(opaque_target.origin()),
                payload: "v1|opaque target".to_owned(),
            },
        ];
        for message in cases {
            let sent: crate::wire::FromRenderer = crate::exchange::Frame::Call {
                id: crate::exchange::RequestId::new(1),
                body: crate::wire::BrowserCall {
                    assignment: crate::wire::RendererAssignmentId::new(1),
                    call: crate::wire::ServiceCall::BrowsingContext(
                        crate::wire::BrowsingContextCall::WindowMessage {
                            window: WindowId::new(7),
                            message: message.clone(),
                        },
                    ),
                },
            };
            let back: crate::wire::FromRenderer = round_trip(&sent);
            let crate::exchange::Frame::Call {
                body:
                    crate::wire::BrowserCall {
                        call:
                            crate::wire::ServiceCall::BrowsingContext(
                                crate::wire::BrowsingContextCall::WindowMessage {
                                    message: back,
                                    ..
                                },
                            ),
                        ..
                    },
                ..
            } = back
            else {
                panic!("expected a window message");
            };
            assert_eq!(back.payload, message.payload);
            match (&back.origin, &message.origin) {
                (SerializedOrigin::Opaque, SerializedOrigin::Opaque) => {}
                (SerializedOrigin::Tuple(back), SerializedOrigin::Tuple(sent)) => {
                    assert_eq!(back, sent);
                }
                _ => panic!("the sender origin changed on the wire"),
            }
            match (&back.target_origin, &message.target_origin) {
                (TargetOrigin::Any, TargetOrigin::Any) => {}
                (TargetOrigin::Exact(back), TargetOrigin::Exact(sent)) => {
                    assert_eq!(
                        matches!(back, url::Origin::Opaque(_)),
                        matches!(sent, url::Origin::Opaque(_)),
                        "an opaque target stays opaque"
                    );
                    if matches!(sent, url::Origin::Tuple(..)) {
                        assert_eq!(back, sent);
                    }
                }
                _ => panic!("the target origin changed on the wire"),
            }
        }
    }

    /// The handshake carries the identity, so the renderer's pages, its CSP
    /// reports, and CDP answers report the product the request headers do
    /// (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
    #[test]
    fn a_handshake_carries_the_browser_identity() {
        let identity = net::ValidIdentity::try_from(net::Identity {
            user_agent: net::UserAgent::parse("probe/1").expect("user agent"),
            brands: vec![net::Brand {
                brand: "Probe".to_owned(),
                version: "7".to_owned(),
            }],
            ..net::Identity::default()
        })
        .expect("identity");
        let message: crate::wire::ToRenderer =
            crate::exchange::Frame::Notify(crate::wire::HostNotice::Hello {
                identity: Box::new(identity.clone()),
            });
        let back: crate::wire::ToRenderer = round_trip(&message);
        let crate::exchange::Frame::Notify(crate::wire::HostNotice::Hello { identity: back }) =
            back
        else {
            panic!("expected a handshake");
        };
        assert_eq!(*back, identity);
    }

    #[test]
    fn control_frames_round_trip() {
        let message: crate::wire::ToRenderer = crate::exchange::Frame::Call {
            id: crate::exchange::RequestId::new(7),
            body: crate::wire::RendererCall::Command {
                assignment: crate::wire::RendererAssignmentId::new(1),
                command: crate::wire::Command::ExecuteScript {
                    frame: renderer::FrameId::MAIN,
                    source: renderer::ScriptSource::Page("1+1".into()),
                    timeout_ms: None,
                },
            },
        };
        let back: crate::wire::ToRenderer = round_trip(&message);
        assert!(matches!(
            back,
            crate::exchange::Frame::Call { id, .. } if id.get() == 7
        ));
    }

    #[test]
    fn body_frames_round_trip_at_the_cap() {
        let payload = vec![7u8; MAX_BODY_CHUNK_BYTES];
        let mut bytes = Vec::new();
        write_body(&mut bytes, 42, &payload).expect("write");
        let mut reader = Cursor::new(bytes);
        let mut buffer = Vec::new();
        let request = read_body(&mut reader, &mut buffer)
            .expect("read")
            .expect("one frame");
        assert_eq!(request, 42);
        assert_eq!(buffer, payload);
        assert!(
            read_body(&mut reader, &mut buffer)
                .expect("clean close")
                .is_none()
        );
    }

    #[test]
    fn oversized_payloads_fail_before_write() {
        let error = write_body(&mut Vec::new(), 1, &vec![0u8; MAX_BODY_CHUNK_BYTES + 1])
            .expect_err("oversized body");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let message: crate::wire::ToRenderer = crate::exchange::Frame::Call {
            id: crate::exchange::RequestId::new(1),
            body: crate::wire::RendererCall::Command {
                assignment: crate::wire::RendererAssignmentId::new(1),
                command: crate::wire::Command::ExecuteScript {
                    frame: renderer::FrameId::MAIN,
                    source: renderer::ScriptSource::Page("x".repeat(MAX_CONTROL_BYTES)),
                    timeout_ms: None,
                },
            },
        };
        let error = write_control(&mut Vec::new(), &message).expect_err("oversized control");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_lengths_fail_before_allocation() {
        for (kind, length) in [
            (
                KIND_CONTROL,
                u32::try_from(MAX_CONTROL_BYTES + 1).expect("fits"),
            ),
            (
                KIND_REQUEST_CHUNK,
                u32::try_from(MAX_BODY_CHUNK_BYTES + 1).expect("fits"),
            ),
        ] {
            let mut reader = Cursor::new(header(kind, 0, 0, length));
            let mut buffer = Vec::new();
            let error = read_frame(&mut reader, &mut buffer).expect_err("oversized length");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn malformed_headers_fail_closed() {
        let mut buffer = Vec::new();

        let mut bad_version = header(KIND_CONTROL, 0, 0, 0);
        bad_version[0] = PROTOCOL_VERSION + 1;
        assert!(read_frame(&mut Cursor::new(bad_version), &mut buffer).is_err());

        assert!(read_frame(&mut Cursor::new(header(9, 0, 0, 0)), &mut buffer).is_err());
        assert!(read_frame(&mut Cursor::new(header(KIND_CONTROL, 1, 0, 0)), &mut buffer).is_err());

        assert!(
            read_control::<crate::wire::ToRenderer>(
                &mut Cursor::new(header(KIND_RESPONSE_CHUNK, 0, 1, 0)),
                &mut buffer
            )
            .is_err()
        );
        assert!(read_body(&mut Cursor::new(header(KIND_CONTROL, 0, 0, 0)), &mut buffer).is_err());
    }

    #[test]
    fn control_payloads_reject_trailing_bytes() {
        let message: crate::wire::ToRenderer =
            crate::exchange::Frame::Notify(crate::wire::HostNotice::Shutdown);
        let mut payload = postcard::to_allocvec(&message).expect("encode");
        assert!(decode_control::<crate::wire::ToRenderer>(&payload).is_ok());
        payload.push(0);
        assert!(decode_control::<crate::wire::ToRenderer>(&payload).is_err());
    }

    #[test]
    fn truncated_and_invalid_control_frames_fail_closed() {
        let mut buffer = Vec::new();

        let mut partial = header(KIND_CONTROL, 0, 0, 4);
        partial.push(b'{');
        assert!(read_frame(&mut Cursor::new(partial), &mut buffer).is_err());

        assert!(
            read_control::<crate::wire::ToRenderer>(
                &mut Cursor::new(header(KIND_CONTROL, 0, 0, 0)),
                &mut buffer
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn closed_channel_reports_a_clean_close() {
        use std::os::unix::net::UnixStream;

        let (mut host, child) = UnixStream::pair().expect("socket pair");
        let mut writer = child.try_clone().expect("clone");
        write_body(&mut writer, 3, b"abc").expect("write");
        drop(writer);
        drop(child);

        let mut buffer = Vec::new();
        let request = read_body(&mut host, &mut buffer)
            .expect("read")
            .expect("one frame");
        assert_eq!(request, 3);
        assert_eq!(buffer, b"abc");
        assert!(
            read_frame(&mut host, &mut buffer)
                .expect("closed channel")
                .is_none()
        );
    }

    /// Every field of a `window.open` call crosses the wire: the source, the
    /// target, the URL (present or absent), and the opener policy
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    #[test]
    fn a_window_open_caller_survives_the_wire() {
        let source = renderer::NavigationSource {
            origin: renderer::SerializedOrigin::Opaque,
            referrer: url::Url::parse("https://a.test/page").expect("referrer"),
            referrer_policy: net::ReferrerPolicy::UnsafeUrl,
            frame: renderer::FrameId::new(7),
            secure_context: false,
        };
        let cases = [
            (
                Some(url::Url::parse("https://b.test/").expect("url")),
                renderer::TargetName::Named(
                    renderer::ContextName::parse("peer").expect("a test name"),
                ),
                renderer::OpenerPolicy::NoOpener,
                source.clone(),
            ),
            (
                None,
                renderer::TargetName::Blank,
                renderer::OpenerPolicy::Keep,
                renderer::NavigationSource {
                    origin: renderer::SerializedOrigin::Tuple(
                        url::Url::parse("https://a.test/page")
                            .expect("origin")
                            .origin(),
                    ),
                    frame: renderer::FrameId::MAIN,
                    ..source
                },
            ),
        ];
        for (url, target, opener, source) in cases {
            let call = crate::wire::ServiceCall::BrowsingContext(
                crate::wire::BrowsingContextCall::WindowOpen {
                    url: url.clone(),
                    target: target.clone(),
                    opener,
                    source: source.clone(),
                },
            );
            let message: crate::wire::FromRenderer = crate::exchange::Frame::Call {
                id: crate::exchange::RequestId::new(1),
                body: crate::wire::BrowserCall {
                    assignment: crate::wire::RendererAssignmentId::new(1),
                    call,
                },
            };
            let back: crate::wire::FromRenderer = round_trip(&message);
            let crate::exchange::Frame::Call { body, .. } = back else {
                panic!("expected a call frame");
            };
            let crate::wire::ServiceCall::BrowsingContext(
                crate::wire::BrowsingContextCall::WindowOpen {
                    url: back_url,
                    target: back_target,
                    opener: back_opener,
                    source: back_source,
                },
            ) = body.call
            else {
                panic!("expected a window.open call");
            };
            assert_eq!(back_url, url);
            assert_eq!(back_target, target);
            assert_eq!(back_opener, opener);
            assert_eq!(back_source, source);
        }
    }
}
