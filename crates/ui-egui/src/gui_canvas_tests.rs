use super::*;
use serde_json::json;

fn gui(width: u32, height: u32) -> (tempfile::TempDir, PhotocraftApp) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.project"), format!("[display]\nwidth={width}\nheight={height}\n")).unwrap();
    std::fs::write(dir.path().join("scene.gui"), "nodes {id:\"panel\" position {x:-200 y:-200} size {x:800 y:600} color {x:0 y:0 z:1}}\n").unwrap();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("defold.openDesign", json!({"path":dir.path().join("scene.gui")})).unwrap();
    (dir, app)
}
fn draw(app: &mut PhotocraftApp, ctx: &egui::Context) -> egui::FullOutput {
    ctx.run_ui(Default::default(), |ui| {
        let xf = ViewXform { rect: ui.available_rect_before_wrap(), zoom: 1.0, center: [0.0, 0.0], flip: false, rotation: 0.0, aspect: 1.0 };
        draw_beyond_canvas(app, ui.painter(), &xf, 0, None);
    })
}

#[test]
#[ignore = "GUI brush performance measurement"]
fn gui_brush_refresh_benchmark() {
    for (width, height) in [(2340, 1080), (6000, 4000)] {
        let (_dir, mut app) = gui(width, height);
        if width == 2340
            && let Ok(path) = std::env::var("PHOTOCRAFT_GUI_BENCH_PATH")
        {
            app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.run("defold.openDesign", json!({"path":path})).unwrap();
            let st = app.session.active().unwrap();
            let layer = st.doc.defold.as_ref().unwrap().nodes.iter().find(|n| n.node == "ui_window_bg_title").unwrap().artwork;
            app.run("layer.select", json!({"layer":layer})).unwrap();
        }
        let doc = &app.session.active().unwrap().doc;
        let extent = photocraft_engine::defold_design::scene_bounds(doc).unwrap().0;
        let band = ring(extent, doc.bounds()).into_iter().max_by_key(|r| u64::from(r.width()) * u64::from(r.height())).unwrap();
        let x = (band.x0 + band.x1) / 2;
        let y = (band.y0 + band.y1) / 2;
        let ctx = egui::Context::default();
        draw(&mut app, &ctx).textures_delta.clear();
        let mut times = Vec::new();
        let mut pixels = 0usize;
        for step in 0..8 {
            app.run("paint.stroke", json!({"points":[[x+step,y]],"size":12,"color":"#ff0000"})).unwrap();
            let start = std::time::Instant::now();
            let mut output = draw(&mut app, &ctx);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            pixels += output.textures_delta.set.values().flatten().map(|d| d.image.width() * d.image.height()).sum::<usize>();
            output.textures_delta.clear();
        }
        times.sort_by(f64::total_cmp);
        println!("GUI_BENCH {width}x{height}: median_ms={:.3} max_ms={:.3} uploaded_pixels={pixels}", times[4], times[7]);
    }
}

fn cache(app: &PhotocraftApp, ctx: &egui::Context) -> BeyondCanvas {
    let id = app.session.active().unwrap().doc.id;
    ctx.data(|d| d.get_temp(egui::Id::new(("crop-beyond-canvas", id.0, None::<u32>)))).unwrap()
}
fn apply_images(out: &mut egui::FullOutput, images: &mut std::collections::HashMap<egui::TextureId, egui::ColorImage>) {
    for (id, deltas) in &out.textures_delta.set {
        for delta in deltas {
            let egui::ImageData::Color(image) = &delta.image;
            if let Some([x, y]) = delta.pos {
                let base = images.get_mut(id).unwrap();
                for row in 0..image.height() {
                    let start = (row + y) * base.width() + x;
                    base.pixels[start..start + image.width()].copy_from_slice(&image.pixels[row * image.width()..(row + 1) * image.width()]);
                }
            } else {
                images.insert(*id, (**image).clone());
            }
        }
    }
    out.textures_delta.clear();
}
fn verify(app: &mut PhotocraftApp, ctx: &egui::Context, images: &std::collections::HashMap<egui::TextureId, egui::ColorImage>) {
    let c = cache(app, ctx);
    let (doc, _) = display_doc(app, 0);
    let (display, _) = canvas_display(app, &doc, None);
    for (_, area, texture) in c.bands {
        let expected = photocraft_compose::render_reduced_rect(&doc, area, area.width() / c.factor, area.height() / c.factor);
        let expected = display_image(display.as_deref(), &expected);
        let actual = images.get(&texture.id()).unwrap();
        let differences: Vec<_> = actual.pixels.iter().zip(&expected.pixels).enumerate().filter(|(_, (a, b))| a != b).take(6).collect();
        assert!(differences.is_empty(), "area={area:?} factor={} differences={differences:?}", c.factor);
    }
}
#[test]
fn gui_outside_paint_patches_match_full_composites_at_native_and_reduced_resolution() {
    for (width, height) in [(160, 80), (6000, 4000)] {
        let (_dir, mut app) = gui(width, height);
        let ctx = egui::Context::default();
        let mut images = std::collections::HashMap::new();
        apply_images(&mut draw(&mut app, &ctx), &mut images);
        let ids: Vec<_> = cache(&app, &ctx).bands.iter().map(|b| b.2.id()).collect();
        for erase in [false, true] {
            app.run("paint.stroke", json!({"points":[[-203,height as i32+201]],"size":12,"color":"#ff0000","erase":erase})).unwrap();
            let mut out = draw(&mut app, &ctx);
            let patches: Vec<_> = out.textures_delta.set.iter().filter(|(id, _)| ids.contains(id)).flat_map(|(_, d)| d).collect();
            assert!(!patches.is_empty());
            assert!(patches.iter().all(|d| d.pos.is_some() && d.image.width() * d.image.height() < 1024));
            assert_eq!(ids, cache(&app, &ctx).bands.iter().map(|b| b.2.id()).collect::<Vec<_>>());
            apply_images(&mut out, &mut images);
            println!("paint case {width} erase={erase} last_damage={:?}", app.session.active().unwrap().last_damage);
            verify(&mut app, &ctx, &images);
        }
        println!("undo case {width}");
        app.run("edit.undo", json!({})).unwrap();
        apply_images(&mut draw(&mut app, &ctx), &mut images);
        verify(&mut app, &ctx, &images);
        app.run("paint.stroke", json!({"points":[[-800,height as i32+200]],"size":12})).unwrap();
        apply_images(&mut draw(&mut app, &ctx), &mut images);
        assert!(cache(&app, &ctx).extent.x0 < -800);
        verify(&mut app, &ctx, &images);
    }
}
#[test]
fn gui_live_dabs_and_commit_reuse_outside_textures_and_unseen_edits_rebuild() {
    let (_dir, mut app) = gui(160, 80);
    app.sync_views();
    app.ui.tool = Tool::Brush;
    app.run("tools.setBrush", json!({"brush":{"size":12,"hardness":1.0}})).unwrap();
    let ctx = egui::Context::default();
    let mut images = std::collections::HashMap::new();
    apply_images(&mut draw(&mut app, &ctx), &mut images);
    let m = egui::Modifiers::NONE;
    tool_event(&mut app, ToolEvent::Down { x: -200.0, y: 280.0, pressure: 1.0 }, m);
    apply_images(&mut draw(&mut app, &ctx), &mut images);
    let ids: Vec<_> = cache(&app, &ctx).bands.iter().map(|b| b.2.id()).collect();
    tool_event(&mut app, ToolEvent::Move { x: -190.0, y: 280.0, pressure: 1.0 }, m);
    let mut out = draw(&mut app, &ctx);
    assert_eq!(ids, cache(&app, &ctx).bands.iter().map(|b| b.2.id()).collect::<Vec<_>>());
    assert!(out.textures_delta.set.values().flatten().all(|d| d.pos.is_some()));
    apply_images(&mut out, &mut images);
    verify(&mut app, &ctx, &images);
    tool_event(&mut app, ToolEvent::Up { x: -190.0, y: 280.0 }, m);
    apply_images(&mut draw(&mut app, &ctx), &mut images);
    verify(&mut app, &ctx, &images);
    for x in [-220, -240] {
        app.run("paint.stroke", json!({"points":[[x,280]],"size":12})).unwrap();
    }
    apply_images(&mut draw(&mut app, &ctx), &mut images);
    assert_ne!(ids, cache(&app, &ctx).bands.iter().map(|b| b.2.id()).collect::<Vec<_>>());
    verify(&mut app, &ctx, &images);
}
