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
) -> Result<()> {
    let Ok(url) = url::Url::parse(name) else {
        return Ok(());
    };
    let violations = {
        let world = world_rc.borrow();
        world.policy.csp.check_request(
            origin,
            &world.document_url,
            &url,
            crate::csp::Destination::Script,
            None,
        )
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
    let module = Module::declare(ctx.clone(), name, source)?;
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
/// policy, and CSP gate, read under one borrow of the realm.
type ModuleFetchInputs = (
    std::sync::Arc<dyn crate::protocol::BrowserServices>,
    url::Origin,
    Url,
    net::ReferrerPolicy,
    crate::protocol::RequestPolicy,
    crate::RedirectGate,
);

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
    (
        world.runtime.services.clone(),
        initiator.clone(),
        world.referrer_url.clone(),
        options
            .referrer_policy
            .map(crate::protocol::referrer_policy)
            .or(world.policy.referrer_policy)
            .unwrap_or_default(),
        policy,
        crate::RedirectGate::new(
            world.policy.csp.clone(),
            &initiator,
            crate::csp::Destination::Script,
        ),
    )
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
    let (services, initiator, referrer_url, referrer_policy, policy, gate) = {
        let world = world_rc.borrow();
        module_fetch_inputs(&world, name)
    };
    // A module graph fetch is a script request; check it before the renderer
    // waits on the dial
    // (<https://w3c.github.io/webappsec-csp/#should-block-request>).
    module_request_blocked(&world_rc, ctx, name, &initiator)?;

    let (send, receive) = mpsc::sync_channel(1);
    let mut request = DialRequest::new(policy, &url, &initiator);
    request.cookie_policy = world_rc.borrow().same_site_cookie_policy.into();
    request.referrer = crate::protocol::ReferrerRequest::Client { url: referrer_url };
    request.csp = Some(Box::new(gate));
    request.referrer_policy = referrer_policy;
    let cancel = services.start_dial(
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
    let final_url = &outcome.final_url;
    if final_url != &url {
        let violations = {
            let world = world_rc.borrow();
            world.policy.csp.check_redirected_request(
                &world.origin,
                &world.document_url,
                final_url,
                &url,
                crate::csp::Destination::Script,
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
