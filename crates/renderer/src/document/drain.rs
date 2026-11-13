use std::time::Duration;

use tokio::time::Instant;

use super::{
    CompletedDial, DialContext, Document, MAX_PENDING_JS_FETCHES, QueuedDial, Task, Timer,
};
use crate::document::dials::AdoptedDials;
use crate::dom::tree::NodeId;
use crate::js::{JsFetchId, RealmRequest};
use crate::protocol::{BrowserEvent, RendererEvent, TimerId};

/// Upper bound on realm requests applied in one turn. Host callbacks can
/// enqueue more work while a request runs, so a turn must not loop forever.
const MAX_REALM_REQUESTS_PER_TURN: usize = 1024;

impl Document {
    /// Runs one batch of ready work and reports whether the engine must
    /// reconcile and run another pass: a task ran (its side effects include a
    /// queued navigation or a new iframe the next task must see), or the
    /// document already has follow-up work.
    ///
    /// The engine calls this instead of draining the whole document at once so
    /// it can reconcile frame documents between tasks: a task that inserts an
    /// iframe must be visible to the next task's `contentDocument` read, and a
    /// navigation queued by the last task must still be applied.
    pub(crate) fn drain_step(&mut self) -> bool {
        self.adopt_dial_completions();
        while let Some(id) = self.due_timer() {
            self.tasks.push_back(Task::Timer(id));
        }
        self.adopt_js_work();
        self.drain_blitz_fetches();
        self.launch_queued_dials();
        self.adopt_dial_completions();
        let Some(task) = self.tasks.pop_front() else {
            let fired = self.check_load();
            return !self.stop.is_set()
                && (fired || self.has_pending_frame_loads() || self.awaits_proxy_replay());
        };
        // Joins the drain's turn when one is open, so serial tasks share
        // its deadline. A task outside a drain still opens its own turn.
        let _turn = self.js.as_ref().map(crate::js::JsRealm::enter_turn);
        self.run_task(task);
        // Deliver resource messages between tasks, but keep the settle for the
        // end of the batch. A later task can observe an arrival (a font
        // registered into the shaping collection, a stylesheet `@import`
        // started), while several mutations still share one style/layout pass
        // at the batch's one rendering opportunity.
        self.pump_blitz_messages();
        // A task ran even when nothing is left: the engine still reconciles the
        // queued navigations and child frames it produced
        // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentdocument>).
        !self.stop.is_set()
    }

    /// Whether a task is queued or a timer is already due.
    pub(crate) fn has_ready_work(&self) -> bool {
        if !self.tasks.is_empty() || self.dials.has_queued() {
            return true;
        }
        // A queued navigation or child traversal is engine work the next
        // reconcile applies, even though no task carries it.
        if !self.frame_traversals.is_empty() || self.world.borrow().has_frame_navigations() {
            return true;
        }
        if self.world.borrow().has_outbox_work() {
            return true;
        }
        let now = Instant::now();
        self.timers
            .values()
            .any(|timer| !timer.fired && timer.when.is_some_and(|when| when <= now))
    }

    /// Whether a dial is queued, in flight, or waiting for its completion task.
    ///
    /// A host that can make progress only through a dial uses this to wait for
    /// the completion instead of declaring the page idle. [`Self::has_ready_work`]
    /// deliberately excludes an in-flight dial so the native engine can sleep on
    /// its wake handle rather than spin.
    pub(crate) fn has_pending_dials(&self) -> bool {
        self.dials.has_pending()
    }

    fn adopt_dial_completions(&mut self) {
        let AdoptedDials { settled, abandoned } = self.dials.adopt();
        for (id, completed) in settled {
            match completed {
                Ok(done) => self.tasks.push_back(Task::DialFinished(id, Box::new(done))),
                Err(fail) => self.tasks.push_back(Task::DialFailed(id, fail)),
            }
        }
        for completed in abandoned {
            self.abandon_dial(completed);
        }
    }

    /// Drops a completion no task will read. A streamed body's task keeps
    /// reading into its channel and holds the tab's dial permits until the
    /// stream is cancelled, so nothing else can reclaim it
    /// (<https://fetch.spec.whatwg.org/#concept-response-body>).
    fn abandon_dial(&self, completed: Result<CompletedDial, DialContext>) {
        if let Ok(done) = completed
            && let crate::protocol::ResponseBody::Streamed(stream) = done.outcome.body
        {
            self.services.cancel_stream(stream);
        }
    }

    /// Earliest timer deadline this document waits for.
    #[must_use]
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.timers
            .values()
            .filter(|timer| !timer.fired)
            .filter_map(|timer| timer.when)
            .min()
    }

    pub(crate) fn schedule_timer(&mut self, delay: Duration) -> TimerId {
        let id = self.next_timer_id.take_next();
        self.timers.insert(
            id,
            Timer {
                when: Instant::now().checked_add(delay),
                fired: false,
                js_id: None,
            },
        );
        id
    }

    /// Stops the frame and drops its queued work. The shared `QuickJS` heap
    /// belongs to the page engine, not the frame, so it is not touched.
    pub(crate) fn shutdown(&mut self) {
        self.stop.request();
        self.release();
    }

    pub(crate) fn release(&mut self) {
        let old = std::mem::replace(
            &mut self.dials,
            super::DialLedger::new(
                std::sync::Arc::clone(&self.stop),
                std::sync::Arc::clone(&self.wake),
            ),
        );
        old.terminate();
        self.decoder = None;
        self.shared.borrow_mut().close_frame_ports(self.frame);
        self.world.borrow_mut().forget_owned_documents();
    }

    pub(crate) fn waiting_for_load(&self) -> bool {
        self.world.borrow().main_ready_state() != crate::ReadyState::Complete
            || self.dials.has_pending_frame_load()
    }

    fn has_pending_frame_loads(&self) -> bool {
        !self.pending_frame_loads.is_empty()
    }

    pub(in crate::document) fn launch_queued_dials(&mut self) {
        // A `data:` response can itself request more `data:` images. Keep
        // decoding those before returning so paint does not run with the
        // fetches still sitting on the queue. Eight passes is the bound for one
        // drain step, so a `data:` cycle cannot spin inside this call.
        for _ in 0..8 {
            let mut completed_data = false;
            // One at a time: a `data:` frame load can commit a new document
            // and replace the ledger, and the rest of the old queue must not
            // start on the new one.
            while let Some(entry) = self.dials.pop_queued() {
                let super::dials::QueuedEntry { dial, delay } = entry;
                // The queue holds a parsed URL, so the scheme is its scheme.
                if dial.req.url.scheme() == "data" {
                    completed_data = true;
                    // Same ceiling as a streamed navigation body. Decoding an
                    // unbounded `data:` URL would sit entirely in this process.
                    if let Some((content_type, body)) =
                        crate::engine::decode_data_url_bounded(dial.req.url.as_str())
                    {
                        drop(delay);
                        let final_url = dial.req.url.clone();
                        self.finish_dial(super::CompletedDial {
                            context: dial.context,
                            req: dial.req,
                            ancestors: dial.ancestors,
                            integrity: dial.integrity,
                            outcome: crate::protocol::DialOutcome {
                                status: 200,
                                status_text: "OK".to_owned(),
                                final_url,
                                content_type: Some(content_type.clone()),
                                // Scheme fetch serves the data: MIME type
                                // as the response's `Content-Type` header
                                // (<https://fetch.spec.whatwg.org/#concept-scheme-fetch>).
                                headers: vec![("content-type".to_owned(), content_type)],
                                body: crate::protocol::ResponseBody::Bytes(body),
                                response_type: net::cors::ResponseType::Basic,
                                crossed_origin: false,
                                redirect_count: 0,
                                cookies_stored: false,
                                referrer: None,
                            },
                        });
                    } else {
                        drop(delay);
                        self.fail_dial(dial.context);
                    }
                    continue;
                }
                let services = &*self.services;
                self.dials.start(dial, delay, &|request, completion| {
                    services.start_dial(request, completion)
                });
            }
            if !completed_data {
                break;
            }
            self.drain_blitz_fetches();
        }
        if self.dials.has_queued() {
            self.wake.notify_one();
        }
    }

    fn run_task(&mut self, task: Task) {
        match task {
            Task::Timer(id) => {
                self.record_event(RendererEvent::Timer(id));
                if self.refresh_timer == Some(id) {
                    self.refresh_timer = None;
                    self.fire_refresh();
                }
                if let Some(timer) = self.timers.remove(&id)
                    && let Some(js_id) = timer.js_id
                {
                    self.js_timers.remove(&js_id);
                    self.fire_js(|js| js.fire_timer(js_id));
                }
                self.adopt_js_work();
            }
            Task::DialFinished(id, done) => {
                if self.dials.settle(id) {
                    self.finish_dial(*done);
                } else {
                    self.abandon_dial(Ok(*done));
                }
            }
            Task::DialFailed(id, fail) => {
                if self.dials.settle(id) {
                    self.fail_dial(fail);
                }
            }
            Task::WindowMessage(message) => self.deliver_window_message(&message),
            Task::StorageEvent(event) => self.deliver_storage_event(&event),
            Task::RemoteMessage(message) => self.deliver_remote_message(&message),
            Task::BroadcastMessage {
                origin,
                name,
                payload,
                source,
            } => self.deliver_broadcast_message(&origin, &name, &payload, source),
            Task::PortMessage {
                endpoint,
                payload,
                ports,
            } => self.deliver_port_message(endpoint, &payload, &ports),
            Task::PortClosed { endpoint } => self.deliver_port_close(endpoint),
            Task::WorkerMessage {
                worker,
                payload,
                ports,
            } => {
                self.fire_js(|js| js.deliver_worker_event(worker.get(), &payload, &ports));
            }
            Task::WorkerError(error) => {
                self.fire_js(|js| {
                    js.deliver_worker_error(
                        error.worker.get(),
                        &error.message,
                        &error.filename,
                        error.lineno,
                        error.colno,
                    )
                });
            }
            Task::WorkerLoadFailed(worker) => {
                self.fire_js(|js| js.deliver_worker_load_error(worker.get()));
            }
            Task::NodeLoad(element) => {
                self.fire_node_load(element);
            }
            Task::NodeError(element) => {
                self.fire_js(|js| js.fire_node_error(element));
                self.adopt_js_work();
            }
            Task::CspViolation(violations) => self.deliver_csp_violations(&violations),
            Task::SelectionChanged(node) => {
                self.fire_js(|js| js.fire_selection_changed(node));
                self.adopt_js_work();
            }
        }
    }

    /// Drains the realm's outbox in enqueue order. Requests generated while
    /// one runs go behind the requests already queued, and one turn stops at
    /// `MAX_REALM_REQUESTS_PER_TURN` so a host callback cannot hold the
    /// renderer forever.
    pub(in crate::document) fn adopt_js_work(&mut self) {
        for _ in 0..MAX_REALM_REQUESTS_PER_TURN {
            let Some(request) = self.world.borrow().pop_outbox() else {
                break;
            };
            self.apply_realm_request(request);
        }
    }

    fn apply_realm_request(&mut self, request: RealmRequest) {
        match request {
            RealmRequest::Timer(timeout) => {
                let page_id = self.schedule_timer(timeout.delay);
                if let Some(timer) = self.timers.get_mut(&page_id) {
                    timer.js_id = Some(timeout.js_id);
                }
                self.js_timers.insert(timeout.js_id, page_id);
            }
            RealmRequest::ClearTimer(js_id) => {
                if let Some(page_id) = self.js_timers.remove(&js_id) {
                    self.timers.remove(&page_id);
                }
            }
            RealmRequest::CookieChanges => {
                // Each realm diffs the jar against what it last processed, so
                // two writes before this turn report one event with both
                // (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>).
                self.fire_js(crate::js::JsRealm::process_cookie_changes);
            }
            RealmRequest::Fetch(fetch) => self.queue_js_fetch(*fetch),
            RealmRequest::AbortFetch(js_id) => self.dials.cancel_fetch(js_id),
            RealmRequest::Script(element) => self.queue_dynamic_script(element),
            RealmRequest::DeclarativeRefresh(value) => {
                self.start_declarative_refresh(&value);
            }
            RealmRequest::HistoryPush {
                url,
                state,
                handling,
            } => {
                self.apply_document_url_same_origin(&url);
                self.history_updates.push(crate::document::HistoryUpdate {
                    url: url.clone(),
                    state: state.clone(),
                    handling,
                });
                self.record_event(RendererEvent::Browser(BrowserEvent::HistoryUpdated {
                    url,
                    state,
                    handling,
                }));
            }
            RealmRequest::FragmentNavigation {
                old_url,
                url,
                handling,
            } => {
                // The setter moved the document URL, the joint entry, and the
                // owner realm's `history` view and `popstate` already. The
                // document URL the loader keeps must move too, and the
                // `hashchange` is the part this turn delivers
                // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment>).
                self.apply_document_url_same_origin(url.as_str());
                // The URL now names a fragment, so `:target` moves to the
                // element it names before the `hashchange` handler can read a
                // style
                // (<https://drafts.csswg.org/selectors-4/#the-target-pseudo>).
                self.update_url_target();
                let url = url.to_string();
                self.history_updates.push(crate::document::HistoryUpdate {
                    url: url.clone(),
                    state: "null".to_owned(),
                    handling,
                });
                // A fragment navigation never carries classic state, so the
                // realm restores `null` rather than a serialized payload.
                let length = self.world.borrow().history.length;
                self.fire_js(|js| {
                    js.restore_history(None, length)?;
                    js.fire_hash_change(&old_url, &url)
                });
                self.record_event(RendererEvent::Browser(BrowserEvent::HistoryUpdated {
                    url,
                    state: "null".to_owned(),
                    handling,
                }));
            }
            RealmRequest::HistoryTraverse { delta } => {
                // A step names a joint position no single renderer can resolve
                // alone, so the engine forwards it: a browser-hosted engine
                // turns it into a tab event, and a standalone engine applies
                // it against its own joint list
                // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta>).
                self.frame_traversals.push(delta);
            }
            RealmRequest::SelectionChanged(node) => self.push_selection_changed(node),
            RealmRequest::CspViolation(Some(target), violations) => {
                self.fire_csp_violations(target, &violations);
            }
            RealmRequest::CspViolation(None, violations) => {
                self.deliver_csp_violations(&violations);
            }
            RealmRequest::WorkerBlocked(worker, violations) => {
                self.deliver_csp_violations(&violations);
                self.push_worker_load_failed(worker);
            }
            RealmRequest::HtmlWrite(write) => {
                self.write_text(&write.html);
            }
            RealmRequest::DocumentStream(command) => {
                let closed = matches!(command, crate::js::DocumentStreamCommand::Close);
                self.apply_document_stream(command);
                self.stream_publish = Some(self.stream_publish.unwrap_or(false) || closed);
            }
        }
    }

    /// A connected `<script src>` element queued its prepare steps from
    /// script; the fetch starts here.
    fn queue_dynamic_script(&mut self, element: NodeId) {
        // Scripting is disabled in a sandboxed frame without `allow-scripts`:
        // a script element never executes, however it was inserted
        // (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-scripts-browsing-context-flag>).
        if self.world.borrow().sandbox.scripts_disabled() {
            return;
        }
        let Some(script) = crate::js::script_at(&self.world.borrow(), element) else {
            return;
        };
        let Some(started) = self.world.borrow().with_document(element, |parsed| {
            parsed.document.script_already_started(element.node)
        }) else {
            return;
        };
        if started {
            return;
        }
        if let Some(mut parsed) = self.world.borrow().document_mut(element) {
            parsed.document.mark_script_started(element.node);
        }
        if let crate::js::Script::Module(source) = script {
            // The graph runs now; the element's `load` or `error` is an element
            // task, so it lands after the current script's turn ends
            // (<https://html.spec.whatwg.org/multipage/scripting.html#attr-script-type-module>).
            match self.run_module_body(element, source) {
                None => self.tasks.push_back(Task::NodeError(element)),
                Some(result) => {
                    if result.is_err() {
                        self.record_event(RendererEvent::ScriptFailed);
                        self.tasks.push_back(Task::NodeError(element));
                    } else {
                        self.tasks.push_back(Task::NodeLoad(element));
                    }
                    self.adopt_js_work();
                }
            }
            return;
        }
        let crate::js::Script::Classic(crate::js::ScriptSource::Src(src)) = script else {
            return;
        };
        let Ok(url) = self.resolve_dial_url(&src) else {
            self.tasks.push_back(Task::NodeError(element));
            return;
        };
        let violations = self.script_url_violations(&url, element);
        if !violations.is_empty() {
            self.fire_csp_violations(element, &violations);
        }
        if crate::csp::blocks(&violations) {
            self.tasks.push_back(Task::NodeError(element));
            return;
        }
        let options = self.world.borrow().subresource_options(element);
        let mut dial = QueuedDial::new(
            DialContext::ClassicScript { element },
            crate::protocol::RequestPolicy::ClassicScript {
                crossorigin: crate::protocol::cross_origin(options.crossorigin),
            },
            &url,
            &self.dial_initiator(),
        );
        dial.req.referrer = crate::protocol::ReferrerRequest::Client {
            url: self.referrer_url(),
        };
        dial.req.referrer_policy = options
            .referrer_policy
            .map(crate::protocol::referrer_policy)
            .or(self.world.borrow().policy.referrer_policy)
            .unwrap_or_default();
        dial.req.csp = Some(Box::new(
            self.redirect_gate(crate::csp::Destination::Script),
        ));
        dial.integrity = self.script_integrity(element);
        // A script-inserted `src` script is async, but it still belongs to
        // the set of scripts that will execute as soon as possible, and
        // `load` waits for that set to drain
        // (<https://html.spec.whatwg.org/multipage/parsing.html#the-end>).
        self.queue_dial(dial);
    }

    /// Runs the module graph of one module script element. The element itself
    /// gets `load` or `error`, so the caller decides when that task runs: a
    /// deferred parser module fires it in the same turn the graph finishes,
    /// and a script-inserted one that prepared after parsing queues the task
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    ///
    /// `None` means CSP blocked the script before its fetch: the caller fires
    /// `error` without running anything
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    pub(super) fn run_module_body(
        &mut self,
        element: crate::dom::tree::NodeId,
        source: crate::js::ScriptSource,
    ) -> Option<Result<(), crate::js::JsError>> {
        let result = match source {
            crate::js::ScriptSource::Inline { source, .. } => {
                let violations = self.inline_script_violations(&source, element);
                if !violations.is_empty() {
                    self.fire_csp_violations(element, &violations);
                }
                if crate::csp::blocks(&violations) {
                    return None;
                }
                let name = format!("{}#inline-module-{element:?}", self.url);
                self.with_module_root(Some(element), |this| {
                    this.js
                        .as_ref()
                        .map_or(Ok(()), |js| js.eval_inline_module(&name, &source))
                })
            }
            crate::js::ScriptSource::Src(src) => match self.resolve_dial_url(&src) {
                Ok(url) => {
                    let violations = self.script_url_violations(&url, element);
                    if !violations.is_empty() {
                        self.fire_csp_violations(element, &violations);
                    }
                    if crate::csp::blocks(&violations) {
                        return None;
                    }
                    self.with_module_root(Some(element), |this| {
                        this.js
                            .as_ref()
                            .map_or(Ok(()), |js| js.eval_external_module(url.as_str()))
                    })
                }
                Err(_) => Err(crate::js::JsError::Engine(
                    "invalid module script URL".into(),
                )),
            },
        };
        Some(result)
    }

    /// Hands off the `keepalive` requests this document queued but never
    /// started, then starts them. A `keepalive` fetch must outlive the
    /// environment that made it, which is the whole point of the flag: pages
    /// report on teardown from an `unload` handler, when the document's next
    /// turn will never run
    /// (<https://fetch.spec.whatwg.org/#keepalive-flag>). Every other queued
    /// request dies with the document, as aborting it requires.
    pub(crate) fn flush_keepalive_fetches(&mut self) {
        let pending = {
            let world = self.world.borrow();
            world.take_queued_keepalive_fetches()
        };
        for request in pending {
            if let RealmRequest::Fetch(fetch) = request {
                self.queue_js_fetch(*fetch);
            }
        }
        let services = &*self.services;
        let shared = self.world.borrow().runtime.shared.clone();
        for entry in self.dials.take_queued_keepalive() {
            // Nothing will read this dial's completion, but the request is
            // still running, so the count keeps the engine from reporting
            // itself idle before the network answers
            // (<https://fetch.spec.whatwg.org/#keepalive-flag>).
            shared.begin_keepalive_fetch();
            let settle = shared.clone();
            self.dials
                .start(entry.dial, entry.delay, &|request, completion| {
                    let settle = settle.clone();
                    services.start_dial(
                        request,
                        Box::new(move |outcome| {
                            settle.end_keepalive_fetch();
                            completion(outcome);
                        }),
                    )
                });
        }
    }

    fn fail_js_fetch(&mut self, js_id: JsFetchId) {
        self.record_event(RendererEvent::FetchFailed);
        self.settle_js_fetch(js_id, None);
    }

    /// Queues one page `fetch` / XHR dial. The connect check is here, where
    /// the request leaves the renderer
    /// (<https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy>).
    fn queue_js_fetch(&mut self, fetch: crate::js::JsFetchRequest) {
        if self.dials.pending_js_fetches() >= MAX_PENDING_JS_FETCHES {
            self.fail_js_fetch(fetch.js_id);
            return;
        }
        let Ok(url) = self.resolve_dial_url(&fetch.url) else {
            self.fail_js_fetch(fetch.js_id);
            return;
        };
        let violations = self.subresource_violations(&url, crate::csp::Destination::Connect);
        if !violations.is_empty()
            && let Some(target) = self.document_node()
        {
            self.fire_csp_violations(target, &violations);
        }
        if crate::csp::blocks(&violations) {
            self.fail_js_fetch(fetch.js_id);
            return;
        }
        let mut dial = QueuedDial::new(
            DialContext::JsFetch { id: fetch.js_id },
            crate::protocol::RequestPolicy::Fetch(fetch.options.cors),
            &url,
            &self.dial_initiator(),
        );
        dial.req.method = fetch.method;
        dial.req.body = fetch.body;
        dial.req.content_type = fetch.content_type;
        dial.req.headers = fetch.headers;
        // The dial is built after the request was queued, and a same-turn
        // `history.replaceState` updates the client URL in between, so resolve
        // the client branch against the document as it is now
        // (<https://html.spec.whatwg.org/multipage/history.html#dom-history-replacestate>).
        dial.req.referrer = match fetch.options.referrer {
            crate::protocol::ReferrerRequest::Client { .. } => {
                crate::protocol::ReferrerRequest::Client {
                    url: self.referrer_url(),
                }
            }
            referrer => referrer,
        };
        // An unset request policy means the client's policy container
        // decides: resolve it here so every redirect hop re-determines with
        // the same policy the initial request used
        // (<https://fetch.spec.whatwg.org/#main-fetch>,
        // <https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
        dial.req.referrer_policy = fetch
            .options
            .referrer_policy
            .or(self.world.borrow().policy.referrer_policy)
            .unwrap_or_default();
        dial.req.keepalive = fetch.options.keepalive;
        dial.req.body_len = fetch.options.body_len;
        dial.req.integrity = fetch.options.integrity;
        dial.req.cache = fetch.options.cache;
        dial.req.delivery = fetch.options.delivery;
        dial.req.csp = Some(Box::new(
            self.redirect_gate(crate::csp::Destination::Connect),
        ));
        self.queue_dial(dial);
    }

    fn due_timer(&mut self) -> Option<TimerId> {
        let now = Instant::now();
        // Ties at the same deadline run in creation order, so tasks queued
        // together keep the order they were queued in
        // (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps>,
        // <https://html.spec.whatwg.org/multipage/webappapis.html#task-queue>).
        let (id, timer) = self
            .timers
            .iter_mut()
            .filter(|(_, timer)| !timer.fired && timer.when.is_some_and(|when| when <= now))
            .min_by_key(|(id, timer)| (timer.when, id.get()))?;
        timer.fired = true;
        Some(*id)
    }
}
