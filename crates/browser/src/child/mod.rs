mod sandbox;
mod session;

const INBOX_CAPACITY: usize = 256;

const OUTBOX_CONTROL_CAPACITY: usize = 256;

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

pub fn serve() -> io::Result<()> {
    sandbox::engage()?;
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
    drop(out_tx);
    Ok(writer)
}

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
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches fallible unix endpoint signature"
)]
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
    stop.request();
    wake.notify_one();
    client.close();
    let _result = command_tx.try_send(Frame::Notify(HostNotice::Shutdown));
}

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

pub(crate) type ServiceClient =
    BlockingClient<BrowserCall, RendererReply, RendererNotice, ServiceReply>;

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
}

fn claim(origin: &renderer::StorageKey) -> renderer::SerializedOrigin {
    renderer::SerializedOrigin::from(origin.origin())
}

impl StorageHost for HostServices {
    fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        match self.call(ServiceCall::Storage(StorageCall::CookieGet {
            url: url.to_string(),
            partition_key: partition_key.map(str::to_owned),
        })) {
            Some(ServiceReply::Cookie(value)) => value,
            _ => String::new(),
        }
    }

    fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let _result = self.call(ServiceCall::Storage(StorageCall::CookieSet {
            value: value.to_owned(),
            url: url.to_string(),
            partition_key: partition_key.map(str::to_owned),
        }));
    }

    fn cookie_store_records(
        &self,
        url: &Url,
        partition_key: Option<&str>,
    ) -> Vec<renderer::ScriptCookie> {
        match self.call(ServiceCall::Storage(StorageCall::CookieStoreGet {
            url: url.to_string(),
            partition_key: partition_key.map(str::to_owned),
        })) {
            Some(ServiceReply::CookieStore(records)) => records,
            _ => Vec::new(),
        }
    }

    fn cookie_store_store(
        &self,
        write: &net::ScriptCookieWrite,
        url: &Url,
        partition_key: Option<&str>,
    ) {
        let _reply = self.call(ServiceCall::Storage(StorageCall::CookieStoreSet {
            write: write.clone(),
            url: url.to_string(),
            partition_key: partition_key.map(str::to_owned),
        }));
    }

    fn remote_session_get(
        &self,
        window: WindowId,
        origin: &renderer::StorageKey,
        key: &str,
    ) -> Option<String> {
        match self.call(ServiceCall::Storage(StorageCall::RemoteSessionGet {
            window,
            origin: claim(origin),
            key: key.to_owned(),
        })) {
            Some(ServiceReply::StorageValue(value)) => value,
            _ => None,
        }
    }
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
