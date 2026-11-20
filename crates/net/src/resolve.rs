use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) enum ResolveFailure {
    System(std::io::Error),
    Denied,
}

impl fmt::Display for ResolveFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System(error) => write!(f, "system resolver failed: {error}"),
            Self::Denied => f.write_str("host not found"),
        }
    }
}

impl std::error::Error for ResolveFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::System(error) => Some(error),
            Self::Denied => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("resolve spec {0:?} must be PATTERN=IPv4 or PATTERN=fail")]
pub struct InvalidResolveRule(Box<str>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveRule {
    pattern: Box<str>,
    target: Target,
}

impl FromStr for ResolveRule {
    type Err = InvalidResolveRule;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let Some((pattern, addr)) = spec.split_once('=') else {
            return Err(InvalidResolveRule(spec.into()));
        };
        if pattern.is_empty() || addr.is_empty() {
            return Err(InvalidResolveRule(spec.into()));
        }
        let pattern = pattern.to_ascii_lowercase().into_boxed_str();
        let target = if addr.eq_ignore_ascii_case("fail") {
            Target::Fail
        } else {
            let ip = addr
                .parse::<Ipv4Addr>()
                .map_err(|_| InvalidResolveRule(spec.into()))?;
            Target::Addr(ip)
        };
        Ok(Self { pattern, target })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Addr(Ipv4Addr),
    Fail,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostMap {
    rules: Arc<Vec<ResolveRule>>,
}

impl HostMap {
    pub(crate) fn lookup(&self, host: &str) -> Option<Target> {
        if self.rules.is_empty() {
            return None;
        }
        let host = host.to_ascii_lowercase();
        for rule in self.rules.iter() {
            if glob_match(&rule.pattern, &host) {
                return Some(rule.target);
            }
        }
        None
    }
}

fn glob_match(pattern: &str, host: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(after_first) = host.strip_prefix(first) else {
        return false;
    };
    let Some(last) = parts.next_back() else {
        return after_first.is_empty();
    };
    let Some(mut rest) = after_first.strip_suffix(last) else {
        return false;
    };
    for part in parts {
        match rest.find(part) {
            None => return false,
            Some(index) => rest = &rest[index + part.len()..],
        }
    }
    true
}

impl From<Vec<ResolveRule>> for HostMap {
    fn from(rules: Vec<ResolveRule>) -> Self {
        Self {
            rules: Arc::new(rules),
        }
    }
}
