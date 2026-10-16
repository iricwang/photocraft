//! Explicit, revision-checked GUI review. Host reads run on workers; accepted
//! fields use the native property mutator in one Undo step, never a document import.
use crate::{
    EngineError, Result, Session,
    defold_design_conflicts::{self as conflicts, Difference, DifferenceKind},
    defold_project_client::{self as client, ClientBinding},
    defold_project_model::*,
    defold_project_publish_client::{ReviewProjection, review_projection, source_descends_from},
};
use photocraft_defold_format::Scene;
use photocraft_doc::Document;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

fn error(message: impl std::fmt::Display) -> EngineError {
    EngineError::Other(message.to_string())
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    index: usize,
    #[serde(flatten)]
    difference: Difference,
    can_keep_mine: bool,
    can_take_theirs: bool,
    blocker: Option<String>,
}
struct CapturedReview {
    binding: ClientBinding,
    external: SceneSnapshot,
    projection: ReviewProjection,
    items: Vec<Item>,
    id: String,
    revision: u64,
}
fn transform_property(property: &str) -> bool {
    property.starts_with("position.")
        || property.starts_with("rotation.")
        || property.starts_with("scale.")
        || property.starts_with("size.")
        || property == "pivot"
}
fn supported_property(property: &str) -> bool {
    matches!(
        property,
        "enabled"
            | "visible"
            | "layer"
            | "material"
            | "blend_mode"
            | "text"
            | "text_leading"
            | "text_tracking"
            | "line_break"
            | "alpha"
            | "inherit_alpha"
            | "position.x"
            | "position.y"
            | "position.z"
            | "rotation.z"
            | "scale.x"
            | "scale.y"
    ) || property.starts_with("color.")
        || property.starts_with("outline.")
        || property.starts_with("shadow.")
}
fn related(scene: &Scene, first: &str, second: &str) -> bool {
    let ancestor = |child: &str, parent: &str| {
        let mut child = child.to_owned();
        for _ in 0..128 {
            if child == parent {
                return true;
            }
            let Some(node) = scene.nodes.iter().find(|node| node.id == child) else { return false };
            if node.parent.is_empty() {
                return false;
            }
            child.clone_from(&node.parent);
        }
        true
    };
    ancestor(first, second) || ancestor(second, first)
}
fn classify(review: conflicts::Review, authored: &Scene) -> Vec<Item> {
    review
        .differences
        .iter()
        .enumerate()
        .map(|(index, difference)| {
            let external_changed = !conflicts::equivalent(difference.base.as_ref(), difference.external.as_ref());
            let relevant = external_changed && difference.kind != DifferenceKind::SameChange;
            let structural =
                difference.node.is_none() || difference.property.starts_with('$') || matches!(difference.property.as_str(), "id" | "parent" | "type");
            let coupled = relevant
                && transform_property(&difference.property)
                && review.differences.iter().any(|local| {
                    transform_property(&local.property)
                        && !conflicts::equivalent(local.base.as_ref(), local.local.as_ref())
                        && (local.node != difference.node || local.property != difference.property)
                        && local.node.as_deref().zip(difference.node.as_deref()).is_some_and(|(a, b)| related(authored, a, b))
                });
            let blocker = if relevant && structural {
                Some("Structure, resource collections and unknown fields require explicit manual reconciliation".into())
            } else if coupled {
                Some("Changes affect dependent transforms in the same node hierarchy".into())
            } else if relevant && difference.preserve_artwork && matches!(difference.property.as_str(), "rotation.z" | "scale.x" | "scale.y") {
                Some("Rotating or scaling baked artwork needs an explicit native edit to preserve its authored pivot".into())
            } else if relevant && !supported_property(&difference.property) {
                Some("This property does not yet have a verified native projection".into())
            } else {
                None
            };
            let supported = relevant && blocker.is_none();
            let pixel_conflict = difference.preserve_artwork && conflicts::appearance_property(&difference.property);
            Item {
                index,
                difference: difference.clone(),
                can_keep_mine: supported && difference.kind == DifferenceKind::Conflict,
                can_take_theirs: supported && !pixel_conflict,
                blocker,
            }
        })
        .collect()
}
fn current_source_head(binding: &ClientBinding) -> Result<Option<String>> {
    match binding.store.read_source(&ReadSourceRequest {
        context: binding.context.clone(),
        package_id: binding.scene.package_id.clone(),
        source_revision: None,
    }) {
        Ok(package) => {
            if package.envelope.package_id != binding.scene.package_id {
                return Err(error("SOURCE_CONFLICT: Source package identity changed"));
            }
            Ok(Some(package.envelope.source_revision))
        }
        Err(failure) if failure.code == ProjectErrorCode::ResourceMissing => Ok(None),
        Err(failure) => Err(error(failure)),
    }
}
fn capture(doc: &Document, mut binding: ClientBinding, revision: u64, opening: u64) -> Result<CapturedReview> {
    if current_source_head(&binding)? != binding.source_revision {
        return Err(error("SOURCE_CONFLICT: Another saved source exists; keep this draft before reloading the shared source"));
    }
    let request = SceneRequest { context: binding.context.clone(), scene_id: binding.scene.scene_id.clone(), path: binding.scene.path.clone() };
    if let Some(latest) = binding.store.latest_publication(&request).map_err(error)? {
        if !latest.phase.is_terminal() {
            return Err(error("RECOVERY_REQUIRED: Resolve the pending publication before reviewing project changes"));
        }
        if latest.phase == PublishPhase::Committed
            && let Some(target) = latest.targets.iter().find(|target| target.scene_id == binding.scene.scene_id)
            && binding.baseline.published_revision.as_ref() != Some(&target.published_revision)
        {
            let source = latest
                .source_revisions
                .iter()
                .find(|source| source.package_id == binding.scene.package_id)
                .ok_or_else(|| error("SOURCE_CONFLICT: Publication has no matching creative source"))?;
            if !source_descends_from(&binding, &source.source_revision)? {
                return Err(error("SOURCE_CONFLICT: Publication is not an ancestor of this creative source"));
            }
            let baseline = binding
                .store
                .published_scene_baseline(&request)
                .map_err(error)?
                .ok_or_else(|| error("RECOVERY_REQUIRED: The last published GUI baseline is unavailable"))?;
            if baseline.scene_id != target.scene_id
                || baseline.path != target.path
                || baseline.disk_hash != target.disk_hash
                || baseline.scene_hash != target.scene_hash
                || baseline.published_revision.as_ref() != Some(&target.published_revision)
            {
                return Err(error("IDEMPOTENCY_MISMATCH: Published baseline does not match its receipt"));
            }
            binding.baseline = baseline;
        }
    }
    let external = binding.store.snapshot(&request).map_err(error)?;
    let projection = review_projection(doc, &binding)?;
    let review = conflicts::review(&binding.baseline.gui_source, &projection.scene.proto.source, &external.source, &projection.painted_nodes)?;
    if review.differences.len() > 512 {
        return Err(error("GUI review exceeds 512 differences; reconcile a smaller change before continuing"));
    }
    let items = classify(review, &projection.authored);
    let id = ContentHash::sha256(
        &serde_json::to_vec(&json!({"context":binding.context,"source":binding.source_revision,"revision":revision,"document":doc.id.0,"opening":opening,
        "base":binding.baseline,"craft":projection.scene.proto.source,"defold":external}))
        .map_err(error)?,
    )
    .to_string();
    Ok(CapturedReview { binding, external, projection, items, id, revision })
}
fn report(review: &CapturedReview, document: u64, open_id: u64) -> Value {
    json!({"reviewId":review.id,"documentId":document,"openId":open_id,"capturedNativeRevision":review.revision,
        "targetDirty":review.external.dirty,"items":review.items,
        "blocked":review.external.dirty || review.items.iter().any(|item| item.blocker.is_some()),
        "unresolved":review.items.iter().filter(|item|item.difference.kind==DifferenceKind::Conflict && item.blocker.is_none()).count()})
}
pub fn review(session: &mut Session) -> Result<Value> {
    let state = session.active().ok_or(EngineError::NoDocument)?;
    if state.floating.is_some() {
        return Err(error("Finish the floating selection before reviewing project changes"));
    }
    let snapshot = Arc::clone(&state.doc);
    let (document, open_id, revision) = (state.doc.id, state.open_id, state.revision);
    let previous = session.project_client.bindings.get(&open_id).cloned();
    crate::jobs::run(
        session,
        "Review Defold changes",
        true,
        move |_| {
            let mut doc = snapshot.as_ref().clone();
            let binding = match previous {
                Some(binding) => binding,
                None => client::bind_document(&mut doc)?,
            };
            capture(&doc, binding, revision, open_id)
        },
        move |session, review| {
            if !session.documents().iter().any(|state| state.doc.id == document && state.open_id == open_id && state.revision == revision) {
                return Err(error("STALE_REVIEW: The native document changed; check project changes again"));
            }
            let result = report(&review, document.0, open_id);
            if let Some(current) = session.project_client.bindings.get_mut(&open_id) {
                if current.context != review.binding.context || current.source_revision != review.binding.source_revision {
                    return Err(error("STALE_REVIEW: The project source binding changed"));
                }
                current.baseline = review.binding.baseline;
            } else {
                session.project_client.bindings.insert(open_id, review.binding);
            }
            Ok(result)
        },
    )
}
fn chosen_edits(review: &CapturedReview, choices: &BTreeMap<usize, String>) -> Result<BTreeMap<String, serde_json::Map<String, Value>>> {
    if review.external.dirty {
        return Err(error("TARGET_DIRTY: Save this GUI in Defold, then check project changes again"));
    }
    if choices.keys().any(|index| review.items.get(*index).is_none()) {
        return Err(error("Unknown GUI review choice"));
    }
    let mut edits = BTreeMap::<String, serde_json::Map<String, Value>>::new();
    for item in &review.items {
        if let Some(reason) = &item.blocker {
            return Err(error(reason));
        }
        let difference = &item.difference;
        let take = match difference.kind {
            DifferenceKind::LocalOnly | DifferenceKind::SameChange => false,
            DifferenceKind::ExternalOnly => {
                if !item.can_take_theirs {
                    return Err(error("Defold's change cannot safely replace painted artwork"));
                }
                true
            }
            DifferenceKind::Conflict => match choices.get(&item.index).map(String::as_str) {
                Some("mine") if item.can_keep_mine => false,
                Some("theirs") if item.can_take_theirs => true,
                _ => return Err(error("Choose Craft or Defold for every resolvable conflicting property")),
            },
        };
        if take {
            let node = difference.node.as_ref().ok_or_else(|| error("Scene-level changes cannot be merged automatically"))?;
            let value = difference.external.clone().ok_or_else(|| error("Node deletion requires manual reconciliation"))?;
            edits.entry(node.clone()).or_default().insert(difference.property.clone(), value);
        }
    }
    Ok(edits)
}
fn native_edits(
    review: &CapturedReview,
    projected: &BTreeMap<String, serde_json::Map<String, Value>>,
) -> Result<BTreeMap<String, serde_json::Map<String, Value>>> {
    let mut edits = projected.clone();
    for (node, values) in &mut edits {
        for (property, value) in values {
            if matches!(property.as_str(), "position.x" | "position.y" | "rotation.z" | "scale.x" | "scale.y") {
                let authored =
                    review.projection.authored.property(node, property).map_err(error)?.as_f64().ok_or_else(|| error("Invalid authored transform"))?;
                let rendered = review.projection.scene.property(node, property).map_err(error)?.as_f64().ok_or_else(|| error("Invalid projected transform"))?;
                let desired = value.as_f64().ok_or_else(|| error("Invalid Defold transform"))?;
                let translated = if property.starts_with("scale.") {
                    if rendered.abs() < 1e-8 {
                        return Err(error("Zero-scale transforms require manual reconciliation"));
                    }
                    authored * desired / rendered
                } else {
                    authored + desired - rendered
                };
                if !translated.is_finite() {
                    return Err(error("Invalid merged transform"));
                }
                *value = json!(translated);
            }
        }
    }
    Ok(edits)
}
pub fn apply(session: &mut Session, params: &Value) -> Result<Value> {
    let expected = params["reviewId"].as_str().ok_or_else(|| error("Check project changes before applying a review"))?.to_owned();
    let choices: BTreeMap<usize, String> = serde_json::from_value(params.get("choices").cloned().unwrap_or_else(|| json!({}))).map_err(error)?;
    let state = session.active().ok_or(EngineError::NoDocument)?;
    if state.floating.is_some() {
        return Err(error("Finish the floating selection before reviewing project changes"));
    }
    let snapshot = Arc::clone(&state.doc);
    let (document, open_id, revision) = (state.doc.id, state.open_id, state.revision);
    let previous = session.project_client.bindings.get(&open_id).cloned();
    let original_source = previous.as_ref().and_then(|binding| binding.source_revision.clone());
    crate::jobs::run(
        session,
        "Apply reviewed Defold changes",
        true,
        move |_| {
            let mut doc = snapshot.as_ref().clone();
            let binding = match previous {
                Some(binding) => binding,
                None => client::bind_document(&mut doc)?,
            };
            let review = capture(&doc, binding, revision, open_id)?;
            if review.id != expected {
                return Err(error("STALE_REVIEW: Craft, Defold or the saved source changed; check project changes again"));
            }
            let projected = chosen_edits(&review, &choices)?;
            let edits = native_edits(&review, &projected)?;
            for (node, values) in &edits {
                crate::defold_design::apply_node_properties(&mut doc, node, values)?;
            }
            let verified = review_projection(&doc, &review.binding)?;
            for (node, values) in &projected {
                for (property, value) in values {
                    let actual = verified.scene.property(node, property).map_err(error)?;
                    if !conflicts::equivalent(Some(&actual), Some(value)) {
                        return Err(error(format!(
                            "The native projection cannot safely represent {node}.{property}; keep the draft and reconcile this property manually"
                        )));
                    }
                }
            }
            let mut expected_scene = review.projection.scene.clone();
            for (node, values) in &projected {
                for (property, value) in values {
                    expected_scene = expected_scene.edit(node, property, value).map_err(error)?;
                }
            }
            let unintended = conflicts::review(&expected_scene.proto.source, &verified.scene.proto.source, &expected_scene.proto.source, &Default::default())?;
            if let Some(difference) = unintended.differences.first() {
                return Err(error(format!(
                    "The native projection also changes {}.{}; keep the draft and reconcile this dependent change manually",
                    difference.node.as_deref().unwrap_or("GUI"),
                    difference.property
                )));
            }
            let request = SceneRequest {
                context: review.binding.context.clone(),
                scene_id: review.binding.scene.scene_id.clone(),
                path: review.binding.scene.path.clone(),
            };
            let current = review.binding.store.snapshot(&request).map_err(error)?;
            if current != review.external || current_source_head(&review.binding)? != review.binding.source_revision {
                return Err(error("STALE_REVIEW: Defold or the saved source changed while applying; check again"));
            }
            Ok((review, edits))
        },
        move |session, (mut review, edits)| {
            let index = session
                .documents()
                .iter()
                .position(|state| state.doc.id == document && state.open_id == open_id && state.revision == revision)
                .ok_or_else(|| error("STALE_REVIEW: The native draft changed; no reviewed changes were applied"))?;
            if session
                .project_client
                .bindings
                .get(&open_id)
                .is_some_and(|binding| binding.context != review.binding.context || binding.source_revision != original_source)
            {
                return Err(error("STALE_REVIEW: The project source binding changed"));
            }
            let focused = session.active_index();
            session.set_active(index);
            let applied = session.edit("Apply reviewed Defold changes", |doc, _| {
                for (node, values) in &edits {
                    crate::defold_design::apply_node_properties(doc, node, values)?;
                }
                Ok(())
            });
            if let Some(focused) = focused {
                session.set_active(focused);
            }
            applied?;
            review.binding.baseline = SceneBaseline {
                scene_id: review.external.scene_id,
                path: review.external.path,
                gui_source: review.external.source,
                disk_hash: review.external.disk_hash,
                scene_hash: review.external.scene_hash,
                published_revision: review.binding.baseline.published_revision.clone(),
            };
            if let Some(current) = session.project_client.bindings.get_mut(&open_id) {
                current.baseline = review.binding.baseline;
            } else {
                session.project_client.bindings.insert(open_id, review.binding);
            }
            Ok(
                json!({"applied":true,"reviewId":review.id,"documentId":document.0,"openId":open_id,"capturedNativeRevision":revision,"properties":edits.values().map(|values|values.len()).sum::<usize>()}),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn related_parent_and_child_transforms_are_blocked_before_native_mutation() {
        let base = "nodes{id:\"parent\" type:TYPE_BOX position{x:10}} nodes{id:\"child\" parent:\"parent\" type:TYPE_BOX position{x:5}}";
        let local = base.replace("x:10", "x:20");
        let external = base.replace("x:5", "x:15");
        let reviewed = conflicts::review(base, &local, &external, &Default::default()).unwrap();
        let items = classify(reviewed, &Scene::parse(local).unwrap());
        assert!(items.iter().any(|item| item.difference.node.as_deref() == Some("child") && item.blocker.is_some()));
    }
}
