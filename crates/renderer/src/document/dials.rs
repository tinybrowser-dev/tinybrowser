//! One document's dial ledger: what is queued, what is in flight, and every
//! cancellation handle.
//!
//! The ledger is the document's fetch group. Every started dial of every kind
//! files its cancellation handle here, so aborting the document terminates
//! classic scripts, frame loads, stylesheets, images, and page `fetch` alike
//! (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#abort-a-document>,
//! <https://fetch.spec.whatwg.org/#concept-fetch-group>). A navigation replaces
//! the ledger, so the new document cannot adopt a completion from the old one.
//!
//! The ledger owns the completion channel and the `load`-event delay policy, so
//! no other module sees a sender, receiver, or guard.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use tokio::sync::Notify;

use super::{CompletedDial, DialContext, DialId, LoadDelay, LoadDelayGuard, QueuedDial};
use crate::js::{JsFetchId, Stop};
use crate::protocol::{DialCancellation, DialCompletion, DialRequest};

/// One queued dial and the load-event delay its request already holds.
pub(crate) struct QueuedEntry {
    pub(crate) dial: QueuedDial,
    pub(crate) delay: Option<LoadDelayGuard>,
}

/// What one adoption pass found: completions whose dial is still filed, and
/// completions that outlived their cancellation.
#[derive(Default)]
pub(crate) struct AdoptedDials {
    pub(crate) settled: Vec<(DialId, Result<CompletedDial, DialContext>)>,
    pub(crate) abandoned: Vec<Result<CompletedDial, DialContext>>,
}

/// One started dial and the resources it releases on settle.
pub(crate) struct InFlightDial {
    context: DialContext,
    /// Held from dial start until `settle` drops the entry, which is what
    /// delays the document's `load` event.
    _delay: Option<LoadDelayGuard>,
    /// Whether the request carries Fetch's keepalive flag, which makes it
    /// outlive the document that started it
    /// (<https://fetch.spec.whatwg.org/#keepalive-flag>).
    keepalive: bool,
    cancel: DialCancellation,
}

pub(crate) struct DialLedger {
    /// Completions from every started dial, keyed by ledger entry so adoption
    /// frees the right cancellation handle. Both ends stay private: a caller
    /// that could clone the sender could outlive the ledger's accounting.
    tx: Sender<(DialId, Result<CompletedDial, DialContext>)>,
    rx: Receiver<(DialId, Result<CompletedDial, DialContext>)>,
    /// Dials waiting for the drain to start them.
    queued: VecDeque<QueuedEntry>,
    /// Started dials, keyed by a ledger-local id. An entry stays here until its
    /// completion task runs, so cancellation can still find adopted work.
    in_flight: HashMap<DialId, InFlightDial>,
    next_id: DialId,
    /// Completions sent but not yet adopted. Incremented before the send and
    /// decremented in [`DialLedger::adopt`], so a host that asks whether the
    /// document owes work never sleeps while a completion waits in the
    /// channel.
    pending: Arc<AtomicUsize>,
    /// A frame that is shutting down drops completions instead of waking the
    /// session for work nobody will run.
    stop: Arc<Stop>,
    /// Wakes the session loop when a dial finishes.
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

    /// Queues one dial for the next drain step. The ledger mints its load-event
    /// delay here, so the policy is one exhaustive match and a new context
    /// cannot silently stop delaying `load`.
    pub(crate) fn queue(&mut self, dial: QueuedDial, delay: &LoadDelay) {
        let guard = delays_load(dial.context).then(|| delay.guard());
        self.queued.push_back(QueuedEntry { dial, delay: guard });
    }

    /// Pops one queued dial. A `data:` completion can reset the document and
    /// replace this ledger, so the caller must not hold a batch across it.
    pub(crate) fn pop_queued(&mut self) -> Option<QueuedEntry> {
        self.queued.pop_front()
    }

    /// Takes the queued `keepalive` page fetches, leaving every other dial
    /// behind. A `keepalive` request must reach the network even when the
    /// document is destroyed before the next drain step, so the teardown
    /// starts these directly
    /// (<https://fetch.spec.whatwg.org/#keepalive-flag>).
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

    /// Whether the document owes dial work: an unstarted dial, a started dial
    /// that has not completed, or a completion that was sent and has not been
    /// adopted yet.
    ///
    /// A host that waits on this must not call it "nothing to do" while a
    /// dial is in flight; the document cannot make progress without running its
    /// completion task.
    pub(crate) fn has_pending(&self) -> bool {
        !self.queued.is_empty()
            || !self.in_flight.is_empty()
            || self.pending.load(Ordering::Acquire) > 0
    }

    pub(crate) fn has_queued(&self) -> bool {
        !self.queued.is_empty() || self.pending.load(Ordering::Acquire) > 0
    }

    /// Starts one network dial and files its cancellation and delay.
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
                // Count before the send so an adopt on another thread never
                // observes the message and decrements first.
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

    /// Collects finished dials in completion order (oldest first). The
    /// `InFlightDial` stays filed, so its guard still delays `load` until the
    /// task that handles the completion actually runs.
    pub(crate) fn adopt(&mut self) -> AdoptedDials {
        let mut adopted = AdoptedDials::default();
        while let Ok((id, completed)) = self.rx.try_recv() {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            if !self.in_flight.contains_key(&id) {
                // The dial was cancelled after it finished, so no task will
                // read this response
                // (<https://fetch.spec.whatwg.org/#concept-response-body>).
                adopted.abandoned.push(completed);
                continue;
            }
            adopted.settled.push((id, completed));
        }
        adopted
    }

    /// Settles the dial whose completion task is running: the entry drops
    /// here, which releases its `load`-event delay. `false` means the dial
    /// was cancelled after its completion was adopted.
    pub(crate) fn settle(&mut self, id: DialId) -> bool {
        self.in_flight.remove(&id).is_some()
    }

    /// Every dial's context, queued or in flight or waiting for its
    /// completion task.
    fn contexts(&self) -> impl Iterator<Item = &DialContext> {
        self.queued
            .iter()
            .map(|entry| &entry.dial.context)
            .chain(self.in_flight.values().map(|dial| &dial.context))
    }

    /// Page fetches in flight, queued, or waiting for their completion task.
    pub(crate) fn pending_js_fetches(&self) -> usize {
        self.contexts()
            .filter(|context| matches!(context, DialContext::JsFetch { .. }))
            .count()
    }

    /// Whether a frame navigation is queued, in flight, or waiting for its
    /// completion task. The frame's current document stays complete until the
    /// response commits, so the parent must not fire the container's `load`
    /// for that old document in the meantime
    /// (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#the-document-object>).
    pub(crate) fn has_pending_frame_load(&self) -> bool {
        self.contexts()
            .any(|context| matches!(context, DialContext::FrameLoad { .. }))
    }

    /// Drops a queued or in-flight page fetch for one JS id, running its
    /// cancellation handle.
    pub(crate) fn cancel_fetch(&mut self, id: JsFetchId) {
        self.cancel_where(
            |context| matches!(context, DialContext::JsFetch { id: fetch_id } if *fetch_id == id),
        );
    }

    /// Cancels a previous frame navigation: the newest load owns the frame.
    pub(crate) fn cancel_frame_loads(&mut self) {
        self.cancel_where(|context| matches!(context, DialContext::FrameLoad { .. }));
    }

    fn cancel_where(&mut self, matches: impl Fn(&DialContext) -> bool) {
        self.queued.retain(|entry| !matches(&entry.dial.context));
        for (_, dial) in self.in_flight.extract_if(|_, dial| matches(&dial.context)) {
            (dial.cancel)();
        }
    }

    /// Aborts the whole document: every queued and started dial is cancelled
    /// and its `load`-event delay is released, except a `keepalive` request,
    /// which the environment's destruction does not cancel
    /// (<https://fetch.spec.whatwg.org/#keepalive-flag>). Dropping its
    /// cancellation handle leaves the request running in the network process;
    /// its completion lands in a channel nothing reads, which is what
    /// outliving the document means here.
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

/// Whether one dial delays its document's `load` event. Images are held by
/// their DOM waiter until decode instead, so the dial itself does not count.
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
