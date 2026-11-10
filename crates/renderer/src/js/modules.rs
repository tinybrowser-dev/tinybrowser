use std::sync::mpsc;
use std::time::Duration;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error, Module, Result};
use url::Url;

use crate::protocol::DialRequest;

pub(super) struct WebModuleResolver;

impl Resolver for WebModuleResolver {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        // `QuickJS` labels code it compiles from a string `<input>`, so `eval`
        // and the engine's own shim have no script URL to resolve against.
        // The realm's base URL stands in, the way an indirect `eval` uses the
        // global's base URL
        // (<https://tc39.es/ecma262/#sec-hostresolveimportedmodule>).
        let base = match Url::parse(base) {
            Ok(base) => base,
            Err(_) => super::bindings::world(ctx)
                .map(|world| world.borrow().document_url.clone())
                .map_err(|_| Error::new_resolving(base, name))?,
        };
        let resolved = Url::parse(name)
            .or_else(|_| base.join(name))
            .map_err(|_| Error::new_resolving(base.as_str(), name))?;
        match resolved.scheme() {
            // The schemes this engine's module loader can fetch: `http`/
            // `https` over the network, and `blob`/`data` locally
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
            "http" | "https" | "blob" | "data" => Ok(resolved.into()),
            _ => Err(Error::new_resolving(base.as_str(), name)),
        }
    }
}

pub(super) struct WebModuleLoader;

impl Loader for WebModuleLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js, Declared>> {
        if attributes.is_some() {
            return Err(Error::new_loading(name));
        }
        load_module(ctx, name).map_err(|error| match error {
            // A module whose fetch failed reaches script as a TypeError, the
            // way Chromium reports it. `QuickJS`'s loader contract turns every
            // host `Loading` failure into a ReferenceError, so the exception is
            // built here instead
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
            Error::Loading { .. } => rquickjs::Exception::throw_type(
                ctx,
                &format!("Failed to fetch dynamically imported module: {name}"),
            ),
            other => other,
        })
    }
}

fn module_request_blocked(
    world_rc: &std::rc::Rc<std::cell::RefCell<super::World>>,
    ctx: &Ctx<'_>,
    name: &str,
    origin: &url::Origin,
    csp: &crate::csp::List,
    destination: crate::csp::Destination,
) -> Result<()> {
    let Ok(url) = url::Url::parse(name) else {
        return Ok(());
    };
    let violations = {
        let world = world_rc.borrow();
        csp.check_request(origin, &world.document_url, &url, destination, None)
    };
    if violations.is_empty() {
        return Ok(());
    }
    let blocked = crate::csp::blocks(&violations);
    if let Ok(target) = super::bindings::main_document(ctx) {
        // The module loader runs inside QuickJS; dispatch later.
        world_rc
            .borrow_mut()
            .queue_csp_violations(target, violations);
    }
    if blocked {
        return Err(Error::new_loading(name));
    }
    Ok(())
}

/// Declares a module and fills its `import.meta`.
///
/// `QuickJS` allocates `import.meta` as an empty null-prototype object and
/// never adds the host-defined `url` member, so the module's own URL is
/// installed here
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#hostgetimportmetaproperties>).
fn declare_module<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    source: impl Into<Vec<u8>>,
) -> Result<Module<'js, Declared>> {
    // The module's static imports are fetched while it declares, and each
    // names this module as its referrer. The realm's dynamic-`import()` URL is
    // reused for that: it already holds the script currently running, and a
    // static import is the same question asked from the loader
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    let world = super::bindings::world(ctx).ok();
    let previous = world.as_ref().map(|world| {
        let mut world = world.borrow_mut();
        let previous = (
            world.state.declaring_module,
            world.state.current_script_url.take(),
        );
        world.state.declaring_module = true;
        world.state.current_script_url = Url::parse(name).ok();
        previous
    });
    let module = Module::declare(ctx.clone(), name, source);
    if let (Some(world), Some(previous)) = (world, previous) {
        let mut world = world.borrow_mut();
        world.state.declaring_module = previous.0;
        world.state.current_script_url = previous.1;
    }
    let module = module?;
    module.meta()?.set("url", name)?;
    Ok(module)
}

/// Declares an inline module script, whose `import.meta.url` is the document
/// URL with the script's unique fragment
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#hostgetimportmetaproperties>).
pub(super) fn declare_inline<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    source: &str,
) -> Result<Module<'js, Declared>> {
    declare_module(ctx, name, source)
}

/// The request inputs for one module fetch: services, initiator, referrer,
/// policy, and the CSP gate, read under one borrow of the realm.
struct ModuleFetchInputs {
    services: std::sync::Arc<dyn crate::protocol::BrowserServices>,
    initiator: url::Origin,
    referrer: Url,
    referrer_policy: net::ReferrerPolicy,
    request_policy: crate::protocol::RequestPolicy,
    /// The policy list this request is checked against.
    csp: crate::csp::List,
    /// The directive this fetch settles under: a static import in a worker is
    /// a `worker-src` request, everything else is a script request.
    destination: crate::csp::Destination,
}

fn module_fetch_inputs(world: &crate::js::World, name: &str) -> ModuleFetchInputs {
    // Every fetch in a module graph keeps the graph root's script fetch
    // options: its `referrerpolicy` and the credentials mode its
    // `crossorigin` state selects. The document records the root element as
    // `current_script` while the graph loads, so the same lookup serves the
    // root and a dynamic `import()` inside a classic script
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-the-descendants-of-and-link-a-module-script>).
    let options = world
        .state
        .module_fetch_root
        .or(world.state.current_script)
        .map_or_default(|element| world.subresource_options(element));
    // A worker's top-level module graph uses the outside settings object's
    // origin; everything else uses this realm's own origin
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    let worker_root =
        world.state.module_client_origin.is_some() && name == world.document_url.as_str();
    let initiator = world
        .state
        .module_client_origin
        .clone()
        .unwrap_or_else(|| world.origin.clone());
    // A module worker's script fetch options carry the worker's
    // `WorkerOptions.credentials`, which every fetch in its graph keeps; a
    // document derives the mode from its root element's `crossorigin` state
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    let credentials = world.state.module_credentials.unwrap_or_else(|| {
        crate::protocol::cross_origin_credentials(crate::protocol::cross_origin(
            options.crossorigin,
        ))
    });
    let policy = if worker_root {
        // The worker's own script must stay same-origin, even when the server
        // sends `Access-Control-Allow-Origin`
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
        crate::protocol::RequestPolicy::ModuleWorkerScript { credentials }
    } else {
        crate::protocol::RequestPolicy::ModuleScript { credentials }
    };
    let worker = matches!(
        world.kind,
        crate::js::world::GlobalKind::DedicatedWorker { .. }
            | crate::js::world::GlobalKind::SharedWorker { .. }
    );
    // A module graph's fetch names the referencing module as its referrer: the
    // top-level worker script is requested by the creator, a static import
    // names the declaring module, and a dynamic `import()` in the worker's
    // body names the worker's own script
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    let referrer_url = if worker && !worker_root {
        if world.state.declaring_module {
            world
                .state
                .current_script_url
                .clone()
                .unwrap_or_else(|| world.document_url.clone())
        } else {
            world.document_url.clone()
        }
    } else {
        world.referrer_url.clone()
    };
    // A worker's static imports keep the referrer policy it inherited from its
    // creator; a dynamic `import()` in its body uses the policy of the worker's
    // own script response instead
    // (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-html>).
    let (referrer_policy, csp, destination) =
        if worker && !worker_root && !world.state.declaring_module {
            (
                world.state.worker_referrer_policy.unwrap_or_default(),
                world
                    .state
                    .worker_module_csp
                    .clone()
                    .unwrap_or_else(|| world.policy.csp.clone()),
                crate::csp::Destination::Script,
            )
        } else {
            (
                options
                    .referrer_policy
                    .map(crate::protocol::referrer_policy)
                    .or(world.policy.referrer_policy)
                    .unwrap_or_default(),
                world.policy.csp.clone(),
                // A static import in a worker is a worker request: `worker-src`
                // decides it, falling back to `child-src` and `script-src`, and
                // `script-src-elem` never applies
                // (<https://w3c.github.io/webappsec-csp/#effective-directive-for-a-request>).
                if worker && !worker_root {
                    crate::csp::Destination::Worker
                } else {
                    crate::csp::Destination::Script
                },
            )
        };
    ModuleFetchInputs {
        services: world.runtime.services.clone(),
        initiator,
        referrer: referrer_url,
        referrer_policy,
        request_policy: policy,
        csp,
        destination,
    }
}

/// Records what a worker's own script response adds to the realm: the referrer
/// policy and the content security policies its dynamic imports run under.
///
/// Static imports resolve while the graph is fetched, before the body runs, so
/// they keep the policy the worker inherited from its creator; a dynamic
/// `import()` in the body uses the script response's own values
/// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-html>,
/// <https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response>).
fn record_worker_script_response(
    world_rc: &std::rc::Rc<std::cell::RefCell<super::World>>,
    name: &str,
    headers: &[(String, String)],
) {
    let worker_root = {
        let world = world_rc.borrow();
        world.state.module_client_origin.is_some() && name == world.document_url.as_str()
    };
    if !worker_root {
        return;
    }
    let policy = net::ReferrerPolicy::from_header(
        headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case("referrer-policy"))
            .map(|(_, value)| value.as_str()),
    );
    let mut world = world_rc.borrow_mut();
    world.state.worker_referrer_policy = policy;
    let mut csp = world.policy.csp.clone();
    csp.append(crate::csp::List::from_headers(
        net::combined_header_in(headers, "content-security-policy").as_deref(),
        net::combined_header_in(headers, "content-security-policy-report-only").as_deref(),
    ));
    world.state.worker_module_csp = Some(csp);
}

pub(super) fn load_module<'js>(ctx: &Ctx<'js>, name: &str) -> Result<Module<'js, Declared>> {
    // A JavaScript module graph is fetched before evaluation
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    // QuickJS's host loader is synchronous, so the renderer waits here while
    // the browser process performs the network operation on its I/O runtime.
    let world_rc = super::bindings::world(ctx)?;
    let Ok(url) = Url::parse(name) else {
        return Err(Error::new_loading(name));
    };
    // A local-scheme module needs no network fetch: `blob:` reads the Blob URL
    // Store and `data:` decodes itself. Neither is MIME-gated; a module script
    // is created from the UTF-8 decoded body
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    if url.scheme() == "blob" || url.scheme() == "data" {
        let source = if url.scheme() == "blob" {
            world_rc
                .borrow()
                .object_url_contents(name)
                .map(|contents| contents.to_string())
        } else {
            crate::engine::decode_data_url_bounded(name)
                .map(|(_, body)| String::from_utf8_lossy(&body).into_owned())
        };
        let Some(source) = source else {
            return Err(Error::new_loading(name));
        };
        return declare_module(ctx, name, source);
    }
    let inputs = {
        let world = world_rc.borrow();
        module_fetch_inputs(&world, name)
    };
    // A module graph fetch is a script request; check it before the renderer
    // waits on the dial
    // (<https://w3c.github.io/webappsec-csp/#should-block-request>).
    module_request_blocked(
        &world_rc,
        ctx,
        name,
        &inputs.initiator,
        &inputs.csp,
        inputs.destination,
    )?;

    let (send, receive) = mpsc::sync_channel(1);
    let mut request = DialRequest::new(inputs.request_policy, &url, &inputs.initiator);
    request.cookie_policy = world_rc.borrow().same_site_cookie_policy.into();
    request.referrer = crate::protocol::ReferrerRequest::Client {
        url: inputs.referrer,
    };
    request.csp = Some(Box::new(crate::RedirectGate::new(
        inputs.csp.clone(),
        &inputs.initiator,
        inputs.destination,
    )));
    request.referrer_policy = inputs.referrer_policy;
    let cancel = inputs.services.start_dial(
        request,
        Box::new(move |outcome| {
            let _result = send.send(outcome);
        }),
    );
    let outcome = if let Ok(outcome) = receive.recv_timeout(Duration::from_secs(30)) {
        outcome.map_err(|_| Error::new_loading(name))?
    } else {
        cancel();
        return Err(Error::new_loading(name));
    };
    // A worker's own script response supplies the referrer policy and the
    // content security policies for the dynamic imports its body runs; its
    // static imports keep what it inherited from the creator.
    record_worker_script_response(&world_rc, name, &outcome.headers);
    let final_url = &outcome.final_url;
    if final_url != &url {
        let violations = {
            let world = world_rc.borrow();
            inputs.csp.check_redirected_request(
                &world.origin,
                &world.document_url,
                final_url,
                &url,
                inputs.destination,
            )
        };
        if !violations.is_empty() {
            let blocked = crate::csp::blocks(&violations);
            if let Ok(target) = super::bindings::main_document(ctx) {
                world_rc
                    .borrow_mut()
                    .queue_csp_violations(target, violations);
            }
            if blocked {
                return Err(Error::new_loading(name));
            }
        }
    }
    if !(200..300).contains(&outcome.status)
        || !super::javascript_module_mime(outcome.content_type.as_deref())
    {
        return Err(Error::new_loading(name));
    }
    let crate::protocol::ResponseBody::Bytes(body) = outcome.body else {
        return Err(Error::new_loading(name));
    };
    // A module script is registered under the URL its response came from, so
    // relative imports in a redirected graph resolve against the final URL
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>).
    declare_module(ctx, outcome.final_url.as_str(), body)
}
