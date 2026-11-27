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
use crate::context::{PartitionServices, UserContextId};
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

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn request_timeout() -> Duration {
    std::env::var_os("TINYBROWSER_RENDERER_REQUEST_TIMEOUT_MS")
        .and_then(|value| value.to_str()?.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(REQUEST_TIMEOUT, Duration::from_millis)
}

const MAX_RENDERER_STREAM_BYTES: usize = 64 * 1024 * 1024;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

const COMMAND_CAPACITY: usize = 256;

type RendererClient =
    exchange::Client<RendererCall, ServiceReply, HostNotice, RendererReply, RendererNotice>;
type BrowserServiceServer =
    exchange::Server<RendererCall, ServiceReply, HostNotice, BrowserCall, RendererNotice>;
type BrowserServiceResponder = exchange::Responder<RendererCall, ServiceReply, HostNotice>;
type RendererRouter = exchange::Router<BrowserCall, RendererReply, RendererNotice>;
type ResponseUpload = exchange::Upload<RendererCall, ServiceReply, HostNotice, RendererReply>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RendererLock {
    pub(crate) user_context: UserContextId,
    pub(crate) site: Site,
}

pub(crate) struct RendererHandle {
    pub(crate) id: RendererId,
    client: RendererClient,
    pub(crate) alive: Arc<AtomicBool>,
    kill: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) lock: Arc<Mutex<Option<RendererLock>>>,
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
    pub(crate) fn bind(
        &self,
        target: &RendererLock,
        partition: &PartitionServices,
        registry: &Arc<AssignmentRegistry>,
        tabs: &TabRegistry,
    ) -> io::Result<()> {
        let bound_fresh = {
            let mut lock = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            match &*lock {
                Some(existing) if existing != target => {
                    return Err(io::Error::other("renderer has a different site lock"));
                }
                Some(_) => false,
                None => {
                    *lock = Some(target.clone());
                    true
                }
            }
        };
        if bound_fresh {
            subscribe_renderer_events(
                partition,
                &self.client,
                &self.kill,
                self.id,
                Arc::clone(registry),
                tabs.clone(),
            );
        }
        Ok(())
    }

    pub(crate) async fn assign_notify(&self, assignment: RendererAssignmentId) -> io::Result<()> {
        self.client
            .notify(HostNotice::Assign { assignment })
            .await
            .map_err(|_| io::Error::other("renderer stopped"))
    }

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

    pub(crate) async fn mount(
        self: &Arc<Self>,
        assignment: RendererAssignmentId,
        frame: FrameId,
        generation: u64,
        mount: Mount,
    ) -> Result<Reply, TabError> {
        let response = self
            .start_response(assignment, frame, generation, &mount)
            .await?;
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
        generation: u64,
        mount: &Mount,
    ) -> Result<ResponseWriter, TabError> {
        let start = Box::new(ResponseStart {
            assignment,
            frame,
            generation,
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

    pub(crate) fn interrupt(&self) {
        let _ = self.kill.send(true);
    }

    pub(crate) fn request_shutdown(&self) {
        let _result = self.client.try_notify(HostNotice::Shutdown);
    }

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
        for task in std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner))
        {
            task.abort();
        }
    }
}

struct RendererViolation;

#[derive(Clone)]
struct LinkFailure {
    router: RendererRouter,
    alive: Arc<AtomicBool>,
    kill: watch::Sender<bool>,
    renderer: RendererId,
    registry: Arc<AssignmentRegistry>,
}

impl LinkFailure {
    fn trigger(&self) {
        self.alive.store(false, Ordering::Relaxed);
        self.router.close();
        self.registry.close_renderer(self.renderer);
        let _ = self.kill.send(true);
    }
}

struct ActiveDial {
    assignment: RendererAssignmentId,
    cancel: watch::Sender<bool>,
}

struct ServiceContext {
    responder: BrowserServiceResponder,
    renderer: RendererId,
    registry: Arc<AssignmentRegistry>,
    tabs: TabRegistry,
    sessions: Arc<SessionStorage>,
    kill: watch::Sender<bool>,
    browser: crate::browser::BrowserHandle,
    dials: Arc<Mutex<HashMap<RequestId, ActiveDial>>>,
}

async fn writer_task(
    mut rx: exchange::Receiver<ToRenderer>,
    mut writer: Box<dyn AsyncWrite + Send + Unpin>,
    failure: LinkFailure,
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
            failure.trigger();
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

// https://html.spec.whatwg.org/multipage/browsers.html#processes
fn report_reader_error(failure: &LinkFailure, error: &std::io::Error) {
    let killed = *failure.kill.borrow();
    if killed {
        logging::debug!(target: "browser::link", "renderer channel closed: {error}");
    } else {
        logging::error!(target: "browser::link", "bad renderer message: {error}");
    }
}

async fn reader_task(
    mut reader: Box<dyn AsyncRead + Send + Unpin>,
    failure: LinkFailure,
    ready: Option<oneshot::Sender<Result<(), String>>>,
) {
    let mut ready = ready;
    let mut buffer = Vec::new();
    loop {
        let frame = match crate::wire::channel::read_frame_async(&mut *reader, &mut buffer).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                report_reader_error(&failure, &error);
                break;
            }
        };
        let message = match frame.kind {
            crate::wire::channel::FrameKind::Control => {
                match crate::wire::channel::decode_control::<FromRenderer>(&buffer) {
                    Ok(message) => message,
                    Err(error) => {
                        report_reader_error(&failure, &error);
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
            match message {
                Frame::Notify(RendererNotice::Ready) => {
                    let _ = ready_tx.send(Ok(()));
                    continue;
                }
                Frame::Notify(RendererNotice::StartupFailed { message }) => {
                    let _ = ready_tx.send(Err(message));
                    break;
                }
                _ => {
                    let _ =
                        ready_tx.send(Err("unexpected message during renderer handshake".into()));
                    break;
                }
            }
        }
        if failure.router.route(message).is_err() {
            break;
        }
    }
    if let Some(ready_tx) = ready {
        let _ = ready_tx.send(Err("renderer exited before completing handshake".into()));
    }
    failure.trigger();
}

async fn service_task(mut server: BrowserServiceServer, context: ServiceContext) {
    while let Some(input) = server.recv().await {
        let result = match input {
            ServerInput::Call {
                id,
                body: BrowserCall { assignment, call },
            } => route_service_call(&context, id, assignment, call).await,
            ServerInput::Notify(RendererNotice::Event {
                assignment,
                generation,
                frame,
                event,
            }) => route_event(&context, assignment, generation, frame, &event),
            ServerInput::Notify(RendererNotice::WaiterSettled { assignment, id }) => {
                route_waiter_settled(&context, assignment, id)
            }
            ServerInput::Notify(RendererNotice::Ready | RendererNotice::StartupFailed { .. })
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
    generation: u64,
    frame: FrameId,
    event: &BrowserEvent,
) -> Result<(), RendererViolation> {
    let Some(assignment) = live_assignment(context, assignment)? else {
        return Ok(());
    };
    if assignment.publish_event(generation, frame, event) {
        Err(RendererViolation)
    } else {
        Ok(())
    }
}

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
        if context.registry.was_released(assignment_id) {
            return send_released_reply(&context.responder, id, &call).await;
        }
        return Err(RendererViolation);
    };
    match call {
        ServiceCall::Network(NetworkCall::Dial(request)) => {
            return route_dial_call(context, id, assignment_id, &assignment, request);
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
        ServiceCall::Messaging(MessagingCall::BroadcastPost {
            origin,
            name,
            payload,
            channel,
        }) => {
            let origin = context
                .tabs
                .storage_key(assignment.tab, &assignment.site, &origin)
                .ok_or(RendererViolation)?;
            assignment
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

fn route_dial_call(
    context: &ServiceContext,
    id: RequestId,
    assignment_id: RendererAssignmentId,
    assignment: &Assignment,
    mut request: Box<renderer::DialRequest>,
) -> Result<(), RendererViolation> {
    // https://fetch.spec.whatwg.org/#concept-request-origin
    let initiator =
        match context
            .tabs
            .authorize_origin(assignment.tab, &assignment.site, &request.initiator)
        {
            Some(initiator) => initiator,
            None if request.initiator.is_opaque() => url::Origin::new_opaque(),
            None => return Err(RendererViolation),
        };
    request.partition_key = assignment.site.as_partition_key().map(str::to_owned);
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
        // https://www.rfc-editor.org/rfc/rfc9110#section-15.5.20
        if request.method.is_idempotent() && matches!(&outcome, Ok(head) if head.status == 421) {
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
        if let Ok(head) = &outcome
            && let renderer::ResponseBody::Streamed(stream) = &head.body
            && registry.resolve(assignment_id).is_none()
        {
            worker_network.cancel_stream(assignment_id, *stream);
        }
        // https://w3c.github.io/webappsec-csp/#sandbox
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#check-a-navigation-response's-adherence-to-x-frame-options
        if request.policy == renderer::RequestPolicy::FrameLoad
            && let Some(frame) = request.frame
            && frame != FrameId::MAIN
            && let Ok(head) = &outcome
        {
            let ancestors = tabs.ancestor_origins(tab_id, frame);
            if frame_blocked_by_headers(&head.headers, &head.final_url, &ancestors) {
                if let renderer::ResponseBody::Streamed(stream) = &head.body {
                    worker_network.cancel_stream(assignment_id, *stream);
                }
                outcome = Err(renderer::DialFailure::Cancelled);
            } else {
                let csp = renderer::SandboxFlags::from_csp_headers(&head.headers);
                tabs.commit_child_frame(tab_id, frame, &head.final_url, csp);
            }
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
    Ok(())
}

fn message_related(context: &ServiceContext, assignment: &Assignment, target: TabId) -> bool {
    target == assignment.tab
        || context.tabs.opener(target) == Some(assignment.tab)
        || context.tabs.opener(assignment.tab) == Some(target)
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#script-closable
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
            || match browser.tab(target) {
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

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
fn navigable_popup_url(url: Url) -> Option<Url> {
    matches!(url.scheme(), "about" | "http" | "https").then_some(url)
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#check-a-navigation-response's-adherence-to-x-frame-options
// https://w3c.github.io/webappsec-csp/#directive-frame-ancestors
fn frame_blocked_by_headers(
    headers: &[(String, String)],
    url: &Url,
    ancestors: &[url::Origin],
) -> bool {
    let target_origin = url.origin();
    let mut has_frame_ancestors = false;
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("content-security-policy") {
            continue;
        }
        for policy in value.split(',') {
            for directive in policy.split(';') {
                let mut tokens = directive.split_ascii_whitespace();
                let Some(directive_name) = tokens.next() else {
                    continue;
                };
                if !directive_name.eq_ignore_ascii_case("frame-ancestors") {
                    continue;
                }
                let sources: Vec<&str> = tokens.collect();
                has_frame_ancestors = true;
                if ancestors.iter().any(|ancestor| {
                    !sources
                        .iter()
                        .any(|source| ancestor_matches_source(ancestor, &target_origin, source))
                }) {
                    return true;
                }
            }
        }
    }
    if has_frame_ancestors {
        return false;
    }
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("x-frame-options") {
            continue;
        }
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("deny") {
            return true;
        }
        if trimmed.eq_ignore_ascii_case("sameorigin")
            && ancestors.iter().any(|ancestor| ancestor != &target_origin)
        {
            return true;
        }
    }
    false
}

fn ancestor_matches_source(
    ancestor: &url::Origin,
    target_origin: &url::Origin,
    source: &str,
) -> bool {
    if source.eq_ignore_ascii_case("'none'") {
        return false;
    }
    if source.eq_ignore_ascii_case("'self'") {
        return ancestor == target_origin;
    }
    let url::Origin::Tuple(scheme, host, port) = ancestor else {
        return false;
    };
    if source == "*" {
        return matches!(scheme.as_str(), "http" | "https");
    }
    if let Some(required_scheme) = source.strip_suffix(':') {
        return scheme.eq_ignore_ascii_case(required_scheme);
    }
    let (required_scheme, rest) = match source.split_once("://") {
        Some((s, r)) => (Some(s), r),
        None => (None, source),
    };
    if let Some(required) = required_scheme {
        if !scheme.eq_ignore_ascii_case(required) {
            return false;
        }
    } else if let url::Origin::Tuple(target_scheme, _, _) = target_origin {
        let scheme_ok = scheme == target_scheme || (target_scheme == "http" && scheme == "https");
        if !scheme_ok {
            return false;
        }
    } else {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host_pattern, required_port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') => (h, Some(p)),
        _ => (authority, None),
    };
    if let Some(req_port) = required_port {
        if req_port != "*" && req_port.parse::<u16>().ok() != Some(*port) {
            return false;
        }
    } else {
        let default_port = match scheme.as_str() {
            "http" => Some(80u16),
            "https" => Some(443u16),
            _ => None,
        };
        if default_port != Some(*port) {
            return false;
        }
    }
    let host_str = host.to_string();
    if let Some(suffix) = host_pattern.strip_prefix("*.") {
        let suffix = suffix.to_ascii_lowercase();
        let host_lower = host_str.to_ascii_lowercase();
        host_lower.ends_with(&format!(".{suffix}"))
    } else {
        host_str.eq_ignore_ascii_case(host_pattern)
    }
}

fn window_initiator(
    tabs: &TabRegistry,
    assignment: &Assignment,
    source: renderer::NavigationSource,
    opener: renderer::OpenerPolicy,
) -> Result<crate::actor::NavigationInitiator, RendererViolation> {
    let renderer::NavigationSource {
        origin: claim,
        referrer,
        referrer_policy,
        frame: _,
        secure_context,
    } = source;
    // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
    let origin = match tabs.authorize_origin(assignment.tab, &assignment.site, &claim) {
        Some(origin) => origin,
        None if claim.is_opaque() => url::Origin::new_opaque(),
        None => return Err(RendererViolation),
    };
    let referrer = tabs
        .allows_url(assignment.tab, &assignment.site, &referrer)
        .then_some(referrer)
        .filter(|referrer| referrer.origin() == origin);
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
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

#[derive(Clone, Copy)]
struct WindowRouter<'a> {
    context: &'a ServiceContext,
    assignment: &'a Assignment,
    request: RequestId,
}

impl WindowRouter<'_> {
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
        if matches!(
            target,
            renderer::TargetName::Current
                | renderer::TargetName::Parent
                | renderer::TargetName::Top
        ) || !target.is_canonical()
        {
            return Err(RendererViolation);
        }
        // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag
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
        let initiator = window_initiator(&context.tabs, assignment, source, opener)?;
        // https://html.spec.whatwg.org/multipage/browsers.html#sandbox-propagates-to-auxiliary-browsing-contexts-flag
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
        let Some(target) = context.tabs.find_by_name(assignment.tab, &name) else {
            return send_reply(&context.responder, id, ServiceReply::Window(None)).await;
        };
        if let Some(url) = url.and_then(navigable_popup_url) {
            // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
            let Some(active) = context.tabs.frame_sandbox(assignment.tab, source.frame) else {
                return Err(RendererViolation);
            };
            let created_by_caller = context.tabs.opener(target) == Some(assignment.tab)
                && context.tabs.opener_frame(target) == Some(source.frame);
            if target != assignment.tab && active.blocks_navigation() && !created_by_caller {
                logging::debug!(
                    target: "browser::tabs",
                    "window.open navigation refused: a sandboxed document cannot navigate another browsing context"
                );
            } else {
                let initiator = window_initiator(&context.tabs, assignment, source, opener)?;
                if let Ok(handle) = context.browser.tab(target) {
                    tokio::spawn(async move {
                        let _result = handle.goto_from(url.as_str(), initiator).await;
                    });
                }
            }
        }
        let tab = renderer::WindowId::new(target.get());
        send_reply(&context.responder, id, ServiceReply::Window(Some(tab))).await
    }
}

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
            // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
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
        BrowsingContextCall::NavigateTopLevel { url, source } => {
            route_navigate_top_level(context, assignment, id, url, source).await?;
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
            // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close
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
            if !context.tabs.allows_message_origin(
                assignment.tab,
                &assignment.site,
                &message.origin,
            ) {
                return Err(RendererViolation);
            }
            let target = TabId::new(window.get());
            if message_related(context, assignment, target) {
                let _queued = context
                    .tabs
                    .deliver(target, crate::actor::RemoteWindowMessage { message });
            }
            send_reply(&context.responder, id, ServiceReply::Unit).await?;
        }
        BrowsingContextCall::UserPrompt {
            frame,
            kind,
            message,
            default_value,
        } => {
            spawn_user_prompt(
                context,
                assignment.tab,
                id,
                frame,
                kind,
                message,
                default_value,
            );
        }
    }
    Ok(())
}

async fn route_navigate_top_level(
    context: &ServiceContext,
    assignment: &Assignment,
    id: RequestId,
    url: Url,
    source: renderer::NavigationSource,
) -> Result<(), RendererViolation> {
    if let Some(url) = navigable_popup_url(url) {
        let initiator = window_initiator(
            &context.tabs,
            assignment,
            source,
            renderer::OpenerPolicy::Keep,
        )?;
        if let Ok(handle) = context.browser.tab(assignment.tab) {
            tokio::spawn(async move {
                let _result = handle.goto_from(url.as_str(), initiator).await;
            });
        }
    }
    send_reply(&context.responder, id, ServiceReply::Unit).await
}

fn spawn_user_prompt(
    context: &ServiceContext,
    tab: TabId,
    id: RequestId,
    frame: FrameId,
    kind: renderer::PromptKind,
    message: String,
    default_value: Option<String>,
) {
    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#simple-dialogs
    let broker = Arc::clone(context.browser.prompt_broker());
    let responder = context.responder.clone();
    let kill = context.kill.clone();
    tokio::spawn(async move {
        let outcome = broker
            .open_prompt(tab, frame, kind, message, default_value)
            .await;
        if responder
            .reply(id, ServiceReply::Prompt(outcome))
            .await
            .is_err()
        {
            let _ = kill.send(true);
        }
    });
}

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
            StorageCall::RemoteSessionGet {
                window,
                origin,
                key,
            } => {
                let origin = self.key(&origin)?;
                self.remote_session_get(window, &origin, &key).await
            }
            StorageCall::CookieGet {
                url,
                partition_key: _,
            } => self.cookie_get(&url).await,
            StorageCall::CookieSet {
                value,
                url,
                partition_key: _,
            } => self.cookie_set(&value, &url).await,
            StorageCall::CookieStoreGet {
                url,
                partition_key: _,
            } => self.cookie_store_get(&url).await,
            StorageCall::CookieStoreSet {
                write,
                url,
                partition_key: _,
            } => self.cookie_store_set(&write, &url).await,
        }
    }

    async fn remote_session_get(
        &self,
        window: renderer::WindowId,
        origin: &StorageKey,
        key: &str,
    ) -> Result<(), RendererViolation> {
        let target = TabId::new(window.get());
        let value = message_related(self.context, self.authority, target)
            .then(|| self.context.sessions.get(target, origin, key))
            .flatten();
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageValue(value),
        )
        .await
    }

    // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
    // https://privacycg.github.io/CHIPS/
    async fn cookie_get(&self, url: &str) -> Result<(), RendererViolation> {
        let url = self.authorize_cookie_url(url)?;
        let partition_key = self.authority.site.as_partition_key();
        let cookies = self
            .authority
            .partition
            .cookies
            .cookies_for(&url, partition_key);
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::Cookie(cookies),
        )
        .await
    }

    async fn cookie_set(&self, value: &str, url: &str) -> Result<(), RendererViolation> {
        let url = self.authorize_cookie_url(url)?;
        let partition_key = self.authority.site.as_partition_key();
        self.authority
            .partition
            .cookies
            .set_cookie(value, &url, partition_key);
        send_reply(&self.context.responder, self.request, ServiceReply::Unit).await
    }

    async fn cookie_store_get(&self, url: &str) -> Result<(), RendererViolation> {
        let url = self.authorize_cookie_url(url)?;
        let partition_key = self.authority.site.as_partition_key();
        let records = self
            .authority
            .partition
            .cookies
            .cookie_store_records(&url, partition_key);
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::CookieStore(records),
        )
        .await
    }

    async fn cookie_store_set(
        &self,
        write: &net::ScriptCookieWrite,
        url: &str,
    ) -> Result<(), RendererViolation> {
        let url = self.authorize_cookie_url(url)?;
        let partition_key = self.authority.site.as_partition_key();
        self.authority
            .partition
            .cookies
            .cookie_store_store(write, &url, partition_key);
        send_reply(&self.context.responder, self.request, ServiceReply::Unit).await
    }

    fn key(&self, origin: &renderer::SerializedOrigin) -> Result<StorageKey, RendererViolation> {
        self.context
            .tabs
            .storage_key(self.authority.tab, &self.authority.site, origin)
            .ok_or(RendererViolation)
    }

    fn authorize_cookie_url(&self, url: &str) -> Result<Url, RendererViolation> {
        Url::parse(url)
            .ok()
            .filter(|parsed| {
                self.context
                    .tabs
                    .allows_url(self.authority.tab, &self.authority.site, parsed)
            })
            .ok_or(RendererViolation)
    }

    fn authorize_storage_event_url(&self, url: &str) -> Result<(), RendererViolation> {
        Url::parse(url)
            .ok()
            .filter(|parsed| {
                matches!(parsed.scheme(), "about" | "data")
                    || self.context.tabs.allows_url(
                        self.authority.tab,
                        &self.authority.site,
                        parsed,
                    )
            })
            .map(|_| ())
            .ok_or(RendererViolation)
    }

    async fn get(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        key: &str,
    ) -> Result<(), RendererViolation> {
        let value = match kind {
            StorageKind::Local => self.authority.partition.local_storage.get(origin, key),
            StorageKind::Session => self.context.sessions.get(self.authority.tab, origin, key),
            StorageKind::IndexedDb => self.authority.partition.indexed_db.get(origin, key),
        };
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageValue(value),
        )
        .await
    }

    async fn keys(&self, kind: StorageKind, origin: &StorageKey) -> Result<(), RendererViolation> {
        let keys = match kind {
            StorageKind::Local => self.authority.partition.local_storage.keys(origin),
            StorageKind::Session => self.context.sessions.keys(self.authority.tab, origin),
            StorageKind::IndexedDb => self.authority.partition.indexed_db.keys(origin),
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
        self.authorize_storage_event_url(url)?;
        let change = match kind {
            StorageKind::Local => self
                .authority
                .partition
                .local_storage
                .set(origin, key, value),
            StorageKind::Session => {
                self.context
                    .sessions
                    .set(self.authority.tab, origin, key, value)
            }
            StorageKind::IndexedDb => self.authority.partition.indexed_db.set(origin, key, value),
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
        self.authorize_storage_event_url(url)?;
        let change = match kind {
            StorageKind::Local => self.authority.partition.local_storage.remove(origin, key),
            StorageKind::Session => self
                .context
                .sessions
                .remove(self.authority.tab, origin, key),
            StorageKind::IndexedDb => self.authority.partition.indexed_db.remove(origin, key),
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
        self.authorize_storage_event_url(url)?;
        let change = match kind {
            StorageKind::Local => self.authority.partition.local_storage.clear(origin),
            StorageKind::Session => self.context.sessions.clear(self.authority.tab, origin),
            StorageKind::IndexedDb => self.authority.partition.indexed_db.clear(origin),
        };
        self.publish(source, kind, origin, url, change.as_ref());
        send_reply(
            &self.context.responder,
            self.request,
            ServiceReply::StorageChanged(Ok(change)),
        )
        .await
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
                &self.authority.partition,
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
    partition: &PartitionServices,
    assignment: RendererAssignmentId,
    source: FrameId,
    origin: &StorageKey,
    url: &str,
    change: &StorageChange,
) {
    partition
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

const MAX_STDERR_LINE_BYTES: usize = 16 * 1024;

async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    out: &mut Vec<u8>,
) -> io::Result<Option<bool>> {
    out.clear();
    let mut saw_any = false;
    let mut truncated = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(saw_any.then_some(truncated));
        }
        saw_any = true;
        let newline = available.iter().position(|&b| b == b'\n');
        let take = newline.map_or(available.len(), |pos| pos + 1);
        let remaining = MAX_STDERR_LINE_BYTES.saturating_sub(out.len());
        let copy = take.min(remaining);
        out.extend_from_slice(&available[..copy]);
        if take > copy {
            truncated = true;
        }
        reader.consume(take);
        if newline.is_some() {
            return Ok(Some(truncated));
        }
    }
}

async fn forward_stderr(stderr: tokio::process::ChildStderr) {
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut bytes = Vec::new();
    while let Ok(Some(truncated)) = read_bounded_line(&mut reader, &mut bytes).await {
        let text = String::from_utf8_lossy(&bytes);
        if truncated {
            logging::log_forwarded(&format!("{text} [truncated]\n"));
        } else {
            logging::log_forwarded(&text);
        }
    }
}

struct SpawnedRenderer {
    child: Child,
    reader: Box<dyn AsyncRead + Send + Unpin>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    #[cfg(windows)]
    job: Option<crate::child::sandbox::ProcessJob>,
}

#[cfg(unix)]
fn spawn_transport(sandbox: crate::child::RendererSandbox) -> io::Result<SpawnedRenderer> {
    let mut command = renderer_command(sandbox)?;
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
fn spawn_transport(sandbox: crate::child::RendererSandbox) -> io::Result<SpawnedRenderer> {
    let mut command = renderer_command(sandbox)?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let job = match child
        .raw_handle()
        .ok_or_else(|| io::Error::other("renderer process handle missing"))
        .and_then(|handle| crate::child::sandbox::ProcessJob::assign(handle, sandbox))
    {
        Ok(job) => Some(job),
        Err(error) => {
            let _ = child.start_kill();
            return Err(error);
        }
    };
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
        #[cfg(windows)]
        job,
    })
}

fn subscribe_renderer_events(
    partition: &PartitionServices,
    client: &RendererClient,
    kill: &watch::Sender<bool>,
    renderer: RendererId,
    registry: Arc<AssignmentRegistry>,
    tabs: TabRegistry,
) {
    let client = client.clone();
    let kill = kill.clone();
    partition.events.subscribe(Box::new(move |event| {
        // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
        if !registry.renderer_hosts_origin(renderer, &tabs, event.origin().origin()) {
            return true;
        }
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

pub(crate) struct RendererServices {
    pub(crate) identity: net::ValidIdentity,
    pub(crate) sessions: Arc<SessionStorage>,
    pub(crate) browser: crate::browser::BrowserHandle,
    pub(crate) registry: Arc<AssignmentRegistry>,
    pub(crate) tabs: TabRegistry,
    pub(crate) sandbox: crate::child::RendererSandbox,
}

fn renderer_command(sandbox: crate::child::RendererSandbox) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--renderer");
    match sandbox {
        crate::child::RendererSandbox::Enabled => {}
        crate::child::RendererSandbox::Disabled => {
            command.arg("--no-sandbox");
        }
    }
    command
        .env_clear()
        .env("TINYBROWSER_LOG", logging::level().as_str())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(target_os = "linux")]
    {
        static LINUX_FONT_DIRS: std::sync::OnceLock<Option<std::ffi::OsString>> =
            std::sync::OnceLock::new();
        if let Some(joined) = LINUX_FONT_DIRS.get_or_init(|| {
            let dirs = renderer::linux_system_font_dirs();
            if dirs.is_empty() {
                None
            } else {
                std::env::join_paths(dirs).ok()
            }
        }) {
            command.env("TINYBROWSER_FONT_DIRS", joined);
        }
    }
    for key in [
        "FONTCONFIG_FILE",
        "FONTCONFIG_PATH",
        "XDG_DATA_DIRS",
        "LANG",
        "LC_ALL",
        "TZ",
        "SYSTEMROOT",
    ] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    Ok(command)
}

pub(crate) async fn spawn_process(
    id: RendererId,
    bound: Option<(RendererLock, PartitionServices)>,
    services: RendererServices,
    slot: tokio::sync::OwnedSemaphorePermit,
) -> io::Result<RendererHandle> {
    let RendererServices {
        identity,
        sessions,
        browser,
        registry,
        tabs,
        sandbox,
    } = services;
    let SpawnedRenderer {
        mut child,
        reader,
        writer,
        #[cfg(windows)]
        job,
    } = spawn_transport(sandbox)?;
    let Some(stderr) = child.stderr.take() else {
        let _ = child.start_kill();
        return Err(io::Error::other("renderer stderr missing"));
    };
    let (tx, rx) = exchange::pair(COMMAND_CAPACITY, COMMAND_CAPACITY);
    let (client, server, router) = exchange::endpoint(tx, COMMAND_CAPACITY);
    let (kill, kill_rx) = watch::channel(false);
    let lock = match bound {
        Some((lock, partition)) => {
            subscribe_renderer_events(
                &partition,
                &client,
                &kill,
                id,
                Arc::clone(&registry),
                tabs.clone(),
            );
            Some(lock)
        }
        None => None,
    };
    let alive = Arc::new(AtomicBool::new(true));
    let (ready_tx, ready_rx) = oneshot::channel();
    let failure = LinkFailure {
        router,
        alive: Arc::clone(&alive),
        kill: kill.clone(),
        renderer: id,
        registry: Arc::clone(&registry),
    };
    let writer_task = tokio::spawn(writer_task(rx, writer, failure.clone(), kill_rx.clone()));
    let lock = Arc::new(Mutex::new(lock));
    let service_context = ServiceContext {
        responder: server.responder(),
        renderer: id,
        registry,
        tabs,
        sessions,
        kill: kill.clone(),
        browser,
        dials: Arc::new(Mutex::new(HashMap::new())),
    };
    let reader_task = tokio::spawn(reader_task(reader, failure, Some(ready_tx)));
    let service_task = tokio::spawn(service_task(server, service_context));
    let stderr_task = tokio::spawn(forward_stderr(stderr));
    let _reaper = tokio::spawn(child_task(
        child,
        kill_rx,
        #[cfg(windows)]
        job,
    ));
    let tasks = vec![writer_task, reader_task, service_task, stderr_task];
    if let Err(error) = complete_handshake(&client, identity, ready_rx).await {
        let _ = kill.send(true);
        for task in tasks {
            let _result = task.await;
        }
        return Err(error);
    }
    logging::debug!(
        target: "browser::link",
        "renderer {id:?} ready for site lock {lock:?}"
    );
    Ok(RendererHandle {
        id,
        client,
        alive,
        kill,
        tasks: Mutex::new(tasks),
        lock,
        assignments: AtomicUsize::new(0),
        _slot: slot,
    })
}

async fn complete_handshake(
    client: &RendererClient,
    identity: net::ValidIdentity,
    ready_rx: oneshot::Receiver<Result<(), String>>,
) -> io::Result<()> {
    client
        .notify(HostNotice::Hello {
            identity: Box::new(identity),
        })
        .await
        .map_err(|_| io::Error::other("renderer handshake failed: channel closed"))?;
    match timeout(HANDSHAKE_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(message))) => Err(io::Error::other(format!(
            "renderer handshake failed: {message}"
        ))),
        Ok(Err(_)) => Err(io::Error::other("renderer handshake failed")),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "renderer handshake timed out",
        )),
    }
}

async fn child_task(
    mut child: Child,
    mut kill: watch::Receiver<bool>,
    #[cfg(windows)] _job: Option<crate::child::sandbox::ProcessJob>,
) {
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

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxing
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

    // https://w3c.github.io/webappsec-csp/#directive-frame-ancestors
    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#the-x-frame-options-header
    #[test]
    fn frame_ancestors_and_xfo_enforce_exact_ancestor_chain() {
        let target = url::Url::parse("https://app.example.test/embed").expect("target");
        let same_origin = target.origin();
        let same_site_other = url::Url::parse("https://other.example.test/")
            .expect("url")
            .origin();
        let partner = url::Url::parse("https://partner.test/")
            .expect("url")
            .origin();
        let sub_partner = url::Url::parse("https://sub.partner.test/")
            .expect("url")
            .origin();
        let evil = url::Url::parse("https://evilpartner.test/")
            .expect("url")
            .origin();

        let csp_self = vec![(
            "content-security-policy".to_owned(),
            "frame-ancestors 'self'".to_owned(),
        )];
        assert!(!super::frame_blocked_by_headers(
            &csp_self,
            &target,
            std::slice::from_ref(&same_origin),
        ));
        assert!(super::frame_blocked_by_headers(
            &csp_self,
            &target,
            std::slice::from_ref(&same_site_other),
        ));

        let csp_allow = vec![(
            "content-security-policy".to_owned(),
            "frame-ancestors https://partner.test *.partner.test".to_owned(),
        )];
        assert!(!super::frame_blocked_by_headers(
            &csp_allow,
            &target,
            &[partner, sub_partner],
        ));
        assert!(super::frame_blocked_by_headers(
            &csp_allow,
            &target,
            std::slice::from_ref(&evil),
        ));

        let permissive_fa_overrides_xfo = vec![
            (
                "content-security-policy".to_owned(),
                "frame-ancestors 'self'".to_owned(),
            ),
            ("x-frame-options".to_owned(), "DENY".to_owned()),
        ];
        assert!(!super::frame_blocked_by_headers(
            &permissive_fa_overrides_xfo,
            &target,
            std::slice::from_ref(&same_origin),
        ));

        let xfo_sameorigin = vec![("x-frame-options".to_owned(), "SAMEORIGIN".to_owned())];
        assert!(!super::frame_blocked_by_headers(
            &xfo_sameorigin,
            &target,
            std::slice::from_ref(&same_origin),
        ));
        assert!(super::frame_blocked_by_headers(
            &xfo_sameorigin,
            &target,
            &[same_origin, evil],
        ));
        assert!(super::frame_blocked_by_headers(
            &xfo_sameorigin,
            &target,
            &[same_site_other],
        ));

        let schemeless = vec![(
            "content-security-policy".to_owned(),
            "frame-ancestors partner.test".to_owned(),
        )];
        let http_partner = url::Url::parse("http://partner.test/")
            .expect("url")
            .origin();
        let alt_port_partner = url::Url::parse("https://partner.test:8443/")
            .expect("url")
            .origin();
        assert!(super::frame_blocked_by_headers(
            &schemeless,
            &target,
            std::slice::from_ref(&http_partner),
        ));
        assert!(super::frame_blocked_by_headers(
            &schemeless,
            &target,
            std::slice::from_ref(&alt_port_partner),
        ));
        let http_target = url::Url::parse("http://app.example.test/embed").expect("target");
        let secure_partner = url::Url::parse("https://partner.test/")
            .expect("url")
            .origin();
        assert!(!super::frame_blocked_by_headers(
            &schemeless,
            &http_target,
            &[http_partner, secure_partner],
        ));
    }

    // https://w3c.github.io/webappsec-csp/#directive-frame-ancestors
    #[test]
    fn empty_frame_ancestors_blocks_embedding() {
        let target = url::Url::parse("https://app.example.test/embed").expect("target");
        let same_origin = target.origin();
        let empty = vec![(
            "content-security-policy".to_owned(),
            "frame-ancestors".to_owned(),
        )];
        assert!(super::frame_blocked_by_headers(
            &empty,
            &target,
            std::slice::from_ref(&same_origin),
        ));
    }

    #[tokio::test]
    async fn read_bounded_line_caps_unbounded_stderr_and_recovers() {
        let mut input = vec![b'a'; super::MAX_STDERR_LINE_BYTES + 4096];
        input.push(b'\n');
        input.extend_from_slice(b"next line\n");
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(input));
        let mut buf = Vec::new();
        assert_eq!(
            super::read_bounded_line(&mut reader, &mut buf)
                .await
                .expect("first line"),
            Some(true)
        );
        assert_eq!(buf.len(), super::MAX_STDERR_LINE_BYTES);
        assert_eq!(
            super::read_bounded_line(&mut reader, &mut buf)
                .await
                .expect("second line"),
            Some(false)
        );
        assert_eq!(buf, b"next line\n");
        assert_eq!(
            super::read_bounded_line(&mut reader, &mut buf)
                .await
                .expect("eof"),
            None
        );
    }
}
