//! wptserve's `.sub.headers` template substitution.
//!
//! wptserve runs a small expression language over `.sub.headers` and `.sub.*`
//! bodies (`third_party/wpt/tools/wptserve/wptserve/pipes.py`, `template`).
//! Only the expressions the checkout's header files use are implemented:
//! `host`, `ports`, `domains`, `hosts`, `GET`, `location`, `uuid`,
//! `header_or_default`, and `$name:` assignment. An expression this module does
//! not understand is an error, never an empty substitution: a silently dropped
//! header changes what the test observes.

use std::collections::HashMap;

use url::Url;

/// The request facts a header template can read.
pub struct Context<'a> {
    /// The URL of the request the header belongs to.
    pub url: &'a Url,
    /// Request headers, as sent by the engine.
    pub headers: &'a [(String, String)],
}

/// The browser host `tools/serve/serve.py` configures.
const BROWSER_HOST: &str = "web-platform.test";
/// The alternate host, reached as `hosts[alt]`.
const ALTERNATE_HOST: &str = "not-web-platform.test";

/// The subdomains serve.py registers, already IDNA-encoded the way
/// `wptserve/config.py` encodes them (`www`, `www1`, `www2`, and the two
/// non-ASCII names). Upstream makes the one- and two-level products.
const SUBDOMAINS: [&str; 5] = [
    "www",
    "www1",
    "www2",
    "xn--n8j6ds53lwwkrqhv28a",
    "xn--lve-6lad",
];

/// How a substituted value is escaped in the payload.
///
/// wptserve picks this from the response's extension: markup gets HTML
/// escaping, every other pipe (`sub(none)`) gets the raw value
/// (`wptserve/handlers.py::wrap_pipeline`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Escape {
    /// `html.escape(value, quote=True)`.
    Html,
    /// The value as the template produced it.
    None,
}

impl Escape {
    /// The escape mode wptserve uses for a path's extension.
    #[must_use]
    pub fn for_path(path: &str) -> Self {
        let extension = path.rsplit('.').next().unwrap_or_default();
        if matches!(extension, "html" | "htm" | "xht" | "xhtml" | "xml" | "svg") {
            Self::Html
        } else {
            Self::None
        }
    }

    /// Applies the mode to one expanded value.
    fn apply(self, value: &str) -> String {
        match self {
            Self::None => value.to_owned(),
            Self::Html => {
                let mut out = String::with_capacity(value.len());
                for character in value.chars() {
                    match character {
                        '&' => out.push_str("&amp;"),
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        '"' => out.push_str("&quot;"),
                        // `html.escape(quote=True)` uses this named form.
                        '\'' => out.push_str("&#x27;"),
                        other => out.push(other),
                    }
                }
                out
            }
        }
    }
}

/// Replaces every `{{...}}` expression in `text`, without escaping.
///
/// This is the header form: wptserve expands `.sub.headers` with
/// `template(..., escape_type="none")`.
///
/// # Errors
///
/// An unterminated expression, or one this module does not implement.
pub fn substitute(text: &str, context: &Context<'_>) -> Result<String, String> {
    substitute_escaped(text, context, Escape::None)
}

/// Replaces every `{{...}}` expression in `text`, escaping each value.
///
/// # Errors
///
/// An unterminated expression, or one this module does not implement.
pub fn substitute_escaped(
    text: &str,
    context: &Context<'_>,
    escape: Escape,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut variables: HashMap<String, String> = HashMap::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("unterminated template expression in {rest:?}"));
        };
        let expanded = evaluate(after[..end].trim(), context, &mut variables)?;
        out.push_str(&escape.apply(&expanded));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Evaluates one expression, handling `$name:` assignment and `$name` reuse.
fn evaluate(
    expression: &str,
    context: &Context<'_>,
    variables: &mut HashMap<String, String>,
) -> Result<String, String> {
    if let Some(body) = expression.strip_prefix('$') {
        return match body.split_once(':') {
            Some((name, expression)) => {
                let expanded = value(expression.trim(), context)?;
                variables.insert(name.trim().to_owned(), expanded.clone());
                Ok(expanded)
            }
            None => variables
                .get(body.trim())
                .cloned()
                .ok_or_else(|| format!("undefined template variable ${}", body.trim())),
        };
    }
    value(expression, context)
}

/// A `[...]` index or a `(...)` argument list.
enum Selector {
    Index(String),
    Arguments(String),
}

/// Evaluates a value expression such as `ports[https][0]`.
fn value(expression: &str, context: &Context<'_>) -> Result<String, String> {
    let head_end = expression.find(['[', '(', '.']).unwrap_or(expression.len());
    let head = &expression[..head_end];
    let mut rest = &expression[head_end..];
    let mut selectors = Vec::new();
    while !rest.is_empty() {
        let (close, kind) = match rest.as_bytes()[0] {
            b'[' => (']', 0),
            b'(' => (')', 1),
            _ => return Err(format!("unexpected token in {{{{{expression}}}}}")),
        };
        let after = &rest[1..];
        let Some(end) = after.find(close) else {
            return Err(format!("unterminated selector in {{{{{expression}}}}}"));
        };
        let inner = after[..end].to_owned();
        selectors.push(if kind == 0 {
            Selector::Index(inner)
        } else {
            Selector::Arguments(inner)
        });
        rest = &after[end + 1..];
    }
    match head {
        "host" => {
            expect_no_selectors(head, &selectors)?;
            Ok(BROWSER_HOST.to_owned())
        }
        "uuid" => {
            expect_arguments(head, &selectors, "")?;
            Ok(uuid())
        }
        "ports" => {
            let [Selector::Index(scheme), Selector::Index(index)] = selectors.as_slice() else {
                return Err(format!(
                    "ports needs [scheme][index] in {{{{{expression}}}}}"
                ));
            };
            port(scheme, index).ok_or_else(|| format!("unknown port {scheme}[{index}]"))
        }
        "domains" => {
            let [Selector::Index(subdomain)] = selectors.as_slice() else {
                return Err(format!("domains needs [subdomain] in {{{{{expression}}}}}"));
            };
            domain(subdomain).ok_or_else(|| format!("unknown domain {subdomain}"))
        }
        "hosts" => {
            let [Selector::Index(alias), Selector::Index(subdomain)] = selectors.as_slice() else {
                return Err(format!(
                    "hosts needs [alias][subdomain] in {{{{{expression}}}}}"
                ));
            };
            host(alias, subdomain).ok_or_else(|| format!("unknown host {alias}[{subdomain}]"))
        }
        "GET" => {
            let [Selector::Index(key)] = selectors.as_slice() else {
                return Err(format!("GET needs [key] in {{{{{expression}}}}}"));
            };
            context
                .url
                .query_pairs()
                .find(|(name, _)| name.as_ref() == key.as_str())
                .map(|(_, value)| value.into_owned())
                .ok_or_else(|| format!("missing query parameter {key}"))
        }
        "location" => {
            let [Selector::Index(field)] = selectors.as_slice() else {
                return Err(format!("location needs [field] in {{{{{expression}}}}}"));
            };
            location(field, context.url).ok_or_else(|| format!("unknown location field {field}"))
        }
        "header_or_default" => {
            let [Selector::Arguments(arguments)] = selectors.as_slice() else {
                return Err(format!(
                    "header_or_default needs (name, default) in {{{{{expression}}}}}"
                ));
            };
            let Some((name, default)) = arguments.split_once(',') else {
                return Err(format!(
                    "header_or_default needs (name, default) in {{{{{expression}}}}}"
                ));
            };
            Ok(context
                .headers
                .iter()
                .find(|(header, _)| header.eq_ignore_ascii_case(name.trim()))
                .map_or(default.trim(), |(_, value)| value)
                .to_owned())
        }
        other => Err(format!("unsupported template function `{other}`")),
    }
}

/// Rejects selectors on a bare value such as `host`.
fn expect_no_selectors(head: &str, selectors: &[Selector]) -> Result<(), String> {
    if selectors.is_empty() {
        Ok(())
    } else {
        Err(format!("`{head}` takes no selector"))
    }
}

/// Rejects anything but an empty argument list.
fn expect_arguments(head: &str, selectors: &[Selector], expected: &str) -> Result<(), String> {
    match selectors {
        [] => Ok(()),
        [Selector::Arguments(arguments)] if arguments.trim() == expected => Ok(()),
        _ => Err(format!("`{head}` takes `({expected})`")),
    }
}

/// The port serve.py gives a scheme and index.
///
/// The table is the one the `WebDriver` lane runs against
/// (`tools/wptrunner/wptrunner/environment.py::TestEnvironment.build_config`);
/// the standalone `serve.py` defaults differ in the second `http` port and in
/// every `auto` scheme.
fn port(scheme: &str, index: &str) -> Option<String> {
    let ports: &[u16] = match scheme {
        "http" => &[8000, 8001],
        "http-local" => &[8002],
        "http-public" => &[8003],
        "https" => &[8443, 8444],
        "https-local" => &[8445],
        "https-public" => &[8446],
        "ws" => &[8888],
        "wss" => &[8889],
        "h2" => &[9000],
        "webtransport-h3" => &[11000],
        "dns" => &[8053],
        _ => return None,
    };
    let index: usize = index.trim().parse().ok()?;
    ports.get(index).map(ToString::to_string)
}

/// `domains[subdomain]` under the browser host, with `""` naming the host
/// itself (`wptserve/config.py::_get_domains`).
fn domain(subdomain: &str) -> Option<String> {
    if subdomain.is_empty() {
        return Some(BROWSER_HOST.to_owned());
    }
    encoded_subdomain(subdomain).map(|encoded| format!("{encoded}.{BROWSER_HOST}"))
}

/// `hosts[alias][subdomain]`, where the alias is `""` (browser) or `alt`.
fn host(alias: &str, subdomain: &str) -> Option<String> {
    let base = match alias {
        "" => BROWSER_HOST,
        "alt" => ALTERNATE_HOST,
        _ => return None,
    };
    if subdomain.is_empty() {
        return Some(base.to_owned());
    }
    encoded_subdomain(subdomain).map(|encoded| format!("{encoded}.{base}"))
}

/// The registered one- or two-level subdomain in `name`, with every label
/// IDNA-encoded.
///
/// `serve.py` registers the configured names twice: once as written (the
/// non-ASCII `天気の良い日` and `élève` included) for the server's host
/// matching, and IDNA-encoded in `domains[...]`
/// (`wptserve/config.py::_get_domains`). A template can therefore name either
/// spelling, and the expansion is always the ASCII form.
fn encoded_subdomain(name: &str) -> Option<String> {
    let labels: Vec<&str> = name.split('.').collect();
    if labels.is_empty() || labels.len() > 2 {
        return None;
    }
    let mut encoded = Vec::with_capacity(labels.len());
    for label in labels {
        let url::Host::Domain(ascii) = url::Host::parse(label).ok()? else {
            return None;
        };
        if !SUBDOMAINS.contains(&ascii.as_str()) {
            return None;
        }
        encoded.push(ascii);
    }
    Some(encoded.join("."))
}

/// `location[field]` for one request URL.
fn location(field: &str, url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    Some(match field {
        "scheme" => url.scheme().to_owned(),
        "hostname" => host.to_owned(),
        "port" => port.to_string(),
        "host" => format!("{host}:{port}"),
        "server" => format!("{}://{host}:{port}", url.scheme()),
        "path" | "pathname" => url.path().to_owned(),
        "query" => format!("?{}", url.query().unwrap_or("")),
        _ => return None,
    })
}

/// A UUID for `uuid()` and `$name:uuid()`.
fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::{Context, substitute};

    fn url(query: &str) -> url::Url {
        url::Url::parse(&format!(
            "http://www.web-platform.test:8000/dir/file.html{query}"
        ))
        .expect("test URL")
    }

    fn expand(text: &str, query: &str) -> String {
        substitute(
            text,
            &Context {
                url: &url(query),
                headers: &[("Origin".into(), "https://a.test".into())],
            },
        )
        .expect("template expands")
    }

    #[test]
    fn the_frontmatter_style_expressions_expand_like_wptserve() {
        assert_eq!(expand("{{host}}", ""), "web-platform.test");
        assert_eq!(expand("{{ports[http][0]}}", ""), "8000");
        assert_eq!(expand("{{ports[https][0]}}", ""), "8443");
        assert_eq!(expand("{{domains[]}}", ""), "web-platform.test");
        assert_eq!(expand("{{domains[www]}}", ""), "www.web-platform.test");
        // The tests spell the two non-ASCII subdomains as written; the
        // expansion is the IDNA form `serve.py` serves
        // (`wptserve/config.py::_get_domains`).
        assert_eq!(
            expand("{{domains[天気の良い日]}}", ""),
            "xn--n8j6ds53lwwkrqhv28a.web-platform.test"
        );
        assert_eq!(
            expand("{{domains[élève]}}", ""),
            "xn--lve-6lad.web-platform.test"
        );
        assert_eq!(
            expand("{{hosts[][élève.www]}}", ""),
            "xn--lve-6lad.www.web-platform.test"
        );
        assert_eq!(
            expand("{{hosts[alt][www]}}", ""),
            "www.not-web-platform.test"
        );
        assert_eq!(expand("{{hosts[][www]}}", ""), "www.web-platform.test");
        assert_eq!(expand("{{hosts[alt][]}}", ""), "not-web-platform.test");
        assert_eq!(expand("{{GET[id]}}", "?id=42"), "42");
        assert_eq!(
            expand("{{location[host]}}", ""),
            "www.web-platform.test:8000"
        );
        assert_eq!(expand("{{location[path]}}", ""), "/dir/file.html");
        assert_eq!(expand("{{location[query]}}", "?a=b"), "?a=b");
        assert_eq!(
            expand("{{header_or_default(Origin, *)}}", ""),
            "https://a.test"
        );
        assert_eq!(expand("{{header_or_default(Referer, *)}}", ""), "*");
    }

    #[test]
    fn an_assigned_uuid_is_reused_and_distinct_ids_differ() {
        let text = expand("{{$id:uuid()}} {{$id2:uuid()}} {{$id}} {{$id2}}", "");
        let parts: Vec<&str> = text.split(' ').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], parts[2]);
        assert_eq!(parts[1], parts[3]);
        assert_ne!(parts[0], parts[1]);
    }

    #[test]
    fn unknown_expressions_are_errors_not_empty_strings() {
        for text in [
            "{{nope}}",
            "{{ports[http]}}",
            "{{GET[missing]}}",
            "{{domains[unknown]}}",
            "{{location[nope]}}",
            "{{$missing}}",
            "{{host{{",
        ] {
            assert!(
                substitute(
                    text,
                    &Context {
                        url: &url(""),
                        headers: &[],
                    }
                )
                .is_err(),
                "{text}"
            );
        }
    }
}
