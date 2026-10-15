//! Native, undoable design-source links. No editor/runtime dependency.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Design {
    pub version: u32,
    pub root: String,
    pub gui: String,
    pub source: String,
    pub disk_hash: String,
    pub nodes: Vec<NodeLink>,
    #[serde(default)]
    pub remote: Option<String>,
    /// GUI-space lower-left corner of the editable canvas. Older sources used (0, 0).
    #[serde(default)]
    pub canvas_origin: [i32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeLink {
    pub layer: u64,
    pub artwork: u64,
    pub node: String,
    pub appearance: String,
    #[serde(default)]
    pub text: Option<String>,
}
