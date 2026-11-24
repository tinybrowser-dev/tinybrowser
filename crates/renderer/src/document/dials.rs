// https://html.spec.whatwg.org/multipage/document-lifecycle.html#abort-a-document
// https://fetch.spec.whatwg.org/#concept-fetch-group

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use tokio::sync::Notify;

use super::{CompletedDial, DialContext, DialId, LoadDelay, LoadDelayGuard, QueuedDial};
use crate::js::{JsFetchId, Stop};
use crate::protocol::{DialCancellation, DialCompletion, DialRequest};

pub(crate) struct QueuedEntry {
    pub(crate) dial: QueuedDial,
    pub(crate) delay: Option<LoadDelayGuard>,
}

#[derive(Default)]
pub(crate) struct AdoptedDials {
    pub(crate) settled: Vec<(DialId, Result<CompletedDial, DialContext>)>,
    pub(crate) abandoned: Vec<Result<CompletedDial, DialContext>>,
}

pub(crate) struct InFlightDial {
    context: DialContext,
    _delay: Option<LoadDelayGuard>,
    // https://fetch.spec.whatwg.org/#keepalive-flag
    keepalive: bool,
    cancel: DialCancellation,
}

pub(crate) struct DialLedger {
    tx: Sender<(DialId, Result<CompletedDial, DialContext>)>,
    rx: Receiver<(DialId, Result<CompletedDial, DialContext>)>,
    queued: VecDeque<QueuedEntry>,
    in_flight: HashMap<DialId, InFlightDial>,
    next_id: DialId,
    pending: Arc<AtomicUsize>,
    stop: Arc<Stop>,
    wake: Arc<Notify>,
}

impl DialLedger {
    pub(crate) fn new(stop: Arc<Stop>, wake: Arc<Notify>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            queued: VecDeque::new(),
            in_flight: HashMap::new(),
            next_id: DialId::default(),
            pending: Arc::new(AtomicUsize::new(0)),
            stop,
            wake,
        }
    }

    pub(crate) fn queue(&mut self, dial: QueuedDial, delay: &LoadDelay) {
        let guard = delays_load(dial.context).then(|| delay.guard());
        self.queued.push_back(QueuedEntry { dial, delay: guard });
    }

    pub(crate) fn pop_queued(&mut self) -> Option<QueuedEntry> {
        self.queued.pop_front()
    }

    // https://fetch.spec.whatwg.org/#keepalive-flag
    pub(crate) fn take_queued_keepalive(&mut self) -> Vec<QueuedEntry> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.queued.len());
        for entry in self.queued.drain(..) {
            if entry.dial.req.keepalive && matches!(entry.dial.context, DialContext::JsFetch { .. })
            {
                taken.push(entry);
            } else {
                kept.push_back(entry);
            }
        }
        self.queued = kept;
        taken
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.queued.is_empty()
            || !self.in_flight.is_empty()
            || self.pending.load(Ordering::Acquire) > 0
    }

    pub(crate) fn has_queued(&self) -> bool {
        !self.queued.is_empty() || self.pending.load(Ordering::Acquire) > 0
    }

    pub(crate) fn start(
        &mut self,
        dial: QueuedDial,
        delay: Option<LoadDelayGuard>,
        start: &dyn Fn(DialRequest, DialCompletion) -> DialCancellation,
    ) {
        let id = self.next_id.take_next();
        let completed = self.tx.clone();
        let wake = Arc::clone(&self.wake);
        let stop = Arc::clone(&self.stop);
        let pending = Arc::clone(&self.pending);
        let context = dial.context;
        let dial_keepalive = dial.req.keepalive;
        let request = dial.req.clone();
        let cancel = start(
            request,
            Box::new(move |outcome| {
                if stop.is_set() {
                    return;
                }
                let result = super::dial::complete(dial, outcome);
                pending.fetch_add(1, Ordering::AcqRel);
                if completed.send((id, result)).is_ok() {
                    wake.notify_one();
                } else {
                    pending.fetch_sub(1, Ordering::AcqRel);
                }
            }),
        );
        self.in_flight.insert(
            id,
            InFlightDial {
                context,
                _delay: delay,
                keepalive: dial_keepalive,
                cancel,
            },
        );
    }

    pub(crate) fn adopt(&mut self) -> AdoptedDials {
        let mut adopted = AdoptedDials::default();
        while let Ok((id, completed)) = self.rx.try_recv() {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            if !self.in_flight.contains_key(&id) {
                // https://fetch.spec.whatwg.org/#concept-response-body
                adopted.abandoned.push(completed);
                continue;
            }
            adopted.settled.push((id, completed));
        }
        adopted
    }

    pub(crate) fn settle(&mut self, id: DialId) -> bool {
        self.in_flight.remove(&id).is_some()
    }

    fn contexts(&self) -> impl Iterator<Item = &DialContext> {
        self.queued
            .iter()
            .map(|entry| &entry.dial.context)
            .chain(self.in_flight.values().map(|dial| &dial.context))
    }

    pub(crate) fn pending_js_fetches(&self) -> usize {
        self.contexts()
            .filter(|context| matches!(context, DialContext::JsFetch { .. }))
            .count()
    }

    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#the-document-object
    pub(crate) fn has_pending_frame_load(&self) -> bool {
        self.contexts()
            .any(|context| matches!(context, DialContext::FrameLoad { .. }))
    }

    pub(crate) fn cancel_fetch(&mut self, realm: u64, id: JsFetchId) {
        self.cancel_where(|context| {
            matches!(
                context,
                DialContext::JsFetch { id: fetch_id, realm: fetch_realm }
                    if *fetch_id == id && *fetch_realm == realm
            )
        });
    }

    pub(crate) fn cancel_frame_loads(&mut self) {
        self.cancel_where(|context| matches!(context, DialContext::FrameLoad { .. }));
    }

    fn cancel_where(&mut self, matches: impl Fn(&DialContext) -> bool) {
        self.queued.retain(|entry| !matches(&entry.dial.context));
        for (_, dial) in self.in_flight.extract_if(|_, dial| matches(&dial.context)) {
            (dial.cancel)();
        }
    }

    // https://fetch.spec.whatwg.org/#keepalive-flag
    pub(crate) fn terminate(mut self) {
        self.queued.clear();
        for (_, dial) in std::mem::take(&mut self.in_flight) {
            if dial.keepalive {
                continue;
            }
            (dial.cancel)();
        }
    }
}

fn delays_load(context: DialContext) -> bool {
    match context {
        DialContext::JsFetch { .. }
        | DialContext::CspReport
        | DialContext::BlitzResource {
            destination: Some(crate::csp::Destination::Img),
            ..
        } => false,
        DialContext::ClassicScript { .. }
        | DialContext::FrameLoad { .. }
        | DialContext::BlitzResource { .. } => true,
    }
}
