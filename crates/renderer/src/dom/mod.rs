// https://dom.spec.whatwg.org/#concept-node-adopt

pub(crate) mod adoption;
pub(crate) mod equality;
pub(crate) mod forms;
pub(crate) mod input;
pub(crate) mod insertion;
pub(crate) mod microsyntaxes;
pub(crate) mod namespaces;
pub(crate) mod parse;
pub(crate) mod range;
pub(crate) mod serialization;
pub(crate) mod text;
pub(crate) mod tree;

// https://dom.spec.whatwg.org/#dom-domerror
#[derive(Clone, Copy, Debug)]
pub(crate) enum TreeError {
    Stale,
    Hierarchy,
    NotFound,
}

impl std::fmt::Display for TreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale => f.write_str("stale node handle"),
            Self::Hierarchy => f.write_str("hierarchy or content model forbids this operation"),
            Self::NotFound => f.write_str("target has no parent to insert beside"),
        }
    }
}
