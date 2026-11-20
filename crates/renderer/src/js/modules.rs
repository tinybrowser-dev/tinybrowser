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
        // https://tc39.es/ecma262/#sec-hostresolveimportedmodule
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
            // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
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
            // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
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
    let target = super::bindings::main_document(ctx).ok();
    world_rc
        .borrow_mut()
        .queue_csp_violations(target, violations);
    if blocked {
        return Err(Error::new_loading(name));
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#hostgetimportmetaproperties
fn declare_module<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    source: impl Into<Vec<u8>>,
) -> Result<Module<'js, Declared>> {
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
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

// https://html.spec.whatwg.org/multipage/webappapis.html#hostgetimportmetaproperties
pub(super) fn declare_inline<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    source: &str,
) -> Result<Module<'js, Declared>> {
    declare_module(ctx, name, source)
}

struct ModuleFetchInputs {
    services: std::sync::Arc<dyn crate::protocol::BrowserServices>,
    initiator: url::Origin,
    referrer: Url,
    referrer_policy: net::ReferrerPolicy,
    request_policy: crate::protocol::RequestPolicy,
    csp: crate::csp::List,
    destination: crate::csp::Destination,
    partition_key: Option<String>,
}

fn module_fetch_inputs(world: &crate::js::World, name: &str) -> ModuleFetchInputs {
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-the-descendants-of-and-link-a-module-script
    let options = world
        .state
        .module_fetch_root
        .or(world.state.current_script)
        .map_or_default(|element| world.subresource_options(element));
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    let worker_root =
        world.state.module_client_origin.is_some() && name == world.document_url.as_str();
    let initiator = world
        .state
        .module_client_origin
        .clone()
        .unwrap_or_else(|| world.origin.clone());
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    let credentials = world.state.module_credentials.unwrap_or_else(|| {
        crate::protocol::cross_origin_credentials(crate::protocol::cross_origin(
            options.crossorigin,
        ))
    });
    let policy = if worker_root {
        // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
        crate::protocol::RequestPolicy::ModuleWorkerScript { credentials }
    } else {
        crate::protocol::RequestPolicy::ModuleScript { credentials }
    };
    let worker = matches!(
        world.kind,
        crate::js::world::GlobalKind::DedicatedWorker { .. }
            | crate::js::world::GlobalKind::SharedWorker { .. }
    );
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
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
    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-html
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
                // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
                if worker {
                    world
                        .state
                        .worker_owner_csp
                        .clone()
                        .unwrap_or_else(|| world.policy.csp.clone())
                } else {
                    world.policy.csp.clone()
                },
                // https://w3c.github.io/webappsec-csp/#effective-directive-for-a-request
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
        partition_key: world.partition_key(),
    }
}

// https://w3c.github.io/webappsec-referrer-policy/#integration-with-html
// https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response
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
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    let world_rc = super::bindings::world(ctx)?;
    let Ok(url) = Url::parse(name) else {
        return Err(Error::new_loading(name));
    };
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
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
    // https://w3c.github.io/webappsec-csp/#should-block-request
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
    request.partition_key.clone_from(&inputs.partition_key);
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
            let target = super::bindings::main_document(ctx).ok();
            world_rc
                .borrow_mut()
                .queue_csp_violations(target, violations);
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
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    declare_module(ctx, outcome.final_url.as_str(), body)
}
