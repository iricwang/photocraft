//! Portable project context, independent from any host's Codex credentials or thread IDs.
use crate::{CommandSpec, EngineError, Result, Session};
use base64::Engine as _;
use photocraft_doc::ai::{MAX_PROJECT_REFERENCES, MAX_REFERENCE_BYTES, ProjectReference};
use serde_json::{Value, json};
use std::sync::Arc;

fn bad(id: &str, why: &str) -> EngineError {
    EngineError::BadParams { cmd: id.into(), msg: why.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document open")?;
    if s.active_job().is_some() {
        return Err("wait for the document operation to finish".into());
    }
    if crate::float_cmds::floating(st).is_some() {
        return Err("finish moving the floating selection first".into());
    }
    Ok(())
}

fn fresh_id() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| EngineError::Other(format!("cannot create project identity: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn ensure(s: &mut Session, _: &Value) -> Result<Value> {
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    if st.doc.ai_project.id.is_empty() {
        let id = fresh_id()?;
        let memory = &mut Arc::make_mut(&mut st.doc).ai_project;
        memory.id = id;
        memory.record("project", "Linked AI project memory");
        st.revision += 1;
        st.last_damage = Some(photocraft_geom::Rect::EMPTY);
    }
    Ok(json!({"id": st.doc.ai_project.id}))
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.referenceAdd";
    let name = p.get("name").and_then(Value::as_str).ok_or_else(|| bad(ID, "name is required"))?;
    if name.is_empty() || name.len() > 480 || name.chars().any(char::is_control) {
        return Err(bad(ID, "reference name must be 1-480 bytes without control characters"));
    }
    let name = name.rsplit(['/', '\\']).next().unwrap_or("Reference").to_owned();
    let encoded = p.get("imageBase64").and_then(Value::as_str).ok_or_else(|| bad(ID, "imageBase64 is required"))?;
    if encoded.len() > MAX_REFERENCE_BYTES.div_ceil(3) * 4 {
        return Err(bad(ID, "reference exceeds 8 MiB"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| bad(ID, "invalid base64 image"))?;
    if bytes.len() > MAX_REFERENCE_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(bad(ID, "reference must be a PNG no larger than 8 MiB"));
    }
    let limits = photocraft_codecs::Limits { max_width: 1536, max_height: 1536, max_pixels: 1536 * 1536, max_alloc: 64 << 20 };
    photocraft_codecs::decode_with(&bytes, &photocraft_codecs::DecodeOptions { limits, ..Default::default() })
        .map_err(|e| bad(ID, &format!("invalid reference PNG: {e}")))?;
    let state = s.active().ok_or(EngineError::NoDocument)?;
    if state.doc.ai_project.references.len() >= MAX_PROJECT_REFERENCES {
        return Err(bad(ID, "up to 4 reference images"));
    }
    let project_id = if state.doc.ai_project.id.is_empty() { fresh_id()? } else { state.doc.ai_project.id.clone() };
    let id = state.doc.ai_project.references.iter().map(|r| r.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| bad(ID, "reference ids exhausted"))?;
    s.edit("Add Reference Image", |doc, _| {
        doc.ai_project.id = project_id;
        doc.ai_project.references.push(ProjectReference { id, name, png: Arc::new(bytes) });
        Ok(json!({"id":id}))
    })
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.referenceRemove";
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(ID, "id is required"))?;
    if !s.active().ok_or(EngineError::NoDocument)?.doc.ai_project.references.iter().any(|r| r.id == id) {
        return Err(bad(ID, "reference was not found"));
    }
    s.edit("Remove Reference Image", |doc, _| {
        doc.ai_project.references.retain(|r| r.id != id);
        Ok(json!({"id":id}))
    })
}

fn record(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "ai.projectRecord";
    let kind = p.get("kind").and_then(Value::as_str).ok_or_else(|| bad(ID, "kind is required"))?;
    let summary = p.get("summary").and_then(Value::as_str).ok_or_else(|| bad(ID, "summary is required"))?;
    if kind.is_empty() || kind.len() > 64 || summary.len() > 16384 || kind.chars().any(char::is_control) {
        return Err(bad(ID, "kind or summary is too large or invalid"));
    }
    let prompt = p.get("prompt").map(|v| v.as_str().ok_or_else(|| bad(ID, "prompt must be a string"))).transpose()?;
    if prompt.is_some_and(|p| p.len() > 4096) {
        return Err(bad(ID, "prompt exceeds 4096 bytes"));
    }
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    let memory = &mut Arc::make_mut(&mut st.doc).ai_project;
    memory.record(kind, summary);
    if let Some(prompt) = prompt {
        memory.last_prompt = prompt.into();
    }
    st.revision += 1;
    st.last_damage = Some(photocraft_geom::Rect::EMPTY);
    Ok(json!({"sequence":memory.next_sequence.saturating_sub(1)}))
}

pub(crate) fn record_edit(doc: &mut photocraft_doc::Document, label: &str, active: Option<photocraft_doc::LayerId>) {
    if doc.ai_project.id.is_empty() {
        return;
    }
    let target = active.and_then(|id| doc.layer(id)).map(|l| format!("; layer {}: {}", l.id.0, l.name)).unwrap_or_default();
    let summary = format!("{label}; canvas {}x{}; {} root layers{target}", doc.size.width, doc.size.height, doc.layers.len());
    doc.ai_project.record("edit", &summary);
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ai.projectEnsure",
            label: "Link AI Project",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: has_doc,
            run: ensure,
            journal: false,
        },
        CommandSpec {
            id: "ai.referenceAdd",
            label: "Add Reference Image",
            menu: &[],
            shortcut: None,
            params: r#"{"name":string,"imageBase64":string}"#,
            enabled: has_doc,
            run: add,
            journal: false,
        },
        CommandSpec {
            id: "ai.referenceRemove",
            label: "Remove Reference Image",
            menu: &[],
            shortcut: None,
            params: r#"{"id":positive-integer}"#,
            enabled: has_doc,
            run: remove,
            journal: false,
        },
        CommandSpec {
            id: "ai.projectRecord",
            label: "Record AI Project Event",
            menu: &[],
            shortcut: None,
            params: r#"{"kind":string,"summary":string,"prompt":string?}"#,
            enabled: has_doc,
            run: record,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":8,"height":8})).unwrap();
        s
    }
    fn image() -> String {
        let image = photocraft_codecs::Image::from_u8(1, 1, photocraft_codecs::ChannelLayout::Rgba, vec![50, 100, 150, 255]).unwrap();
        base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
    }
    #[test]
    fn references_undo_but_project_identity_and_audit_do_not() {
        let mut s = session();
        let id = s.execute("ai.projectEnsure", json!({})).unwrap()["id"].clone();
        s.execute("ai.referenceAdd", json!({"name":"style.png","imageBase64":image()})).unwrap();
        s.execute("ai.projectRecord", json!({"kind":"user","summary":"make a player","prompt":"make a player"})).unwrap();
        let seq = s.active().unwrap().doc.ai_project.next_sequence;
        assert!(s.undo());
        let memory = &s.active().unwrap().doc.ai_project;
        assert!(memory.references.is_empty());
        assert_eq!(memory.id, id.as_str().unwrap());
        assert_eq!(memory.last_prompt, "make a player");
        assert!(memory.next_sequence > seq);
        assert!(s.redo());
        assert_eq!(s.active().unwrap().doc.ai_project.references.len(), 1);
        assert_eq!(s.active().unwrap().doc.ai_project.events.last().unwrap().kind, "redo");
    }
    #[test]
    fn malformed_references_and_limits_do_not_modify_document() {
        let mut s = session();
        let before = s.active().unwrap().doc.clone();
        for value in ["abc".to_owned(), base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\ncorrupt")] {
            assert!(s.execute("ai.referenceAdd", json!({"name":"x.png","imageBase64":value})).is_err());
            assert_eq!(s.active().unwrap().doc, before);
        }
        for _ in 0..4 {
            s.execute("ai.referenceAdd", json!({"name":"x.png","imageBase64":image()})).unwrap();
        }
        assert!(s.execute("ai.referenceAdd", json!({"name":"x.png","imageBase64":image()})).is_err());
    }
    #[test]
    fn existing_link_is_read_only_and_reference_changes_are_saved_document_edits() {
        let mut s = session();
        s.execute("ai.projectEnsure", json!({})).unwrap();
        let before = s.active().unwrap().doc.clone();
        let revision = s.active().unwrap().revision;
        s.execute("ai.projectEnsure", json!({})).unwrap();
        assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
        assert_eq!(revision, s.active().unwrap().revision);
        s.active_mut().unwrap().saved_to("renamed.psd".into());
        assert!(!s.active().unwrap().is_dirty());
        assert_eq!(s.active().unwrap().doc.ai_project.id, before.ai_project.id);
        let reference = s.execute("ai.referenceAdd", json!({"name":"x.png","imageBase64":image()})).unwrap();
        assert!(s.active().unwrap().is_dirty());
        s.active_mut().unwrap().saved_to("renamed.psd".into());
        s.execute("ai.referenceRemove", json!({"id":reference["id"]})).unwrap();
        assert!(s.active().unwrap().is_dirty());
    }
    #[test]
    fn reopening_a_project_gets_a_new_admission_identity() {
        let mut s = session();
        s.execute("ai.projectEnsure", json!({})).unwrap();
        let saved = (*s.active().unwrap().doc).clone();
        let open_id = s.active().unwrap().open_id;
        s.close(0);
        s.add_document(saved.clone(), Some("project.psd".into()));
        let reopened = s.active().unwrap();
        assert_eq!(reopened.doc.id, saved.id);
        assert_eq!(reopened.doc.ai_project.id, saved.ai_project.id);
        assert_ne!(reopened.open_id, open_id);
    }
    #[test]
    fn audit_ring_is_bounded_and_utf8_safe() {
        let mut memory = photocraft_doc::ai::ProjectMemory::default();
        for _ in 0..300 {
            memory.record("edit", &"界".repeat(2000));
        }
        assert_eq!(memory.events.len(), 256);
        assert!(memory.is_valid());
    }
    #[test]
    fn asynchronous_edits_record_their_original_document_and_duplicates_fork() {
        let mut s = session();
        s.execute("ai.projectEnsure", json!({})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let before = s.active().unwrap().doc.ai_project.next_sequence;
        let crate::jobs::Started::Job(job) = s.start("filter.blur.gaussianBlur", json!({"radius":2})).unwrap() else { panic!("expected a worker") };
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let other = s.active_index().unwrap();
        s.wait_job(job).unwrap();
        assert_eq!(s.active_index(), Some(other));
        assert!(s.documents()[0].doc.ai_project.next_sequence > before);
        assert!(s.documents()[1].doc.ai_project.events.is_empty());
        s.set_active(0);
        let original_id = s.active().unwrap().doc.ai_project.id.clone();
        s.execute("image.duplicate", json!({})).unwrap();
        assert!(s.active().unwrap().doc.ai_project.id.is_empty());
        s.execute("ai.projectEnsure", json!({})).unwrap();
        assert_ne!(s.active().unwrap().doc.ai_project.id, original_id);
    }
}
