use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RemoteValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    List(Vec<RemoteValue>),
    Map(Vec<(String, RemoteValue)>),
    Node(u64),
}
