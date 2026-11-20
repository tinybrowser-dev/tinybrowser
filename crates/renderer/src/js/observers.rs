use std::collections::BTreeMap;

use rquickjs::{Function, Object, Persistent};

use crate::documents::{DocumentStore, JournalEntry};
use crate::dom::tree::NodeId;
use crate::protocol::FrameId;

use super::world::{
    NodeReference, Observation, ObserverOptions, ObserverState, ReadyObserver, RecordData,
    match_observation, observes_region,
};

// https://dom.spec.whatwg.org/#mutation-observers
#[derive(Default)]
pub(crate) struct MutationObservers {
    observers: BTreeMap<u64, ObserverState>,
    next_id: u64,
    next_registration: u64,
    pending: Vec<u64>,
    pub(crate) scheduled: bool,
}

impl MutationObservers {
    pub(crate) fn create(
        &mut self,
        owner: FrameId,
        callback: Persistent<Function<'static>>,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.observers.insert(
            id,
            ObserverState {
                owner,
                callback,
                object: None,
                observations: Vec::new(),
                transient: Vec::new(),
                queue: Vec::new(),
            },
        );
        id
    }

    // https://dom.spec.whatwg.org/#dom-mutationobserver-observe
    pub(crate) fn observe(
        &mut self,
        id: u64,
        target: NodeReference,
        options: ObserverOptions,
        object: Persistent<Object<'static>>,
        documents: &mut DocumentStore,
    ) {
        self.drain(documents);
        if let Some(observer) = self.observers.get_mut(&id) {
            if let Some(existing) = observer
                .observations
                .iter_mut()
                .find(|item| item.target == target)
            {
                let order = existing.order;
                existing.options = options;
                // https://dom.spec.whatwg.org/#dom-mutationobserver-observe
                observer
                    .transient
                    .retain(|transient| transient.order != order);
            } else {
                self.next_registration += 1;
                observer.observations.push(Observation {
                    target,
                    options,
                    order: self.next_registration,
                });
            }
            observer.object = Some(object);
            if let Some(parsed) = documents.get_mut(target.scope().document_id()) {
                parsed.document.set_recording(true);
            }
        }
    }

    // https://dom.spec.whatwg.org/#dom-mutationobserver-disconnect
    pub(crate) fn disconnect(&mut self, id: u64, documents: &mut DocumentStore) {
        self.drain(documents);
        if let Some(observer) = self.observers.get_mut(&id) {
            observer.queue.clear();
            observer.observations.clear();
            observer.transient.clear();
            observer.object = None;
        }
        self.refresh_recording(documents);
    }

    // https://dom.spec.whatwg.org/#dom-mutationobserver-takerecords
    pub(crate) fn take_records(
        &mut self,
        id: u64,
        documents: &mut DocumentStore,
    ) -> Vec<RecordData> {
        self.drain(documents);
        self.observers
            .get_mut(&id)
            .map_or_default(|observer| std::mem::take(&mut observer.queue))
    }

    pub(crate) fn forget_frame(&mut self, owner: FrameId, documents: &mut DocumentStore) {
        self.drain(documents);
        self.observers.retain(|_, observer| observer.owner != owner);
        self.pending.retain(|id| self.observers.contains_key(id));
        self.refresh_recording(documents);
    }

    fn refresh_recording(&self, documents: &mut DocumentStore) {
        for parsed in documents.values_mut() {
            let id = parsed.id;
            let watched = self.observers.values().any(|observer| {
                observer
                    .observations
                    .iter()
                    .any(|item| item.target.scope().document_id() == id)
            });
            parsed.document.set_recording(watched);
        }
    }

    // https://dom.spec.whatwg.org/#queue-a-mutation-record — notifies interested observers in ancestor/registration order per DOM rather than Chromium's creation-priority sort
    pub(crate) fn drain(&mut self, documents: &mut DocumentStore) -> (usize, bool) {
        let mut mutations = Vec::new();
        for parsed in documents.values_mut() {
            let id = parsed.id;
            mutations.extend(
                std::mem::take(&mut parsed.document.side.journal)
                    .into_iter()
                    .map(|(position, entry)| (position, id, entry)),
            );
        }
        mutations.sort_unstable_by_key(|(position, _, _)| *position);
        let queued = mutations.len();
        let mut observed = false;
        for (_, document, entry) in mutations {
            if let Some(parsed) = documents.get(document) {
                let mut interested: Vec<_> = self
                    .observers
                    .iter()
                    .filter_map(|(&id, observer)| {
                        match_observation(&parsed.document.base, document, observer, &entry)
                            .map(|(depth, order, record)| (depth, order, id, record))
                    })
                    .collect();
                if interested.is_empty() && !observed {
                    observed = self.observers.values().any(|observer| {
                        observes_region(&parsed.document.base, document, observer, &entry)
                    });
                }
                // https://dom.spec.whatwg.org/#queue-a-mutation-record — notifies interested observers in ancestor/registration order per DOM rather than Chromium's creation-priority sort
                interested.sort_by_key(|(depth, order, _, _)| (*depth, *order));
                for (_, _, id, record) in interested {
                    if let Some(observer) = self.observers.get_mut(&id) {
                        observer.queue.push(record);
                    }
                    if !self.pending.contains(&id) {
                        self.pending.push(id);
                    }
                }
                self.register_transients(&parsed.document.base, document, &entry);
            }
        }
        (queued, observed)
    }

    // https://dom.spec.whatwg.org/#mutation-observers
    fn register_transients(
        &mut self,
        base: &blitz_dom::BaseDocument,
        document: u32,
        entry: &JournalEntry,
    ) {
        let JournalEntry::ChildList {
            target, removed, ..
        } = entry
        else {
            return;
        };
        if removed.is_empty() {
            return;
        }
        let mut ancestors = Vec::new();
        let mut current = Some(target.node);
        while let Some(id) = current {
            ancestors.push(id);
            current = base.get_node(id).and_then(|node| node.parent);
        }
        let mut sources: Vec<(u64, ObserverOptions, u64)> = Vec::new();
        for (&id, observer) in &self.observers {
            for observation in observer.observations.iter().chain(&observer.transient) {
                if !observation.options.subtree {
                    continue;
                }
                let Some(root) = observation.target.tree() else {
                    continue;
                };
                if root.document != document || !ancestors.contains(&root.node) {
                    continue;
                }
                sources.push((id, observation.options.clone(), observation.order));
            }
        }
        for (id, options, order) in sources {
            let Some(observer) = self.observers.get_mut(&id) else {
                continue;
            };
            for removed_id in removed {
                let root = NodeReference::Tree(NodeId {
                    document: removed_id.document,
                    node: removed_id.node,
                });
                if observer
                    .transient
                    .iter()
                    .any(|existing| existing.target == root && existing.order == order)
                {
                    continue;
                }
                observer.transient.push(Observation {
                    target: root,
                    options: options.clone(),
                    order,
                });
            }
        }
    }

    pub(crate) fn needs_notification(&self) -> bool {
        !self.scheduled && !self.pending.is_empty()
    }

    // https://dom.spec.whatwg.org/#notify-mutation-observers
    pub(crate) fn begin_notification(&mut self, documents: &mut DocumentStore) -> Vec<u64> {
        self.drain(documents);
        self.scheduled = false;
        // https://dom.spec.whatwg.org/#notify-mutation-observers
        for observer in self.observers.values_mut() {
            observer.transient.clear();
        }
        std::mem::take(&mut self.pending)
    }

    pub(crate) fn delivery(&mut self, id: u64) -> Option<ReadyObserver> {
        let observer = self.observers.get_mut(&id)?;
        let records = std::mem::take(&mut observer.queue);
        if records.is_empty() {
            return None;
        }
        Some(ReadyObserver {
            callback: observer.callback.clone(),
            object: observer.object.clone()?,
            records,
        })
    }
}
