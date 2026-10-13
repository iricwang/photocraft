//! Persistent generation families and exclusive version selection.
use crate::{CommandSpec, EngineError, Result, Session};
use photocraft_doc::{LayerId, ai::GenerationGroup};
use serde_json::{Value, json};

fn bad(msg: &str) -> EngineError {
    EngineError::Other(msg.into())
}

pub(crate) fn validate_group(g: &GenerationGroup) -> Result<()> {
    if !g.is_valid() {
        return Err(bad("invalid saved AI generation geometry or metadata"));
    }
    for bytes in [&g.reference_png, &g.context_png].into_iter().flatten() {
        let edge = if g.mode == photocraft_doc::ai::GenerationMode::Component { 2048 } else { 1024 };
        if photocraft_codecs::detect(bytes) != Some(photocraft_codecs::Format::Png) {
            return Err(bad("saved AI reference is not a PNG"));
        }
        photocraft_codecs::decode_with(
            bytes,
            &photocraft_codecs::DecodeOptions {
                limits: photocraft_codecs::Limits { max_width: edge, max_height: edge, max_pixels: u64::from(edge) * u64::from(edge), max_alloc: 64 << 20 },
                ..Default::default()
            },
        )
        .map_err(|e| bad(&e.to_string()))?;
    }
    Ok(())
}

fn group_id(p: &Value, keys: &[&str]) -> Result<u64> {
    let obj = p.as_object().ok_or_else(|| bad("params must be an object"))?;
    if obj.keys().any(|k| !keys.contains(&k.as_str())) {
        return Err(bad("unknown history parameter"));
    }
    p.get("groupId").and_then(Value::as_u64).filter(|id| *id > 0).ok_or_else(|| bad("positive groupId required"))
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let group_id = group_id(p, &["groupId", "layerId"])?;
    let chosen = match p.get("layerId") {
        None | Some(Value::Null) => None,
        Some(v) => Some(LayerId(v.as_u64().filter(|id| *id > 0).ok_or_else(|| bad("layerId must be positive or null for Original"))?)),
    };
    s.edit("Select AI Version", |doc, active| {
        let g = doc.ai_generations.iter().find(|g| g.id == group_id).ok_or_else(|| bad("generation group not found"))?;
        let ids: Vec<_> = g.versions.iter().filter_map(|v| v.layer).collect();
        if let Some(id) = chosen
            && (!ids.contains(&id) || doc.layer(id).is_none())
        {
            return Err(bad("generation layer was deleted or does not belong to this group"));
        }
        for id in ids {
            if let Some(layer) = doc.layer_mut(id) {
                layer.visible = Some(id) == chosen;
            }
        }
        if let Some(id) = chosen {
            *active = Some(id);
        }
        Ok(json!({"groupId":group_id,"layerId":chosen.map(|id| id.0)}))
    })
}

fn forget(s: &mut Session, p: &Value) -> Result<Value> {
    let group_id = group_id(p, &["groupId"])?;
    s.edit("Remove AI Record", |doc, _| {
        let index = doc.ai_generations.iter().position(|g| g.id == group_id).ok_or_else(|| bad("generation group not found"))?;
        doc.ai_generations.remove(index);
        Ok(json!({"groupId":group_id,"removed":true}))
    })
}

fn history(s: &mut Session, _: &Value) -> Result<Value> {
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    Ok(json!({"documentId":doc.id.0,"groups":doc.ai_generations.iter().map(|g| json!({
        "groupId":g.id,"area":g.area,"canvas":g.canvas,"mode":g.mode,"hasReference":g.reference_png.is_some(),
        "versions":g.versions.iter().map(|v| json!({"layerId":v.layer.map(|l|l.0),"prompt":v.prompt,
            "optimizedPrompt":v.optimized_prompt,"provider":v.provider,"model":v.model,
            "available":v.layer.is_some_and(|id|doc.layer(id).is_some()),
            "visible":v.layer.and_then(|id|doc.layer(id)).is_some_and(|l|l.visible)})).collect::<Vec<_>>()
    })).collect::<Vec<_>>()}))
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ai.retryGeneration",
            label: "Retry AI Generation",
            menu: &[],
            shortcut: None,
            params: r#"{"groupId":integer,"prompt":string,"provider":"codex"|"images-api","model":string=""}"#,
            enabled,
            run: crate::ai_cmds::retry,
            journal: false,
        },
        CommandSpec {
            id: "ai.selectVersion",
            label: "Select AI Version",
            menu: &[],
            shortcut: None,
            params: r#"{"groupId":integer,"layerId":integer|null}"#,
            enabled,
            run: select,
            journal: true,
        },
        CommandSpec {
            id: "ai.forgetHistory",
            label: "Remove AI Record",
            menu: &[],
            shortcut: None,
            params: r#"{"groupId":integer} // keeps result layers"#,
            enabled,
            run: forget,
            journal: true,
        },
        CommandSpec { id: "ai.history", label: "AI Generation History", menu: &[], shortcut: None, params: "{}", enabled, run: history, journal: false },
    ]
}
