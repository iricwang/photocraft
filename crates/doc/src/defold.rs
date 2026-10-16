//! Native, undoable design-source links. No editor/runtime dependency.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

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
    /// Project-owned creative data. Legacy links remain readable without this attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectLink>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeLink {
    /// Stable publication identity, independent of the layer or GUI node name.
    #[serde(default)]
    pub asset_id: String,
    pub layer: u64,
    pub artwork: u64,
    pub node: String,
    pub appearance: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// A malformed persistent identifier. Runtime numeric layer IDs are a separate namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityError;

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Expected a non-nil canonical UUID")
    }
}
impl std::error::Error for IdentityError {}

/// UUID parsing/generation shared by the typed project identities, without an RNG in the doc crate.
pub trait StableIdentity: Sized {
    fn from_bytes(bytes: [u8; 16]) -> Self;
}

fn canonical_uuid(value: &str) -> Result<String, IdentityError> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == b'-' } else { c.is_ascii_hexdigit() })
        || value.bytes().all(|c| c == b'0' || c == b'-')
    {
        return Err(IdentityError);
    }
    Ok(value.to_ascii_lowercase())
}

fn uuid_from_bytes(mut bytes: [u8; 16]) -> String {
    // Fixed-size arrays and literal positions, independent of file input.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut value = String::with_capacity(36);
    for (i, byte) in bytes.into_iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            value.push('-');
        }
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

macro_rules! identity {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn parse(value: &str) -> Result<Self, IdentityError> { canonical_uuid(value).map(Self) }
            pub fn as_str(&self) -> &str { &self.0 }
            pub fn from_bytes(bytes: [u8; 16]) -> Self { Self(uuid_from_bytes(bytes)) }
        }
        impl StableIdentity for $name {
            fn from_bytes(bytes: [u8; 16]) -> Self { Self::from_bytes(bytes) }
        }
        impl TryFrom<String> for $name {
            type Error = IdentityError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::parse(&value) }
        }
        impl From<$name> for String { fn from(value: $name) -> String { value.0 } }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
    )+};
}

identity!(ProjectId, CheckoutId, WorkspaceId, SceneId, NodeId, StableLayerId, AssetId, PackageId, OperationId, TransactionId, ClientId, EditorSessionId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LayerRole {
    GuiNode,
    ArtworkRoot,
    ArtworkLayer,
    DesignOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AppearanceSource {
    SelfLayer,
    Subtree { root_layer_id: StableLayerId },
    Shared { source: SharedAppearanceRef },
}

/// The carrier GUI node inside one immutable native source package. Numeric LayerId values
/// are deliberately absent: they are meaningful only while that package is loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtworkSourceNode {
    pub scene_id: SceneId,
    pub node_id: NodeId,
    pub root_layer_id: StableLayerId,
}

/// A consumer keeps this pinned reference; any displayed pixels are a disposable read-only cache.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedAppearanceRef {
    pub asset_id: AssetId,
    pub package_id: PackageId,
    pub source_revision: String,
    pub source_node: ArtworkSourceNode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerBinding {
    pub layer_id: StableLayerId,
    pub role: LayerRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gui_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<AppearanceSource>,
    /// Content-only fingerprint; GUI position changes must not turn clean artwork into pixel edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork_fingerprint: Option<String>,
}

/// Undoable creative identity only. Acknowledged host revisions, leases and credentials stay outside history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLink {
    pub schema_version: u32,
    pub project_id: ProjectId,
    pub scene_id: SceneId,
    pub package_id: PackageId,
    pub gui_path: String,
    /// Immutable baseline captured when importing the GUI, before any local creative edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_gui_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_disk_hash: Option<String>,
    #[serde(default)]
    pub layers: BTreeMap<u64, LayerBinding>,
    #[serde(default)]
    pub legacy_aliases: BTreeMap<String, String>,
}

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn identities_reject_runtime_ids_and_normalize_uuid_case() {
        assert!(ProjectId::parse("188").is_err());
        assert!(AssetId::parse("0665a52aa6cbec8c").is_err());
        assert!(SceneId::parse("00000000-0000-0000-0000-000000000000").is_err());
        let id = SceneId::parse("ABCDEF12-1234-4234-8234-123456789ABC").unwrap();
        assert_eq!(id.as_str(), "abcdef12-1234-4234-8234-123456789abc");
        assert_eq!(serde_json::from_str::<SceneId>(&serde_json::to_string(&id).unwrap()).unwrap(), id);
    }

    #[test]
    fn legacy_design_does_not_require_project_attachment() {
        let design: Design = serde_json::from_value(serde_json::json!({
            "version": 3, "root": "old", "gui": "old.gui", "source": "", "disk_hash": "", "nodes": []
        }))
        .unwrap();
        assert!(design.project.is_none());
        assert!(serde_json::to_value(design).unwrap().get("project").is_none());
    }

    #[test]
    fn shared_artwork_roundtrips_pinned_stable_owner_without_local_layer_ids() {
        let source = AppearanceSource::Shared {
            source: SharedAppearanceRef {
                asset_id: AssetId::from_bytes([1; 16]),
                package_id: PackageId::from_bytes([2; 16]),
                source_revision: "immutable-1".into(),
                source_node: ArtworkSourceNode {
                    scene_id: SceneId::from_bytes([3; 16]),
                    node_id: NodeId::from_bytes([4; 16]),
                    root_layer_id: StableLayerId::from_bytes([5; 16]),
                },
            },
        };
        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(value["kind"], "shared");
        assert_eq!(value["source"]["sourceRevision"], "immutable-1");
        assert!(value["source"]["sourceNode"].get("rootLayerId").unwrap().is_string());
        assert_eq!(serde_json::from_value::<AppearanceSource>(value).unwrap(), source);
    }
}
