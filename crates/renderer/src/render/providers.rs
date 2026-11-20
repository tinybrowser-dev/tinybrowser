use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub(crate) struct BlitzFetch {
    pub id: u64,
    pub request: blitz_traits::net::Request,
}

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

pub(crate) struct TinyNetProvider {
    tx: std::sync::mpsc::Sender<(BlitzFetch, CountingHandler)>,
    settle_tx: std::sync::mpsc::Sender<blitz_dom::ImageSettlement>,
    in_flight: std::sync::Arc<AtomicUsize>,
}

impl TinyNetProvider {
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

    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

impl blitz_dom::ImageSettleObserver for TinyNetProvider {
    fn settled(&self, settlement: blitz_dom::ImageSettlement) {
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
        let _ = self.tx.send((fetch, counting));
    }
}

pub(crate) struct TinyShell {
    redraw: AtomicBool,
}

impl TinyShell {
    pub(crate) fn new() -> Self {
        Self {
            redraw: AtomicBool::new(false),
        }
    }

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
