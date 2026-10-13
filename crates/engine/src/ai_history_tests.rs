use crate::Session;
use base64::Engine as _;
use photocraft_doc::{LayerId, Rect};
use serde_json::{Value, json};

fn image() -> String {
    let image = photocraft_codecs::Image::from_u8(4, 4, photocraft_codecs::ChannelLayout::Rgba, [200, 30, 10, 255].repeat(16)).unwrap();
    base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
}

fn begin(s: &mut Session) -> Value {
    s.execute("ai.beginGeneration", json!({"provider":"codex","model":"selected-model","prompt":"raised playback bar","destination":"layer"})).unwrap()
}

fn finish(s: &mut Session, start: &Value) -> Value {
    s.execute("ai.receiveCandidate",json!({"candidateId":start["candidateId"],"imageBase64":image(),"optimizedPrompt":"Preserve geometry; add bevel and highlights.","model":"resolved-model"})).unwrap()
}

#[test]
fn versions_survive_reopen_and_retry_uses_frozen_reference_and_selection() {
    for depth in [8, 16, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":80,"height":48,"depth":depth})).unwrap();
        s.execute("select.rect", json!({"x":0,"y":40,"width":80,"height":8})).unwrap();
        let start = begin(&mut s);
        let one = finish(&mut s, &start);
        let first = LayerId(one["layerId"].as_u64().unwrap());
        let gid = one["groupId"].clone();
        let initial_group = s.active().unwrap().doc.ai_generations[0].clone();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("layer.new.layer", json!({"name":"Unrelated edits"})).unwrap();
        s.execute("edit.fill", json!({"contents":"white"})).unwrap();
        let unrelated = s.active().unwrap().active_layer.unwrap();
        let retry_params = json!({"groupId":gid,"provider":"codex","model":"another-model","prompt":"softer lighting"});
        let retry = s.execute("ai.retryGeneration", retry_params.clone()).unwrap();
        assert_eq!(retry["sourcePng"], start["sourcePng"]);
        assert_eq!(retry["editInstructions"], start["editInstructions"]);
        let two = finish(&mut s, &retry);
        let second = LayerId(two["layerId"].as_u64().unwrap());
        let doc = &s.active().unwrap().doc;
        assert!(!doc.layer(first).unwrap().visible);
        assert!(doc.layer(second).unwrap().visible);
        assert!(doc.layer(unrelated).unwrap().visible);
        assert_eq!(doc.layer(second).unwrap().surface().unwrap().rgba(0, 0)[3], 0.0);
        assert!(doc.layer(second).unwrap().surface().unwrap().rgba(0, 40)[3] > 0.99);
        assert_eq!(doc.ai_generations[0].versions[1].prompt, "softer lighting");
        assert_eq!(doc.ai_generations[0].versions[1].model, "resolved-model");
        assert_eq!(doc.ai_generations[0].reference_png, initial_group.reference_png);
        s.execute("ai.selectVersion", json!({"groupId":gid,"layerId":first.0})).unwrap();
        assert!(s.active().unwrap().doc.layer(first).unwrap().visible);
        assert!(!s.active().unwrap().doc.layer(second).unwrap().visible);
        s.execute("ai.selectVersion", json!({"groupId":gid,"layerId":null})).unwrap();
        assert!(!s.active().unwrap().doc.layer(first).unwrap().visible);
        assert!(!s.active().unwrap().doc.layer(second).unwrap().visible);
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layer(first).unwrap().visible);
        s.execute("edit.redo", json!({})).unwrap();
        let bytes = photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap();
        let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
        assert_eq!(loaded.ai_generations, s.active().unwrap().doc.ai_generations);
        assert!(!loaded.layer(first).unwrap().visible);
        assert!(!loaded.layer(second).unwrap().visible);
        let mut reopened = Session::new();
        reopened.add_document(loaded, None);
        let again = reopened.execute("ai.retryGeneration", retry_params).unwrap();
        assert_eq!(again["sourcePng"], start["sourcePng"]);
        assert_eq!(again["area"], json!([0, 40, 80, 48]));
        let three = finish(&mut reopened, &again);
        assert_eq!(three["groupId"], gid);
        assert_eq!(reopened.active().unwrap().doc.ai_generations[0].versions.len(), 3);
        assert_eq!(reopened.active().unwrap().doc.ai_generations[0].selection, initial_group.selection);
    }
}

#[test]
fn history_handles_deleted_layers_undo_and_invalid_requests_without_losing_pixels() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":32,"height":24})).unwrap();
    let start = begin(&mut s);
    let receipt = finish(&mut s, &start);
    let before = s.active().unwrap().doc.clone();
    for params in [json!({}), json!({"groupId":0}), json!({"groupId":receipt["groupId"],"layerId":-1}), json!({"groupId":receipt["groupId"],"layerId":999999})]
    {
        assert!(s.execute("ai.selectVersion", params).is_err());
        assert_eq!(*s.active().unwrap().doc, *before);
    }
    let id = LayerId(receipt["layerId"].as_u64().unwrap());
    s.edit("Delete result fixture", |doc, _| {
        doc.layers.retain(|l| l.id != id);
        Ok(())
    })
    .unwrap();
    assert!(s.execute("ai.selectVersion", json!({"groupId":receipt["groupId"],"layerId":id.0})).is_err());
    assert_eq!(s.execute("ai.history", json!({})).unwrap()["groups"][0]["versions"][0]["available"], false);
    let bytes = photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap();
    let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(loaded.ai_generations[0].versions[0].layer, None);
    s.execute("ai.forgetHistory", json!({"groupId":receipt["groupId"]})).unwrap();
    assert!(s.active().unwrap().doc.ai_generations.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.ai_generations.len(), 1);
    s.edit("Resize fixture", |doc, _| {
        doc.size.width += 1;
        Ok(())
    })
    .unwrap();
    assert!(s.execute("ai.retryGeneration", json!({"groupId":receipt["groupId"],"prompt":"retry","provider":"codex"})).is_err());
    let mut malformed = before.ai_generations[0].clone();
    malformed.frame = Some(Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX));
    assert!(super::ai_history_cmds::validate_group(&malformed).is_err());
}

#[test]
fn duplicate_document_remaps_versions_and_merged_copy_has_no_stale_history() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":32,"height":24})).unwrap();
    let start = begin(&mut s);
    let first = finish(&mut s, &start);
    s.execute("image.duplicate", json!({})).unwrap();
    let record = &s.active().unwrap().doc.ai_generations[0];
    let layer = record.versions[0].layer.unwrap();
    assert_ne!(Some(layer.0), first["layerId"].as_u64());
    assert!(s.active().unwrap().doc.layer(layer).is_some());
    s.execute("ai.selectVersion", json!({"groupId":record.id,"layerId":null})).unwrap();
    assert!(!s.active().unwrap().doc.layer(layer).unwrap().visible);
    s.execute("image.duplicate", json!({"mergedOnly":true})).unwrap();
    assert!(s.active().unwrap().doc.ai_generations.is_empty());
}
