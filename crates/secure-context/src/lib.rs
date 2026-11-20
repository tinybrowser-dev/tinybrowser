// https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy

use url::{Host, Origin, Url};

// https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy
#[must_use]
pub fn is_potentially_trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "about" => matches!(url.path(), "blank" | "srcdoc"),
        "data" | "file" => true,
        "blob" => is_trustworthy_origin(&url.origin()) || blob_creator_is_file(url),
        _ => is_trustworthy_origin(&url.origin()),
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
#[must_use]
pub fn is_secure_environment(url: &Url, creator: Option<bool>) -> bool {
    url.scheme() != "data" && is_potentially_trustworthy(url) && creator.unwrap_or(true)
}

// https://w3c.github.io/webappsec-secure-contexts/#is-origin-trustworthy
fn is_trustworthy_origin(origin: &Origin) -> bool {
    let Origin::Tuple(scheme, host, _) = origin else {
        return false;
    };
    matches!(scheme.as_str(), "https" | "wss") || is_localhost(host)
}

// https://w3c.github.io/webappsec-secure-contexts/#is-origin-trustworthy
fn is_localhost(host: &Host<String>) -> bool {
    match host {
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
        Host::Domain(host) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost" || host.ends_with(".localhost")
        }
    }
}

fn blob_creator_is_file(url: &Url) -> bool {
    url.as_str()
        .strip_prefix("blob:")
        .and_then(|inner| Url::parse(inner).ok())
        .is_some_and(|inner| inner.scheme() == "file")
}

#[cfg(test)]
mod tests {
    use super::{is_potentially_trustworthy, is_secure_environment};
    use url::Url;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("url")
    }

    fn trustworthy(value: &str) -> bool {
        is_potentially_trustworthy(&url(value))
    }

    #[test]
    fn local_documents_without_an_http_response_are_trustworthy() {
        assert!(trustworthy("about:srcdoc"));
        assert!(trustworthy("file:///tmp/page.html"));
        assert!(trustworthy("blob:https://example.com/uuid"));
        assert!(trustworthy("blob:http://localhost/uuid"));
        assert!(trustworthy("blob:file:///tmp/page.html"));
    }

    #[test]
    fn a_data_url_is_trustworthy_but_never_a_secure_environment() {
        // https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy
        assert!(trustworthy("data:,x"));
        assert!(!is_secure_environment(&url("data:,x"), None));
        assert!(!is_secure_environment(&url("data:,x"), Some(true)));
    }

    #[test]
    fn an_environment_is_secure_only_inside_a_secure_creator() {
        assert!(is_secure_environment(&url("https://a.test/"), None));
        assert!(is_secure_environment(&url("https://a.test/"), Some(true)));
        assert!(!is_secure_environment(&url("https://a.test/"), Some(false)));
        assert!(!is_secure_environment(&url("http://a.test/"), None));
        assert!(is_secure_environment(&url("http://localhost/"), None));
    }

    #[test]
    fn a_blob_url_answers_for_its_creator() {
        // https://url.spec.whatwg.org/#origin
        assert!(!trustworthy("blob:data:,x"));
        assert!(!trustworthy("blob:http://a.test/uuid"));
    }
}
