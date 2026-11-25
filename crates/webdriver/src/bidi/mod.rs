// https://w3c.github.io/webdriver-bidi/

pub(crate) mod browser_mod;
pub(crate) mod browsing_context;
pub(crate) mod emulation;
pub(crate) mod input;
pub(crate) mod network;
pub(crate) mod protocol;
pub(crate) mod script;
pub(crate) mod session;
pub(crate) mod storage;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use browser::{
    BrowserHandle, BrowserLifecycleEvent, FrameId, NetworkEvent, NetworkRequestId, PromptEvent,
    TabEvent, TabId, UserContextId, now_millis,
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use server::ws::{WebSocket, WsMessage};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use crate::bidi::protocol::{BidiError, BidiResult, error_response, event_message, ok_response};
use crate::session::{Sessions, format_context_id};

pub(crate) const BIDI_CHANNEL_BINDING: &str = "__tbBidiChannelEmit";

enum SubscriptionScope {
    Global,
    Contexts(HashSet<TabId>),
    UserContexts(HashSet<UserContextId>),
}

struct SubscriptionEntry {
    id: String,
    events: HashSet<String>,
    scope: SubscriptionScope,
}

#[derive(Default)]
pub(crate) struct SubscriptionSet {
    entries: Vec<SubscriptionEntry>,
}

impl SubscriptionSet {
    pub(crate) fn subscribe(
        &mut self,
        id: String,
        events: &[String],
        contexts: Option<&[TabId]>,
        user_contexts: Option<&[UserContextId]>,
    ) {
        let expanded: HashSet<String> = expand_events(events).into_iter().collect();
        let scope = if let Some(contexts) = contexts {
            SubscriptionScope::Contexts(contexts.iter().copied().collect())
        } else if let Some(user_contexts) = user_contexts {
            SubscriptionScope::UserContexts(user_contexts.iter().cloned().collect())
        } else {
            SubscriptionScope::Global
        };
        self.entries.push(SubscriptionEntry {
            id,
            events: expanded,
            scope,
        });
    }

    pub(crate) fn unsubscribe_by_ids(&mut self, ids: &[String]) -> Result<(), BidiError> {
        for id in ids {
            if !self.entries.iter().any(|entry| entry.id == *id) {
                return Err(BidiError::invalid_argument(format!(
                    "unknown subscription id: {id}"
                )));
            }
        }
        let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        self.entries
            .retain(|entry| !id_set.contains(entry.id.as_str()));
        Ok(())
    }

    pub(crate) fn unsubscribe_by_events(
        &mut self,
        events: &[String],
        contexts: Option<&[TabId]>,
    ) -> Result<(), BidiError> {
        let expanded = expand_events(events);
        if let Some(contexts) = contexts {
            for &tab in contexts {
                for event in &expanded {
                    let exists = self.entries.iter().any(|entry| {
                        entry.events.contains(event)
                            && matches!(&entry.scope, SubscriptionScope::Contexts(set) if set.contains(&tab))
                    });
                    if !exists {
                        return Err(BidiError::invalid_argument(format!(
                            "not subscribed to {event}"
                        )));
                    }
                }
            }
            for &tab in contexts {
                let mut split_entries = Vec::new();
                for entry in &mut self.entries {
                    if let SubscriptionScope::Contexts(ref mut set) = entry.scope
                        && set.contains(&tab)
                    {
                        let removed: Vec<String> = expanded
                            .iter()
                            .filter(|ev| entry.events.contains(*ev))
                            .cloned()
                            .collect();
                        if !removed.is_empty() {
                            if set.len() == 1 {
                                for ev in &removed {
                                    entry.events.remove(ev);
                                }
                            } else {
                                set.remove(&tab);
                                let remaining_events: HashSet<String> = entry
                                    .events
                                    .iter()
                                    .filter(|ev| !removed.contains(*ev))
                                    .cloned()
                                    .collect();
                                if !remaining_events.is_empty() {
                                    let mut single = HashSet::new();
                                    single.insert(tab);
                                    split_entries.push(SubscriptionEntry {
                                        id: entry.id.clone(),
                                        events: remaining_events,
                                        scope: SubscriptionScope::Contexts(single),
                                    });
                                }
                            }
                        }
                    }
                }
                self.entries.extend(split_entries);
            }
        } else {
            for event in &expanded {
                let exists = self.entries.iter().any(|entry| {
                    entry.events.contains(event) && matches!(entry.scope, SubscriptionScope::Global)
                });
                if !exists {
                    return Err(BidiError::invalid_argument(format!(
                        "not subscribed to {event}"
                    )));
                }
            }
            for entry in &mut self.entries {
                if matches!(entry.scope, SubscriptionScope::Global) {
                    for event in &expanded {
                        entry.events.remove(event);
                    }
                }
            }
        }
        self.entries.retain(|entry| !entry.events.is_empty());
        Ok(())
    }

    #[must_use]
    pub(crate) fn matches(
        &self,
        event: &str,
        tab: Option<TabId>,
        user_context: Option<&UserContextId>,
    ) -> bool {
        self.entries.iter().any(|entry| {
            if !entry.events.contains(event) {
                return false;
            }
            match &entry.scope {
                SubscriptionScope::Global => true,
                SubscriptionScope::Contexts(set) => tab.is_some_and(|t| set.contains(&t)),
                SubscriptionScope::UserContexts(set) => {
                    user_context.is_some_and(|uc| set.contains(uc))
                }
            }
        })
    }
}

pub(crate) fn expand_events(events: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in events {
        match name.as_str() {
            "browsingContext" => {
                for ev in [
                    "browsingContext.contextCreated",
                    "browsingContext.contextDestroyed",
                    "browsingContext.navigationStarted",
                    "browsingContext.fragmentNavigated",
                    "browsingContext.domContentLoaded",
                    "browsingContext.load",
                    "browsingContext.navigationAborted",
                    "browsingContext.navigationFailed",
                    "browsingContext.navigationCommitted",
                    "browsingContext.userPromptOpened",
                    "browsingContext.userPromptClosed",
                ] {
                    out.push(ev.to_owned());
                }
            }
            "network" => {
                for ev in [
                    "network.beforeRequestSent",
                    "network.responseStarted",
                    "network.responseCompleted",
                    "network.fetchError",
                    "network.authRequired",
                ] {
                    out.push(ev.to_owned());
                }
            }
            "log" => out.push("log.entryAdded".to_owned()),
            "script" => {
                for ev in [
                    "script.message",
                    "script.realmCreated",
                    "script.realmDestroyed",
                ] {
                    out.push(ev.to_owned());
                }
            }
            "input" => out.push("input.fileDialogOpened".to_owned()),
            other => out.push(other.to_owned()),
        }
    }
    out
}

pub(crate) fn is_known_event_or_module(name: &str) -> bool {
    matches!(
        name,
        "browsingContext"
            | "browsingContext.contextCreated"
            | "browsingContext.contextDestroyed"
            | "browsingContext.navigationStarted"
            | "browsingContext.fragmentNavigated"
            | "browsingContext.domContentLoaded"
            | "browsingContext.load"
            | "browsingContext.navigationAborted"
            | "browsingContext.navigationFailed"
            | "browsingContext.navigationCommitted"
            | "browsingContext.userPromptOpened"
            | "browsingContext.userPromptClosed"
            | "browsingContext.historyUpdated"
            | "browsingContext.downloadWillBegin"
            | "browsingContext.downloadEnd"
            | "network"
            | "network.beforeRequestSent"
            | "network.responseStarted"
            | "network.responseCompleted"
            | "network.fetchError"
            | "network.authRequired"
            | "log"
            | "log.entryAdded"
            | "script"
            | "script.message"
            | "script.realmCreated"
            | "script.realmDestroyed"
            | "input"
            | "input.fileDialogOpened"
    )
}

pub(crate) struct DataCollector {
    pub(crate) data_types: Vec<String>,
    pub(crate) max_encoded_data_size: u64,
    pub(crate) disowned: HashSet<(String, NetworkRequestId)>,
}

pub(crate) struct BufferedLogEntry {
    pub(crate) tab: TabId,
    pub(crate) user_context: UserContextId,
    pub(crate) params: Value,
}

pub(crate) struct BidiState {
    pub(crate) sessions: Arc<Mutex<Sessions>>,
    pub(crate) browser: BrowserHandle,
    pub(crate) session_id: Mutex<Option<String>>,
    pub(crate) subscriptions: Arc<Mutex<SubscriptionSet>>,
    pub(crate) out_tx: mpsc::UnboundedSender<Value>,
    pub(crate) attached_tabs: Arc<Mutex<HashMap<TabId, JoinHandle<()>>>>,
    pub(crate) attaching_tabs: Arc<Mutex<HashSet<TabId>>>,
    pub(crate) tab_user_contexts: Arc<Mutex<HashMap<TabId, UserContextId>>>,
    pub(crate) script_disabled: Mutex<HashMap<TabId, bool>>,
    pub(crate) log_buffer: Arc<Mutex<VecDeque<BufferedLogEntry>>>,
    pub(crate) data_collectors: Arc<Mutex<HashMap<String, DataCollector>>>,
    pub(crate) intercepts: std::sync::Mutex<Vec<browser::InterceptId>>,
}

impl BidiState {
    pub(crate) async fn emit(&self, method: &str, tab: Option<TabId>, params: Value) -> bool {
        let uc = if let Some(t) = tab {
            self.tab_user_contexts.lock().await.get(&t).cloned()
        } else {
            None
        };
        let subscribed = self
            .subscriptions
            .lock()
            .await
            .matches(method, tab, uc.as_ref());
        if subscribed {
            let _ = self.out_tx.send(event_message(method, &params));
        }
        subscribed
    }

    pub(crate) async fn emit_or_buffer_log(
        &self,
        tab: TabId,
        user_context: UserContextId,
        params: Value,
    ) {
        if !self.emit("log.entryAdded", Some(tab), params.clone()).await {
            let mut buf = self.log_buffer.lock().await;
            if buf.len() >= 128 {
                buf.pop_front();
            }
            buf.push_back(BufferedLogEntry {
                tab,
                user_context,
                params,
            });
        }
    }

    pub(crate) async fn attach_tab(self: &Arc<Self>, tab_id: TabId) {
        {
            let attached = self.attached_tabs.lock().await;
            if attached.contains_key(&tab_id) {
                return;
            }
            let mut attaching = self.attaching_tabs.lock().await;
            if !attaching.insert(tab_id) {
                return;
            }
        }
        let Ok(tab) = self.browser.tab(tab_id).await else {
            self.attaching_tabs.lock().await.remove(&tab_id);
            return;
        };
        let uc = tab.user_context().clone();
        self.tab_user_contexts.lock().await.insert(tab_id, uc);
        let Ok(mut rx) = tab.subscribe() else {
            self.attaching_tabs.lock().await.remove(&tab_id);
            return;
        };
        let state = Arc::clone(self);
        let handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                forward_tab_event(&state, tab_id, &tab, event).await;
            }
            state.attached_tabs.lock().await.remove(&tab_id);
        });
        self.attached_tabs.lock().await.insert(tab_id, handle);
        self.attaching_tabs.lock().await.remove(&tab_id);
    }

    pub(crate) async fn attach_all_existing_tabs(self: &Arc<Self>) {
        if let Ok(ids) = self.browser.tabs().await {
            for id in ids {
                self.attach_tab(id).await;
            }
        }
    }
}

fn spawn_lifecycle_forwarder(
    state: Arc<BidiState>,
    mut rx: tokio::sync::broadcast::Receiver<BrowserLifecycleEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                BrowserLifecycleEvent::TabCreated {
                    tab,
                    user_context,
                    opener,
                } => {
                    state
                        .tab_user_contexts
                        .lock()
                        .await
                        .insert(tab, user_context.clone());
                    state.attach_tab(tab).await;
                    let info = json!({
                        "context": format_context_id(tab, FrameId::MAIN),
                        "url": "about:blank",
                        "children": null,
                        "parent": null,
                        "userContext": user_context.as_str(),
                        "originalOpener": opener.map(|id| format_context_id(id, FrameId::MAIN)),
                        "clientWindow": format_context_id(tab, FrameId::MAIN),
                    });
                    state
                        .emit("browsingContext.contextCreated", Some(tab), info)
                        .await;
                }
                BrowserLifecycleEvent::TabClosed { tab, user_context } => {
                    let info = json!({
                        "context": format_context_id(tab, FrameId::MAIN),
                        "url": "about:blank",
                        "children": [],
                        "parent": null,
                        "userContext": user_context.as_str(),
                        "originalOpener": null,
                        "clientWindow": format_context_id(tab, FrameId::MAIN),
                    });
                    state
                        .emit("browsingContext.contextDestroyed", Some(tab), info)
                        .await;
                    state.tab_user_contexts.lock().await.remove(&tab);
                    if let Some(handle) = state.attached_tabs.lock().await.remove(&tab) {
                        handle.abort();
                    }
                }
            }
        }
    })
}

pub(crate) async fn serve_websocket(
    socket: WebSocket,
    sessions: Arc<Mutex<Sessions>>,
    initial_session: Option<String>,
) {
    let browser = sessions.lock().await.browser.clone();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let state = Arc::new(BidiState {
        sessions,
        browser: browser.clone(),
        session_id: Mutex::new(initial_session),
        subscriptions: Arc::new(Mutex::new(SubscriptionSet::default())),
        out_tx,
        attached_tabs: Arc::new(Mutex::new(HashMap::new())),
        attaching_tabs: Arc::new(Mutex::new(HashSet::new())),
        tab_user_contexts: Arc::new(Mutex::new(HashMap::new())),
        script_disabled: Mutex::new(HashMap::new()),
        log_buffer: Arc::new(Mutex::new(VecDeque::new())),
        data_collectors: Arc::new(Mutex::new(HashMap::new())),
        intercepts: std::sync::Mutex::new(Vec::new()),
    });
    state.attach_all_existing_tabs().await;

    let lifecycle_task = spawn_lifecycle_forwarder(Arc::clone(&state), browser.subscribe());
    let mut network_rx = browser.network_broker().subscribe();
    let mut prompt_rx = browser.prompt_broker().subscribe();

    let network_state = Arc::clone(&state);
    let network_task = tokio::spawn(async move {
        while let Ok(event) = network_rx.recv().await {
            forward_network_event(&network_state, event).await;
        }
    });

    let prompt_state = Arc::clone(&state);
    let prompt_task = tokio::spawn(async move {
        while let Ok(event) = prompt_rx.recv().await {
            forward_prompt_event(&prompt_state, event).await;
        }
    });

    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink
                .send(WsMessage::Text(msg.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(incoming) = stream.next().await {
        let Ok(message) = incoming else {
            break;
        };
        let text = match message {
            WsMessage::Text(text) => text.to_string(),
            WsMessage::Close(_) => break,
            WsMessage::Ping(_)
            | WsMessage::Pong(_)
            | WsMessage::Binary(_)
            | WsMessage::Frame(_) => {
                continue;
            }
        };
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let reply = handle_message(&state_clone, &text).await;
            let _ = state_clone.out_tx.send(reply);
        });
    }

    lifecycle_task.abort();
    network_task.abort();
    prompt_task.abort();
    writer.abort();
    state.clear_intercepts();
    let attached = std::mem::take(&mut *state.attached_tabs.lock().await);
    for (_, handle) in attached {
        handle.abort();
    }
}

impl BidiState {
    pub(crate) fn clear_intercepts(&self) {
        let ids: Vec<browser::InterceptId> = self
            .intercepts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect();
        let broker = self.browser.network_broker();
        for id in ids {
            broker.remove_intercept(id);
        }
    }
}

async fn handle_message(state: &Arc<BidiState>, raw: &str) -> Value {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(err) => {
            return error_response(None, &BidiError::invalid_argument(err.to_string()));
        }
    };
    let Some(obj) = parsed.as_object() else {
        return error_response(
            None,
            &BidiError::invalid_argument("command must be a JSON object"),
        );
    };
    let Some(id) = obj.get("id").and_then(Value::as_u64) else {
        return error_response(
            None,
            &BidiError::invalid_argument("command id must be a non-negative integer"),
        );
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return error_response(
            Some(id),
            &BidiError::invalid_argument("command method must be a string"),
        );
    };
    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return error_response(
            Some(id),
            &BidiError::invalid_argument("command params must be an object"),
        );
    }
    match dispatch_command(state, method, &params).await {
        Ok(result) => ok_response(id, &result),
        Err(err) => error_response(Some(id), &err),
    }
}

async fn dispatch_command(state: &Arc<BidiState>, method: &str, params: &Value) -> BidiResult {
    match method {
        // https://w3c.github.io/webdriver-bidi/#module-session
        "session.status" => session::status(state).await,
        "session.new" => session::new(state, params).await,
        "session.end" => session::end(state).await,
        "session.subscribe" => session::subscribe(state, params).await,
        "session.unsubscribe" => session::unsubscribe(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-browser
        "browser.close" => browser_mod::close(state).await,
        "browser.createUserContext" => browser_mod::create_user_context(state, params).await,
        "browser.getUserContexts" => browser_mod::get_user_contexts(state).await,
        "browser.removeUserContext" => browser_mod::remove_user_context(state, params).await,
        "browser.getClientWindows" => browser_mod::get_client_windows(state).await,
        "browser.setClientWindowState" => browser_mod::set_client_window_state(state, params).await,
        "browser.setDownloadBehavior" => browser_mod::set_download_behavior(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-browsingContext
        "browsingContext.create" => browsing_context::create(state, params).await,
        "browsingContext.close" => browsing_context::close(state, params).await,
        "browsingContext.getTree" => browsing_context::get_tree(state, params).await,
        "browsingContext.navigate" => browsing_context::navigate(state, params).await,
        "browsingContext.reload" => browsing_context::reload(state, params).await,
        "browsingContext.traverseHistory" => {
            browsing_context::traverse_history(state, params).await
        }
        "browsingContext.activate" => browsing_context::activate(state, params).await,
        "browsingContext.setViewport" => browsing_context::set_viewport(state, params).await,
        "browsingContext.captureScreenshot" => {
            browsing_context::capture_screenshot(state, params).await
        }
        "browsingContext.locateNodes" => browsing_context::locate_nodes(state, params).await,
        "browsingContext.handleUserPrompt" => {
            browsing_context::handle_user_prompt(state, params).await
        }
        "browsingContext.setBypassCSP" => browsing_context::set_bypass_csp(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-script
        "script.evaluate" => script::evaluate(state, params).await,
        "script.callFunction" => script::call_function(state, params).await,
        "script.addPreloadScript" => script::add_preload_script(state, params).await,
        "script.removePreloadScript" => script::remove_preload_script(state, params).await,
        "script.disown" => script::disown(state, params).await,
        "script.getRealms" => script::get_realms(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-network
        "network.addIntercept" => network::add_intercept(state, params).await,
        "network.removeIntercept" => network::remove_intercept(state, params),
        "network.continueRequest" => network::continue_request(state, params),
        "network.continueResponse" => network::continue_response(state, params),
        "network.provideResponse" => network::provide_response(state, params),
        "network.failRequest" => network::fail_request(state, params),
        "network.setCacheBehavior" => network::set_cache_behavior(state, params).await,
        "network.setExtraHeaders" => network::set_extra_headers(state, params).await,
        "network.addDataCollector" => network::add_data_collector(state, params).await,
        "network.removeDataCollector" => network::remove_data_collector(state, params).await,
        "network.disownData" => network::disown_data(state, params).await,
        "network.getData" => network::get_data(state, params).await,

        // https://w3c.github.io/permissions/#webdriver-bidi
        "permissions.setPermission" => emulation::set_permission(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-storage
        "storage.getCookies" => storage::get_cookies(state, params).await,
        "storage.setCookie" => storage::set_cookie(state, params).await,
        "storage.deleteCookies" => storage::delete_cookies(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-input
        "input.performActions" => input::perform_actions(state, params).await,
        "input.releaseActions" => input::release_actions(state, params).await,
        "input.setFiles" => input::set_files(state, params).await,

        // https://w3c.github.io/webdriver-bidi/#module-emulation
        "emulation.setUserAgentOverride" => emulation::set_user_agent_override(state, params).await,
        "emulation.setScriptingEnabled" => emulation::set_scripting_enabled(state, params).await,
        "emulation.setTimezoneOverride" => emulation::set_timezone_override(state, params).await,
        "emulation.setLocaleOverride" => emulation::set_locale_override(state, params).await,
        "emulation.setGeolocationOverride" => {
            emulation::set_geolocation_override(state, params).await
        }
        "emulation.setForcedColorsModeThemeOverride" => {
            emulation::set_forced_colors_mode_theme_override(state, params).await
        }
        "emulation.setScreenOrientationOverride" => {
            emulation::set_screen_orientation_override(state, params).await
        }

        other => Err(BidiError::unknown_command(format!(
            "unknown command: {other}"
        ))),
    }
}

async fn emit_nav_event(
    state: &Arc<BidiState>,
    method: &str,
    tab_id: TabId,
    context: &str,
    url: &str,
    timestamp: u64,
) {
    state
        .emit(
            method,
            Some(tab_id),
            json!({
                "context": context,
                "navigation": null,
                "timestamp": timestamp,
                "url": url,
            }),
        )
        .await;
}

async fn forward_console_event(
    state: &Arc<BidiState>,
    tab_id: TabId,
    tab: &browser::TabHandle,
    main_ctx: &str,
    level: browser::ConsoleLevel,
    args: &str,
    ts: u64,
) {
    let (bidi_level, method) = console_level_mapping(level);
    let parsed_args: Vec<Value> = serde_json::from_str::<Vec<Value>>(args)
        .unwrap_or_default()
        .iter()
        .map(cdp_remote_to_bidi_remote)
        .collect();
    let text = parsed_args
        .iter()
        .map(bidi_remote_text)
        .collect::<Vec<_>>()
        .join(" ");
    let uc = tab.user_context().clone();
    state
        .emit_or_buffer_log(
            tab_id,
            uc.clone(),
            json!({
                "type": "console",
                "level": bidi_level,
                "source": {
                    "realm": format!("realm-{main_ctx}"),
                    "context": main_ctx,
                    "userContext": uc.as_str(),
                },
                "text": text,
                "timestamp": ts,
                "method": method,
                "args": parsed_args,
            }),
        )
        .await;
}

async fn forward_channel_binding(
    state: &Arc<BidiState>,
    tab_id: TabId,
    main_ctx: &str,
    payload: &str,
) {
    if let Ok(val) = serde_json::from_str::<Value>(payload) {
        let channel = val
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = val
            .get("data")
            .cloned()
            .unwrap_or(json!({"type": "undefined"}));
        let realm = val.get("realm").and_then(Value::as_str).unwrap_or_default();
        let ctx = val
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or(main_ctx);
        state
            .emit(
                "script.message",
                Some(tab_id),
                json!({
                    "channel": channel,
                    "data": data,
                    "source": {
                        "realm": realm,
                        "context": ctx,
                    }
                }),
            )
            .await;
    }
}

async fn emit_frame_lifecycle(
    state: &Arc<BidiState>,
    method: &str,
    tab_id: TabId,
    frame: FrameId,
    parent: Option<FrameId>,
    url: &str,
    user_context: &str,
) {
    let main_ctx = format_context_id(tab_id, FrameId::MAIN);
    let parent_ctx = parent.map_or_else(|| main_ctx.clone(), |p| format_context_id(tab_id, p));
    let children = if parent.is_some() {
        Value::Null
    } else {
        json!([])
    };
    let info = json!({
        "context": format_context_id(tab_id, frame),
        "url": url,
        "children": children,
        "parent": parent_ctx,
        "userContext": user_context,
        "originalOpener": null,
        "clientWindow": main_ctx,
    });
    state.emit(method, Some(tab_id), info).await;
}

async fn forward_exception_event(
    state: &Arc<BidiState>,
    tab_id: TabId,
    tab: &browser::TabHandle,
    event: TabEvent,
    ts: u64,
) {
    let TabEvent::ExceptionThrown {
        frame,
        text,
        filename,
        line,
        column,
    } = event
    else {
        return;
    };
    let ctx = format_context_id(tab_id, frame);
    let clean_text = text.strip_prefix("Uncaught ").unwrap_or(&text);
    let uc = tab.user_context().clone();
    state
        .emit_or_buffer_log(
            tab_id,
            uc.clone(),
            json!({
                "type": "javascript",
                "level": "error",
                "source": {
                    "realm": format!("realm-{ctx}"),
                    "context": ctx,
                    "userContext": uc.as_str(),
                },
                "text": clean_text,
                "timestamp": ts,
                "stackTrace": {
                    "callFrames": [{
                        "functionName": "",
                        "url": filename,
                        "lineNumber": line,
                        "columnNumber": column,
                    }]
                }
            }),
        )
        .await;
}

async fn forward_tab_event(
    state: &Arc<BidiState>,
    tab_id: TabId,
    tab: &browser::TabHandle,
    event: TabEvent,
) {
    let ts = now_millis();
    let main_ctx = format_context_id(tab_id, FrameId::MAIN);
    match event {
        TabEvent::NavigationRequested { url } => {
            emit_nav_event(
                state,
                "browsingContext.navigationStarted",
                tab_id,
                &main_ctx,
                &url,
                ts,
            )
            .await;
        }
        TabEvent::Navigated
        | TabEvent::SameDocumentNavigation
        | TabEvent::DomContentLoaded
        | TabEvent::Load
        | TabEvent::NavigationFailed(_) => {
            let method = match event {
                TabEvent::Navigated => "browsingContext.navigationCommitted",
                TabEvent::SameDocumentNavigation => "browsingContext.fragmentNavigated",
                TabEvent::DomContentLoaded => "browsingContext.domContentLoaded",
                TabEvent::Load => "browsingContext.load",
                _ => "browsingContext.navigationFailed",
            };
            let url = tab.document_url().await.unwrap_or_default();
            emit_nav_event(state, method, tab_id, &main_ctx, &url, ts).await;
        }
        TabEvent::FrameAttached { frame, parent, url } => {
            emit_frame_lifecycle(
                state,
                "browsingContext.contextCreated",
                tab_id,
                frame,
                Some(parent),
                &url,
                tab.user_context().as_str(),
            )
            .await;
        }
        TabEvent::FrameDetached { frame } => {
            emit_frame_lifecycle(
                state,
                "browsingContext.contextDestroyed",
                tab_id,
                frame,
                None,
                "about:blank",
                tab.user_context().as_str(),
            )
            .await;
        }
        TabEvent::FrameNavigated { frame, url, .. } => {
            let ctx = format_context_id(tab_id, frame);
            emit_nav_event(
                state,
                "browsingContext.navigationCommitted",
                tab_id,
                &ctx,
                &url,
                ts,
            )
            .await;
        }
        TabEvent::FrameDomContentLoaded { frame, url } => {
            let ctx = format_context_id(tab_id, frame);
            emit_nav_event(
                state,
                "browsingContext.domContentLoaded",
                tab_id,
                &ctx,
                &url,
                ts,
            )
            .await;
        }
        TabEvent::FrameLoad { frame, url } => {
            let ctx = format_context_id(tab_id, frame);
            emit_nav_event(state, "browsingContext.load", tab_id, &ctx, &url, ts).await;
        }
        TabEvent::Console { level, args } => {
            forward_console_event(state, tab_id, tab, &main_ctx, level, &args, ts).await;
        }
        event @ TabEvent::ExceptionThrown { .. } => {
            forward_exception_event(state, tab_id, tab, event, ts).await;
        }
        TabEvent::Binding { name, payload } if name == BIDI_CHANNEL_BINDING => {
            forward_channel_binding(state, tab_id, &main_ctx, &payload).await;
        }
        TabEvent::NavigationResponded { .. }
        | TabEvent::RealmReplaced
        | TabEvent::Binding { .. } => {}
    }
}

fn bidi_request_data(
    request_id: NetworkRequestId,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    is_navigation: bool,
    timestamp_ms: u64,
) -> Value {
    json!({
        "request": request_id.to_string(),
        "url": url,
        "method": method,
        "headers": headers_to_bidi(headers),
        "cookies": [],
        "headersSize": headers_byte_size(headers),
        "bodySize": 0,
        "destination": if is_navigation { "document" } else { "" },
        "initiatorType": null,
        "timings": empty_timings(timestamp_ms),
    })
}

fn attach_intercepts(params: &mut Value, intercepts: &[browser::InterceptId]) {
    if !intercepts.is_empty()
        && let Some(obj) = params.as_object_mut()
    {
        obj.insert(
            "intercepts".into(),
            json!(
                intercepts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
        );
    }
}

fn nav_value(is_navigation: bool, request_id: NetworkRequestId) -> Value {
    if is_navigation {
        Value::String(format!("nav-{request_id}"))
    } else {
        Value::Null
    }
}

fn bidi_response_data(
    url: &str,
    status: u16,
    status_text: &str,
    from_cache: bool,
    headers: &[(String, String)],
    mime_type: &str,
    body_len: usize,
) -> Value {
    json!({
        "url": url,
        "protocol": "http/1.1",
        "status": status,
        "statusText": status_text,
        "fromCache": from_cache,
        "headers": headers_to_bidi(headers),
        "mimeType": mime_type,
        "bytesReceived": body_len,
        "headersSize": headers_byte_size(headers),
        "bodySize": body_len,
        "content": { "size": body_len },
    })
}

async fn forward_before_request_sent(state: &Arc<BidiState>, event: NetworkEvent) {
    let NetworkEvent::BeforeRequestSent {
        request_id,
        tab,
        frame,
        is_navigation,
        url,
        method,
        headers,
        timestamp_ms,
        is_blocked,
        intercepts,
        ..
    } = event
    else {
        return;
    };
    let mut params = json!({
        "context": tab.map(|t| format_context_id(t, frame)),
        "isBlocked": is_blocked,
        "navigation": nav_value(is_navigation, request_id),
        "redirectCount": 0,
        "request": bidi_request_data(request_id, &url, &method, &headers, is_navigation, timestamp_ms),
        "timestamp": timestamp_ms,
        "initiator": { "type": "other" },
    });
    attach_intercepts(&mut params, &intercepts);
    state.emit("network.beforeRequestSent", tab, params).await;
}

async fn forward_network_event(state: &Arc<BidiState>, event: NetworkEvent) {
    match event {
        event @ NetworkEvent::BeforeRequestSent { .. } => {
            forward_before_request_sent(state, event).await;
        }
        NetworkEvent::ResponseStarted {
            request_id,
            tab,
            frame,
            is_navigation,
            url,
            status,
            status_text,
            headers,
            mime_type,
            from_cache,
            timestamp_ms,
            is_blocked,
            intercepts,
        } => {
            let mut params = json!({
                "context": tab.map(|t| format_context_id(t, frame)),
                "isBlocked": is_blocked,
                "navigation": nav_value(is_navigation, request_id),
                "redirectCount": 0,
                "request": bidi_request_data(request_id, &url, "GET", &[], is_navigation, timestamp_ms),
                "response": bidi_response_data(&url, status, &status_text, from_cache, &headers, &mime_type, 0),
                "timestamp": timestamp_ms,
            });
            attach_intercepts(&mut params, &intercepts);
            state.emit("network.responseStarted", tab, params).await;
        }
        NetworkEvent::ResponseCompleted {
            request_id,
            tab,
            frame,
            is_navigation,
            url,
            status,
            status_text,
            headers,
            mime_type,
            from_cache,
            encoded_data_length,
            timestamp_ms,
        } => {
            let params = json!({
                "context": tab.map(|t| format_context_id(t, frame)),
                "isBlocked": false,
                "navigation": nav_value(is_navigation, request_id),
                "redirectCount": 0,
                "request": bidi_request_data(request_id, &url, "GET", &[], is_navigation, timestamp_ms),
                "response": bidi_response_data(&url, status, &status_text, from_cache, &headers, &mime_type, encoded_data_length),
                "timestamp": timestamp_ms,
            });
            state.emit("network.responseCompleted", tab, params).await;
        }
        NetworkEvent::FetchError {
            request_id,
            tab,
            frame,
            is_navigation,
            url,
            error_text,
            timestamp_ms,
        } => {
            let params = json!({
                "context": tab.map(|t| format_context_id(t, frame)),
                "isBlocked": false,
                "navigation": nav_value(is_navigation, request_id),
                "redirectCount": 0,
                "request": bidi_request_data(request_id, &url, "GET", &[], is_navigation, timestamp_ms),
                "timestamp": timestamp_ms,
                "errorText": error_text,
            });
            state.emit("network.fetchError", tab, params).await;
        }
    }
}

async fn forward_prompt_event(state: &Arc<BidiState>, event: PromptEvent) {
    match event {
        PromptEvent::Opened { info, handler } => {
            let mut params = json!({
                "context": format_context_id(info.tab, info.frame),
                "handler": handler.as_bidi_str(),
                "message": info.message,
                "type": info.kind.as_str(),
            });
            if let Some(default_val) = info.default_value
                && let Some(obj) = params.as_object_mut()
            {
                obj.insert("defaultValue".into(), json!(default_val));
            }
            state
                .emit("browsingContext.userPromptOpened", Some(info.tab), params)
                .await;
        }
        PromptEvent::Closed {
            tab,
            frame,
            kind,
            accepted,
            user_text,
        } => {
            let mut params = json!({
                "context": format_context_id(tab, frame),
                "accepted": accepted,
                "type": kind.as_str(),
            });
            if let Some(text) = user_text
                && let Some(obj) = params.as_object_mut()
            {
                obj.insert("userText".into(), json!(text));
            }
            state
                .emit("browsingContext.userPromptClosed", Some(tab), params)
                .await;
        }
    }
}

fn headers_to_bidi(headers: &[(String, String)]) -> Vec<Value> {
    headers
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name,
                "value": {
                    "type": "string",
                    "value": value,
                }
            })
        })
        .collect()
}

fn headers_byte_size(headers: &[(String, String)]) -> usize {
    headers.iter().map(|(k, v)| k.len() + v.len() + 4).sum()
}

fn empty_timings(timestamp_ms: u64) -> Value {
    json!({
        "timeOrigin": timestamp_ms,
        "requestTime": 0,
        "redirectStart": 0,
        "redirectEnd": 0,
        "fetchStart": 0,
        "dnsStart": 0,
        "dnsEnd": 0,
        "connectStart": 0,
        "connectEnd": 0,
        "tlsStart": 0,
        "requestStart": 0,
        "responseStart": 0,
        "responseEnd": 0,
    })
}

fn console_level_mapping(level: browser::ConsoleLevel) -> (&'static str, &'static str) {
    match level {
        browser::ConsoleLevel::Log => ("info", "log"),
        browser::ConsoleLevel::Debug => ("debug", "debug"),
        browser::ConsoleLevel::Info => ("info", "info"),
        browser::ConsoleLevel::Warning => ("warn", "warn"),
        browser::ConsoleLevel::Error => ("error", "error"),
        browser::ConsoleLevel::Assert => ("error", "assert"),
        browser::ConsoleLevel::Trace => ("debug", "trace"),
        browser::ConsoleLevel::Dir => ("info", "dir"),
        browser::ConsoleLevel::Table => ("info", "table"),
        browser::ConsoleLevel::StartGroup => ("info", "group"),
        browser::ConsoleLevel::StartGroupCollapsed => ("info", "groupCollapsed"),
        browser::ConsoleLevel::EndGroup => ("info", "groupEnd"),
    }
}

fn cdp_remote_to_bidi_remote(val: &Value) -> Value {
    let typ = val
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    match typ {
        "boolean" => {
            json!({"type": "boolean", "value": val.get("value").cloned().unwrap_or(json!(false))})
        }
        "number" => {
            if let Some(unser) = val.get("unserializableValue").and_then(Value::as_str) {
                json!({"type": "number", "value": unser})
            } else {
                json!({"type": "number", "value": val.get("value").cloned().unwrap_or(json!(0))})
            }
        }
        "string" => {
            json!({"type": "string", "value": val.get("value").cloned().unwrap_or(json!("")) })
        }
        "bigint" => {
            let s = val
                .get("unserializableValue")
                .and_then(Value::as_str)
                .unwrap_or("0")
                .trim_end_matches('n');
            json!({"type": "bigint", "value": s})
        }
        "symbol" => json!({"type": "symbol"}),
        "function" => json!({"type": "function"}),
        "object" => {
            if val.get("subtype").and_then(Value::as_str) == Some("null") {
                json!({"type": "null"})
            } else if val.get("subtype").and_then(Value::as_str) == Some("array") {
                json!({"type": "array"})
            } else {
                json!({"type": "object"})
            }
        }
        _ => json!({"type": "undefined"}),
    }
}

fn bidi_remote_text(val: &Value) -> String {
    match val.get("type").and_then(Value::as_str) {
        Some("string") => val
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        Some("number" | "boolean" | "bigint") => val.get("value").map_or_default(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        Some("null") => "null".to_owned(),
        Some("undefined") => "undefined".to_owned(),
        _ => val.to_string(),
    }
}
