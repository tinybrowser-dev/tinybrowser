use std::collections::{HashMap, HashSet, VecDeque};
use std::future::ready;
use std::sync::Arc;
use std::time::Duration;

use renderer::{Engine, FrameId, MAX_RESPONSE_BODY_BYTES, Stop, TURN_SCRIPT_BUDGET, TabError};
use tokio::sync::Notify;

use super::{HostServices, ServiceClient};
use crate::exchange::{self, Frame, RequestId};
use crate::wire::channel::MAX_BODY_CHUNK_BYTES;
use crate::wire::{
    Command, FromRenderer, HostNotice, RendererAssignmentId, RendererCall, RendererNotice,
    RendererReply, Reply, ResponseStart, ToRenderer,
};

const MAX_CANCELLED_STREAMS: usize = 256;

enum Handled {
    Reply(Reply),
    Screenshot(Vec<u8>),
}

pub(super) async fn run(
    mut inbox: exchange::Receiver<ToRenderer>,
    outbox: &exchange::BlockingSender<FromRenderer>,
    client: Arc<ServiceClient>,
    stop: &Arc<Stop>,
    wake: Arc<Notify>,
) {
    let mut engines = HashMap::<RendererAssignmentId, Engine>::new();
    let mut responses = ResponseStreams::default();
    let mut session = Session {
        identity: net::ValidIdentity::default(),
        stop: Arc::clone(stop),
        wake: Arc::clone(&wake),
    };
    loop {
        if stop.is_set() {
            stop.request();
            break;
        }
        match drain_engines(&mut engines, outbox) {
            Drained::Failed => {
                stop.request();
                break;
            }
            Drained::Yielded => match poll_inbox(&mut inbox).await {
                Polled::Empty => {}
                Polled::Received(received) => {
                    if !handle_host(
                        *received,
                        &mut engines,
                        &mut responses,
                        outbox,
                        &client,
                        &mut session,
                    ) {
                        stop.request();
                        break;
                    }
                }
            },
            Drained::Idle => {
                let deadline = engines.values().filter_map(Engine::next_deadline).min();
                tokio::select! {
                    received = inbox.recv() => {
                        if !handle_host(
                            received,
                            &mut engines,
                            &mut responses,
                            outbox,
                            &client,
                            &mut session,
                        ) {
                            stop.request();
                            break;
                        }
                    }
                    () = wake.notified() => {}
                    () = wait_for_deadline(deadline) => {}
                }
            }
        }
    }
    for (_, mut engine) in engines {
        engine.shutdown();
    }
}

struct Session {
    identity: net::ValidIdentity,
    stop: Arc<Stop>,
    wake: Arc<Notify>,
}

fn handle_host(
    received: Option<ToRenderer>,
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    responses: &mut ResponseStreams,
    outbox: &exchange::BlockingSender<FromRenderer>,
    client: &Arc<ServiceClient>,
    session: &mut Session,
) -> bool {
    match received {
        Some(Frame::Notify(HostNotice::Hello { identity })) => {
            session.identity = *identity;
            true
        }
        Some(Frame::Notify(HostNotice::Assign { assignment })) => {
            assign_engine(engines, assignment, client, session)
        }
        Some(Frame::Notify(HostNotice::Release { assignment })) => {
            responses.release(assignment, engines);
            if let Some(mut engine) = engines.remove(&assignment) {
                engine.release();
            }
            true
        }
        Some(Frame::Call {
            id,
            body:
                RendererCall::Command {
                    assignment,
                    command,
                },
        }) => handle_request(engines, outbox, id, assignment, command),
        Some(Frame::Call {
            id,
            body: RendererCall::Response { response },
        }) => {
            let Some(engine) = engines.get_mut(&response.assignment) else {
                responses.cancel(id, engines);
                return reply_result(
                    outbox,
                    id,
                    (
                        response.assignment,
                        Err(stream_error("response assignment is gone")),
                    ),
                );
            };
            match responses.start(id, &response, engine) {
                Ok(()) => true,
                Err(error) => reply_result(outbox, id, (response.assignment, Err(error))),
            }
        }
        Some(Frame::RequestChunk { id, bytes }) => match responses.push(id, &bytes, engines) {
            Ok(()) => true,
            Err((assignment, error)) => reply_result(outbox, id, (assignment, Err(error))),
        },
        Some(Frame::RequestEnd { id, error: None }) => {
            if responses.is_cancelled(id) {
                true
            } else {
                reply_result(outbox, id, responses.finish(id, engines))
            }
        }
        Some(Frame::RequestEnd {
            id,
            error: Some(error),
        }) => {
            if responses.is_cancelled(id) {
                true
            } else {
                reply_result(outbox, id, responses.abort(id, &error, engines))
            }
        }
        Some(Frame::Notify(HostNotice::StorageEvent {
            origin,
            kind,
            key,
            old_value,
            new_value,
            url,
            source,
        })) => {
            queue_storage_event(
                engines,
                &renderer::PendingStorageEvent::broadcast(
                    origin, kind, key, old_value, new_value, url,
                ),
                source,
            );
            true
        }
        Some(Frame::Notify(HostNotice::BroadcastMessage {
            origin,
            name,
            payload,
            source,
        })) => {
            queue_broadcast_message(engines, &origin, &name, &payload, source);
            true
        }
        Some(Frame::Cancel { id }) => {
            responses.cancel(id, engines);
            true
        }
        Some(
            Frame::Notify(HostNotice::Shutdown) | Frame::Reply { .. } | Frame::ResponseChunk { .. },
        )
        | None => false,
    }
}

fn handle_request(
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    outbox: &exchange::BlockingSender<FromRenderer>,
    id: RequestId,
    assignment: RendererAssignmentId,
    command: Command,
) -> bool {
    let Some(engine) = engines.get_mut(&assignment) else {
        let reply = match command {
            Command::ExecuteScript { .. } => Reply::Value(Err(stream_error("unknown assignment"))),
            Command::Screenshot { .. } => Reply::Screenshot {
                result: Err(stream_error("unknown assignment")),
            },
            Command::Dom { .. } => Reply::Dom(Err(stream_error("unknown assignment"))),
            Command::DocumentState { .. } => {
                Reply::DocumentState(Err(stream_error("unknown assignment")))
            }
            Command::AddInitScript { .. }
            | Command::AddBinding { .. }
            | Command::RemoveInitScript { .. }
            | Command::SetViewport { .. }
            | Command::SetIdentity { .. }
            | Command::ContentSize { .. }
            | Command::WaitForJs { .. }
            | Command::CancelWait { .. }
            | Command::WindowMessage(..)
            | Command::HistoryTraverse { .. }
            | Command::HistoryTraverseFrame { .. }
            | Command::SetHistory { .. } => Reply::Unit(Err(stream_error("unknown assignment"))),
        };
        return send_to_browser(
            outbox,
            Frame::Reply {
                id,
                body: RendererReply { assignment, reply },
            },
        );
    };
    match handle_command(engine, command) {
        Handled::Reply(reply) => send_to_browser(
            outbox,
            Frame::Reply {
                id,
                body: RendererReply { assignment, reply },
            },
        ),
        Handled::Screenshot(png) => stream_screenshot(outbox, id, assignment, &png),
    }
}

fn reply_result(
    outbox: &exchange::BlockingSender<FromRenderer>,
    id: RequestId,
    (assignment, result): (RendererAssignmentId, Result<(), TabError>),
) -> bool {
    send_to_browser(
        outbox,
        Frame::Reply {
            id,
            body: RendererReply {
                assignment,
                reply: Reply::Unit(result),
            },
        },
    )
}

fn assign_engine(
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    assignment: RendererAssignmentId,
    client: &Arc<ServiceClient>,
    session: &Session,
) -> bool {
    if engines.contains_key(&assignment) {
        return false;
    }
    let assignment_services = Arc::new(HostServices::new(assignment, Arc::clone(client)));
    engines.insert(
        assignment,
        Engine::new(
            assignment_services,
            Arc::clone(&session.stop),
            Arc::clone(&session.wake),
            session.identity.clone(),
        ),
    );
    true
}

fn queue_broadcast_message(
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    origin: &renderer::StorageKey,
    name: &str,
    payload: &str,
    source: Option<(RendererAssignmentId, u64)>,
) {
    for (assignment, engine) in engines {
        let source_channel = source.and_then(|(source_assignment, channel)| {
            (source_assignment == *assignment).then_some(channel)
        });
        engine.receive_broadcast_message(origin, name, payload, source_channel);
    }
}

fn queue_storage_event(
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    event: &renderer::PendingStorageEvent,
    source: Option<(RendererAssignmentId, FrameId)>,
) {
    for (assignment, engine) in engines {
        let mut event = event.clone();
        event.source = source.and_then(|(source_assignment, frame)| {
            (source_assignment == *assignment).then_some(frame)
        });
        engine.receive_storage_event(event);
    }
}

enum Drained {
    Idle,
    Yielded,
    Failed,
}

enum Polled {
    Empty,
    Received(Box<Option<ToRenderer>>),
}

async fn poll_inbox(inbox: &mut exchange::Receiver<ToRenderer>) -> Polled {
    tokio::select! {
        biased;
        received = inbox.recv() => Polled::Received(Box::new(received)),
        () = ready(()) => Polled::Empty,
    }
}

const DRAIN_SLICE: Duration = Duration::from_millis(50);

fn drain_engines(
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    outbox: &exchange::BlockingSender<FromRenderer>,
) -> Drained {
    let now = std::time::Instant::now();
    let yield_deadline = now + DRAIN_SLICE;
    let script_deadline = now + TURN_SCRIPT_BUDGET;
    let mut yielded = false;
    for (assignment, engine) in engines {
        if std::time::Instant::now() >= yield_deadline {
            yielded = true;
            break;
        }
        if engine.drain_ready_slice(yield_deadline, script_deadline) {
            yielded = true;
        }
        if !publish(*assignment, engine, outbox) {
            return Drained::Failed;
        }
        if engine.has_ready_work() {
            yielded = true;
        }
    }
    if yielded {
        Drained::Yielded
    } else {
        Drained::Idle
    }
}

struct ActiveResponse {
    assignment: RendererAssignmentId,
    frame: FrameId,
    bytes: usize,
}

#[derive(Default)]
struct ResponseStreams {
    active: HashMap<RequestId, ActiveResponse>,
    cancelled: HashSet<RequestId>,
    cancelled_order: VecDeque<RequestId>,
}

impl ResponseStreams {
    fn is_cancelled(&self, id: RequestId) -> bool {
        self.cancelled.contains(&id)
    }

    fn tombstone(&mut self, id: RequestId) {
        if self.cancelled.insert(id) {
            self.cancelled_order.push_back(id);
        }
        while self.cancelled.len() > MAX_CANCELLED_STREAMS {
            let Some(oldest) = self.cancelled_order.pop_front() else {
                break;
            };
            self.cancelled.remove(&oldest);
        }
    }

    fn start(
        &mut self,
        id: RequestId,
        response: &ResponseStart,
        engine: &mut Engine,
    ) -> Result<(), TabError> {
        if self.cancelled.contains(&id) {
            return Ok(());
        }
        if self.active.contains_key(&id) {
            return Err(stream_error("duplicate response start"));
        }
        engine.open_body(response.frame, &response.head, &response.tab)?;
        self.active.insert(
            id,
            ActiveResponse {
                assignment: response.assignment,
                frame: response.frame,
                bytes: 0,
            },
        );
        Ok(())
    }

    fn push(
        &mut self,
        id: RequestId,
        bytes: &[u8],
        engines: &mut HashMap<RendererAssignmentId, Engine>,
    ) -> Result<(), (RendererAssignmentId, TabError)> {
        if self.cancelled.contains(&id) {
            return Ok(());
        }
        let Some(response) = self.active.get_mut(&id) else {
            return Ok(());
        };
        response.bytes = response.bytes.saturating_add(bytes.len());
        if response.bytes > MAX_RESPONSE_BODY_BYTES {
            let assignment = response.assignment;
            let frame = response.frame;
            self.active.remove(&id);
            self.tombstone(id);
            if let Some(engine) = engines.get_mut(&assignment) {
                let _result = engine.abort_body(frame);
            }
            return Err((
                assignment,
                stream_error("streamed response exceeds body limit"),
            ));
        }
        let assignment = response.assignment;
        let frame = response.frame;
        let result = match engines.get_mut(&assignment) {
            Some(engine) => engine
                .write_body(frame, bytes)
                .map_err(|error| (assignment, error)),
            None => Err((assignment, stream_error("response assignment is gone"))),
        };
        if result.is_err() {
            self.active.remove(&id);
            self.tombstone(id);
            if let Some(engine) = engines.get_mut(&assignment) {
                let _result = engine.abort_body(frame);
            }
        }
        result
    }

    fn finish(
        &mut self,
        id: RequestId,
        engines: &mut HashMap<RendererAssignmentId, Engine>,
    ) -> (RendererAssignmentId, Result<(), TabError>) {
        let response = match self.take(id, "response end without start") {
            Ok(response) => response,
            Err((assignment, error)) => return (assignment, Err(error)),
        };
        let assignment = response.assignment;
        let result =
            engine_mut(engines, assignment).and_then(|engine| engine.end_body(response.frame));
        (assignment, result)
    }

    fn abort(
        &mut self,
        id: RequestId,
        failure: &str,
        engines: &mut HashMap<RendererAssignmentId, Engine>,
    ) -> (RendererAssignmentId, Result<(), TabError>) {
        let response = match self.take(id, "response error without start") {
            Ok(response) => response,
            Err((assignment, error)) => return (assignment, Err(error)),
        };
        if let Ok(engine) = engine_mut(engines, response.assignment) {
            let _ = engine.abort_body(response.frame);
        }
        (
            response.assignment,
            Err(stream_error(&format!(
                "streamed response failed: {failure}"
            ))),
        )
    }

    fn cancel(&mut self, id: RequestId, engines: &mut HashMap<RendererAssignmentId, Engine>) {
        if let Some(response) = self.active.remove(&id)
            && let Some(engine) = engines.get_mut(&response.assignment)
        {
            let _result = engine.abort_body(response.frame);
        }
        self.tombstone(id);
    }

    fn take(
        &mut self,
        id: RequestId,
        missing_start: &str,
    ) -> Result<ActiveResponse, (RendererAssignmentId, TabError)> {
        self.active
            .remove(&id)
            .ok_or_else(|| (RendererAssignmentId::new(0), stream_error(missing_start)))
    }

    fn release(
        &mut self,
        assignment: RendererAssignmentId,
        engines: &mut HashMap<RendererAssignmentId, Engine>,
    ) {
        let stale: Vec<_> = self
            .active
            .iter()
            .filter(|(_, response)| response.assignment == assignment)
            .map(|(&id, response)| (id, response.frame))
            .collect();
        for (id, frame) in stale {
            self.active.remove(&id);
            self.tombstone(id);
            if let Some(engine) = engines.get_mut(&assignment) {
                let _result = engine.abort_body(frame);
            }
        }
    }
}

fn engine_mut(
    engines: &mut HashMap<RendererAssignmentId, Engine>,
    assignment: RendererAssignmentId,
) -> Result<&mut Engine, TabError> {
    engines
        .get_mut(&assignment)
        .ok_or_else(|| stream_error("response assignment is gone"))
}

fn stream_error(message: &str) -> TabError {
    TabError::RendererUnavailable {
        message: message.to_owned(),
    }
}

async fn wait_for_deadline(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

fn handle_command(engine: &mut Engine, command: Command) -> Handled {
    match command {
        Command::ExecuteScript {
            frame,
            source,
            timeout_ms,
        } => {
            let timeout = timeout_ms.map(Duration::from_millis);
            let value = engine.execute_remote_in(frame, source.as_ref(), timeout);
            Handled::Reply(Reply::Value(value))
        }
        Command::AddInitScript {
            id,
            source,
            run_immediately,
        } => {
            engine.add_init_script(id, &source, run_immediately);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
        Command::AddBinding {
            name,
            run_immediately,
        } => {
            engine.add_binding(&name, run_immediately);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        Command::RemoveInitScript { id } => {
            engine.remove_init_script(id);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        Command::SetViewport {
            frame,
            width,
            height,
        } => Handled::Reply(Reply::Unit(engine.set_viewport(frame, width, height))),
        Command::SetIdentity { identity } => {
            engine.set_identity(*identity);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        Command::ContentSize { frame } => {
            Handled::Reply(Reply::ContentSize(engine.content_size(frame)))
        }
        Command::DocumentState { frame } => {
            Handled::Reply(Reply::DocumentState(engine.document_state(frame)))
        }
        Command::Dom { frame, request } => {
            Handled::Reply(Reply::Dom(engine.dom(frame, &request).map(Box::new)))
        }
        Command::WaitForJs { id, frame, source } => {
            engine.add_js_waiter(id, frame, source);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        Command::CancelWait { id } => {
            engine.cancel_js_waiter(id);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        Command::Screenshot { frame, request } => match engine.screenshot_frame(frame, &request) {
            Ok(png) => Handled::Screenshot(png),
            Err(error) => Handled::Reply(Reply::Screenshot { result: Err(error) }),
        },
        Command::WindowMessage(message) => {
            engine.receive_remote_window_message(message);
            Handled::Reply(Reply::Unit(Ok(())))
        }
        Command::HistoryTraverse { url, history } => {
            Handled::Reply(Reply::Unit(engine.traverse_history(&url, &history)))
        }
        Command::HistoryTraverseFrame {
            frame,
            index,
            url,
            history,
            same_document,
            reload,
        } => Handled::Reply(Reply::Unit(engine.traverse_frame_history(
            frame,
            index,
            &url,
            &history,
            same_document,
            reload,
        ))),
        Command::SetHistory { frame, history } => {
            engine.set_history(frame, &history);
            Handled::Reply(Reply::Unit(Ok(())))
        }
    }
}

fn stream_screenshot(
    outbox: &exchange::BlockingSender<FromRenderer>,
    id: RequestId,
    assignment: RendererAssignmentId,
    png: &[u8],
) -> bool {
    let Ok(len) = u32::try_from(png.len()) else {
        return send_to_browser(
            outbox,
            Frame::Reply {
                id,
                body: RendererReply {
                    assignment,
                    reply: Reply::Screenshot {
                        result: Err(stream_error("screenshot exceeds the IPC length cap")),
                    },
                },
            },
        );
    };
    for chunk in png.chunks(MAX_BODY_CHUNK_BYTES) {
        if outbox
            .try_send(Frame::ResponseChunk {
                id,
                bytes: chunk.to_vec(),
            })
            .is_err()
        {
            return false;
        }
    }
    send_to_browser(
        outbox,
        Frame::Reply {
            id,
            body: RendererReply {
                assignment,
                reply: Reply::Screenshot { result: Ok(len) },
            },
        },
    )
}

fn publish(
    assignment: RendererAssignmentId,
    engine: &mut Engine,
    outbox: &exchange::BlockingSender<FromRenderer>,
) -> bool {
    let settled = engine.settle_js_waiters();
    let Ok(events) = engine.take_browser_events() else {
        return false;
    };
    for (frame, event) in events {
        if !send_to_browser(
            outbox,
            Frame::Notify(RendererNotice::Event {
                assignment,
                frame,
                event,
            }),
        ) {
            return false;
        }
    }
    for id in settled {
        if !send_to_browser(
            outbox,
            Frame::Notify(RendererNotice::WaiterSettled { assignment, id }),
        ) {
            return false;
        }
    }
    true
}

fn send_to_browser(outbox: &exchange::BlockingSender<FromRenderer>, message: FromRenderer) -> bool {
    outbox.try_send(message).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftover_chunks_after_cancel_are_ignored() {
        let mut streams = ResponseStreams::default();
        let mut engines = HashMap::new();
        let id = RequestId::new(1);
        streams.cancel(id, &mut engines);
        assert!(streams.push(id, b"leftover", &mut engines).is_ok());
        assert!(streams.is_cancelled(id));
    }

    #[test]
    fn cancel_before_start_ignores_later_chunks() {
        let mut streams = ResponseStreams::default();
        let mut engines = HashMap::new();
        let id = RequestId::new(7);
        streams.cancel(id, &mut engines);
        assert!(streams.push(id, b"early", &mut engines).is_ok());
    }
}
