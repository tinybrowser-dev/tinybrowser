//! Browser side of the renderer seam: factory, handles, and async routing.
//!
//! One private platform channel per renderer, length-prefixed frames, async
//! reader and writer tasks, and oneshot replies. The handle stays value-only.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::process::Child;

use crate::actor::TabId;
use crate::assignment::{Assignment, AssignmentRegistry};
use crate::broadcast::{BroadcastMessage, ContextEvent, StorageBroadcast};
use crate::browser::TabRegistry;
use crate::context::PartitionServices;
use crate::exchange::{self, Frame, RequestId, ServerInput};
use crate::manager::RendererId;
use crate::site::Site;
use crate::storage::SessionStorage;
use crate::wire::{
    BrowserCall, BrowsingContextCall, Command as RendererCommand, FromRenderer, HostNotice,
    MessagingCall, NetworkCall, RendererAssignmentId, RendererCall, RendererNotice, RendererReply,
    Reply, ResponseStart, ServiceCall, ServiceReply, StorageCall, ToRenderer,
};
use renderer::{BrowserEvent, FrameId, Mount, StorageChange, StorageKey, StorageKind, TabError};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite};
use tokio::process::Command;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use url::Url;

/// Upper bound on one request to a renderer. The renderer budget is seconds;
/// this is a last-resort wake-up if its reply path dies silently. A renderer
/// that misses it is interrupted so the tab can re-acquire a fresh process
/// instead of paying the timeout on every later operation.
///
/// `TINYBROWSER_RENDERER_REQUEST_TIMEOUT_MS` overrides it so recovery tests
/// do not wait a full minute.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The effective renderer request deadline.
fn request_timeout() -> Duration {
    std::env::var_os("TINYBROWSER_RENDERER_REQUEST_TIMEOUT_MS")
        .and_then(|value| value.to_str()?.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(REQUEST_TIMEOUT, Duration::from_millis)
}

/// Upper bound for one downloaded renderer payload such as a PNG.
const MAX_RENDERER_STREAM_BYTES: usize = 64 * 1024 * 1024;

/// How long a `renderer` child has to send [`RendererNotice::Ready`].
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long teardown waits for transport tasks to finish.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded browser-to-renderer command queue.
const COMMAND_CAPACITY: usize = 256;

type RendererClient =
    exchange::Client<RendererCall, ServiceReply, HostNotice, RendererReply, RendererNotice>;
type BrowserServiceServer =
    exchange::Server<RendererCall, ServiceReply, HostNotice, BrowserCall, RendererNotice>;
type BrowserServiceResponder = exchange::Responder<RendererCall, ServiceReply, HostNotice>;
type RendererRouter = exchange::Router<BrowserCall, RendererReply, RendererNotice>;
type ResponseUpload = exchange::Upload<RendererCall, ServiceReply, HostNotice, RendererReply>;

/// Value-only transport handle to one renderer process.
pub(crate) struct RendererHandle {
    /// Process identity, checked against an assignment's host before serving
    /// a renderer call: the assignment registry is shared by every process, so
    /// a call must be bound to the process that sent it.
    pub(crate) id: RendererId,
    client: RendererClient,
    pub(crate) alive: Arc<AtomicBool>,
    kill: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) site: Arc<Mutex<Option<Site>>>,
    /// Live assignments counted against this process. The assignment registry
    /// increments while the manager's state lock is held; release decrements
    /// before the manager re-evaluates shutdown.
    pub(crate) assignments: AtomicUsize,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

pub(crate) struct ResponseWriter {
    assignment: RendererAssignmentId,
    process: Arc<RendererHandle>,
    upload: ResponseUpload,
}

impl ResponseWriter {
    pub(crate) async fn write(&self, payload: Vec<u8>) -> Result<(), TabError> {
        if payload.len() > crate::wire::channel::MAX_BODY_CHUNK_BYTES {
            return Err(TabError::RendererUnavailable {
                message: "response chunk exceeds IPC limit".into(),
            });
        }
        if let Ok(result) = timeout(request_timeout(), self.upload.write(payload)).await {
            result.map_err(|_| TabError::ActorStopped)
        } else {
            self.process.interrupt();
            Err(TabError::ActorStopped)
        }
    }

    pub(crate) async fn finish(self) -> Result<Reply, TabError> {
        let Self {
            assignment,
            process,
            upload,
        } = self;
        let response = if let Ok(result) = timeout(request_timeout(), upload.finish()).await {
            result.map_err(|_| TabError::ActorStopped)?
        } else {
            process.interrupt();
            return Err(TabError::ActorStopped);
        };
        decode_reply(assignment, response)
    }

    pub(crate) async fn abort(self, failure: renderer::DialFailure) -> Result<Reply, TabError> {
        let Self {
            assignment,
            process,
            upload,
        } = self;
        let response = if let Ok(result) =
            timeout(request_timeout(), upload.abort(format!("{failure:?}"))).await
        {
            result.map_err(|_| TabError::ActorStopped)?
        } else {
            process.interrupt();
            return Err(TabError::ActorStopped);
        };
        decode_reply(assignment, response)
    }
}

impl RendererHandle {
    pub(crate) fn bind(&self, site: &Site) -> io::Result<()> {
        let mut lock = self.site.lock().unwrap_or_else(PoisonError::into_inner);
        match &*lock {
            Some(existing) if existing != site => {
                Err(io::Error::other("renderer has a different site lock"))
            }
            Some(_) => Ok(()),
            None => {
                *lock = Some(site.clone());
                Ok(())
            }
        }
    }

    /// Tells the renderer to create the engine for one reserved assignment.
    pub(crate) async fn assign_notify(&self, assignment: RendererAssignmentId) -> io::Result<()> {
        self.client
            .notify(HostNotice::Assign { assignment })
            .await
            .map_err(|_| io::Error::other("renderer stopped"))
    }

    /// Best-effort release notice from the assignment registry's drop path.
    /// A dead renderer needs none, and a full lane must not disconnect the
    /// shared transport, so the notice is delivered on its own task instead of
    /// a `try_notify` that closes the exchange when the lane is full.
    pub(crate) fn try_notify_release(&self, assignment: RendererAssignmentId) {
        let notice = HostNotice::Release { assignment };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let client = self.client.clone();
                runtime.spawn(async move {
                    let _ = client.notify(notice).await;
                });
            }
            Err(_) => {
                let _ = self.client.try_notify_lossy(notice);
            }
        }
    }

    /// Sends one command and waits for its reply.
    ///
    /// # Errors
    ///
    /// [`TabError::ActorStopped`] when the renderer is gone or the reply does
    /// not arrive within the request timeout. A timeout also interrupts the
    /// renderer: a process that stopped answering must not keep its tab
    /// hostage.
    pub(crate) async fn request(
        &self,
        assignment: RendererAssignmentId,
        command: RendererCommand,
    ) -> Result<Reply, TabError> {
        let response = if let Ok(result) = timeout(
            request_timeout(),
            self.client.call(RendererCall::Command {
                assignment,
                command,
            }),
        )
        .await
        {
            result.map_err(|_| TabError::ActorStopped)?
        } else {
            self.interrupt();
            return Err(TabError::ActorStopped);
        };
        decode_reply(assignment, response)
    }

    /// Sends one command whose reply is a streamed byte payload and waits for
    /// it to complete.
    ///
    /// # Errors
    ///
    /// [`TabError::ActorStopped`] when the renderer is gone, the stream is
    /// incomplete when the process dies, or the reply times out.
    pub(crate) async fn request_bytes(
        &self,
        assignment: RendererAssignmentId,
        command: RendererCommand,
    ) -> Result<Vec<u8>, TabError> {
        let (response, bytes) = if let Ok(result) = timeout(
            request_timeout(),
            self.client.call_download(
                RendererCall::Command {
                    assignment,
                    command,
                },
                MAX_RENDERER_STREAM_BYTES,
            ),
        )
        .await
        {
            result.map_err(|_| TabError::ActorStopped)?
        } else {
            self.interrupt();
            return Err(TabError::ActorStopped);
        };
        match decode_reply(assignment, response)? {
            Reply::Screenshot {
                result: Ok(expected),
            } if usize::try_from(expected).ok() == Some(bytes.len()) => Ok(bytes),
            Reply::Screenshot { result: Ok(_) } => Err(TabError::RendererUnavailable {
                message: "screenshot stream length mismatch".into(),
            }),
            Reply::Screenshot { result: Err(error) } => Err(error),
            other => Err(TabError::RendererUnavailable {
                message: format!("unexpected reply for byte request: {other:?}"),
            }),
        }
    }

    /// Streams one top-level response to the renderer and waits for its mount
    /// result. The body is carried only in bounded raw IPC frames.
    pub(crate) async fn mount(
        self: &Arc<Self>,
        assignment: RendererAssignmentId,
        frame: FrameId,
        mount: Mount,
    ) -> Result<Reply, TabError> {
        let response = self.start_response(assignment, frame, &mount).await?;
        for chunk in mount
            .body
            .chunks(crate::wire::channel::MAX_BODY_CHUNK_BYTES)
        {
            response.write(chunk.to_vec()).await?;
        }
        response.finish().await
    }

    pub(crate) async fn start_response(
        self: &Arc<Self>,
        assignment: RendererAssignmentId,
        frame: FrameId,
        mount: &Mount,
    ) -> Result<ResponseWriter, TabError> {
        let start = Box::new(ResponseStart {
            assignment,
            frame,
            head: mount.head.clone(),
            tab: mount.tab.clone(),
        });
        let upload = if let Ok(result) = timeout(
            request_timeout(),
            self.client
                .begin_upload(RendererCall::Response { response: start }),
        )
        .await
        {
            result.map_err(|_| TabError::ActorStopped)?
        } else {
            self.interrupt();
            return Err(TabError::ActorStopped);
        };
        Ok(ResponseWriter {
            assignment,
            process: Arc::clone(self),
            upload,
        })
    }

    /// Interrupts a blocked script by killing the renderer process.
    pub(crate) fn interrupt(&self) {
        let _ = self.kill.send(true);
    }

    /// Asks the renderer loop to stop without waiting.
    pub(crate) fn request_shutdown(&self) {
        let _result = self.client.try_notify(HostNotice::Shutdown);
    }

    /// Stops the renderer and waits for its transport tasks.
    pub(crate) async fn shutdown(&self) {
        self.request_shutdown();
        self.interrupt();
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for mut task in tasks {
            if timeout(SHUTDOWN_TIMEOUT, &mut task).await.is_err() {
                task.abort();
                let _result = task.await;
            }
        }
    }
}

fn decode_reply(
    assignment: RendererAssignmentId,
    response: RendererReply,
) -> Result<Reply, TabError> {
    if response.assignment == assignment {
        Ok(response.reply)
    } else {
        Err(TabError::ActorStopped)
    }
}

impl Drop for RendererHandle {
    fn drop(&mut self) {
        // Dropping `kill` stops the child task's watch and reaps the child.
        for task in std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner))
        {
            task.abort();
        }
    }
}

struct RendererViolation;

struct ReaderContext {
    router: RendererRouter,
    alive: Arc<AtomicBool>,
    kill: watch::Sender<bool>,
}

struct ActiveDial {
    assignment: RendererAssignmentId,
    cancel: watch::Sender<bool>,
}

struct ServiceContext {
    responder: BrowserServiceResponder,
    /// Process this service task belongs to; renderer calls may only resolve
    /// assignments this process hosts.
    renderer: RendererId,
    registry: Arc<AssignmentRegistry>,
    /// Cross-tab calls resolve through this registry, never through the
    /// browser owner task: the wait-for graph stays acyclic.
    tabs: TabRegistry,
    partition: PartitionServices,
    sessions: Arc<SessionStorage>,
    kill: watch::Sender<bool>,
    /// Browser command handle: renderer links create tabs for `window.open`.
    browser: crate::browser::BrowserHandle,
    dials: Arc<Mutex<HashMap<RequestId, ActiveDial>>>,
}

async fn writer_task(
    mut rx: exchange::Receiver<ToRenderer>,
    mut writer: Box<dyn AsyncWrite + Send + Unpin>,
    alive: Arc<AtomicBool>,
    router: RendererRouter,
    kill: watch::Sender<bool>,
    mut kill_rx: watch::Receiver<bool>,
) {
    if *kill_rx.borrow() {
        return;
    }
    loop {
        let message = tokio::select! {
            biased;
            message = rx.recv() => match message {
                Some(message) => message,
                None => return,
            },
            _ = kill_rx.changed() => return,
        };
        let result = write_to_renderer(&mut *writer, &message).await;
        if result.is_err() {
            fail(&alive, &router, &kill);
            return;
        }
    }
}

async fn write_to_renderer<W>(writer: &mut W, message: &ToRenderer) -> io::Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    match message {
        Frame::RequestChunk { id, bytes } => {
            crate::wire::channel::write_frame_async(
                writer,
                crate::wire::channel::FrameKind::RequestChunk,
                id.get(),
                bytes,
            )
            .await
        }
        Frame::ResponseChunk { id, bytes } => {
            crate::wire::channel::write_frame_async(
                writer,
                crate::wire::channel::FrameKind::ResponseChunk,
                id.get(),
                bytes,
            )
            .await
        }
        _ => crate::wire::channel::write_control_async(writer, message).await,
    }
}

async fn reader_task(
    mut reader: Box<dyn AsyncRead + Send + Unpin>,
    context: ReaderContext,
    ready: Option<oneshot::Sender<bool>>,
) {
    let mut ready = ready;
    let mut buffer = Vec::new();
    loop {
        let frame = match crate::wire::channel::read_frame_async(&mut *reader, &mut buffer).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                logging::error!(target: "browser::link", "bad renderer message: {error}");
                break;
            }
        };
        let message = match frame.kind {
            crate::wire::channel::FrameKind::Control => {
                match crate::wire::channel::decode_control::<FromRenderer>(&buffer) {
                    Ok(message) => message,
                    Err(error) => {
                        logging::error!(target: "browser::link", "bad renderer message: {error}");
                        break;
                    }
                }
            }
            crate::wire::channel::FrameKind::RequestChunk => Frame::RequestChunk {
                id: RequestId::new(frame.request),
                bytes: buffer.clone(),
            },
            crate::wire::channel::FrameKind::ResponseChunk => Frame::ResponseChunk {
                id: RequestId::new(frame.request),
                bytes: buffer.clone(),
            },
        };
        if let Some(ready_tx) = ready.take() {
            let ok = matches!(message, Frame::Notify(RendererNotice::Ready));
            let _ = ready_tx.send(ok);
            if !ok {
                break;
            }
            continue;
        }
        if context.router.route(message).is_err() {
            break;
        }
    }
    if let Some(ready_tx) = ready {
        let _ = ready_tx.send(false);
    }
    fail(&context.alive, &context.router, &context.kill);
}

/// Runs browser services and renderer notifications after the exchange router
/// has separated them from replies and stream chunks.
async fn service_task(mut server: BrowserServiceServer, context: ServiceContext) {
    while let Some(input) = server.recv().await {
        let result = match input {
            ServerInput::Call {
                id,
                body: BrowserCall { assignment, call },
            } => route_service_call(&context, id, assignment, call).await,
            ServerInput::Notify(RendererNotice::Event {
                assignment,
                frame,
                event,
            }) => route_event(&context, assignment, frame, &event),
            ServerInput::Notify(RendererNotice::WaiterSettled { assignment, id }) => {
                route_waiter_settled(&context, assignment, id)
            }
            ServerInput::Notify(RendererNotice::Ready)
            | ServerInput::RequestChunk
            | ServerInput::RequestEnd
            | ServerInput::Cancel { .. } => Err(RendererViolation),
        };
        if result.is_err() {
            let _result = context.kill.send(true);
            return;
        }
    }
}

fn route_event(
    context: &ServiceContext,
    assignment: RendererAssignmentId,
    frame: FrameId,
    event: &BrowserEvent,
) -> Result<(), RendererViolation> {
    let Some(assignment) = live_assignment(context, assignment)? else {
        return Ok(());
    };
    if assignment.publish_event(frame, event) {
        Err(RendererViolation)
    } else {
        Ok(())
    }
}

/// Routes a settled JS waiter. It shares the event route's authorization: a
/// renderer may only speak for an assignment it hosts.
fn route_waiter_settled(
    context: &ServiceContext,
    assignment: RendererAssignmentId,
    id: u64,
) -> Result<(), RendererViolation> {
    if let Some(assignment) = live_assignment(context, assignment)?
        && assignment.publish_waiter_settled(id)
    {
        return Err(RendererViolation);
    }
    Ok(())
}

/// The live assignment `id` names. `None` means it was already released and
/// the notice is stale, which is not a violation.
fn live_assignment(
    context: &ServiceContext,
    assignment: RendererAssignmentId,
) -> Result<Option<Arc<Assignment>>, RendererViolation> {
    let Some(assignment) = context
        .registry
        .resolve(assignment)
        .filter(|assignment| assignment.hosted_by(context.renderer))
    else {
        return if context.registry.was_released(assignment) {
            Ok(None)
        } else {
            Err(RendererViolation)
        };
    };
    Ok(Some(assignment))
}

/// Runs one dial attempt, racing the process kill switch and this call's
/// cancellation.
async fn race_dial(
    network: &crate::network::TabNetworkHandle,
    request: &renderer::DialRequest,
    initiator: &url::Origin,
    assignment: RendererAssignmentId,
    mut kill: watch::Receiver<bool>,
    mut cancel: watch::Receiver<bool>,
) -> Result<renderer::DialOutcome, renderer::DialFailure> {
    tokio::select! {
        biased;
        _ = kill.changed() => Err(renderer::DialFailure::Cancelled),
        _ = cancel.changed() => Err(renderer::DialFailure::Cancelled),
        result = network.dial_request(request, initiator, assignment) => result,
    }
}

/// Routes one browser service call from a renderer. This is a dispatch table:
/// each arm validates and forwards one call shape.
#[expect(
    clippy::too_many_lines,
    reason = "dispatch table with one arm per service call; splitting it is a refactor, not a lint fix"
)]
async fn route_service_call(
    context: &ServiceContext,
    id: RequestId,
    assignment_id: RendererAssignmentId,
    call: ServiceCall,
) -> Result<(), RendererViolation> {
    let Some(assignment) = context
        .registry
        .resolve(assignment_id)
        .filter(|assignment| assignment.hosted_by(context.renderer))
    else {
        // The renderer may have queued the call before it processed
        // `Release`. Answer with a benign failure so a synchronous
        // `document.cookie` caller cannot block forever, and keep the
        // process alive for its other assignments.
        if context.registry.was_released(assignment_id) {
            return send_released_reply(&context.responder, id, &call).await;
        }
        return Err(RendererViolation);
    };
    match call {
        ServiceCall::Network(NetworkCall::Dial(request)) => {
            // An opaque initiator (`data:`, `blob:` with an opaque creator, a
            // sandboxed document) has no site to authorize. It may still
            // fetch: the request goes out as `Origin: null`, with no
            // credentials it would not otherwise send and no referrer, so it
            // cannot claim the renderer's own site
            // (<https://fetch.spec.whatwg.org/#concept-request-origin>).
            let initiator = match assignment.site.authorize_origin(&request.initiator) {
                Some(initiator) => initiator,
                None if request.initiator.is_opaque() => url::Origin::new_opaque(),
                None => return Err(RendererViolation),
            };
            let worker_network = assignment.network.clone();
            let responder = context.responder.clone();
            let worker_kill = context.kill.clone();
            let kill = context.kill.subscribe();
            let (cancel_tx, cancel_rx) = watch::channel(false);
            context
                .dials
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(
                    id,
                    ActiveDial {
                        assignment: assignment_id,
                        cancel: cancel_tx,
                    },
                );
            let dials = context.dials.clone();
            let registry = context.registry.clone();
            let tabs = context.tabs.clone();
            let tab_id = assignment.tab;
            tokio::spawn(async move {
                let mut outcome = race_dial(
                    &worker_network,
                    &request,
                    &initiator,
                    assignment_id,
                    kill.clone(),
                    cancel_rx.clone(),
                )
                .await;
                // A 421 asks for a retry on a different connection, but only an
                // idempotent method may be replayed
                // (<https://www.rfc-editor.org/rfc/rfc9110#section-15.5.20>).
                if request.method.is_idempotent()
                    && matches!(&outcome, Ok(head) if head.status == 421)
                {
                    if let Ok(head) = &outcome
                        && let renderer::ResponseBody::Streamed(stream) = &head.body
                    {
                        worker_network.cancel_stream(assignment_id, *stream);
                    }
                    outcome = race_dial(
                        &worker_network,
                        &request,
                        &initiator,
                        assignment_id,
                        kill.clone(),
                        cancel_rx.clone(),
                    )
                    .await;
                }
                // `release_streams` may have run while the dial was in flight.
                if let Ok(head) = &outcome
                    && let renderer::ResponseBody::Streamed(stream) = &head.body
                    && registry.resolve(assignment_id).is_none()
                {
                    worker_network.cancel_stream(assignment_id, *stream);
                }
                // A frame navigation carries the response head the browser
                // already sees, so parse the enforced CSP `sandbox`
                // directives here and narrow the frame's own record before the
                // renderer can commit the document and run its scripts
                // (<https://w3c.github.io/webappsec-csp/#sandbox>).
                if let (Ok(head), Some(frame)) = (&outcome, request.frame) {
                    let csp = renderer::SandboxFlags::from_csp_headers(&head.headers);
                    tabs.commit_frame_sandbox(tab_id, frame, csp);
                }
                dials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                if responder
                    .reply(id, ServiceReply::Dial(Box::new(outcome)))
                    .await
                    .is_err()
                {
                    let _result = worker_kill.send(true);
                }
            });
        }
        ServiceCall::Network(NetworkCall::DialRead { id: stream }) => {
            let result = assignment.network.read_stream(assignment_id, stream).await;
            send_reply(&context.responder, id, ServiceReply::StreamChunk(result)).await?;
        }
        ServiceCall::Network(NetworkCall::CancelStream { id: stream }) => {
            assignment.network.cancel_stream(assignment_id, stream);
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        ServiceCall::Network(NetworkCall::CancelDial { id: target }) => {
            if let Some(dial) = context
                .dials
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&target)
            {
                if dial.assignment != assignment_id {
                    return Err(RendererViolation);
                }
                let _sent = dial.cancel.send(true);
            }
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        ServiceCall::Network(NetworkCall::CookieGet { url, partition_key }) => {
            let Some(url) = assignment.site.authorize_url(&url) else {
                return Err(RendererViolation);
            };
            let reply = ServiceReply::Cookie(
                assignment
                    .network
                    .cookies_for(&url, partition_key.as_deref()),
            );
            send_reply(&context.responder, id, reply).await?;
        }
        // Every cookie arm authorizes the URL and passes the partition key the
        // renderer named. The key only narrows the same host's cookie set to
        // one partition, so a renderer that names another top-level site reads
        // data it already owns, never another origin's.
        ServiceCall::Network(NetworkCall::CookieSet {
            value,
            url,
            partition_key,
        }) => {
            let Some(url) = assignment.site.authorize_url(&url) else {
                return Err(RendererViolation);
            };
            assignment
                .network
                .set_cookie(&value, &url, partition_key.as_deref());
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        ServiceCall::Network(NetworkCall::CookieStoreGet { url, partition_key }) => {
            let Some(url) = assignment.site.authorize_url(&url) else {
                return Err(RendererViolation);
            };
            let records = assignment
                .network
                .cookie_store_records(&url, partition_key.as_deref());
            send_reply(&context.responder, id, ServiceReply::CookieStore(records)).await?;
        }
        ServiceCall::Network(NetworkCall::CookieStoreSet {
            write,
            url,
            partition_key,
        }) => {
            let Some(url) = assignment.site.authorize_url(&url) else {
                return Err(RendererViolation);
            };
            assignment
                .network
                .cookie_store_store(&write, &url, partition_key.as_deref());
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        ServiceCall::Messaging(MessagingCall::BroadcastPost {
            origin,
            name,
            payload,
            channel,
        }) => {
            // `BroadcastChannel` is origin-scoped; a frame's origin is the
            // frame's, so the browser bound is the process site lock.
            let origin = assignment
                .site
                .storage_key(&origin)
                .ok_or(RendererViolation)?;
            context
                .partition
                .events
                .broadcast(&ContextEvent::Broadcast(BroadcastMessage {
                    origin,
                    name,
                    payload,
                    source: (assignment_id, channel),
                }));
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        ServiceCall::Storage(call) => {
            return route_storage_call(context, assignment_id, &assignment, id, call).await;
        }
        ServiceCall::BrowsingContext(call) => {
            return route_window_call(context, &assignment, id, call).await;
        }
    }
    Ok(())
}

/// Whether a page may address `target` with `postMessage`: itself, its
/// opener, or a tab it opened.
fn message_related(context: &ServiceContext, assignment: &Assignment, target: TabId) -> bool {
    target == assignment.tab
        || context.tabs.opener(target) == Some(assignment.tab)
        || context.tabs.opener(assignment.tab) == Some(target)
}

/// Authorizes and runs one `window.close()` after its synchronous reply.
///
/// The caller's renderer has already been answered, so the target's
/// coordinator is free to report its history. A page may close itself or a tab
/// it opened; a tab is script-closable when script created it (`noopener`
/// included) or while it has one history entry
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#script-closable>).
fn spawn_window_close(
    browser: crate::browser::BrowserHandle,
    tabs: TabRegistry,
    caller: TabId,
    target: TabId,
) {
    tokio::spawn(async move {
        if target != caller && tabs.opener(target) != Some(caller) {
            return;
        }
        let script_closable = tabs.is_script_created(target)
            || match browser.tab(target).await {
                Ok(tab) => matches!(
                    tab.history().await,
                    Ok(history) if history.entries().len() == 1
                ),
                Err(_) => false,
            };
        if script_closable {
            let _result = browser.close_tab(target).await;
        }
    });
}

/// The URL a browser-hosted popup may load.
///
/// The renderer resolved the URL; the browser still checks the scheme, because
/// a renderer is not the trust boundary. `data:` and `blob:` popups would need
/// a browser-side document store this engine does not have, so they are
/// refused here rather than mis-navigated
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
fn navigable_popup_url(url: Url) -> Option<Url> {
    matches!(url.scheme(), "about" | "http" | "https").then_some(url)
}

/// The authorized navigation a `window.open`-family call runs under.
fn window_initiator(
    assignment: &Assignment,
    source: renderer::NavigationSource,
    opener: renderer::OpenerPolicy,
) -> Result<crate::actor::NavigationInitiator, RendererViolation> {
    let renderer::NavigationSource {
        origin: claim,
        referrer,
        referrer_policy,
        // The browser checks the calling frame against its own per-frame
        // sandbox record; the initiator does not carry that state.
        frame: _,
        secure_context,
    } = source;
    // The calling document's origin must belong to this renderer's site lock;
    // an opaque document may still open a window, but its origin stays opaque.
    // The referrer is the document's own URL, so it must be same-origin with
    // that claim and inside the lock: a URL that is neither is not this
    // document's to name, and the popup goes out without one rather than
    // leaking it. The dial seam makes the same check
    // (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
    let origin = match assignment.site.authorize_origin(&claim) {
        Some(origin) => origin,
        None if claim.is_opaque() => url::Origin::new_opaque(),
        None => return Err(RendererViolation),
    };
    let referrer = assignment
        .site
        .allows_url(&referrer)
        .then_some(referrer)
        .filter(|referrer| referrer.origin() == origin);
    // `noreferrer` is the opener feature's referrer half, and the browser owns
    // the feature's meaning: the renderer sends the document's policy and this
    // overrides it
    // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    let referrer_policy = if opener == renderer::OpenerPolicy::NoReferrer {
        net::ReferrerPolicy::NoReferrer
    } else {
        referrer_policy
    };
    Ok(crate::actor::NavigationInitiator {
        origin,
        referrer,
        referrer_policy,
        secure_context,
    })
}

/// The window calls that create or navigate a browser tab.
#[derive(Clone, Copy)]
struct WindowRouter<'a> {
    context: &'a ServiceContext,
    assignment: &'a Assignment,
    request: RequestId,
}

impl WindowRouter<'_> {
    /// Creates the browser tab a `window.open` call asked for.
    async fn open(
        self,
        url: Option<Url>,
        target: renderer::TargetName,
        opener: renderer::OpenerPolicy,
        source: renderer::NavigationSource,
    ) -> Result<(), RendererViolation> {
        let Self {
            context,
            assignment,
            request: id,
        } = self;
        // `_self`, `_parent`, and `_top` never create a browsing context; the
        // renderer navigates those in its own frame tree. Sending one here is a
        // renderer bug or a forgery, not a request the browser should soften.
        if matches!(
            target,
            renderer::TargetName::Current
                | renderer::TargetName::Parent
                | renderer::TargetName::Top
        ) || !target.is_canonical()
        {
            return Err(RendererViolation);
        }
        // The browser's per-frame record is the whole check. A renderer
        // reports an `iframe`'s `sandbox` attribute once, at creation, and the
        // browser unions it with its own parent set; a call can therefore only
        // be refused, never widened, by anything the renderer says now
        // (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag>).
        let Some(active) = context.tabs.frame_sandbox(assignment.tab, source.frame) else {
            return Err(RendererViolation);
        };
        if active.blocks_auxiliary_navigation() {
            logging::debug!(
                target: "browser::tabs",
                "window.open refused: the browser's sandbox set blocks auxiliary navigation"
            );
            return Err(RendererViolation);
        }
        let url = match url {
            Some(url) => {
                let Some(url) = navigable_popup_url(url) else {
                    return send_reply(&context.responder, id, ServiceReply::Window(None)).await;
                };
                Some(url)
            }
            None => None,
        };
        let calling_frame = source.frame;
        let initiator = window_initiator(assignment, source, opener)?;
        // Every document in the new tab inherits this set. `auxiliary_flags`
        // is the spec's one release: `allow-popups-to-escape-sandbox` empties
        // it, and only the actively governing set can carry that keyword
        // (<https://html.spec.whatwg.org/multipage/browsers.html#sandbox-propagates-to-auxiliary-browsing-contexts-flag>).
        let popup_sandbox = active.auxiliary_flags();
        let tab = context
            .browser
            .open_window(crate::browser::OpenWindowOptions {
                url,
                target,
                source: Some(assignment.tab),
                source_frame: calling_frame,
                opener,
                initiator,
                popup_sandbox,
            })
            .await
            .ok()
            .map(|tab| renderer::WindowId::new(tab.get()));
        send_reply(&context.responder, id, ServiceReply::Window(tab)).await
    }

    /// Selects the browser tab this tab's group holds under `name` and
    /// navigates it. The reply is the selected tab, or `None` when the name
    /// selects nothing so the caller creates a context.
    async fn navigate(
        self,
        name: renderer::ContextName,
        url: Option<Url>,
        opener: renderer::OpenerPolicy,
        source: renderer::NavigationSource,
    ) -> Result<(), RendererViolation> {
        let Self {
            context,
            assignment,
            request: id,
        } = self;
        // The lookup runs here, once: the caller names a context, the browser
        // decides which tab that is, and the answer is what it selected. A
        // renderer cannot name a tab outside its own group.
        let Some(target) = context.tabs.find_by_name(assignment.tab, &name) else {
            return send_reply(&context.responder, id, ServiceReply::Window(None)).await;
        };
        if let Some(url) = url.and_then(navigable_popup_url) {
            // A sandboxed document may navigate itself, the contexts nested
            // inside it, and the auxiliary contexts it opened — and only those.
            // The set is the browser's per-frame record, never a claim the
            // renderer makes now
            // (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag>).
            let Some(active) = context.tabs.frame_sandbox(assignment.tab, source.frame) else {
                return Err(RendererViolation);
            };
            // "The auxiliary contexts it created" is a frame relationship: the
            // tab that hosts the sandboxed document is not the document, so a
            // frame cannot inherit its parent document's permission.
            let created_by_caller = context.tabs.opener(target) == Some(assignment.tab)
                && context.tabs.opener_frame(target) == Some(source.frame);
            if target != assignment.tab && active.blocks_navigation() && !created_by_caller {
                logging::debug!(
                    target: "browser::tabs",
                    "window.open navigation refused: a sandboxed document cannot navigate another browsing context"
                );
            } else {
                let initiator = window_initiator(assignment, source, opener)?;
                if let Ok(handle) = context.browser.tab(target).await {
                    tokio::spawn(async move {
                        let _result = handle.goto_from(url.as_str(), initiator).await;
                    });
                }
            }
        }
        // A refused navigation still selected the tab, so the caller answers
        // the `WindowProxy` it already has. Only a missing name creates one.
        let tab = renderer::WindowId::new(target.get());
        send_reply(&context.responder, id, ServiceReply::Window(Some(tab))).await
    }
}

/// Routes one `window.open`/`window.close`/`postMessage` service call. Step 1
/// created the tab and its first navigation; step 2 adds the opener link,
/// cross-tab messaging, and the relatedness gates.
async fn route_window_call(
    context: &ServiceContext,
    assignment: &Assignment,
    id: RequestId,
    call: BrowsingContextCall,
) -> Result<(), RendererViolation> {
    match call {
        BrowsingContextCall::WindowOpen {
            url,
            target,
            opener,
            source,
        } => {
            WindowRouter {
                context,
                assignment,
                request: id,
            }
            .open(url, target, opener, source)
            .await?;
        }
        BrowsingContextCall::WindowName { change } => {
            // The call names no target, so a renderer can only rename its own
            // tab. The name is the target a `window.open` from any tab in the
            // group may select
            // (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
            // The registry is the one store: the tab actor reads the document
            // state's name from it when it commits, so this handler never waits
            // on a coordinator that may be running the script that made the
            // call. The registry itself skips a tab that closed in flight.
            context.tabs.rename(assignment.tab, change);
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        BrowsingContextCall::NavigateNamed {
            name,
            url,
            opener,
            source,
        } => {
            WindowRouter {
                context,
                assignment,
                request: id,
            }
            .navigate(name, url, opener, source)
            .await?;
        }
        BrowsingContextCall::FrameCreated {
            frame,
            parent,
            kind,
            sandbox,
        } => {
            route_frame_created(context, assignment, id, frame, parent, kind, sandbox).await?;
        }
        BrowsingContextCall::FrameDestroyed { frame } => {
            route_frame_destroyed(context, assignment, id, frame).await?;
        }
        BrowsingContextCall::WindowClose { window } => {
            let target = TabId::new(window.get());
            // Reply before the close runs. The renderer is blocked in this
            // call, and the target may be the very tab executing the script,
            // so asking it for its history here would deadlock the coordinator
            // behind the renderer that is waiting for this reply. Closing is
            // fire-and-forget by spec
            // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close>).
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
            spawn_window_close(
                context.browser.clone(),
                context.tabs.clone(),
                assignment.tab,
                target,
            );
        }
        BrowsingContextCall::Opener => {
            let opener = context
                .tabs
                .opener(assignment.tab)
                .map(|tab| renderer::WindowId::new(tab.get()));
            send_reply(&context.responder, id, ServiceReply::Window(opener)).await?;
        }
        BrowsingContextCall::WindowMessage { window, message } => {
            // A page can only hold a `WindowProxy` for a related window: its
            // own, its opener, or a tab it opened. An unrelated target is a
            // forged call and is ignored.
            // `MessageEvent.origin` is what a receiving page authenticates
            // with, so the sender's claim must stay inside this renderer's
            // site lock. An opaque origin has no authority to claim: its
            // serialized `null` passes through, and no tuple-origin document
            // admits it.
            if !assignment.site.allows_message_origin(&message.origin) {
                return Err(RendererViolation);
            }
            let target = TabId::new(window.get());
            if message_related(context, assignment, target) {
                // Enqueue on the target's mailbox and return: the spec's
                // `postMessage` is asynchronous, and waiting here would let a
                // busy target block this renderer.
                let _queued = context
                    .tabs
                    .deliver(target, crate::actor::RemoteWindowMessage { message });
            }
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        BrowsingContextCall::RemoteSessionGet {
            window,
            origin,
            key,
        } => {
            let origin = assignment
                .site
                .storage_key(&origin)
                .ok_or(RendererViolation)?;
            let target = TabId::new(window.get());
            let related = target == assignment.tab
                || context.tabs.opener(target) == Some(assignment.tab)
                || context.tabs.opener(assignment.tab) == Some(target);
            let value = related
                .then(|| context.sessions.get(target, &origin, &key))
                .flatten();
            send_reply(&context.responder, id, ServiceReply::StorageValue(value)).await?;
        }
    }
    Ok(())
}

/// Registers a frame the renderer created. The call arrives on the
/// synchronous service lane before the frame's document can run script, so a
/// `window.open` from the new frame always finds its record.
async fn route_frame_created(
    context: &ServiceContext,
    assignment: &Assignment,
    id: RequestId,
    frame: FrameId,
    parent: FrameId,
    kind: renderer::FrameKind,
    sandbox: renderer::SandboxFlags,
) -> Result<(), RendererViolation> {
    if !context
        .tabs
        .register_frame(assignment.tab, frame, parent, kind, sandbox)
    {
        return Err(RendererViolation);
    }
    send_reply(&context.responder, id, ServiceReply::Unit).await
}

/// Drops a frame the renderer tore down.
async fn route_frame_destroyed(
    context: &ServiceContext,
    assignment: &Assignment,
    id: RequestId,
    frame: FrameId,
) -> Result<(), RendererViolation> {
    if !context.tabs.remove_frame(assignment.tab, frame) {
        return Err(RendererViolation);
    }
    send_reply(&context.responder, id, ServiceReply::Unit).await
}

struct StorageRouter<'a> {
    context: &'a ServiceContext,
    assignment: RendererAssignmentId,
    authority: &'a Assignment,
    request: RequestId,
}

impl StorageRouter<'_> {
    async fn route(&self, call: StorageCall) -> Result<(), RendererViolation> {
        // Every storage call names one origin; authorize and type it once,
        // before any area is touched.
        match call {
            StorageCall::Get { kind, origin, key } => {
                let origin = self.key(&origin)?;
                self.get(kind, &origin, &key).await
            }
            StorageCall::Keys { kind, origin } => {
                let origin = self.key(&origin)?;
                self.keys(kind, &origin).await
            }
            StorageCall::Set {
                kind,
                origin,
                url,
                key,
                value,
                source,
            } => {
                let origin = self.key(&origin)?;
                self.set(kind, &origin, &url, &key, &value, source).await
            }
            StorageCall::Remove {
                kind,
                origin,
                url,
                key,
                source,
            } => {
                let origin = self.key(&origin)?;
                self.remove(kind, &origin, &url, &key, source).await
            }
            StorageCall::Clear {
                kind,
                origin,
                url,
                source,
            } => {
                let origin = self.key(&origin)?;
                self.clear(kind, &origin, &url, source).await
            }
        }
    }

    /// The typed key for one renderer's origin claim, or a refusal.
    fn key(&self, origin: &renderer::SerializedOrigin) -> Result<StorageKey, RendererViolation> {
        self.authority
            .site
            .storage_key(origin)
            .ok_or(RendererViolation)
    }

    async fn get(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        key: &str,
    ) -> Result<(), RendererViolation> {
        self.authorize(None)?;
        let value = match kind {
            StorageKind::Local => self.context.partition.local_storage.get(origin, key),
            StorageKind::Session => self.context.sessions.get(self.authority.tab, origin, key),
            StorageKind::IndexedDb => self.context.partition.indexed_db.get(origin, key),
        };
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageValue(value),
        )
        .await
    }

    async fn keys(&self, kind: StorageKind, origin: &StorageKey) -> Result<(), RendererViolation> {
        self.authorize(None)?;
        let keys = match kind {
            StorageKind::Local => self.context.partition.local_storage.keys(origin),
            StorageKind::Session => self.context.sessions.keys(self.authority.tab, origin),
            StorageKind::IndexedDb => self.context.partition.indexed_db.keys(origin),
        };
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageKeys(keys),
        )
        .await
    }

    async fn set(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        key: &str,
        value: &str,
        source: FrameId,
    ) -> Result<(), RendererViolation> {
        self.authorize(Some(url))?;
        let change = match kind {
            StorageKind::Local => self.context.partition.local_storage.set(origin, key, value),
            StorageKind::Session => {
                self.context
                    .sessions
                    .set(self.authority.tab, origin, key, value)
            }
            StorageKind::IndexedDb => self.context.partition.indexed_db.set(origin, key, value),
        };
        self.publish(
            source,
            kind,
            origin,
            url,
            change.as_ref().ok().and_then(Option::as_ref),
        );
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageChanged(change),
        )
        .await
    }

    async fn remove(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        key: &str,
        source: FrameId,
    ) -> Result<(), RendererViolation> {
        self.authorize(Some(url))?;
        let change = match kind {
            StorageKind::Local => self.context.partition.local_storage.remove(origin, key),
            StorageKind::Session => self
                .context
                .sessions
                .remove(self.authority.tab, origin, key),
            StorageKind::IndexedDb => self.context.partition.indexed_db.remove(origin, key),
        };
        self.publish(source, kind, origin, url, change.as_ref());
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageChanged(Ok(change)),
        )
        .await
    }

    async fn clear(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        source: FrameId,
    ) -> Result<(), RendererViolation> {
        self.authorize(Some(url))?;
        let change = match kind {
            StorageKind::Local => self.context.partition.local_storage.clear(origin),
            StorageKind::Session => self.context.sessions.clear(self.authority.tab, origin),
            StorageKind::IndexedDb => self.context.partition.indexed_db.clear(origin),
        };
        self.publish(source, kind, origin, url, change.as_ref());
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageChanged(Ok(change)),
        )
        .await
    }

    fn authorize(&self, url: Option<&str>) -> Result<(), RendererViolation> {
        if url.is_some_and(|url| self.authority.site.authorize_url(url).is_none()) {
            return Err(RendererViolation);
        }
        Ok(())
    }

    fn publish(
        &self,
        source: FrameId,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        change: Option<&StorageChange>,
    ) {
        let Some(change) = change else {
            return;
        };
        if kind == StorageKind::Local {
            broadcast_local_storage_event(
                self.context,
                self.assignment,
                source,
                origin,
                url,
                change,
            );
        }
    }
}

async fn route_storage_call(
    context: &ServiceContext,
    assignment: RendererAssignmentId,
    authority: &Assignment,
    id: RequestId,
    call: StorageCall,
) -> Result<(), RendererViolation> {
    StorageRouter {
        context,
        assignment,
        authority,
        request: id,
    }
    .route(call)
    .await
}

fn broadcast_local_storage_event(
    context: &ServiceContext,
    assignment: RendererAssignmentId,
    source: FrameId,
    origin: &StorageKey,
    url: &str,
    change: &StorageChange,
) {
    context
        .partition
        .events
        .broadcast(&ContextEvent::Storage(StorageBroadcast {
            origin: origin.clone(),
            kind: StorageKind::Local,
            key: change.key.clone(),
            old_value: change.old_value.clone(),
            new_value: change.new_value.clone(),
            url: url.to_owned(),
            source: (assignment, source),
        }));
}

/// Answers a late call from a released assignment so the renderer's blocking
/// service path stays unblocked while its engine is torn down. Only the dial
/// answer differs from the benign default: the submitted dial is cancelled,
/// and its completion gets a typed cancellation failure rather than a
/// transport one.
async fn send_released_reply(
    responder: &BrowserServiceResponder,
    id: RequestId,
    call: &ServiceCall,
) -> Result<(), RendererViolation> {
    let reply = match call {
        ServiceCall::Network(NetworkCall::Dial(_)) => {
            ServiceReply::Dial(Box::new(Err(renderer::DialFailure::Cancelled)))
        }
        call => ServiceReply::benign(call),
    };
    send_reply(responder, id, reply).await
}

async fn send_reply(
    responder: &BrowserServiceResponder,
    id: RequestId,
    reply: ServiceReply,
) -> Result<(), RendererViolation> {
    responder
        .reply(id, reply)
        .await
        .map_err(|_| RendererViolation)
}

fn fail(alive: &Arc<AtomicBool>, router: &RendererRouter, kill: &watch::Sender<bool>) {
    alive.store(false, Ordering::Relaxed);
    router.close();
    let _ = kill.send(true);
}

/// Forwards one renderer child's stderr into this process's logger.
///
/// The child formats its own level and target; the browser only forwards the
/// lines, so renderer records land in the daemon's console and file without a
/// second file writer.
async fn forward_stderr(stderr: tokio::process::ChildStderr) {
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(_) => logging::log_forwarded(&String::from_utf8_lossy(&bytes)),
        }
    }
}

/// One spawned child and its host-side channel endpoints.
struct SpawnedRenderer {
    child: Child,
    reader: Box<dyn AsyncRead + Send + Unpin>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
}

/// Spawns one renderer child and returns its host-side reader and writer.
///
/// Unix passes one end of an unnamed socket pair as the child's file
/// descriptor 0. Other platforms keep the piped stdin/stdout transport until
/// their platform channel lands.
#[cfg(unix)]
fn spawn_transport(command: &mut Command) -> io::Result<SpawnedRenderer> {
    let (host, child_end) = std::os::unix::net::UnixStream::pair()?;
    command
        .stdin(Stdio::from(std::os::fd::OwnedFd::from(child_end)))
        .stdout(Stdio::null());
    let child = command.spawn()?;
    let writer_std = host.try_clone()?;
    host.set_nonblocking(true)?;
    writer_std.set_nonblocking(true)?;
    let reader = tokio::net::UnixStream::from_std(host)?;
    let writer = tokio::net::UnixStream::from_std(writer_std)?;
    Ok(SpawnedRenderer {
        child,
        reader: Box::new(reader),
        writer: Box::new(writer),
    })
}

#[cfg(not(unix))]
fn spawn_transport(command: &mut Command) -> io::Result<SpawnedRenderer> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = command.spawn()?;
    let Some(stdin) = child.stdin.take() else {
        let _ = child.start_kill();
        return Err(io::Error::other("renderer stdin missing"));
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.start_kill();
        return Err(io::Error::other("renderer stdout missing"));
    };
    Ok(SpawnedRenderer {
        child,
        reader: Box::new(stdout),
        writer: Box::new(stdin),
    })
}

/// Registers browser-to-renderer notifications. A saturated command inbox
/// drops storage and broadcast notices instead of disconnecting the renderer;
/// the renderer may be blocked in a synchronous service call, and losing an
/// observable event is better than killing the page. A closed client removes
/// the subscription and stops the renderer.
fn subscribe_renderer_events(
    partition: &PartitionServices,
    client: &RendererClient,
    kill: &watch::Sender<bool>,
) {
    let client = client.clone();
    let kill = kill.clone();
    partition.events.subscribe(Box::new(move |event| {
        let notice = match event {
            ContextEvent::Storage(event) => HostNotice::StorageEvent {
                origin: event.origin.clone(),
                kind: event.kind,
                key: event.key.clone(),
                old_value: event.old_value.clone(),
                new_value: event.new_value.clone(),
                url: event.url.clone(),
                source: Some(event.source),
            },
            ContextEvent::Broadcast(message) => HostNotice::BroadcastMessage {
                origin: message.origin.clone(),
                name: message.name.clone(),
                payload: message.payload.clone(),
                source: Some(message.source),
            },
        };
        if client.try_notify_lossy(notice).is_ok() {
            true
        } else {
            let _result = kill.send(true);
            false
        }
    }));
}

/// Browser services one renderer process hosts for its assignments.
pub(crate) struct RendererServices {
    /// The identity the new renderer reports to its pages and its CDP clients.
    pub(crate) identity: net::ValidIdentity,
    /// Partition services shared with the new process.
    pub(crate) partition: PartitionServices,
    /// Session storage owned by the browser.
    pub(crate) sessions: Arc<SessionStorage>,
    /// Browser command handle for `window.open` callbacks.
    pub(crate) browser: crate::browser::BrowserHandle,
    /// Assignment registry shared by every renderer of this browser context.
    pub(crate) registry: Arc<AssignmentRegistry>,
    /// Tab registry for cross-tab calls from renderer service tasks.
    pub(crate) tabs: TabRegistry,
}

pub(crate) async fn spawn_process(
    id: RendererId,
    site: Option<Site>,
    services: RendererServices,
    slot: tokio::sync::OwnedSemaphorePermit,
) -> io::Result<RendererHandle> {
    let RendererServices {
        identity,
        partition,
        sessions,
        browser,
        registry,
        tabs,
    } = services;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("renderer")
        .env("TINYBROWSER_LOG", logging::level().as_str())
        .stderr(Stdio::piped());
    let SpawnedRenderer {
        mut child,
        reader,
        writer,
    } = spawn_transport(&mut command)?;
    let Some(stderr) = child.stderr.take() else {
        let _ = child.start_kill();
        return Err(io::Error::other("renderer stderr missing"));
    };
    let (tx, rx) = exchange::pair(COMMAND_CAPACITY, COMMAND_CAPACITY);
    let (client, server, router) = exchange::endpoint(tx, COMMAND_CAPACITY);
    let (kill, kill_rx) = watch::channel(false);
    subscribe_renderer_events(&partition, &client, &kill);
    let alive = Arc::new(AtomicBool::new(true));
    let (ready_tx, ready_rx) = oneshot::channel();
    let writer_task = tokio::spawn(writer_task(
        rx,
        writer,
        Arc::clone(&alive),
        router.clone(),
        kill.clone(),
        kill_rx.clone(),
    ));
    let site = Arc::new(Mutex::new(site));
    let reader_context = ReaderContext {
        router: router.clone(),
        alive: Arc::clone(&alive),
        kill: kill.clone(),
    };
    let service_context = ServiceContext {
        responder: server.responder(),
        renderer: id,
        registry,
        tabs,
        partition,
        sessions,
        kill: kill.clone(),
        browser,
        dials: Arc::new(Mutex::new(HashMap::new())),
    };
    let reader_task = tokio::spawn(reader_task(reader, reader_context, Some(ready_tx)));
    let service_task = tokio::spawn(service_task(server, service_context));
    let stderr_task = tokio::spawn(forward_stderr(stderr));
    // Detached child task: it reaps the child when the channel closes or the
    // handle signals a kill. Dropping every kill sender also stops it.
    let _reaper = tokio::spawn(child_task(child, kill_rx));
    if client
        .notify(HostNotice::Hello {
            identity: Box::new(identity),
        })
        .await
        .is_err()
        || timeout(HANDSHAKE_TIMEOUT, ready_rx).await != Ok(Ok(true))
    {
        let _ = kill.send(true);
        let _result = writer_task.await;
        let _result = reader_task.await;
        let _result = service_task.await;
        let _result = stderr_task.await;
        return Err(io::Error::other("renderer handshake failed"));
    }
    logging::debug!(
        target: "browser::link",
        "renderer {id:?} ready for site lock {site:?}"
    );
    let tasks = vec![writer_task, reader_task, service_task, stderr_task];
    Ok(RendererHandle {
        id,
        client,
        alive,
        kill,
        tasks: Mutex::new(tasks),
        site,
        assignments: AtomicUsize::new(0),
        _slot: slot,
    })
}

async fn child_task(mut child: Child, mut kill: watch::Receiver<bool>) {
    if *kill.borrow() {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return;
    }
    tokio::select! {
        _ = kill.changed() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        status = child.wait() => match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                logging::error!(target: "browser::link", "renderer exited: {status}");
            }
            Err(error) => {
                logging::error!(target: "browser::link", "renderer wait failed: {error}");
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use renderer::SandboxFlags;

    /// The browser's tab-level set is the baseline and the calling document's
    /// set only adds to it, so a renderer that under-reports its own sandbox
    /// cannot free a popup whose tab the browser knows is sandboxed
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxing>).
    #[test]
    fn a_source_can_add_sandbox_flags_but_never_remove_the_tabs() {
        let tab = SandboxFlags::parse(Some("allow-scripts allow-popups"));
        assert!(tab.propagates_to_auxiliary());
        assert_eq!(
            tab.union(SandboxFlags::empty()).auxiliary_flags(),
            tab,
            "an empty claim cannot widen what the browser knows"
        );
        assert_eq!(
            tab.union(tab).auxiliary_flags(),
            tab,
            "an honest renderer sends its active set, which already contains the tab's"
        );

        let stricter = tab.union(SandboxFlags::parse(Some("")));
        assert_eq!(
            tab.union(stricter).auxiliary_flags(),
            stricter,
            "a claim that adds a flag the browser cannot see is kept"
        );

        let escaping = SandboxFlags::parse(Some(
            "allow-scripts allow-popups allow-popups-to-escape-sandbox",
        ));
        assert!(!escaping.propagates_to_auxiliary());
        assert_eq!(
            escaping.auxiliary_flags(),
            SandboxFlags::empty(),
            "the escaping set is the one legitimate release"
        );
        assert_eq!(
            SandboxFlags::empty()
                .union(SandboxFlags::empty())
                .auxiliary_flags(),
            SandboxFlags::empty(),
            "an unsandboxed tab's popup starts unsandboxed"
        );
        assert_eq!(
            SandboxFlags::empty().union(tab).auxiliary_flags(),
            tab,
            "an iframe's flags reach its popup through the claim"
        );
    }
}
