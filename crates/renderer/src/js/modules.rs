use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error, Module, Result};
use url::Url;

use crate::protocol::DialRequest;

type SpecifierMap = Vec<(String, Option<Url>)>;
type ScopesMap = Vec<(String, SpecifierMap)>;

// https://html.spec.whatwg.org/multipage/webappapis.html#import-maps
#[derive(Clone, Debug, Default)]
pub(crate) struct ImportMap {
    imports: SpecifierMap,
    scopes: ScopesMap,
    integrity: HashMap<String, String>,
}

impl ImportMap {
    pub(crate) fn clear(&mut self) {
        self.imports.clear();
        self.scopes.clear();
        self.integrity.clear();
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#parse-an-import-map-string
    pub(crate) fn parse(input: &str, base_url: &Url) -> std::result::Result<Self, ()> {
        let parsed: serde_json::Value = serde_json::from_str(input).map_err(|_| ())?;
        let serde_json::Value::Object(root) = parsed else {
            return Err(());
        };
        let imports = match root.get("imports") {
            Some(serde_json::Value::Object(map)) => sort_and_normalize_specifier_map(map, base_url),
            Some(_) => return Err(()),
            None => Vec::new(),
        };
        let scopes = match root.get("scopes") {
            Some(serde_json::Value::Object(map)) => sort_and_normalize_scopes(map, base_url)?,
            Some(_) => return Err(()),
            None => Vec::new(),
        };
        let integrity = match root.get("integrity") {
            Some(serde_json::Value::Object(map)) => normalize_integrity_map(map, base_url),
            Some(_) => return Err(()),
            None => HashMap::new(),
        };
        Ok(Self {
            imports,
            scopes,
            integrity,
        })
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#merge-existing-and-new-import-maps
    pub(crate) fn merge(&mut self, incoming: Self) {
        for (scope, entries) in incoming.scopes {
            if let Some((_, existing)) = self.scopes.iter_mut().find(|(key, _)| *key == scope) {
                merge_specifier_entries(existing, entries);
            } else {
                self.scopes.push((scope, entries));
            }
        }
        self.scopes
            .sort_by(|(a, _), (b, _)| cmp_code_units_desc(a, b));
        merge_specifier_entries(&mut self.imports, incoming.imports);
        for (url, value) in incoming.integrity {
            self.integrity.entry(url).or_insert(value);
        }
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#resolve-a-module-specifier
    pub(crate) fn resolve(&self, specifier: &str, base_url: &Url) -> std::result::Result<Url, ()> {
        let as_url = resolve_url_like_module_specifier(specifier, base_url);
        let normalized = as_url.as_ref().map_or(specifier, Url::as_str);
        let base_serialized = base_url.as_str();
        for (scope_prefix, scope_imports) in &self.scopes {
            if (base_serialized == scope_prefix
                || (scope_prefix.ends_with('/')
                    && base_serialized.starts_with(scope_prefix.as_str())))
                && let Some(resolved) =
                    resolve_imports_match(normalized, as_url.as_ref(), scope_imports)
            {
                return resolved;
            }
        }
        if let Some(resolved) = resolve_imports_match(normalized, as_url.as_ref(), &self.imports) {
            return resolved;
        }
        as_url.ok_or(())
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#code-unit-less-than
fn cmp_code_units_desc(a: &str, b: &str) -> std::cmp::Ordering {
    b.encode_utf16().cmp(a.encode_utf16())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#resolve-a-url-like-module-specifier
fn resolve_url_like_module_specifier(specifier: &str, base_url: &Url) -> Option<Url> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return base_url.join(specifier).ok();
    }
    Url::parse(specifier).ok()
}

// https://url.spec.whatwg.org/#is-special
fn is_special_scheme(url: &Url) -> bool {
    matches!(
        url.scheme(),
        "http" | "https" | "ws" | "wss" | "ftp" | "file"
    )
}

// https://html.spec.whatwg.org/multipage/webappapis.html#normalize-a-specifier-key
fn normalize_specifier_key(specifier_key: &str, base_url: &Url) -> Option<String> {
    if specifier_key.is_empty() {
        return None;
    }
    if let Some(url) = resolve_url_like_module_specifier(specifier_key, base_url) {
        return Some(url.into());
    }
    Some(specifier_key.to_owned())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#sort-and-normalize-a-module-specifier-map
fn sort_and_normalize_specifier_map(
    original: &serde_json::Map<String, serde_json::Value>,
    base_url: &Url,
) -> SpecifierMap {
    let mut normalized = HashMap::with_capacity(original.len());
    for (specifier_key, value) in original {
        let Some(key) = normalize_specifier_key(specifier_key, base_url) else {
            continue;
        };
        let Some(text) = value.as_str() else {
            normalized.insert(key, None);
            continue;
        };
        let Some(address) = resolve_url_like_module_specifier(text, base_url) else {
            normalized.insert(key, None);
            continue;
        };
        if key.ends_with('/') && !address.as_str().ends_with('/') {
            normalized.insert(key, None);
            continue;
        }
        normalized.insert(key, Some(address));
    }
    let mut entries: SpecifierMap = normalized.into_iter().collect();
    entries.sort_by(|(a, _), (b, _)| cmp_code_units_desc(a, b));
    entries
}

// https://html.spec.whatwg.org/multipage/webappapis.html#sort-and-normalize-scopes
fn sort_and_normalize_scopes(
    original: &serde_json::Map<String, serde_json::Value>,
    base_url: &Url,
) -> std::result::Result<ScopesMap, ()> {
    let mut scopes = HashMap::with_capacity(original.len());
    for (prefix, value) in original {
        let serde_json::Value::Object(map) = value else {
            return Err(());
        };
        let Ok(url) = base_url.join(prefix) else {
            continue;
        };
        scopes.insert(
            url.to_string(),
            sort_and_normalize_specifier_map(map, base_url),
        );
    }
    let mut entries: ScopesMap = scopes.into_iter().collect();
    entries.sort_by(|(a, _), (b, _)| cmp_code_units_desc(a, b));
    Ok(entries)
}

// https://html.spec.whatwg.org/multipage/webappapis.html#normalize-a-module-integrity-map
fn normalize_integrity_map(
    original: &serde_json::Map<String, serde_json::Value>,
    base_url: &Url,
) -> HashMap<String, String> {
    let mut normalized = HashMap::new();
    for (key, value) in original {
        let Some(url) = resolve_url_like_module_specifier(key, base_url) else {
            continue;
        };
        let Some(integrity) = value.as_str() else {
            continue;
        };
        normalized.insert(url.into(), integrity.to_owned());
    }
    normalized
}

fn merge_specifier_entries(
    target: &mut Vec<(String, Option<Url>)>,
    incoming: Vec<(String, Option<Url>)>,
) {
    for (key, value) in incoming {
        if !target.iter().any(|(existing, _)| *existing == key) {
            target.push((key, value));
        }
    }
    target.sort_by(|(a, _), (b, _)| cmp_code_units_desc(a, b));
}

// https://html.spec.whatwg.org/multipage/webappapis.html#resolve-an-imports-match
fn resolve_imports_match(
    normalized: &str,
    as_url: Option<&Url>,
    specifier_map: &[(String, Option<Url>)],
) -> Option<std::result::Result<Url, ()>> {
    for (key, value) in specifier_map {
        if key == normalized {
            return Some(value.clone().ok_or(()));
        }
        if key.ends_with('/')
            && normalized.starts_with(key.as_str())
            && as_url.is_none_or(is_special_scheme)
        {
            let Some(resolution_result) = value else {
                return Some(Err(()));
            };
            let after_prefix = &normalized[key.len()..];
            let Ok(url) = resolution_result.join(after_prefix) else {
                return Some(Err(()));
            };
            if !url.as_str().starts_with(resolution_result.as_str()) {
                return Some(Err(()));
            }
            return Some(Ok(url));
        }
    }
    None
}

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
        // https://html.spec.whatwg.org/multipage/webappapis.html#resolve-a-module-specifier
        let world = super::bindings::world(ctx).ok();
        let base = match Url::parse(base) {
            Ok(base) => base,
            Err(_) => world
                .as_ref()
                .map(|world| world.borrow().document_url.clone())
                .ok_or_else(|| Error::new_resolving(base, name))?,
        };
        let resolved = match world {
            Some(world) => world.borrow().import_map.resolve(name, &base),
            None => resolve_url_like_module_specifier(name, &base).ok_or(()),
        }
        .map_err(|()| Error::new_resolving(base.as_str(), name))?;
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

#[cfg(test)]
mod tests {
    use super::ImportMap;
    use url::Url;

    #[test]
    fn import_map_resolves_bare_prefix_scoped_and_null_entries() {
        let base = Url::parse("https://example.test/app/page.html").expect("base");
        let mut map = ImportMap::parse(
            r#"{
              "imports": {
                "react": "https://cdn.example/react.js",
                "pkg/": "./vendor/pkg/",
                "blocked": null
              },
              "scopes": {
                "/app/admin/": {
                  "react": "https://cdn.example/react-admin.js"
                }
              }
            }"#,
            &base,
        )
        .expect("parse");

        assert_eq!(
            map.resolve("react", &base).expect("react").as_str(),
            "https://cdn.example/react.js"
        );
        assert_eq!(
            map.resolve("pkg/sub.js", &base).expect("prefix").as_str(),
            "https://example.test/app/vendor/pkg/sub.js"
        );
        assert!(map.resolve("blocked", &base).is_err());
        assert!(map.resolve("unmapped", &base).is_err());

        let admin = Url::parse("https://example.test/app/admin/index.js").expect("admin");
        assert_eq!(
            map.resolve("react", &admin).expect("scoped").as_str(),
            "https://cdn.example/react-admin.js"
        );

        let incoming = ImportMap::parse(
            r#"{
              "imports": {
                "react": "https://cdn.example/react-second.js",
                "extra": "/extra.js"
              }
            }"#,
            &base,
        )
        .expect("incoming");
        map.merge(incoming);
        assert_eq!(
            map.resolve("react", &base).expect("existing wins").as_str(),
            "https://cdn.example/react.js"
        );
        assert_eq!(
            map.resolve("extra", &base).expect("merged").as_str(),
            "https://example.test/extra.js"
        );
    }
}
