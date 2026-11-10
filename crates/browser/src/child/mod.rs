//! The renderer *as a child process*: this crate's second role.
//!
//! The child is the same executable re-executed with the `renderer` argument.
//! On Unix the browser hands it one end of an unnamed socket pair as file
//! descriptor 0; other platforms keep the stdin/stdout pipes. stderr stays for
//! diagnostics, which `crate::link` forwards into the daemon's log.
//!
//! This module is child-side only. It must not reach for the `link`,
//! `manager`, `actor`, or `store` modules: a renderer serves one process's
//! pages and never acts as a browser. `serve` takes no handles for exactly that
//! reason — everything it needs comes from fd 0 and the engine.
//!
//! The loop runs as a future on one current-thread Tokio runtime and owns every
//! wait: commands, dial completions, timer deadlines, and shutdown. The reader
//! and writer stay blocking threads because synchronous browser-service calls
//! (`document.cookie`) must make progress while the page engine runs. The
//! reader never waits for inbox capacity: a full command inbox disconnects.
//! Storage and broadcast notices are best-effort, like the spec's task queue:
//! the engine may be blocked in a synchronous service call, so dropping them
//! on a full inbox keeps the service reply path alive.

mod session;

/// Bounded command channel: one renderer's inbound messages.
const INBOX_CAPACITY: usize = 256;

/// Bounded outbox control lane: cancellation.
const OUTBOX_CONTROL_CAPACITY: usize = 256;

/// Bounded outbox data lane: replies, reverse calls, events, and screenshot chunks.
const OUTBOX_DATA_CAPACITY: usize = 4096;

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::thread;

use renderer::{
    BrowsingContextHost, DialCompletion, DialRequest, FrameId, MessagingHost, NetworkHost, Stop,
    StorageChange, StorageError, StorageHost, StorageKind,
};
use tokio::sync::Notify;
use url::Url;

use crate::exchange::{self, BlockingClient, Frame, RequestId};
use crate::wire::channel::{FrameKind, decode_control, read_frame, write_frame};
use crate::wire::{
    BrowserCall, BrowsingContextCall, FromRenderer, HostNotice, MessagingCall, NetworkCall,
    RendererAssignmentId, RendererNotice, RendererReply, ServiceCall, ServiceReply, StorageCall,
    ToRenderer,
};
use renderer::{OpenerPolicy, WindowId};

/// Runs the renderer child until `Shutdown` or the channel closes.
///
/// # Errors
///
/// Runtime creation, channel bootstrap, or writer failure.
pub fn serve() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let writer = runtime.block_on(serve_async())?;
    writer
        .join()
        .map_err(|_| io::Error::other("writer panicked"))?
}

async fn serve_async() -> io::Result<thread::JoinHandle<io::Result<()>>> {
    let (input, output) = endpoint()?;
    let (command_tx, command_rx) = exchange::pair(INBOX_CAPACITY, INBOX_CAPACITY);
    let (out_tx, out_rx) = exchange::blocking_pair(OUTBOX_CONTROL_CAPACITY, OUTBOX_DATA_CAPACITY);
    let stop = Arc::new(Stop::new());
    let wake = Arc::new(Notify::new());
    let writer_stop = Arc::clone(&stop);
    let writer_wake = Arc::clone(&wake);
    let writer_commands = command_tx.clone();
    let writer = thread::spawn(move || {
        let result = write_messages(&out_rx, output);
        if result.is_err() {
            writer_stop.request();
            writer_wake.notify_one();
            let shutdown = Frame::Notify(HostNotice::Shutdown);
            let _ = writer_commands.try_send(shutdown);
        }
        result
    });
    let client = Arc::new(BlockingClient::new(out_tx.clone()));
    let _ready = out_tx.try_send(Frame::Notify(RendererNotice::Ready));
    logging::info!(target: "renderer", "ready");
    let reader_client = Arc::clone(&client);
    let reader_stop = Arc::clone(&stop);
    let reader_wake = Arc::clone(&wake);
    thread::spawn(move || {
        read_messages(
            input,
            &command_tx,
            &reader_client,
            &reader_stop,
            &reader_wake,
        );
    });
    session::run(command_rx, &out_tx, client, &stop, Arc::clone(&wake)).await;
    // The reader returns on `Shutdown`, dropping its `ServiceClient` clone, so
    // the writer channel closes and the child can exit.
    drop(out_tx);
    Ok(writer)
}

/// One child endpoint. Unix duplicates the inherited descriptor; other
/// platforms split stdin and stdout.
#[cfg(unix)]
fn endpoint() -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    let descriptor = std::io::stdin().as_fd().try_clone_to_owned()?;
    let stream = UnixStream::from(descriptor);
    let input = stream.try_clone()?;
    Ok((Box::new(input), Box::new(stream)))
}

#[cfg(not(unix))]
fn endpoint() -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    Ok((Box::new(std::io::stdin()), Box::new(std::io::stdout())))
}

fn read_messages(
    mut input: Box<dyn Read + Send>,
    command_tx: &exchange::Sender<ToRenderer>,
    client: &Arc<ServiceClient>,
    stop: &Arc<Stop>,
    wake: &Arc<Notify>,
) {
    let mut buffer = Vec::new();
    let mut greeted = false;
    loop {
        let frame = match read_frame(&mut input, &mut buffer) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                logging::error!(target: "renderer::ipc", "bad host message: {error}");
                break;
            }
        };
        let message = match frame.kind {
            FrameKind::Control => match decode_control::<ToRenderer>(&buffer) {
                Ok(message) => message,
                Err(error) => {
                    logging::error!(target: "renderer::ipc", "bad host message: {error}");
                    break;
                }
            },
            FrameKind::RequestChunk => Frame::RequestChunk {
                id: RequestId::new(frame.request),
                bytes: buffer.clone(),
            },
            FrameKind::ResponseChunk => Frame::ResponseChunk {
                id: RequestId::new(frame.request),
                bytes: buffer.clone(),
            },
        };
        if !greeted {
            match message {
                // The handshake is the first command the session sees, so it
                // holds the identity every later `Assign` builds its engine
                // with. Losing it would leave every engine on the default, so a
                // full lane stops the renderer instead of dropping the frame.
                Frame::Notify(HostNotice::Hello { identity }) => {
                    greeted = true;
                    if command_tx
                        .try_send(Frame::Notify(HostNotice::Hello { identity }))
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                Frame::Reply { id, body } => {
                    client.deliver(id, body);
                    continue;
                }
                _ => {
                    logging::error!(target: "renderer::ipc", "message before handshake");
                    break;
                }
            }
        }
        if !route_message(message, command_tx, client) {
            break;
        }
    }
    // The host is gone (EOF or a broken pipe). A renderer must not outlive its
    // browser: interrupt in-flight work and stop the loop.
    stop.request();
    wake.notify_one();
    client.close();
    let _result = command_tx.try_send(Frame::Notify(HostNotice::Shutdown));
}

/// Routes one post-handshake host message; `false` stops the read loop.
fn route_message(
    message: ToRenderer,
    command_tx: &exchange::Sender<ToRenderer>,
    client: &Arc<ServiceClient>,
) -> bool {
    match message {
        Frame::Notify(HostNotice::Hello { .. }) => {
            logging::error!(target: "renderer::ipc", "duplicate handshake");
            false
        }
        Frame::Notify(HostNotice::Shutdown) => {
            let _result = command_tx.try_send(message);
            false
        }
        Frame::Reply { id, body } => {
            client.deliver(id, body);
            true
        }
        Frame::Notify(HostNotice::StorageEvent { .. } | HostNotice::BroadcastMessage { .. }) => {
            command_tx.try_send_lossy(message).is_ok()
        }
        _ => command_tx.try_send(message).is_ok(),
    }
}

fn write_messages(
    rx: &exchange::BlockingReceiver<FromRenderer>,
    mut output: Box<dyn Write + Send>,
) -> io::Result<()> {
    while let Some(message) = rx.recv() {
        match message {
            Frame::RequestChunk { id, bytes } => {
                write_frame(&mut output, FrameKind::RequestChunk, id.get(), &bytes)?;
            }
            Frame::ResponseChunk { id, bytes } => {
                write_frame(&mut output, FrameKind::ResponseChunk, id.get(), &bytes)?;
            }
            _ => crate::wire::channel::write_control(&mut output, &message)?,
        }
    }
    Ok(())
}

/// The blocking service client one child process shares: the reader thread
/// delivers replies on it, and every assignment's web services bind it with
/// their id.
pub(crate) type ServiceClient =
    BlockingClient<BrowserCall, RendererReply, RendererNotice, ServiceReply>;

/// Web services for one renderer assignment, answered by the browser process
/// over the control channel.
pub(crate) struct HostServices {
    assignment: RendererAssignmentId,
    client: Arc<ServiceClient>,
}

impl HostServices {
    pub(crate) fn new(assignment: RendererAssignmentId, client: Arc<ServiceClient>) -> Self {
        Self { assignment, client }
    }

    fn call(&self, call: ServiceCall) -> Option<ServiceReply> {
        self.client
            .call(BrowserCall {
                assignment: self.assignment,
                call,
            })
            .ok()
    }
}

impl NetworkHost for HostServices {
    fn start_dial(
        &self,
        request: DialRequest,
        completion: DialCompletion,
    ) -> renderer::DialCancellation {
        let assignment = self.assignment;
        let id = self.client.call_with(
            BrowserCall {
                assignment,
                call: ServiceCall::Network(NetworkCall::Dial(Box::new(request))),
            },
            move |reply| match reply {
                Ok(ServiceReply::Dial(outcome)) => completion(*outcome),
                Ok(_) | Err(_) => completion(Err(renderer::DialFailure::Connect)),
            },
        );
        let client = Arc::clone(&self.client);
        Box::new(move || {
            if let Some(id) = id {
                let _call = client.call_with(
                    BrowserCall {
                        assignment,
                        call: ServiceCall::Network(NetworkCall::CancelDial { id }),
                    },
                    |_| {},
                );
            }
        })
    }

    fn dial_read(
        &self,
        stream: renderer::StreamId,
    ) -> Result<Option<Vec<u8>>, renderer::DialFailure> {
        match self.call(ServiceCall::Network(NetworkCall::DialRead { id: stream })) {
            Some(ServiceReply::StreamChunk(result)) => result,
            _ => Err(renderer::DialFailure::Connect),
        }
    }

    fn cancel_stream(&self, stream: renderer::StreamId) {
        let _result = self.call(ServiceCall::Network(NetworkCall::CancelStream {
            id: stream,
        }));
    }

    fn dial_sync(
        &self,
        request: renderer::DialRequest,
    ) -> Result<renderer::DialOutcome, renderer::DialFailure> {
        match self.call(ServiceCall::Network(NetworkCall::Dial(Box::new(request)))) {
            Some(ServiceReply::Dial(outcome)) => *outcome,
            _ => Err(renderer::DialFailure::Connect),
        }
    }

    fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        match self.call(ServiceCall::Network(NetworkCall::CookieGet {
            url: url.to_string(),
            partition_key: partition_key.map(str::to_owned),
        })) {
            Some(ServiceReply::Cookie(value)) => value,
            _ => String::new(),
        }
    }

    fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let _result = self.call(ServiceCall::Network(NetworkCall::CookieSet {
            value: value.to_owned(),
            url: url.to_string(),
            partition_key: partition_key.map(str::to_owned),
        }));
    }
}

/// One storage key as the wire claim, which is the serialized origin.
fn claim(origin: &renderer::StorageKey) -> renderer::SerializedOrigin {
    renderer::SerializedOrigin::from(origin.origin())
}

impl StorageHost for HostServices {
    fn storage_get(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        key: &str,
    ) -> Option<String> {
        match self.call(ServiceCall::Storage(StorageCall::Get {
            kind,
            origin: claim(origin),
            key: key.to_owned(),
        })) {
            Some(ServiceReply::StorageValue(value)) => value,
            _ => None,
        }
    }

    fn storage_keys(&self, kind: StorageKind, origin: &renderer::StorageKey) -> Vec<String> {
        match self.call(ServiceCall::Storage(StorageCall::Keys {
            kind,
            origin: claim(origin),
        })) {
            Some(ServiceReply::StorageKeys(keys)) => keys,
            _ => Vec::new(),
        }
    }

    fn storage_set(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        url: &str,
        key: &str,
        value: &str,
        source: FrameId,
    ) -> Result<Option<StorageChange>, StorageError> {
        match self.call(ServiceCall::Storage(StorageCall::Set {
            kind,
            origin: claim(origin),
            url: url.to_owned(),
            key: key.to_owned(),
            value: value.to_owned(),
            source,
        })) {
            Some(ServiceReply::StorageChanged(change)) => change,
            _ => Ok(None),
        }
    }

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        url: &str,
        key: &str,
        source: FrameId,
    ) -> Option<StorageChange> {
        match self.call(ServiceCall::Storage(StorageCall::Remove {
            kind,
            origin: claim(origin),
            url: url.to_owned(),
            key: key.to_owned(),
            source,
        })) {
            Some(ServiceReply::StorageChanged(Ok(change))) => change,
            _ => None,
        }
    }

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        url: &str,
        source: FrameId,
    ) -> Option<StorageChange> {
        match self.call(ServiceCall::Storage(StorageCall::Clear {
            kind,
            origin: claim(origin),
            url: url.to_owned(),
            source,
        })) {
            Some(ServiceReply::StorageChanged(Ok(change))) => change,
            _ => None,
        }
    }
}

impl BrowsingContextHost for HostServices {
    fn window_open(
        &self,
        url: Option<&Url>,
        target: &renderer::TargetName,
        opener: OpenerPolicy,
        source: &renderer::NavigationSource,
    ) -> Option<WindowId> {
        match self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::WindowOpen {
                url: url.cloned(),
                target: target.clone(),
                opener,
                source: source.clone(),
            },
        )) {
            Some(ServiceReply::Window(tab)) => tab,
            _ => None,
        }
    }

    fn navigate_named(
        &self,
        name: &renderer::ContextName,
        url: Option<&Url>,
        opener: OpenerPolicy,
        source: &renderer::NavigationSource,
    ) -> Option<WindowId> {
        match self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::NavigateNamed {
                name: name.clone(),
                url: url.cloned(),
                opener,
                source: source.clone(),
            },
        )) {
            Some(ServiceReply::Window(tab)) => tab,
            _ => None,
        }
    }

    fn frame_created(
        &self,
        frame: FrameId,
        parent: FrameId,
        kind: renderer::FrameKind,
        sandbox: renderer::SandboxFlags,
    ) {
        let _result = self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::FrameCreated {
                frame,
                parent,
                kind,
                sandbox,
            },
        ));
    }

    fn frame_destroyed(&self, frame: FrameId) {
        let _result = self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::FrameDestroyed { frame },
        ));
    }

    fn set_window_name(&self, change: renderer::NameChange) {
        let _result = self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::WindowName { change },
        ));
    }

    fn window_close(&self, window: WindowId) {
        let _result = self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::WindowClose { window },
        ));
    }

    fn window_opener(&self) -> Option<WindowId> {
        match self.call(ServiceCall::BrowsingContext(BrowsingContextCall::Opener)) {
            Some(ServiceReply::Window(tab)) => tab,
            _ => None,
        }
    }

    fn window_post_message(&self, window: WindowId, message: renderer::PostedMessage) {
        let _result = self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::WindowMessage { window, message },
        ));
    }

    fn remote_session_get(
        &self,
        window: WindowId,
        origin: &renderer::StorageKey,
        key: &str,
    ) -> Option<String> {
        match self.call(ServiceCall::BrowsingContext(
            BrowsingContextCall::RemoteSessionGet {
                window,
                origin: claim(origin),
                key: key.to_owned(),
            },
        )) {
            Some(ServiceReply::StorageValue(value)) => value,
            _ => None,
        }
    }
}

impl MessagingHost for HostServices {
    fn broadcast_post(
        &self,
        origin: &renderer::StorageKey,
        name: &str,
        payload: &str,
        channel: u64,
    ) {
        let _result = self.call(ServiceCall::Messaging(MessagingCall::BroadcastPost {
            origin: claim(origin),
            name: name.to_owned(),
            payload: payload.to_owned(),
            channel,
        }));
    }
}
