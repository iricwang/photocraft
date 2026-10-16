//! Pure, bounded three-way GUI review. It never replaces native layers or pixels.
//! All three inputs must use the same projection space. The caller maps accepted
//! runtime properties back through verified native document transform commands.
use crate::{EngineError, Result};
use photocraft_defold_format::{Field, Scene, TextProto};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DifferenceKind {
    LocalOnly,
    ExternalOnly,
    SameChange,
    Conflict,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Difference {
    pub node: Option<String>,
    pub property: String,
    pub base: Option<Value>,
    pub local: Option<Value>,
    pub external: Option<Value>,
    pub kind: DifferenceKind,
    pub preserve_artwork: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub differences: Vec<Difference>,
}
impl Review {
    pub fn has_conflicts(&self) -> bool {
        self.differences.iter().any(|item| item.kind == DifferenceKind::Conflict)
    }
}
fn error(message: impl ToString) -> EngineError {
    EngineError::Other(message.to_string())
}
pub(crate) fn equivalent(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a.and_then(Value::as_f64), b.and_then(Value::as_f64)) {
        (Some(a), Some(b)) => a == b || (a.is_finite() && b.is_finite() && a as f32 == b as f32),
        _ => a == b,
    }
}
fn semantic(proto: &TextProto, fields: &[Field]) -> Value {
    let mut object = serde_json::Map::new();
    for field in fields {
        let value = if field.message {
            semantic(proto, &field.children)
        } else {
            let raw = &proto.source[field.value.clone()];
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()))
        };
        if let Some(values) = object.entry(field.name.clone()).or_insert_with(|| json!([])).as_array_mut() {
            values.push(value);
        }
    }
    Value::Object(object)
}
fn properties(scene: &Scene, id: &str) -> Result<BTreeMap<String, Value>> {
    let node = scene.nodes.iter().find(|node| node.id == id).ok_or_else(|| error("Missing GUI node"))?;
    let mut out = BTreeMap::from([("parent".into(), json!(node.parent)), ("type".into(), json!(node.kind))]);
    let schema = photocraft_defold_format::properties(&node.kind);
    for property in &schema {
        out.insert(property.path.clone(), scene.property(id, &property.path).map_err(error)?);
    }
    let known: BTreeSet<_> = schema.iter().map(|property| property.path.split('.').next().unwrap_or_default()).chain(["id", "parent", "type"]).collect();
    let field = scene
        .proto
        .fields
        .iter()
        .find(|field| field.name == "nodes" && scene.proto.text(&field.children, "id") == id)
        .ok_or_else(|| error("Missing node source"))?;
    let unknown: Vec<_> = field.children.iter().filter(|field| !known.contains(field.name.as_str())).cloned().collect();
    out.insert("$unknown".into(), semantic(&scene.proto, &unknown));
    Ok(out)
}
pub(crate) fn appearance_property(property: &str) -> bool {
    !property.starts_with("position.")
        && !property.starts_with("rotation.")
        && !property.starts_with("scale.")
        && !matches!(property, "parent" | "enabled" | "visible" | "layer")
}
fn add(review: &mut Review, node: Option<&str>, property: &str, values: [Option<Value>; 3], painted: bool, force: bool) {
    let [base, local, external] = values;
    let local_changed = !equivalent(base.as_ref(), local.as_ref());
    let external_changed = !equivalent(base.as_ref(), external.as_ref());
    if !local_changed && !external_changed {
        return;
    }
    let mut kind = if equivalent(local.as_ref(), external.as_ref()) {
        DifferenceKind::SameChange
    } else if !local_changed {
        DifferenceKind::ExternalOnly
    } else if !external_changed {
        DifferenceKind::LocalOnly
    } else {
        DifferenceKind::Conflict
    };
    if external_changed && !equivalent(local.as_ref(), external.as_ref()) && (force || painted && appearance_property(property)) {
        kind = DifferenceKind::Conflict;
    }
    review.differences.push(Difference { node: node.map(str::to_owned), property: property.into(), base, local, external, kind, preserve_artwork: painted });
}
/// `painted_nodes` comes from the frozen native draft. A Defold appearance change
/// that would redraw those pixels always requires a choice; transform-only changes
/// remain reviewable while retaining the exact layer subtree.
pub fn review(base: &str, local: &str, external: &str, painted_nodes: &BTreeSet<String>) -> Result<Review> {
    let base = Scene::parse(base.into()).map_err(error)?;
    let local = Scene::parse(local.into()).map_err(error)?;
    let external = Scene::parse(external.into()).map_err(error)?;
    let ids: BTreeSet<_> = base.nodes.iter().chain(&local.nodes).chain(&external.nodes).map(|node| node.id.clone()).collect();
    if ids.len() > 4096 {
        return Err(error("GUI reconciliation exceeds node limit"));
    }
    let mut out = Review::default();
    for id in ids {
        let get = |scene: &Scene| if scene.nodes.iter().any(|node| node.id == id) { properties(scene, &id).map(Some) } else { Ok(None) };
        let (b, l, e) = (get(&base)?, get(&local)?, get(&external)?);
        let painted = painted_nodes.contains(&id);
        if b.is_none() || l.is_none() || e.is_none() {
            // A deleted node with edited pixels is not an unchanged draft.
            add(&mut out, Some(&id), "$node", [b.map(|v| json!(v)), l.map(|v| json!(v)), e.map(|v| json!(v))], painted, false);
            continue;
        }
        if let (Some(b), Some(l), Some(e)) = (b, l, e) {
            let keys: BTreeSet<_> = b.keys().chain(l.keys()).chain(e.keys()).collect();
            for property in keys {
                add(
                    &mut out,
                    Some(&id),
                    property,
                    [b.get(property).cloned(), l.get(property).cloned(), e.get(property).cloned()],
                    painted,
                    property == "$unknown",
                );
            }
        }
    }
    let top = |scene: &Scene| semantic(&scene.proto, &scene.proto.fields.iter().filter(|field| field.name != "nodes").cloned().collect::<Vec<_>>());
    // Resource collections, layout overrides and unknown scene fields are preserved
    // as explicit differences. They need specialized validation before native import.
    add(&mut out, None, "$scene", [Some(top(&base)), Some(top(&local)), Some(top(&external))], false, true);
    let order = |scene: &Scene| json!(scene.nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>());
    add(&mut out, None, "$nodeOrder", [Some(order(&base)), Some(order(&local)), Some(order(&external))], false, false);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = "nodes {id:\"button\" type:TYPE_BOX position{x:10 y:20} size{x:32 y:32} future_flag:17}";
    #[test]
    fn independent_property_edits_and_formatting_are_reviewable_without_conflict() {
        let local = BASE.replace("x:10", "x:30");
        let external = BASE.replace("y:20", "y:40");
        let report = review(BASE, &local, &external, &BTreeSet::new()).unwrap();
        assert!(!report.has_conflicts());
        assert_eq!(report.differences.len(), 2);
        assert!(report.differences.iter().any(|d| d.property == "position.x" && d.kind == DifferenceKind::LocalOnly));
        assert!(report.differences.iter().any(|d| d.property == "position.y" && d.kind == DifferenceKind::ExternalOnly));
        assert!(review(BASE, BASE, &format!("# serialization\n{}", BASE.replace("x:10", "x:10.0")), &BTreeSet::new()).unwrap().differences.is_empty());
    }
    #[test]
    fn conflicting_properties_and_deleted_painted_nodes_require_a_choice() {
        assert!(review(BASE, &BASE.replace("x:10", "x:30"), &BASE.replace("x:10", "x:40"), &BTreeSet::new()).unwrap().has_conflicts());
        let painted = BTreeSet::from(["button".into()]);
        let report = review(BASE, BASE, "", &painted).unwrap();
        assert!(report.has_conflicts());
        assert!(report.differences.iter().any(|d| d.property == "$node" && d.preserve_artwork));
        assert!(review(BASE, BASE, &BASE.replace("x:32", "x:64"), &painted).unwrap().has_conflicts());
        assert!(!review(BASE, BASE, &BASE.replace("x:10", "x:64"), &painted).unwrap().has_conflicts());
    }
    #[test]
    fn new_nodes_unknown_fields_and_resource_changes_are_never_dropped() {
        assert!(review(BASE, BASE, &BASE.replace("future_flag:17", "future_flag:18"), &BTreeSet::new()).unwrap().has_conflicts());
        assert!(review(BASE, BASE, &format!("{BASE}\ntextures{{name:\"ui\" texture:\"/ui.atlas\"}}"), &BTreeSet::new()).unwrap().has_conflicts());
        let external = format!("{BASE}\nnodes{{id:\"extra\" type:TYPE_BOX}}");
        let report = review(BASE, BASE, &external, &BTreeSet::new()).unwrap();
        assert!(report.differences.iter().any(|d| d.node.as_deref() == Some("extra") && d.kind == DifferenceKind::ExternalOnly));
    }
}
