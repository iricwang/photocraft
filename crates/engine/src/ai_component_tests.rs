use crate::Session;
use base64::Engine as _;
use photocraft_doc::{LayerId, ai::GenerationMode};
use serde_json::json;

fn asset(alpha: u8, background: u8) -> String {
    let mut bytes = vec![background; 40 * 20 * 4];
    for y in 8..12 {
        for x in 4..36 {
            let i = (y * 40 + x) * 4;
            bytes[i..i + 4].copy_from_slice(&[220, 40, 80, alpha]);
        }
    }
    let image = photocraft_codecs::Image::from_u8(40, 20, photocraft_codecs::ChannelLayout::Rgba, bytes).unwrap();
    base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
}

#[test]
fn transparent_component_keeps_alpha_geometry_and_both_references_after_reopen() {
    for depth in [8, 16, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":128,"height":80,"depth":depth})).unwrap();
        s.execute("select.rect", json!({"x":0,"y":60,"width":128,"height":16})).unwrap();
        let original = s.active().unwrap().doc.clone();
        let begin = s.execute("ai.beginGeneration", json!({"prompt":"raised controls","provider":"codex","mode":"component","destination":"layer"})).unwrap();
        assert_eq!(begin["mode"], "component");
        assert!(!begin["editInstructions"].as_str().unwrap().contains("ENTIRE square"));
        for (key, size) in [("sourcePng", (128, 16)), ("contextPng", (128, 80))] {
            let bytes = base64::engine::general_purpose::STANDARD.decode(begin[key].as_str().unwrap()).unwrap();
            let image = photocraft_codecs::decode(&bytes).unwrap();
            assert_eq!((image.width(), image.height()), size);
        }
        let receive = s.execute("ai.receiveCandidate", json!({"candidateId":begin["candidateId"],"imageBase64":asset(128, 0)})).unwrap();
        let id = LayerId(receive["layerId"].as_u64().unwrap());
        let doc = &s.active().unwrap().doc;
        assert_eq!(doc.layers[0], original.layers[0]);
        let surface = doc.layer(id).unwrap().surface().unwrap();
        assert!((surface.rgba(64, 68)[3] - 128.0 / 255.0).abs() < 0.02);
        assert_eq!(surface.rgba(64, 59)[3], 0.0);
        assert_eq!(surface.rgba(64, 76)[3], 0.0);
        assert_eq!(doc.ai_generations[0].mode, GenerationMode::Component);
        let saved = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
        let loaded = photocraft_format::load_from_bytes(&saved).unwrap();
        assert_eq!(loaded.ai_generations, doc.ai_generations);
        let mut reopened = Session::new();
        reopened.add_document(loaded, None);
        reopened.execute("select.deselect", json!({})).unwrap();
        let retry = reopened.execute("ai.retryGeneration", json!({"groupId":receive["groupId"],"provider":"codex","prompt":"softer highlights"})).unwrap();
        for key in ["sourcePng", "contextPng", "mode", "editInstructions"] {
            assert_eq!(begin[key], retry[key]);
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len());
        s.execute("edit.redo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layer(id).is_some());
    }
}

#[test]
fn transparent_component_rejects_fake_transparency_and_requires_selection() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":80,"height":60})).unwrap();
    let params = json!({"prompt":"raised controls","provider":"codex","mode":"component","destination":"layer"});
    assert!(s.execute("ai.beginGeneration", params.clone()).is_err());
    s.execute("select.rect", json!({"x":0,"y":40,"width":80,"height":16})).unwrap();
    let original = s.active().unwrap().doc.clone();
    for image in [asset(255, 255), asset(0, 0)] {
        let begin = s.execute("ai.beginGeneration", params.clone()).unwrap();
        assert!(s.execute("ai.receiveCandidate", json!({"candidateId":begin["candidateId"],"imageBase64":image})).is_err());
        assert_eq!(s.active().unwrap().doc.layers, original.layers);
        s.execute("ai.cancelGeneration", json!({"candidateId":begin["candidateId"]})).unwrap();
    }
}

#[test]
fn redesigned_bar_can_grow_upward_without_changing_saved_selection() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":128,"height":80})).unwrap();
    s.execute("select.rect", json!({"x":0,"y":72,"width":128,"height":8})).unwrap();
    let begin = s.execute("ai.beginGeneration", json!({"prompt":"redesign","provider":"codex","mode":"component","destination":"layer"})).unwrap();
    let receipt = s.execute("ai.receiveCandidate", json!({"candidateId":begin["candidateId"],"imageBase64":asset(255,0),"expandComponent":true})).unwrap();
    let doc = &s.active().unwrap().doc;
    let surface = doc.layer(LayerId(receipt["layerId"].as_u64().unwrap())).unwrap().surface().unwrap();
    assert!(surface.rgba(64, 66)[3] > 0.9, "new bar must not be shrunk into the old 8-pixel height");
    assert_eq!(surface.rgba(64, 63)[3], 0.0);
    assert_eq!(doc.ai_generations[0].area, photocraft_geom::Rect::from_xywh(0, 72, 128, 8));
    let next = s.execute("ai.beginGeneration", json!({"prompt":"redesign","provider":"codex","mode":"component","destination":"layer"})).unwrap();
    let receipt = s.execute("ai.receiveCandidate", json!({"candidateId":next["candidateId"],"imageBase64":asset(255,0)})).unwrap();
    let doc = &s.active().unwrap().doc;
    let surface = doc.layer(LayerId(receipt["layerId"].as_u64().unwrap())).unwrap().surface().unwrap();
    assert_eq!(surface.rgba(64, 66)[3], 0.0, "default placement must not cover neighboring content");
}

#[test]
fn transparent_component_alpha_dust_does_not_shrink_the_subject() {
    use photocraft_doc::{Document, Layer, LayerContent};
    use photocraft_geom::{Rect, Size};
    for opacity in [0.5, 1.0] {
        let mut image = Document::new("Provider result", Size::new(100, 100), photocraft_doc::ColorMode::Rgb, photocraft_color::SampleType::F32);
        let mut pixels = photocraft_raster::Surface::new(image.pixel_format());
        pixels.write_pixel(0, 0, &[1.0, 1.0, 1.0, 1.0 / 255.0]);
        pixels.write_pixel(99, 99, &[1.0, 1.0, 1.0, 1.0 / 255.0]);
        for y in 40..60 {
            for x in 5..95 {
                pixels.write_pixel(x, y, &[0.0, 0.0, 0.0, opacity * 0.1]);
            }
        }
        for y in 45..55 {
            for x in 10..90 {
                pixels.write_pixel(x, y, &[1.0, 0.0, 0.0, opacity]);
            }
        }
        image.layers.push(Layer::new("Component", LayerContent::Raster(pixels)));
        let area = Rect::from_xywh(0, 0, 160, 20);
        let (placed, _) = crate::ai_component::mapped_result(&image, area, area, image.pixel_format()).unwrap();
        for x in [0, 80, 159] {
            assert!((placed.rgba(x, 10)[3] - opacity).abs() < 0.01);
        }
        assert_eq!(placed.rgba(80, 20)[3], 0.0);
    }
}
