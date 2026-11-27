// https://dom.spec.whatwg.org/#concept-node-adopt

pub mod accessibility;
pub mod adoption;
pub mod equality;
pub mod forms;
pub mod input;
pub mod insertion;
pub mod microsyntaxes;
pub mod namespaces;
pub mod parse;
pub mod range;
pub mod serialization;
pub mod text;
pub mod tree;

// https://dom.spec.whatwg.org/#dom-domerror
#[derive(Clone, Copy, Debug)]
pub enum TreeError {
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
