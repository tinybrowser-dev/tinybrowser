//! Blitz service providers bridged onto carrier dials.
//!
//! The renderer owns no network runtime of its own (the carrier runtime has
//! IO disabled, so our Tokio-based agent cannot run here): subresource
//! fetches enqueue into the owning document's dial queue through a channel,
//! and completions deliver Blitz handler bytes on the document's thread.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// One Blitz subresource fetch awaiting a carrier dial.
pub(crate) struct BlitzFetch {
    /// Handler id the document files the delivery target under.
    pub id: u64,
    /// What Blitz asked for: the URL, destination, initiating element, and the
    /// element state the request was created with.
    pub request: blitz_traits::net::Request,
}

/// Delivery target for one fetch: Blitz's response handler, counted so the
/// resolve loop can tell settled from outstanding.
pub(crate) struct CountingHandler {
    inner: Option<Box<dyn blitz_traits::net::NetHandler>>,
    in_flight: std::sync::Arc<AtomicUsize>,
}

impl blitz_traits::net::NetHandler for CountingHandler {
    fn bytes(mut self: Box<Self>, resolved_url: String, bytes: blitz_traits::net::Bytes) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        inner.bytes(resolved_url, bytes);
    }

    fn response(
        mut self: Box<Self>,
        resolved_url: String,
        bytes: blitz_traits::net::Bytes,
        info: blitz_traits::net::ResponseInfo,
    ) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        inner.response(resolved_url, bytes, info);
    }
}

impl CountingHandler {
    /// The handler the document files, and the fetch record carrying its id.
    /// Delivery decrements the provider count exactly once: the document
    /// always calls the filed handler on completion or failure (failures
    /// deliver empty bytes, which Blitz reports as a failed resource and
    /// releases its critical hold).
    fn file(
        in_flight: &std::sync::Arc<AtomicUsize>,
        request: blitz_traits::net::Request,
        handler: Box<dyn blitz_traits::net::NetHandler>,
    ) -> (Self, BlitzFetch) {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        in_flight.fetch_add(1, Ordering::SeqCst);
        let fetch = BlitzFetch { id, request };
        (
            Self {
                inner: Some(handler),
                in_flight: std::sync::Arc::clone(in_flight),
            },
            fetch,
        )
    }
}

/// Fetches Blitz subresources (stylesheets, images, fonts) through carrier
/// dials: `fetch` files the handler and enqueues the URL; the owning
/// document dials it and delivers the bytes back.
pub(crate) struct TinyNetProvider {
    tx: std::sync::mpsc::Sender<(BlitzFetch, CountingHandler)>,
    settle_tx: std::sync::mpsc::Sender<blitz_dom::ImageSettlement>,
    in_flight: std::sync::Arc<AtomicUsize>,
}

impl TinyNetProvider {
    /// Builds a provider enqueueing into `tx`, drained by the owning document.
    pub(crate) fn new(
        tx: std::sync::mpsc::Sender<(BlitzFetch, CountingHandler)>,
        settle_tx: std::sync::mpsc::Sender<blitz_dom::ImageSettlement>,
    ) -> Self {
        Self {
            tx,
            settle_tx,
            in_flight: std::sync::Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Count of fetches filed but not yet delivered: the queue plus every
    /// handler a dial has not answered yet. A host that reports ready work
    /// reads it so a fetch queued during a settle's microtask continuation
    /// keeps the engine ticking until the next drain ingests it.
    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

impl blitz_dom::ImageSettleObserver for TinyNetProvider {
    fn settled(&self, settlement: blitz_dom::ImageSettlement) {
        // A closed channel means teardown; the settlement is then invisible by
        // design.
        let _ = self.settle_tx.send(settlement);
    }
}

impl blitz_traits::net::NetProvider for TinyNetProvider {
    fn fetch(
        &self,
        _doc_id: usize,
        request: blitz_traits::net::Request,
        handler: Box<dyn blitz_traits::net::NetHandler>,
    ) {
        let (counting, fetch) = CountingHandler::file(&self.in_flight, request, handler);
        // The document drains the queue on its thread; a disconnected
        // receiver means teardown, and the count already covers the drop.
        let _ = self.tx.send((fetch, counting));
    }
}

/// No-op shell except for the redraw request: a DOM mutation that changes
/// rendered output asks for a frame, which is the embedder's cue to resolve
/// style and layout (and so start the image fetches style references).
pub(crate) struct TinyShell {
    redraw: AtomicBool,
}

impl TinyShell {
    /// A shell whose redraw flag starts clear.
    pub(crate) fn new() -> Self {
        Self {
            redraw: AtomicBool::new(false),
        }
    }

    /// Takes the pending redraw request, clearing it.
    pub(crate) fn take_redraw(&self) -> bool {
        self.redraw.swap(false, Ordering::SeqCst)
    }
}

impl blitz_traits::shell::ShellProvider for TinyShell {
    fn request_redraw(&self) {
        self.redraw.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::{CountingHandler, TinyNetProvider};
    use blitz_traits::net::{NetHandler, NetProvider};

    struct VecHandler {
        tx: std::sync::mpsc::Sender<(String, Vec<u8>)>,
    }

    impl NetHandler for VecHandler {
        fn bytes(self: Box<Self>, resolved_url: String, bytes: blitz_traits::net::Bytes) {
            let _ = self.tx.send((resolved_url, bytes.to_vec()));
        }
    }

    #[test]
    fn fetch_enqueues_and_delivery_settles() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (settle_tx, _settle_rx) = std::sync::mpsc::channel();
        let provider = TinyNetProvider::new(tx, settle_tx);
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        provider.fetch(
            1,
            blitz_traits::net::Request::get("http://127.0.0.1/a.css".parse().expect("url")),
            Box::new(VecHandler { tx: reply_tx }),
        );
        assert_eq!(provider.in_flight(), 1);
        let (fetch, handler) = rx.try_recv().expect("enqueued");
        assert_eq!(fetch.request.url.as_str(), "http://127.0.0.1/a.css");
        Box::new(handler).bytes(
            fetch.request.url.as_str().to_owned(),
            blitz_traits::net::Bytes::from_static(b"div{color:red}"),
        );
        assert_eq!(provider.in_flight(), 0);
        let (resolved, bytes) = reply_rx.try_recv().expect("delivered");
        assert_eq!(resolved, "http://127.0.0.1/a.css");
        assert_eq!(bytes, b"div{color:red}");
    }

    #[test]
    fn second_delivery_is_noop() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        let in_flight = Arc::new(AtomicUsize::new(1));
        let spent = CountingHandler {
            inner: None,
            in_flight: Arc::clone(&in_flight),
        };
        Box::new(spent).bytes(
            "http://127.0.0.1/b.css".to_owned(),
            blitz_traits::net::Bytes::from_static(b"x"),
        );
        assert_eq!(in_flight.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
