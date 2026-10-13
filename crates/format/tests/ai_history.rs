use photocraft_doc::{
    ColorMode, Document, Layer, Rect, SampleType, Size,
    ai::{GenerationGroup, GenerationVersion},
};
use std::sync::Arc;

fn fixture() -> Document {
    let mut doc = Document::new("AI history", Size::new(64, 48), ColorMode::Rgb, SampleType::U16);
    let layer = Layer::raster("AI 1", doc.pixel_format());
    let id = layer.id;
    doc.layers.push(layer);
    doc.ai_generations.push(GenerationGroup {
        id: 1,
        canvas: doc.size,
        area: Rect::new(0, 40, 64, 48),
        frame: Some(Rect::new(0, -8, 64, 56)),
        selection: None,
        reference_png: Some(Arc::new(vec![1, 2, 3])),
        mode: Default::default(),
        context_png: None,
        versions: vec![GenerationVersion {
            layer: Some(id),
            prompt: "original brief".into(),
            optimized_prompt: "optimized brief".into(),
            provider: "codex".into(),
            model: "selected-model".into(),
        }],
    });
    doc
}

#[test]
fn history_roundtrips_and_follows_remapped_layer_ids() {
    let doc = fixture();
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(doc.ai_generations, loaded.ai_generations);
    let remapped = photocraft_format::load_from_bytes_with(&bytes, &photocraft_format::LoadOptions { preserve_ids: false, ..Default::default() }).unwrap();
    assert_ne!(doc.layers[0].id, remapped.layers[0].id);
    assert_eq!(remapped.ai_generations[0].versions[0].layer, Some(remapped.layers[0].id));
    assert_eq!(doc.ai_generations[0].reference_png, remapped.ai_generations[0].reference_png);
}

#[test]
fn older_projects_default_to_no_history_and_invalid_records_cannot_be_saved() {
    let mut doc = fixture();
    doc.ai_generations.clear();
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    assert!(photocraft_format::load_from_bytes(&bytes).unwrap().ai_generations.is_empty());
    for change in 0..4 {
        let mut doc = fixture();
        match change {
            0 => doc.ai_generations[0].versions[0].prompt = "x".repeat(4097),
            1 => doc.ai_generations[0].frame = Some(Rect::new(i32::MIN, 0, i32::MAX, 1)),
            2 => doc.ai_generations.push(doc.ai_generations[0].clone()),
            _ => doc.ai_generations[0].reference_png = Some(Arc::new(vec![0; photocraft_doc::ai::MAX_REFERENCE_BYTES + 1])),
        }
        assert!(photocraft_format::save_to_bytes(&doc, &Default::default()).is_err());
    }
}
