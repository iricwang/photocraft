use super::*;
use photocraft_color::{PixelFormat, SampleType};

#[test]
fn whole_visible_canvas_is_frozen_for_tools_but_not_attached_to_repaint() {
    for depth in [8, 16, 32] {
        let mut s = session(depth, "rgb");
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("layer.new.layer", json!({"name":"Visible green"})).unwrap();
        s.execute("edit.fill", json!({"contents":"color","color":[0.0,1.0,0.0,1.0]})).unwrap();
        s.execute("layer.new.layer", json!({"name":"Empty active layer"})).unwrap();
        let before = s.active().unwrap().doc.clone();
        let began = s
            .execute(
                "ai.beginGeneration",
                json!({"prompt":"Redraw the current screen using my reference","provider":"codex","reference":false,"destination":"layer"}),
            )
            .unwrap();
        assert!(began["sourcePng"].is_null());
        assert!(began["contextPng"].is_null(), "full canvas is available on demand, not an automatic image input");
        assert_eq!(began["canvas"], json!([before.size.width, before.size.height]));
        let frozen = began["canvasPng"].as_str().unwrap();
        let image = base64::engine::general_purpose::STANDARD.decode(frozen).unwrap();
        let canvas = photocraft_io::import("canvas.png", &image).unwrap().document;
        assert_eq!(canvas.size, before.size);
        let pixel = canvas.layers[0].surface().unwrap().rgba(5, 5);
        assert!(pixel[1] > 0.99 && pixel[3] > 0.99, "include visible lower layers even when the active layer is empty");
        let result = s.execute("ai.receiveCandidate", json!({"candidateId":began["candidateId"],"imageBase64":generated_image()})).unwrap();
        let retry = s.execute("ai.retryGeneration", json!({"groupId":result["groupId"],"prompt":"Try again","provider":"codex"})).unwrap();
        assert_eq!(retry["canvasPng"], frozen, "retry reads the same frozen artwork, not the newly generated layer");
        assert!(retry["contextPng"].is_null());
    }
}

#[test]
fn edit_prompts_only_identify_the_region_and_output_geometry() {
    let area = Rect::new(10, 70, 90, 90);
    let local = edit_instructions(area, Rect::new(0, 0, 100, 100));
    assert!(local.contains("x=10.000%..90.000%, y=70.000%..90.000%"));
    assert!(local.contains("1024x1024"));
    assert!(local.contains("outside the rectangle"));
    let component = crate::ai_component::instructions(area, photocraft_geom::Size::new(100, 100));
    assert!(component.contains("x=10.000%..90.000%, y=70.000%..90.000%"));
    assert!(component.contains("selected region: 80 x 20 pixels"));
    for prompt in [local, component] {
        for extra in ["3D", "bevel", "material", "lighting", "front-facing", "grid", "control count", "spacing", "separate buttons"] {
            assert!(!prompt.contains(extra), "unrequested direction: {extra}");
        }
    }
    let mut s = session(8, "rgb");
    let prompt = "Redesign the complete playback bar as one transparent asset";
    s.execute("ai.beginGeneration", json!({"prompt":prompt,"provider":"codex","mode":"component","destination":"layer"})).unwrap();
    assert_eq!(s.ai.pending.as_ref().unwrap().prompt, prompt);
}

#[test]
fn component_atlas_creates_persistent_independent_layers_without_changing_source() {
    let specs = crate::ai_assets::parse("Play\nPause", "Progress").unwrap();
    let mut image = Document::new("Atlas", photocraft_geom::Size::new(128, 128), photocraft_doc::ColorMode::Rgb, SampleType::U8);
    let mut surface = Surface::new(PixelFormat::RGBA8);
    for rect in [Rect::new(16, 16, 48, 48), Rect::new(80, 16, 112, 48), Rect::new(16, 80, 112, 112)] {
        surface.fill_rect(rect, &[0.2, 0.8, 0.1, 1.0]);
    }
    image.layers.push(photocraft_doc::Layer::new("Atlas", LayerContent::Raster(surface)));
    let encoded = base64::engine::general_purpose::STANDARD.encode(photocraft_io::export(&image, "atlas.png", &Default::default()).unwrap().bytes);
    for depth in [8, 16, 32] {
        let mut s = session(depth, "rgb");
        let began = s.execute("ai.beginGeneration", json!({"prompt":"3D controls","provider":"codex","mode":"component","destination":"layer"})).unwrap();
        let before = s.active().unwrap().doc.clone();
        for invalid in [json!([{"name":"Only one"}]), json!([{"name":"a"},{"name":"a"}]), json!(false)] {
            assert!(s.execute("ai.receiveCandidate", json!({"candidateId":began["candidateId"],"imageBase64":encoded,"components":invalid})).is_err());
            assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
            assert_eq!(s.documents().len(), 1);
        }
        let receipt = s.execute("ai.receiveCandidate", json!({"candidateId":began["candidateId"],"imageBase64":encoded,"components":specs})).unwrap();
        assert_eq!(receipt["components"].as_array().unwrap().len(), 3);
        assert_eq!(s.documents().len(), 2);
        assert!(Arc::ptr_eq(&before, &s.documents()[0].doc));
        let doc = &s.active().unwrap().doc;
        let children = doc.layers[0].children().unwrap();
        assert_eq!(children.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Play", "Pause", "Progress"]);
        assert!(children.iter().all(|l| l.visible));
        assert!(s.active().unwrap().is_dirty());
        let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
        let restored = photocraft_format::load_from_bytes(&bytes).unwrap();
        assert_eq!(restored.layers[0].children().unwrap().len(), 3);
        assert!(restored.layers[0].children().unwrap()[1].surface().unwrap().rgba(80, 16)[3] > 0.99);
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layers.is_empty());
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.layers[0].children().unwrap().len(), 3);
    }
}

fn generated_image() -> String {
    let image = photocraft_codecs::Image::from_u8(4, 4, photocraft_codecs::ChannelLayout::Rgba, [240, 40, 20, 255].repeat(16)).unwrap();
    base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
}

#[test]
fn thin_local_edits_crop_context_without_distortion_or_outside_changes() {
    for (width, height, area) in [(128, 80, Rect::new(8, 68, 120, 76)), (80, 128, Rect::new(68, 8, 76, 120))] {
        for depth in [8, 16, 32] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width":width,"height":height,"depth":depth})).unwrap();
            s.execute("select.rect", json!({"x":area.x0,"y":area.y0,"width":area.width(),"height":area.height()})).unwrap();
            let before = s.active().unwrap().doc.clone();
            let began =
                s.execute("ai.beginGeneration", json!({"prompt":"3D playback bar","provider":"codex","reference":false,"destination":"layer"})).unwrap();
            assert!(began["sourcePng"].is_string(), "local edits must never fall back to text-only generation");
            assert!(began["editInstructions"].as_str().unwrap().contains("entire square frame"));
            let frame = s.ai.pending.as_ref().unwrap().frame.unwrap();
            let mut output = Document::new(
                "Mock generated frame",
                photocraft_geom::Size::new(frame.width(), frame.height()),
                photocraft_doc::ColorMode::Rgb,
                SampleType::U8,
            );
            let mut surface = Surface::new(PixelFormat::RGBA8);
            surface.fill_rect(output.bounds(), &[0.0, 0.0, 1.0, 1.0]);
            let marker = Rect::new(area.x0 + 2 - frame.x0, area.y0 + 2 - frame.y0, area.x0 + 6 - frame.x0, area.y0 + 6 - frame.y0);
            surface.fill_rect(marker, &[1.0, 0.0, 0.0, 1.0]);
            output.layers.push(photocraft_doc::Layer::new("Frame", LayerContent::Raster(surface)));
            let encoded = base64::engine::general_purpose::STANDARD.encode(photocraft_io::export(&output, "frame.png", &Default::default()).unwrap().bytes);
            let receipt = s.execute("ai.receiveCandidate", json!({"candidateId":began["candidateId"],"imageBase64":encoded})).unwrap();
            let after = &s.active().unwrap().doc;
            let patch = after.layer(LayerId(receipt["layerId"].as_u64().unwrap())).unwrap().surface().unwrap();
            for y in 0..height {
                for x in 0..width {
                    let rgba = patch.rgba(x, y);
                    if x < area.x0 || x >= area.x1 || y < area.y0 || y >= area.y1 {
                        assert_eq!(rgba[3], 0.0);
                    } else if (area.x0 + 2..area.x0 + 6).contains(&x) && (area.y0 + 2..area.y0 + 6).contains(&y) {
                        assert!(rgba[0] > 0.99 && rgba[2] < 0.01, "4x4 marker must remain square: {x},{y}");
                    } else {
                        assert!(rgba[2] > 0.99 && rgba[0] < 0.01);
                    }
                }
            }
            assert_eq!(before.layers[0].surface().unwrap().read_region(before.bounds()), after.layers[0].surface().unwrap().read_region(after.bounds()));
        }
    }
}

#[test]
fn local_reference_uses_visible_composite_and_wrong_shape_does_not_mutate_document() {
    let mut s = session(8, "rgb");
    s.execute("layer.new.layer", json!({"name":"Visible overlay"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("edit.fill", json!({"contents":"color","color":[0.0,1.0,0.0,1.0]})).unwrap();
    s.execute("layer.new.layer", json!({"name":"Empty active"})).unwrap();
    s.execute("select.rect", json!({"x":8,"y":6,"width":12,"height":2})).unwrap();
    let began = s.execute("ai.beginGeneration", json!({"prompt":"polish","provider":"codex","destination":"layer"})).unwrap();
    let source = base64::engine::general_purpose::STANDARD.decode(began["sourcePng"].as_str().unwrap()).unwrap();
    let reference = photocraft_io::import("ref.png", &source).unwrap().document;
    let rgba = reference.layers[0].surface().unwrap().rgba(5, 5);
    assert!(rgba[1] > 0.99 && rgba[3] > 0.99, "must see composite under empty active layer");
    let before = s.active().unwrap().doc.clone();
    let image = photocraft_codecs::Image::from_u8(4, 2, photocraft_codecs::ChannelLayout::Rgba, [240, 40, 20, 255].repeat(8)).unwrap();
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap());
    assert!(
        s.execute("ai.receiveCandidate", json!({"candidateId":began["candidateId"],"imageBase64":encoded})).unwrap_err().to_string().contains("aspect ratio")
    );
    assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
    assert!(s.ai.pending.is_some());
    assert!(edit_frame(Rect::new(0, 0, 4097, 1), Rect::new(0, 0, 4097, 1)).is_err());
}

#[test]
fn generated_layer_survives_edits_and_roundtrips_with_native_depth_and_frozen_selection() {
    for (depth, mode) in [(8, "rgb"), (16, "rgb"), (32, "rgb"), (16, "gray"), (8, "cmyk")] {
        let mut s = session(depth, mode);
        s.edit("Feather fixture", |doc, _| {
            let mut selection = Surface::new(PixelFormat::GRAY8.with_sample(SampleType::F32));
            selection.fill_rect(Rect::new(8, 6, 20, 16), &[0.5]);
            selection.fill_rect(Rect::new(10, 8, 12, 10), &[0.0]);
            doc.selection = Some(selection);
            Ok(())
        })
        .unwrap();
        let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","destination":"layer"})).unwrap()["candidateId"].clone();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("edit.fill", json!({"contents":"color","color":[0.0,1.0,0.0,1.0]})).unwrap();
        s.execute("layer.new.layer", json!({"name":"Working"})).unwrap();
        let edited = s.active().unwrap().doc.clone();
        let steps = s.active().unwrap().history.past_len();
        let result = s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
        assert_eq!(result["savedAsLayer"], true);
        assert_eq!(result["recovered"], false);
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        let layer = LayerId(result["layerId"].as_u64().unwrap());
        assert_eq!(s.active().unwrap().active_layer, edited.top_layer());
        let generated = s.active().unwrap().doc.clone();
        assert_eq!(generated.layers.last().unwrap().name, "AI 1");
        let surface = pixels(&generated, layer);
        assert_eq!(surface.format(), edited.pixel_format());
        assert_eq!(surface.pixel(0, 0).last().copied(), Some(0.0));
        assert_eq!(surface.pixel(10, 8).last().copied(), Some(0.0));
        assert!((surface.pixel(9, 7).last().copied().unwrap() - 0.5).abs() < 0.01);
        for old in &edited.layers {
            assert_eq!(bytes(pixels(&generated, old.id)), bytes(old.surface().unwrap()));
        }
        let encoded = photocraft_format::save_to_bytes(&generated, &Default::default()).unwrap();
        let restored = photocraft_format::load_from_bytes(&encoded).unwrap();
        assert_eq!(bytes(pixels(&restored, layer)), bytes(surface));
        assert_eq!(restored.layers.len(), edited.layers.len() + 1);
        assert!(s.active().unwrap().is_dirty());
        assert!(s.ai.candidate().is_none());
        assert_eq!(s.execute("ai.inspect", json!({})).unwrap()["lastLayer"]["layerId"], layer.0);
        s.execute("layer.select", json!({"layer":edited.layers[0].id.0})).unwrap();
        s.execute("edit.fill", json!({"contents":"color","color":[0.0,0.0,1.0,1.0]})).unwrap();
        assert_eq!(bytes(pixels(&s.active().unwrap().doc, layer)), bytes(surface));
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.layers.len(), edited.layers.len());
        for old in &edited.layers {
            assert_eq!(bytes(pixels(&s.active().unwrap().doc, old.id)), bytes(old.surface().unwrap()));
        }
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(bytes(pixels(&s.active().unwrap().doc, layer)), bytes(surface));
    }
}

#[test]
fn layer_completion_returns_to_the_source_without_stealing_another_tab() {
    let mut s = session(8, "rgb");
    let source_id = s.active().unwrap().doc.id;
    let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","destination":"layer"})).unwrap()["candidateId"].clone();
    s.execute("layer.delete", json!({})).unwrap();
    let edited = s.active().unwrap().doc.clone();
    s.execute("file.new", json!({"width":12,"height":12})).unwrap();
    let other = s.active().unwrap().doc.clone();
    let result = s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
    assert_eq!(result["documentId"], source_id.0);
    assert_eq!(s.active().unwrap().doc.id, other.id);
    assert!(Arc::ptr_eq(&s.active().unwrap().doc, &other));
    let source = s.documents().iter().find(|st| st.doc.id == source_id).unwrap();
    assert_eq!(source.doc.layers.len(), edited.layers.len() + 1);
    assert_eq!(source.doc.layers.last().unwrap().name, "AI 1");
}

#[test]
fn incompatible_or_closed_source_recovers_the_generated_layer_in_a_new_document() {
    for change in ["resize", "mode", "close"] {
        let mut s = session(16, "rgb");
        let source = s.active().unwrap().doc.clone();
        let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","destination":"layer"})).unwrap()["candidateId"].clone();
        match change {
            "resize" => {
                s.execute("image.canvasSize", json!({"width":64,"height":48})).unwrap();
            }
            "mode" => {
                s.execute("image.mode.grayscale", json!({})).unwrap();
            }
            _ => {
                s.close(0);
            }
        }
        let edited = s.active().map(|st| st.doc.clone());
        let result = s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
        assert_eq!(result["recovered"], true);
        let restored = &s.active().unwrap().doc;
        assert_ne!(restored.id, source.id);
        assert_eq!(restored.size, source.size);
        assert_eq!(restored.mode, source.mode);
        assert_eq!(restored.depth, source.depth);
        assert_eq!(restored.layers.len(), 1);
        assert_eq!(restored.layers[0].name, "AI 1");
        if let Some(edited) = edited {
            let original = s.documents().iter().find(|st| st.doc.id == edited.id).unwrap();
            assert!(Arc::ptr_eq(&original.doc, &edited));
        }
    }
}

#[test]
fn cancelled_or_invalid_layer_generations_never_mutate_the_document() {
    let mut s = session(8, "rgb");
    let original = s.active().unwrap().doc.clone();
    for destination in [json!("invalid"), json!(true), json!(null)] {
        assert!(s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","destination":destination})).is_err());
    }
    let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","destination":"layer"})).unwrap()["candidateId"].clone();
    assert!(s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":"invalid"})).is_err());
    assert!(Arc::ptr_eq(&s.active().unwrap().doc, &original));
    s.execute("ai.cancelGeneration", json!({"candidateId":id})).unwrap();
    assert!(s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).is_err());
    assert!(Arc::ptr_eq(&s.active().unwrap().doc, &original));
}

#[test]
fn consecutive_generations_keep_every_layer() {
    let mut s = session(8, "rgb");
    let initial = s.active().unwrap().doc.layers.len();
    for _ in 0..2 {
        let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","destination":"layer"})).unwrap()["candidateId"].clone();
        s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
    }
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.layers.len(), initial + 2);
    assert_eq!(doc.layers[initial].name, "AI 1");
    assert_eq!(doc.layers[initial + 1].name, "AI 2");
    assert_eq!(bytes(doc.layers[initial].surface().unwrap()), bytes(doc.layers[initial + 1].surface().unwrap()));
    assert!(s.journal.iter().all(|(id, _)| id != "ai.receiveCandidate"));
}

#[test]
fn large_canvas_generation_bounds_reference_but_preserves_destination_and_undo() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":1920,"height":1080,"depth":16})).unwrap();
    let before = s.active().unwrap().doc.clone();
    let layer = s.active().unwrap().active_layer.unwrap();
    assert!(s.disabled_reason("ai.beginGeneration").is_none());
    let result = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","reference":true})).unwrap();
    assert_eq!(result["area"], json!([0, 0, 1920, 1080]));
    let reference = base64::engine::general_purpose::STANDARD.decode(result["sourcePng"].as_str().unwrap()).unwrap();
    assert_eq!(photocraft_codecs::decode(&reference).unwrap().dimensions(), (1024, 576));
    let id = result["candidateId"].clone();
    s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
    assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before));
    s.execute("ai.applyCandidate", json!({"candidateId":id})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.size, before.size);
    assert_eq!(pixels(doc, layer).format(), pixels(&before, layer).format());
    assert!((pixels(doc, layer).pixel(1919, 1079)[0] - 240.0 / 255.0).abs() < 0.01);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(bytes(pixels(&s.active().unwrap().doc, layer)), bytes(pixels(&before, layer)));
}

#[test]
fn real_generation_target_is_bounded_and_can_be_reduced_with_selection() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":4096,"height":4096})).unwrap();
    assert!(s.disabled_reason("ai.beginGeneration").is_none());
    assert!(s.disabled_reason("ai.prepareTestCandidate").is_some());
    s.execute("file.new", json!({"width":4097,"height":4096})).unwrap();
    assert!(s.disabled_reason("ai.beginGeneration").is_some());
    assert!(s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex","reference":true})).is_err());
    assert!(s.execute("ai.inspect", json!({})).unwrap()["pending"].is_null());
    s.execute("select.rect", json!({"x":20,"y":30,"width":1920,"height":1080})).unwrap();
    assert!(s.disabled_reason("ai.beginGeneration").is_none());
    let result = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"images-api","reference":true})).unwrap();
    assert_eq!(result["area"], json!([20, 30, 1940, 1110]));
    let reference = base64::engine::general_purpose::STANDARD.decode(result["sourcePng"].as_str().unwrap()).unwrap();
    assert_eq!(photocraft_codecs::decode(&reference).unwrap().dimensions(), (1024, 1024));
}

#[test]
fn generated_pixels_respect_feather_holes_and_alpha_lock() {
    let mut s = session(32, "rgb");
    s.edit("Fixture", |doc, layer| {
        let l = doc.layer_mut(layer.unwrap()).unwrap();
        l.locks.transparency = true;
        l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 32, 24), &[0.2, 0.4, 0.6, 0.5]);
        let mut mask = Surface::new(PixelFormat::GRAY8.with_sample(SampleType::F32));
        mask.fill_rect(Rect::new(8, 6, 20, 16), &[0.5]);
        mask.fill_rect(Rect::new(10, 8, 12, 10), &[0.0]);
        doc.selection = Some(mask);
        Ok(())
    })
    .unwrap();
    let original = s.active().unwrap().doc.clone();
    let layer = s.active().unwrap().active_layer.unwrap();
    let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex"})).unwrap()["candidateId"].clone();
    s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
    s.execute("ai.applyCandidate", json!({"candidateId":id})).unwrap();
    let before = pixels(&original, layer);
    let after = pixels(&s.active().unwrap().doc, layer);
    assert_eq!(before.pixel(10, 8), after.pixel(10, 8));
    assert_eq!(before.pixel(0, 0), after.pixel(0, 0));
    let changed = after.pixel(9, 7);
    assert!((changed[3] - 0.5).abs() < 1e-6);
    assert!((changed[0] - (0.2 + 240.0 / 255.0) * 0.5).abs() < 0.01);
}

#[test]
fn generated_candidate_preserves_depth_selection_history_and_undo() {
    for (depth, mode) in [(8, "rgb"), (16, "rgb"), (32, "rgb"), (16, "gray"), (8, "cmyk")] {
        let mut s = session(depth, mode);
        let before = s.active().unwrap().doc.clone();
        let layer = s.active().unwrap().active_layer.unwrap();
        let steps = s.active().unwrap().history.past_len();
        let result = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"images-api","reference":true})).unwrap();
        let reference = base64::engine::general_purpose::STANDARD.decode(result["sourcePng"].as_str().unwrap()).unwrap();
        let source = photocraft_codecs::decode(&reference).unwrap();
        assert_eq!(source.dimensions(), (13, 13));
        let id = result["candidateId"].as_u64().unwrap();
        s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
        assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before));
        let preview = s.ai.candidate().unwrap().preview(&s).unwrap();
        let old = pixels(&before, layer);
        let new = pixels(&preview, layer);
        assert_eq!(old.format(), new.format());
        assert_ne!(old.pixel(10, 8), new.pixel(10, 8));
        assert_eq!(old.pixel(0, 0), new.pixel(0, 0));
        s.execute("ai.applyCandidate", json!({"candidateId":id})).unwrap();
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(bytes(pixels(&s.active().unwrap().doc, layer)), bytes(old));
    }
}

#[test]
fn generated_results_reject_cancelled_stale_and_malformed_requests() {
    let mut s = session(8, "rgb");
    let begin = json!({"prompt":"red","provider":"codex"});
    let id = s.execute("ai.beginGeneration", begin.clone()).unwrap()["candidateId"].clone();
    assert!(s.execute("ai.beginGeneration", begin.clone()).is_err());
    assert!(s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":"bad"})).is_err());
    s.execute("ai.cancelGeneration", json!({"candidateId":id})).unwrap();
    assert!(s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).is_err());
    let id = s.execute("ai.beginGeneration", begin).unwrap()["candidateId"].clone();
    s.execute("select.deselect", json!({})).unwrap();
    assert!(s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).is_err());
    s.execute("ai.cancelGeneration", json!({"candidateId":id})).unwrap();
    // No selection is a full-canvas generation, not a disabled studio.
    let id = s.execute("ai.beginGeneration", json!({"prompt":"red","provider":"codex"})).unwrap()["candidateId"].clone();
    s.execute("ai.receiveCandidate", json!({"candidateId":id,"imageBase64":generated_image()})).unwrap();
    s.execute("ai.applyCandidate", json!({"candidateId":id})).unwrap();
}

fn session(depth: u32, mode: &str) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 32, "height": 24, "depth": depth, "mode": mode})).unwrap();
    s.execute("layer.new.layer", json!({"name": "Target"})).unwrap();
    s.execute("edit.fill", json!({"contents": "color", "color": [0.2, 0.4, 0.6, 1.0]})).unwrap();
    s.execute("select.rect", json!({"x": 8, "y": 6, "width": 12, "height": 10})).unwrap();
    s.tools.foreground = [0.9, 0.3, 0.1, 1.0];
    s
}

fn pixels(doc: &Document, layer: LayerId) -> &Surface {
    doc.layer(layer).unwrap().surface().unwrap()
}

fn bytes(surface: &Surface) -> std::collections::BTreeMap<(i32, i32), Vec<u8>> {
    surface.tiles().map(|(tc, tile)| ((tc.tx, tc.ty), tile.bytes().to_vec())).collect()
}

fn prepare(s: &mut Session) -> u64 {
    s.execute("ai.prepareTestCandidate", json!({"prompt": "repaint the selected area"})).unwrap()["candidateId"].as_u64().unwrap()
}

#[test]
fn preview_apply_undo_redo_and_native_roundtrip_at_all_depths() {
    for (depth, mode) in [(8, "rgb"), (16, "rgb"), (32, "rgb"), (8, "gray"), (16, "gray"), (32, "gray"), (8, "cmyk")] {
        let mut s = session(depth, mode);
        let st = s.active().unwrap();
        let (before, layer, steps, revision) = (st.doc.clone(), st.active_layer.unwrap(), st.history.past_len(), st.revision);
        let id = prepare(&mut s);
        assert!(Arc::ptr_eq(&s.active().unwrap().doc, &before));
        assert_eq!(s.active().unwrap().revision, revision);
        assert_eq!(s.active().unwrap().history.past_len(), steps);
        let candidate = s.ai.candidate().unwrap();
        assert!(!candidate.metadata(&s)["stale"].as_bool().unwrap());
        let preview = candidate.preview(&s).unwrap();
        assert_ne!(bytes(pixels(&preview, layer)), bytes(pixels(&before, layer)));
        let result = s.execute("ai.applyCandidate", json!({"candidateId": id})).unwrap();
        let after = s.active().unwrap().doc.clone();
        assert!(s.ai.candidate().is_none());
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Apply AI Candidate"));
        assert!(!crate::actions_cmds::replayable("ai.applyCandidate"));
        assert_eq!(bytes(pixels(&after, layer)), bytes(pixels(&preview, layer)));
        assert_eq!(after.id, before.id);
        assert_eq!(after.size, before.size);
        assert_eq!(after.layers.len(), before.layers.len());
        for l in &before.layers {
            let changed = after.layer(l.id).unwrap();
            assert_eq!(changed.name, l.name);
            assert_eq!(changed.locks, l.locks);
            assert_eq!(changed.opacity, l.opacity);
            if l.id != layer {
                assert_eq!(bytes(changed.surface().unwrap()), bytes(l.surface().unwrap()));
            }
        }
        let old = pixels(&before, layer);
        let new = pixels(&after, layer);
        let bpp = old.format().bytes_per_pixel();
        let tc = photocraft_geom::TileCoord::new(0, 0);
        let old_tile = old.tile(tc).unwrap().bytes();
        let new_tile = new.tile(tc).unwrap().bytes();
        for y in 0..24usize {
            for x in 0..32usize {
                if !(8..20).contains(&x) || !(6..16).contains(&y) {
                    let start = (y * photocraft_geom::TILE_SIZE as usize + x) * bpp;
                    assert_eq!(&old_tile[start..start + bpp], &new_tile[start..start + bpp], "{depth} {mode}: {x},{y}");
                }
            }
        }
        // Repeated Apply is an acknowledgement, not another edit.
        assert_eq!(s.execute("ai.applyCandidate", json!({"candidateId": id})).unwrap(), result);
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        let encoded = photocraft_format::save_to_bytes(&after, &Default::default()).unwrap();
        let reopened = photocraft_format::load_from_bytes(&encoded).unwrap();
        assert_eq!(bytes(pixels(&reopened, layer)), bytes(pixels(&after, layer)));
        assert_eq!(reopened.selection.as_ref().map(bytes), after.selection.as_ref().map(bytes));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(bytes(pixels(&s.active().unwrap().doc, layer)), bytes(old));
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(bytes(pixels(&s.active().unwrap().doc, layer)), bytes(new));
    }
}

#[test]
fn feathered_selection_and_transparency_lock_are_respected() {
    for depth in [8, 16, 32] {
        let mut s = session(depth, "rgb");
        s.edit("Feather fixture", |doc, active| {
            let l = doc.layer_mut(active.unwrap()).unwrap();
            l.locks.transparency = true;
            let surface = l.surface_mut().unwrap();
            surface.fill_rect(doc_bounds(), &photocraft_raster::from_rgba(&surface.format(), [0.2, 0.4, 0.6, 0.5]));
            let mut selection = Surface::new(PixelFormat::GRAY8.with_sample(SampleType::F32));
            selection.fill_rect(Rect::new(8, 6, 20, 16), &[0.5]);
            selection.fill_rect(Rect::new(12, 10, 14, 12), &[0.0]);
            doc.selection = Some(selection);
            Ok(())
        })
        .unwrap();
        let layer = s.active().unwrap().active_layer.unwrap();
        let before = s.active().unwrap().doc.clone();
        let id = prepare(&mut s);
        s.execute("ai.applyCandidate", json!({"candidateId": id})).unwrap();
        let out = pixels(&s.active().unwrap().doc, layer);
        let original = pixels(&before, layer);
        assert_eq!(out.rgba(12, 10), original.rgba(12, 10), "hole stays unchanged");
        assert_eq!(out.rgba(8, 6)[3], original.rgba(8, 6)[3], "locked alpha");
        assert!(out.rgba(8, 6)[0] > original.rgba(8, 6)[0] && out.rgba(8, 6)[0] < 0.9);
    }
}

fn doc_bounds() -> Rect {
    Rect::new(0, 0, 32, 24)
}

#[test]
fn stale_wrong_document_wrong_id_and_discard_never_change_pixels() {
    let mut s = session(8, "rgb");
    let id = prepare(&mut s);
    let revision = s.active().unwrap().revision;
    assert!(s.execute("ai.applyCandidate", json!({"candidateId": id + 1})).is_err());
    assert!(s.execute("ai.discardCandidate", json!({"candidateId": id + 1})).is_err());
    assert_eq!(s.active().unwrap().revision, revision);
    s.execute("edit.fill", json!({"contents": "white"})).unwrap();
    let changed = s.active().unwrap().doc.clone();
    assert!(s.ai.candidate().unwrap().preview(&s).is_none());
    assert!(s.execute("ai.applyCandidate", json!({"candidateId": id})).is_err());
    assert!(Arc::ptr_eq(&changed, &s.active().unwrap().doc));
    s.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
    let other = s.active().unwrap().doc.clone();
    assert!(s.execute("ai.applyCandidate", json!({"candidateId": id})).is_err());
    assert!(Arc::ptr_eq(&other, &s.active().unwrap().doc));
    s.execute("ai.discardCandidate", json!({"candidateId": id})).unwrap();
    assert!(s.ai.candidate().is_none());
}

#[test]
fn hostile_params_and_unsupported_targets_are_rejected() {
    let mut s = session(8, "rgb");
    let before = s.active().unwrap().doc.clone();
    for params in [json!({"prompt": 42}), json!({"prompt": "x".repeat(4097)}), json!({"target": "mask"}), json!([])] {
        assert!(s.execute("ai.prepareTestCandidate", params).is_err());
    }
    for params in [
        json!({}),
        json!({"candidateId": 0}),
        json!({"candidateId": -1}),
        json!({"candidateId": 1.5}),
        json!({"candidateId": "1"}),
        json!({"candidateId": 1, "coalesce": "stroke"}),
    ] {
        assert!(s.execute("ai.applyCandidate", params).is_err());
    }
    assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
    s.tools.foreground[0] = f32::NAN;
    assert!(s.execute("ai.prepareTestCandidate", json!({})).is_err());
    s.tools.foreground = [0.0, 0.0, 0.0, 1.0];
    s.tools.mask_targeted = true;
    assert!(!s.is_enabled("ai.prepareTestCandidate"));
    s.tools.mask_targeted = false;
    s.active_mut().unwrap().channel_view.target = crate::channel_cmds::ChannelTarget::Color(0);
    assert!(!s.is_enabled("ai.prepareTestCandidate"));
    s.active_mut().unwrap().channel_view.target = crate::channel_cmds::ChannelTarget::Composite;
    s.edit("lock fixture", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().locks.pixels = true;
        Ok(())
    })
    .unwrap();
    assert!(!s.is_enabled("ai.prepareTestCandidate"));
    s.execute("select.deselect", json!({})).unwrap();
    assert!(!s.is_enabled("ai.prepareTestCandidate"));
    let mut special = session(8, "rgb");
    special
        .edit("unsupported mode fixture", |doc, _| {
            doc.mode = photocraft_doc::ColorMode::Indexed;
            Ok(())
        })
        .unwrap();
    assert!(!special.is_enabled("ai.prepareTestCandidate"));
    assert!(special.execute("ai.prepareTestCandidate", json!({})).is_err());
}

#[test]
fn closing_source_releases_candidate_and_oversized_selections_fail() {
    let mut s = session(8, "rgb");
    prepare(&mut s);
    s.close(0);
    assert!(s.ai.candidate().is_none());
    assert!(s.execute("ai.prepareTestCandidate", json!({})).is_err());
    s.execute("file.new", json!({"width": 2048, "height": 1024})).unwrap();
    s.execute("select.all", json!({})).unwrap();
    assert!(!s.is_enabled("ai.prepareTestCandidate"));
    assert!(s.execute("ai.prepareTestCandidate", json!({})).is_err());
}

#[test]
fn floating_selection_is_not_committed_by_candidate_or_query() {
    let mut s = session(8, "rgb");
    s.execute("select.float", json!({"dx": 2, "dy": 0})).unwrap();
    let before = s.active().unwrap().doc.clone();
    let revision = s.active().unwrap().revision;
    assert!(s.execute("ai.prepareTestCandidate", json!({})).is_err());
    s.execute("ai.inspect", json!({})).unwrap();
    assert!(s.active().unwrap().floating.is_some());
    assert_eq!(s.active().unwrap().revision, revision);
    assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
}
