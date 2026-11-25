use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RequestId(u64);

impl RequestId {
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Frame<C, R, N> {
    Call {
        id: RequestId,
        body: C,
    },
    Reply {
        id: RequestId,
        body: R,
    },
    Notify(N),
    RequestChunk {
        id: RequestId,
        #[serde(skip, default)]
        bytes: Vec<u8>,
    },
    RequestEnd {
        id: RequestId,
        error: Option<String>,
    },
    ResponseChunk {
        id: RequestId,
        #[serde(skip, default)]
        bytes: Vec<u8>,
    },
    Cancel {
        id: RequestId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Closed,
    Protocol,
    SubscriberLimit,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("exchange closed"),
            Self::Protocol => formatter.write_str("exchange protocol violation"),
            Self::SubscriberLimit => formatter.write_str("exchange subscriber limit reached"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy)]
pub(crate) enum Queue {
    Control,
    Data,
}

pub(crate) trait Lane {
    fn queue(&self) -> Queue;
}

impl<C, R, N> Lane for Frame<C, R, N> {
    fn queue(&self) -> Queue {
        match self {
            Self::Cancel { .. } => Queue::Control,
            Self::Call { .. }
            | Self::Reply { .. }
            | Self::Notify(_)
            | Self::RequestChunk { .. }
            | Self::RequestEnd { .. }
            | Self::ResponseChunk { .. } => Queue::Data,
        }
    }
}

pub(crate) struct Sender<T> {
    control: mpsc::Sender<T>,
    data: mpsc::Sender<T>,
    closed: watch::Sender<bool>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            control: self.control.clone(),
            data: self.data.clone(),
            closed: self.closed.clone(),
        }
    }
}

impl<T> Sender<T> {
    fn channel(&self, queue: Queue) -> &mpsc::Sender<T> {
        match queue {
            Queue::Control => &self.control,
            Queue::Data => &self.data,
        }
    }

    fn disconnect(&self) {
        let _result = self.closed.send(true);
    }

    fn is_closed(&self) -> bool {
        *self.closed.borrow() || self.control.is_closed()
    }

    async fn wait_closed(&self) {
        if self.is_closed() {
            return;
        }
        let mut closed = self.closed.subscribe();
        tokio::select! {
            () = self.control.closed() => {}
            () = wait_closed(&mut closed) => {}
        }
    }

    async fn send(&self, value: T) -> Result<(), Error>
    where
        T: Lane,
    {
        let queue = value.queue();
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err(Error::Closed);
        }
        tokio::select! {
            biased;
            result = self.channel(queue).send(value) => result.map_err(|_| Error::Closed),
            () = wait_closed(&mut closed) => Err(Error::Closed),
        }
    }

    pub(crate) fn try_send(&self, value: T) -> Result<(), Error>
    where
        T: Lane,
    {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        match self.channel(value.queue()).try_send(value) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(Error::Closed),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.disconnect();
                Err(Error::Closed)
            }
        }
    }

    pub(crate) fn try_send_lossy(&self, value: T) -> Result<(), Error>
    where
        T: Lane,
    {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        match self.channel(value.queue()).try_send(value) {
            Err(mpsc::error::TrySendError::Closed(_)) => Err(Error::Closed),
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
        }
    }

    async fn reserve(&self, queue: Queue) -> Result<mpsc::OwnedPermit<T>, Error> {
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err(Error::Closed);
        }
        tokio::select! {
            biased;
            permit = self.channel(queue).clone().reserve_owned() => {
                permit.map_err(|_| Error::Closed)
            }
            () = wait_closed(&mut closed) => Err(Error::Closed),
        }
    }
}

pub(crate) struct Receiver<T> {
    control: mpsc::Receiver<T>,
    data: mpsc::Receiver<T>,
    closed: watch::Receiver<bool>,
}

impl<T> Receiver<T> {
    pub(crate) async fn recv(&mut self) -> Option<T> {
        if *self.closed.borrow() {
            self.control.close();
            self.data.close();
            return None;
        }
        tokio::select! {
            biased;
            frame = self.control.recv() => match frame {
                Some(frame) => Some(frame),
                None => self.data.recv().await,
            },
            frame = self.data.recv() => match frame {
                Some(frame) => Some(frame),
                None => self.control.recv().await,
            },
            () = wait_closed(&mut self.closed) => {
                self.control.close();
                self.data.close();
                None
            },
        }
    }
}

async fn wait_closed(closed: &mut watch::Receiver<bool>) {
    loop {
        if *closed.borrow_and_update() {
            return;
        }
        if closed.changed().await.is_err() {
            return;
        }
    }
}

pub(crate) fn pair<T>(control: usize, data: usize) -> (Sender<T>, Receiver<T>) {
    let (control_tx, control_rx) = mpsc::channel(control);
    let (data_tx, data_rx) = mpsc::channel(data);
    let (closed, closed_rx) = watch::channel(false);
    (
        Sender {
            control: control_tx,
            data: data_tx,
            closed,
        },
        Receiver {
            control: control_rx,
            data: data_rx,
            closed: closed_rx,
        },
    )
}

pub(crate) struct BlockingSender<T> {
    control: std_mpsc::SyncSender<T>,
    data: std_mpsc::SyncSender<T>,
    wake: std_mpsc::Sender<()>,
    closed: Arc<AtomicBool>,
}

impl<T> Clone for BlockingSender<T> {
    fn clone(&self) -> Self {
        Self {
            control: self.control.clone(),
            data: self.data.clone(),
            wake: self.wake.clone(),
            closed: Arc::clone(&self.closed),
        }
    }
}

impl<T> BlockingSender<T> {
    fn channel(&self, queue: Queue) -> &std_mpsc::SyncSender<T> {
        match queue {
            Queue::Control => &self.control,
            Queue::Data => &self.data,
        }
    }

    fn disconnect(&self) {
        self.closed.store(true, Ordering::Release);
        let _result = self.wake.send(());
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn send(&self, value: T) -> Result<(), Error>
    where
        T: Lane,
    {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        self.channel(value.queue())
            .send(value)
            .map_err(|_| Error::Closed)?;
        let _result = self.wake.send(());
        Ok(())
    }

    pub(crate) fn try_send(&self, value: T) -> Result<(), Error>
    where
        T: Lane,
    {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        match self.channel(value.queue()).try_send(value) {
            Ok(()) => {
                let _result = self.wake.send(());
                Ok(())
            }
            Err(std_mpsc::TrySendError::Disconnected(_)) => Err(Error::Closed),
            Err(std_mpsc::TrySendError::Full(_)) => {
                self.disconnect();
                Err(Error::Closed)
            }
        }
    }
}

pub(crate) struct BlockingReceiver<T> {
    control: std_mpsc::Receiver<T>,
    data: std_mpsc::Receiver<T>,
    wake: std_mpsc::Receiver<()>,
    closed: Arc<AtomicBool>,
}

impl<T> BlockingReceiver<T> {
    pub(crate) fn recv(&self) -> Option<T> {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            if let Ok(value) = self.control.try_recv() {
                return Some(value);
            }
            if let Ok(value) = self.data.try_recv() {
                return Some(value);
            }
            if self.wake.recv().is_err() {
                return self
                    .control
                    .try_recv()
                    .ok()
                    .or_else(|| self.data.try_recv().ok());
            }
        }
    }
}

pub(crate) fn blocking_pair<T>(
    control: usize,
    data: usize,
) -> (BlockingSender<T>, BlockingReceiver<T>) {
    let (control_tx, control_rx) = std_mpsc::sync_channel(control);
    let (data_tx, data_rx) = std_mpsc::sync_channel(data);
    let (wake_tx, wake_rx) = std_mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    (
        BlockingSender {
            control: control_tx,
            data: data_tx,
            wake: wake_tx,
            closed: Arc::clone(&closed),
        },
        BlockingReceiver {
            control: control_rx,
            data: data_rx,
            wake: wake_rx,
            closed,
        },
    )
}

enum Pending<R> {
    Unary(oneshot::Sender<Result<R, Error>>),
    Download {
        bytes: Vec<u8>,
        limit: usize,
        reply: oneshot::Sender<Result<(R, Vec<u8>), Error>>,
    },
}

type PendingMap<R> = Arc<Mutex<HashMap<RequestId, Pending<R>>>>;
type Subscribers<N> = Arc<Mutex<Vec<mpsc::Sender<N>>>>;

pub(crate) struct Client<OC, OR, ON, IR, IN> {
    tx: Sender<Frame<OC, OR, ON>>,
    pending: PendingMap<IR>,
    subscribers: Subscribers<IN>,
    next: Arc<AtomicU64>,
}

impl<OC, OR, ON, IR, IN> Clone for Client<OC, OR, ON, IR, IN> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            pending: Arc::clone(&self.pending),
            subscribers: Arc::clone(&self.subscribers),
            next: Arc::clone(&self.next),
        }
    }
}

impl<OC, OR, ON, IR, IN> Client<OC, OR, ON, IR, IN>
where
    OC: Send + 'static,
    OR: Send + 'static,
    ON: Send + 'static,
    IR: Send + 'static,
    IN: Clone + Send + 'static,
{
    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    pub(crate) async fn wait_closed(&self) {
        self.tx.wait_closed().await;
    }

    pub(crate) async fn call(&self, body: OC) -> Result<IR, Error> {
        let (id, receiver) = self.start_call(body, Pending::Unary).await?;
        let mut cancel = CancelOnDrop::new(id, self.tx.clone(), Arc::clone(&self.pending));
        let result = receiver.await.unwrap_or(Err(Error::Closed));
        cancel.disarm();
        result
    }

    pub(crate) async fn call_download(
        &self,
        body: OC,
        limit: usize,
    ) -> Result<(IR, Vec<u8>), Error> {
        let (id, receiver) = self
            .start_call(body, |reply| Pending::Download {
                bytes: Vec::new(),
                limit,
                reply,
            })
            .await?;
        let mut cancel = CancelOnDrop::new(id, self.tx.clone(), Arc::clone(&self.pending));
        let result = receiver.await.unwrap_or(Err(Error::Closed));
        cancel.disarm();
        result
    }

    pub(crate) async fn begin_upload(&self, body: OC) -> Result<Upload<OC, OR, ON, IR>, Error> {
        let (id, reply) = self.start_call(body, Pending::Unary).await?;
        Ok(Upload {
            id,
            tx: self.tx.clone(),
            pending: Arc::clone(&self.pending),
            reply: Some(reply),
        })
    }

    pub(crate) fn try_notify(&self, notice: ON) -> Result<(), Error> {
        self.tx.try_send(Frame::Notify(notice))
    }

    pub(crate) fn try_notify_lossy(&self, notice: ON) -> Result<(), Error> {
        self.tx.try_send_lossy(Frame::Notify(notice))
    }

    pub(crate) async fn notify(&self, notice: ON) -> Result<(), Error> {
        self.tx.send(Frame::Notify(notice)).await
    }

    pub(crate) fn subscribe(
        &self,
        capacity: usize,
        limit: usize,
    ) -> Result<mpsc::Receiver<IN>, Error> {
        if self.tx.is_closed() {
            return Err(Error::Closed);
        }
        let (tx, rx) = mpsc::channel(capacity);
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|subscriber| !subscriber.is_closed());
        if subscribers.len() == limit {
            return Err(Error::SubscriberLimit);
        }
        subscribers.push(tx);
        Ok(rx)
    }

    #[must_use]
    pub(crate) fn subscriber_probe(&self) -> SubscriberProbe<IN> {
        SubscriberProbe {
            subscribers: Arc::clone(&self.subscribers),
        }
    }

    async fn start_call<T>(
        &self,
        body: OC,
        pending: impl FnOnce(oneshot::Sender<Result<T, Error>>) -> Pending<IR>,
    ) -> Result<(RequestId, oneshot::Receiver<Result<T, Error>>), Error> {
        let permit = self.tx.reserve(Queue::Data).await?;
        let id = self.next_id();
        let (reply, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, pending(reply));
        permit.send(Frame::Call { id, body });
        Ok((id, receiver))
    }

    fn next_id(&self) -> RequestId {
        RequestId::new(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

struct CancelOnDrop<C, R, N, IR> {
    id: RequestId,
    tx: Sender<Frame<C, R, N>>,
    pending: PendingMap<IR>,
    armed: bool,
}

impl<C, R, N, IR> CancelOnDrop<C, R, N, IR> {
    fn new(id: RequestId, tx: Sender<Frame<C, R, N>>, pending: PendingMap<IR>) -> Self {
        Self {
            id,
            tx,
            pending,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl<C, R, N, IR> Drop for CancelOnDrop<C, R, N, IR> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        let _result = self.tx.try_send(Frame::Cancel { id: self.id });
    }
}

pub(crate) struct Upload<C, R, N, IR> {
    id: RequestId,
    tx: Sender<Frame<C, R, N>>,
    pending: PendingMap<IR>,
    reply: Option<oneshot::Receiver<Result<IR, Error>>>,
}

impl<C, R, N, IR> Upload<C, R, N, IR> {
    pub(crate) async fn write(&self, bytes: Vec<u8>) -> Result<(), Error> {
        self.tx
            .send(Frame::RequestChunk { id: self.id, bytes })
            .await
    }

    pub(crate) async fn finish(self) -> Result<IR, Error> {
        self.end(None).await
    }

    pub(crate) async fn abort(self, message: String) -> Result<IR, Error> {
        self.end(Some(message)).await
    }

    async fn end(mut self, error: Option<String>) -> Result<IR, Error> {
        if self
            .tx
            .send(Frame::RequestEnd { id: self.id, error })
            .await
            .is_err()
        {
            self.remove_pending();
            return Err(Error::Closed);
        }
        let Some(receiver) = self.reply.take() else {
            return Err(Error::Protocol);
        };
        let mut cancel = CancelOnDrop::new(self.id, self.tx.clone(), Arc::clone(&self.pending));
        let result = receiver.await.unwrap_or(Err(Error::Closed));
        cancel.disarm();
        result
    }

    fn remove_pending(&self) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

impl<C, R, N, IR> Drop for Upload<C, R, N, IR> {
    fn drop(&mut self) {
        if self.reply.is_none() {
            return;
        }
        self.remove_pending();
        let _result = self.tx.try_send(Frame::Cancel { id: self.id });
    }
}

#[derive(Debug)]
pub(crate) enum ServerInput<C, N> {
    Call { id: RequestId, body: C },
    Notify(N),
    RequestChunk,
    RequestEnd,
    Cancel { id: RequestId },
}

impl<C, N> Lane for ServerInput<C, N> {
    fn queue(&self) -> Queue {
        match self {
            Self::Cancel { .. } => Queue::Control,
            Self::Call { .. } | Self::Notify(_) | Self::RequestChunk | Self::RequestEnd => {
                Queue::Data
            }
        }
    }
}

pub(crate) struct Server<OC, OR, ON, IC, IN> {
    tx: Sender<Frame<OC, OR, ON>>,
    rx: Receiver<ServerInput<IC, IN>>,
}

pub(crate) struct Notifier<OC, OR, ON> {
    tx: Sender<Frame<OC, OR, ON>>,
}

pub(crate) struct Responder<OC, OR, ON> {
    tx: Sender<Frame<OC, OR, ON>>,
}

impl<OC, OR, ON> Clone for Responder<OC, OR, ON> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<OC, OR, ON> Responder<OC, OR, ON> {
    pub(crate) async fn reply(&self, id: RequestId, body: OR) -> Result<(), Error> {
        self.tx.send(Frame::Reply { id, body }).await
    }
}

impl<OC, OR, ON> Clone for Notifier<OC, OR, ON> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<OC, OR, ON> Notifier<OC, OR, ON> {
    pub(crate) async fn notify(&self, notice: ON) -> Result<(), Error> {
        self.tx.send(Frame::Notify(notice)).await
    }
}

impl<OC, OR, ON, IC, IN> Server<OC, OR, ON, IC, IN> {
    pub(crate) async fn recv(&mut self) -> Option<ServerInput<IC, IN>> {
        self.rx.recv().await
    }

    pub(crate) async fn reply(&self, id: RequestId, body: OR) -> Result<(), Error> {
        self.tx.send(Frame::Reply { id, body }).await
    }

    pub(crate) fn notifier(&self) -> Notifier<OC, OR, ON> {
        Notifier {
            tx: self.tx.clone(),
        }
    }

    pub(crate) fn responder(&self) -> Responder<OC, OR, ON> {
        Responder {
            tx: self.tx.clone(),
        }
    }
}

pub(crate) struct SubscriberProbe<IN> {
    subscribers: Subscribers<IN>,
}

impl<IN> SubscriberProbe<IN> {
    #[must_use]
    pub(crate) fn has_subscribers(&self) -> bool {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|subscriber| !subscriber.is_closed())
    }
}

pub(crate) struct Router<IC, IR, IN> {
    pending: PendingMap<IR>,
    subscribers: Subscribers<IN>,
    server: Sender<ServerInput<IC, IN>>,
}

impl<IC, IR, IN> Clone for Router<IC, IR, IN> {
    fn clone(&self) -> Self {
        Self {
            pending: Arc::clone(&self.pending),
            subscribers: Arc::clone(&self.subscribers),
            server: self.server.clone(),
        }
    }
}

impl<IC, IR, IN> Router<IC, IR, IN>
where
    IN: Clone,
{
    pub(crate) fn route(&self, frame: Frame<IC, IR, IN>) -> Result<(), Error> {
        match frame {
            Frame::Call { id, body } => self.send_server(ServerInput::Call { id, body }),
            Frame::Reply { id, body } => {
                route_reply(&self.pending, id, body);
                Ok(())
            }
            Frame::Notify(notice) => {
                route_notice(&self.subscribers, notice.clone());
                let closed_before = self.server.is_closed();
                match self.server.try_send(ServerInput::Notify(notice)) {
                    Ok(()) => Ok(()),
                    Err(_) if closed_before => Ok(()),
                    Err(error) => Err(error),
                }
            }
            Frame::RequestChunk { .. } => self.send_server(ServerInput::RequestChunk),
            Frame::RequestEnd { .. } => self.send_server(ServerInput::RequestEnd),
            Frame::ResponseChunk { id, bytes } => route_chunk(&self.pending, id, &bytes),
            Frame::Cancel { id } => self.send_server(ServerInput::Cancel { id }),
        }
    }

    pub(crate) fn close(&self) {
        fail_pending(&self.pending);
        self.server.disconnect();
    }

    fn send_server(&self, input: ServerInput<IC, IN>) -> Result<(), Error> {
        self.server.try_send(input)
    }
}

type Endpoint<OC, OR, ON, IC, IR, IN> = (
    Client<OC, OR, ON, IR, IN>,
    Server<OC, OR, ON, IC, IN>,
    Router<IC, IR, IN>,
);
type Bound<OC, OR, ON, IC, IR, IN> = (Client<OC, OR, ON, IR, IN>, Server<OC, OR, ON, IC, IN>);
type Local<C, R, CN, SN> = (
    Client<C, Infallible, CN, R, SN>,
    Server<Infallible, R, SN, C, CN>,
);

pub(crate) fn endpoint<OC, OR, ON, IC, IR, IN>(
    tx: Sender<Frame<OC, OR, ON>>,
    capacity: usize,
) -> Endpoint<OC, OR, ON, IC, IR, IN>
where
    IN: Clone,
{
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let subscribers = Arc::new(Mutex::new(Vec::new()));
    let (server_tx, server_rx) = pair(capacity, capacity);
    let client = Client {
        tx: tx.clone(),
        pending: Arc::clone(&pending),
        subscribers: Arc::clone(&subscribers),
        next: Arc::new(AtomicU64::new(1)),
    };
    let server = Server { tx, rx: server_rx };
    let router = Router {
        pending,
        subscribers,
        server: server_tx,
    };
    (client, server, router)
}

pub(crate) fn bind<OC, OR, ON, IC, IR, IN>(
    tx: Sender<Frame<OC, OR, ON>>,
    mut rx: Receiver<Frame<IC, IR, IN>>,
    capacity: usize,
) -> Bound<OC, OR, ON, IC, IR, IN>
where
    OC: Send + 'static,
    OR: Send + 'static,
    ON: Send + 'static,
    IC: Send + 'static,
    IR: Send + 'static,
    IN: Clone + Send + 'static,
{
    let closed = tx.closed.clone();
    let (client, server, router) = endpoint(tx, capacity);
    tokio::spawn(async move {
        loop {
            let Some(frame) = rx.recv().await else {
                break;
            };
            if router.route(frame).is_err() {
                break;
            }
        }
        router.close();
        let _result = closed.send(true);
    });
    (client, server)
}

pub(crate) fn local<C, R, CN, SN>(capacity: usize) -> Local<C, R, CN, SN>
where
    C: Send + 'static,
    R: Send + 'static,
    CN: Clone + Send + 'static,
    SN: Clone + Send + 'static,
{
    local_lanes(capacity, capacity)
}

fn local_lanes<C, R, CN, SN>(control: usize, data: usize) -> Local<C, R, CN, SN>
where
    C: Send + 'static,
    R: Send + 'static,
    CN: Clone + Send + 'static,
    SN: Clone + Send + 'static,
{
    let (to_server, from_client) = pair(control, data);
    let (to_client, from_server) = pair(control, data);
    let (client, _client_server) = bind(to_server, from_server, control.max(data));
    let (_server_client, server) = bind(to_client, from_client, control.max(data));
    (client, server)
}

enum BlockingPending<R> {
    Wait(std_mpsc::SyncSender<Result<R, Error>>),
    Callback(Box<dyn FnOnce(Result<R, Error>) + Send>),
}

pub(crate) struct BlockingClient<C, OR, N, IR> {
    tx: BlockingSender<Frame<C, OR, N>>,
    pending: Mutex<HashMap<RequestId, BlockingPending<IR>>>,
    next: AtomicU64,
}

impl<C, OR, N, IR> BlockingClient<C, OR, N, IR> {
    pub(crate) fn new(tx: BlockingSender<Frame<C, OR, N>>) -> Self {
        Self {
            tx,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    pub(crate) fn call(&self, body: C) -> Result<IR, Error> {
        let (reply_tx, reply_rx) = std_mpsc::sync_channel(1);
        self.start(body, BlockingPending::Wait(reply_tx))?;
        reply_rx.recv().unwrap_or(Err(Error::Closed))
    }

    pub(crate) fn call_with(
        &self,
        body: C,
        callback: impl FnOnce(Result<IR, Error>) + Send + 'static,
    ) -> Option<RequestId> {
        self.start(body, BlockingPending::Callback(Box::new(callback)))
            .ok()
    }

    pub(crate) fn deliver(&self, id: RequestId, body: IR) {
        let waiting = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        match waiting {
            Some(BlockingPending::Wait(reply)) => {
                let _result = reply.send(Ok(body));
            }
            Some(BlockingPending::Callback(callback)) => callback(Ok(body)),
            None => {}
        }
    }

    pub(crate) fn close(&self) {
        self.tx.disconnect();
        for waiting in self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, waiting)| waiting)
        {
            match waiting {
                BlockingPending::Wait(reply) => {
                    let _result = reply.send(Err(Error::Closed));
                }
                BlockingPending::Callback(callback) => callback(Err(Error::Closed)),
            }
        }
    }

    fn start(&self, body: C, pending: BlockingPending<IR>) -> Result<RequestId, Error> {
        let id = RequestId::new(self.next.fetch_add(1, Ordering::Relaxed));
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, pending);
        if self.tx.send(Frame::Call { id, body }).is_err() {
            let waiting = self
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            if let Some(BlockingPending::Callback(callback)) = waiting {
                callback(Err(Error::Closed));
            }
            return Err(Error::Closed);
        }
        Ok(id)
    }
}

fn route_reply<R>(pending: &Mutex<HashMap<RequestId, Pending<R>>>, id: RequestId, body: R) {
    let waiting = pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&id);
    match waiting {
        Some(Pending::Unary(reply)) => {
            let _result = reply.send(Ok(body));
        }
        Some(Pending::Download { bytes, reply, .. }) => {
            let _result = reply.send(Ok((body, bytes)));
        }
        None => {}
    }
}

fn route_chunk<R>(
    pending: &Mutex<HashMap<RequestId, Pending<R>>>,
    id: RequestId,
    bytes: &[u8],
) -> Result<(), Error> {
    let mut pending = pending.lock().unwrap_or_else(PoisonError::into_inner);
    match pending.get_mut(&id) {
        Some(Pending::Download {
            bytes: buffer,
            limit,
            ..
        }) => {
            if buffer.len().saturating_add(bytes.len()) > *limit {
                return Err(Error::Protocol);
            }
            buffer.extend_from_slice(bytes);
            Ok(())
        }
        Some(Pending::Unary(_)) => Err(Error::Protocol),
        None => Ok(()),
    }
}

fn route_notice<N: Clone>(subscribers: &Mutex<Vec<mpsc::Sender<N>>>, notice: N) {
    subscribers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|subscriber| subscriber.try_send(notice.clone()).is_ok());
}

fn fail_pending<R>(pending: &Mutex<HashMap<RequestId, Pending<R>>>) {
    for waiting in pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain()
        .map(|(_, waiting)| waiting)
    {
        match waiting {
            Pending::Unary(reply) => {
                let _result = reply.send(Err(Error::Closed));
            }
            Pending::Download { reply, .. } => {
                let _result = reply.send(Err(Error::Closed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recv_waits_when_both_lanes_are_empty() {
        let (_tx, mut rx) = pair::<Frame<u8, u8, u8>>(1, 1);
        let result = tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv()).await;
        assert!(result.is_err(), "empty recv should wait, got {result:?}");
    }

    #[tokio::test]
    async fn wait_closed_on_subscribe_does_not_fire_on_false() {
        let (tx, _orig) = watch::channel(false);
        let mut rx = tx.subscribe();
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(20), wait_closed(&mut rx)).await;
        assert!(result.is_err(), "subscribe wait_closed should time out");
        drop(tx);
    }

    #[tokio::test]
    async fn wait_closed_does_not_fire_on_false() {
        let (tx, mut rx) = watch::channel(false);
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(20), wait_closed(&mut rx)).await;
        assert!(result.is_err(), "wait_closed should time out");
        drop(tx);
    }

    #[tokio::test]
    async fn local_call_routes_one_typed_reply() {
        let (client, mut server) = local::<u8, u16, Infallible, Infallible>(2);
        let pending = tokio::spawn(async move { client.call(4).await });
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), server.recv())
            .await
            .expect("server recv timed out");
        let Some(ServerInput::Call { id, body }) = received else {
            panic!("expected call, got {received:?}");
        };
        server.reply(id, u16::from(body) + 1).await.expect("reply");
        assert_eq!(pending.await.expect("join").expect("call"), 5);
    }

    #[tokio::test]
    async fn response_chunks_are_correlated_with_the_call() {
        let (to_peer, mut peer_rx) = pair(2, 2);
        let (from_peer, from_peer_rx) = pair(2, 2);
        let (client, _server) = bind::<u8, Infallible, Infallible, Infallible, u16, Infallible>(
            to_peer,
            from_peer_rx,
            2,
        );
        let pending = tokio::spawn(async move { client.call_download(4, 3).await });
        let Some(Frame::Call { id, body }) = peer_rx.recv().await else {
            panic!("peer expected call");
        };
        assert_eq!(body, 4);
        from_peer
            .send(Frame::ResponseChunk {
                id,
                bytes: vec![1, 2, 3],
            })
            .await
            .expect("chunk");
        from_peer
            .send(Frame::Reply { id, body: 5 })
            .await
            .expect("reply");
        let (reply, bytes) = pending.await.expect("join").expect("download");
        assert_eq!(reply, 5);
        assert_eq!(bytes, [1, 2, 3]);
    }

    #[tokio::test]
    async fn leftover_response_chunks_after_cancel_are_ignored() {
        let (to_peer, mut peer_rx) = pair(2, 2);
        let (from_peer, from_peer_rx) = pair(2, 2);
        let (client, _server) = bind::<u8, Infallible, Infallible, Infallible, u16, Infallible>(
            to_peer,
            from_peer_rx,
            2,
        );
        let client = client.clone();
        let pending = tokio::spawn({
            let client = client.clone();
            async move { client.call_download(4, 8).await }
        });
        let Some(Frame::Call { id, .. }) = peer_rx.recv().await else {
            panic!("peer expected call");
        };
        pending.abort();
        let _result = pending.await;
        let Some(Frame::Cancel { id: cancelled }) = peer_rx.recv().await else {
            panic!("expected cancel");
        };
        assert_eq!(cancelled, id);
        from_peer
            .send(Frame::ResponseChunk { id, bytes: vec![1] })
            .await
            .expect("leftover chunk");
        let pending = tokio::spawn(async move { client.call(9).await });
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), peer_rx.recv())
            .await
            .expect("second call timed out");
        let Some(Frame::Call { body, .. }) = received else {
            panic!("peer expected second call, got {received:?}");
        };
        assert_eq!(body, 9);
        pending.abort();
    }

    #[tokio::test]
    async fn dropping_an_upload_sends_cancellation() {
        let (client, mut server) = local::<u8, u16, Infallible, Infallible>(2);
        let upload = client.begin_upload(4).await.expect("upload");
        let Some(ServerInput::Call { id, body }) = server.recv().await else {
            unreachable!("call routed");
        };
        assert_eq!(body, 4);
        drop(upload);
        let Some(ServerInput::Cancel { id: cancelled }) = server.recv().await else {
            unreachable!("cancel routed");
        };
        assert_eq!(cancelled, id);
    }

    #[tokio::test]
    async fn recv_prefers_control_over_a_full_data_lane() {
        let (tx, mut rx) = pair::<Frame<u8, u8, u8>>(1, 2);
        tx.send(Frame::Notify(1)).await.expect("notice one");
        tx.send(Frame::Notify(2)).await.expect("notice two");
        tx.try_send(Frame::Cancel {
            id: RequestId::new(9),
        })
        .expect("cancel on control");
        let Some(Frame::Cancel { id }) = rx.recv().await else {
            unreachable!("control first");
        };
        assert_eq!(id.get(), 9);
    }

    #[tokio::test]
    async fn try_send_on_a_full_control_lane_disconnects() {
        let (tx, mut rx) = pair::<Frame<u8, u8, u8>>(1, 8);
        tx.try_send(Frame::Cancel {
            id: RequestId::new(1),
        })
        .expect("fill control");
        assert_eq!(
            tx.try_send(Frame::Cancel {
                id: RequestId::new(2),
            }),
            Err(Error::Closed)
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn try_send_on_a_full_data_lane_disconnects() {
        let (tx, mut rx) = pair::<Frame<u8, u8, u8>>(8, 1);
        tx.try_send(Frame::Notify(1)).expect("fill data");
        assert_eq!(tx.try_send(Frame::Notify(2)), Err(Error::Closed));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn try_send_lossy_on_a_full_data_lane_keeps_the_peer() {
        let (tx, mut rx) = pair::<Frame<u8, u8, u8>>(8, 1);
        tx.try_send(Frame::Notify(1)).expect("fill data");
        tx.try_send_lossy(Frame::Notify(2))
            .expect("drop instead of disconnect");
        let Some(Frame::Notify(1)) = rx.recv().await else {
            unreachable!("queued notice stays");
        };
        tx.try_send(Frame::Notify(3)).expect("lane drained");
        let Some(Frame::Notify(3)) = rx.recv().await else {
            unreachable!("lossy send did not close the lane");
        };
    }

    #[tokio::test]
    async fn cancel_is_delivered_while_the_data_lane_is_full() {
        let (client, mut server) = local_lanes::<u8, u16, u8, Infallible>(4, 2);
        let upload = client.begin_upload(4).await.expect("upload");
        let Some(ServerInput::Call { id, .. }) = server.recv().await else {
            unreachable!("call routed");
        };
        client.notify(1).await.expect("notice one");
        client.notify(2).await.expect("notice two");
        drop(upload);
        let Some(ServerInput::Cancel { id: cancelled }) = server.recv().await else {
            unreachable!("cancel before data");
        };
        assert_eq!(cancelled, id);
    }

    #[tokio::test]
    async fn lagging_subscriber_is_disconnected() {
        let (client, server) = local::<Infallible, Infallible, Infallible, u8>(2);
        let mut subscriber = client.subscribe(1, 4).expect("subscribe");
        let notifier = server.notifier();
        for notice in 0..8_u8 {
            notifier.notify(notice).await.expect("notice");
        }
        assert_eq!(subscriber.recv().await, Some(0));
        assert_eq!(subscriber.recv().await, None);
    }

    #[tokio::test]
    async fn cancel_can_arrive_before_its_call() {
        let (tx, mut rx) = pair::<Frame<u8, u8, u8>>(1, 1);
        tx.send(Frame::Call {
            id: RequestId::new(1),
            body: 4,
        })
        .await
        .expect("call");
        tx.try_send(Frame::Cancel {
            id: RequestId::new(1),
        })
        .expect("cancel");
        let Some(Frame::Cancel { id }) = rx.recv().await else {
            unreachable!("cancel first");
        };
        assert_eq!(id.get(), 1);
        let Some(Frame::Call { id, body }) = rx.recv().await else {
            unreachable!("call after cancel");
        };
        assert_eq!(id.get(), 1);
        assert_eq!(body, 4);
    }

    #[tokio::test]
    async fn wait_closed_returns_when_the_flag_is_true() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).expect("set closed");
        tokio::time::timeout(std::time::Duration::from_millis(20), wait_closed(&mut rx))
            .await
            .expect("wait_closed should return");
    }

    #[tokio::test]
    async fn subscribe_fails_when_the_peer_is_gone() {
        let (tx, rx) = pair::<Frame<Infallible, Infallible, u8>>(2, 2);
        let (client, _server, _router) =
            endpoint::<Infallible, Infallible, u8, Infallible, Infallible, u8>(tx, 2);
        drop(rx);
        assert!(matches!(client.subscribe(1, 4), Err(Error::Closed)));
    }

    #[tokio::test]
    async fn subscribe_fails_at_the_subscriber_limit() {
        let (client, _server) = local::<Infallible, Infallible, Infallible, u8>(2);
        let _first = client.subscribe(1, 1).expect("first");
        assert!(matches!(
            client.subscribe(1, 1),
            Err(Error::SubscriberLimit)
        ));
    }
}
