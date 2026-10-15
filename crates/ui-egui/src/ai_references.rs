//! Project-owned references with a disposable thumbnail cache; never pasted into layers.
use crate::{
    FileDialogAnswer, FileDialogRequest, PhotocraftApp,
    ai_service::{Job, ReferenceImage},
};
use base64::Engine as _;
use photocraft_codecs::{ChannelLayout, Image, SampleType};
use std::sync::mpsc::TryRecvError;

const MAX_FILES: usize = 4;
const MAX_BYTES: usize = 32 << 20;
const LIMITS: photocraft_codecs::Limits = photocraft_codecs::Limits { max_width: 16384, max_height: 16384, max_pixels: 16 << 20, max_alloc: 128 << 20 };

pub(crate) type SourceDocument = (photocraft_doc::DocId, u64);

pub(crate) struct Attachment {
    pub id: u64,
    pub image: ReferenceImage,
    pixels: egui::ColorImage,
    texture: Option<egui::TextureHandle>,
}

impl Attachment {
    pub fn texture(&mut self, ctx: &egui::Context) -> &egui::TextureHandle {
        self.texture.get_or_insert_with(|| ctx.load_texture("ai-reference", self.pixels.clone(), egui::TextureOptions::LINEAR))
    }
}

pub(crate) fn snapshot(app: &PhotocraftApp) -> Vec<ReferenceImage> {
    app.session
        .active()
        .map(|st| {
            st.doc
                .ai_project
                .references
                .iter()
                .map(|r| ReferenceImage { name: r.name.clone(), png: base64::engine::general_purpose::STANDARD.encode(r.png.as_ref()) })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn sync(app: &mut PhotocraftApp) {
    let active = app.session.active();
    let document = active.map(|st| (st.doc.id, st.open_id));
    let sources: Vec<_> = active.map(|st| st.doc.ai_project.references.iter().map(|r| (r.id, r.png.clone())).collect()).unwrap_or_default();
    if app.ai_runtime.reference_document == document
        && sources.len() == app.ai_runtime.reference_sources.len()
        && sources.iter().zip(&app.ai_runtime.reference_sources).all(|((id, bytes), (old_id, old))| id == old_id && std::sync::Arc::ptr_eq(bytes, old))
    {
        return;
    }
    let mut attachments = Vec::new();
    if let Some(st) = active {
        for reference in &st.doc.ai_project.references {
            let pixels = photocraft_codecs::decode_with(&reference.png, &photocraft_codecs::DecodeOptions { limits: LIMITS, ..Default::default() });
            match pixels {
                Ok(image) => {
                    let image = image.convert(ChannelLayout::Rgba, SampleType::U8);
                    attachments.push(Attachment {
                        id: reference.id,
                        image: ReferenceImage { name: reference.name.clone(), png: base64::engine::general_purpose::STANDARD.encode(reference.png.as_ref()) },
                        pixels: egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.data()),
                        texture: None,
                    });
                }
                Err(error) => app.ui.ai.status = error.to_string(),
            }
        }
    }
    app.ai_runtime.references = attachments;
    app.ai_runtime.reference_document = document;
    app.ai_runtime.reference_sources = sources;
    app.ai_runtime.references_revision = app.ai_runtime.references_revision.wrapping_add(1);
}

pub(crate) fn remove(app: &mut PhotocraftApp, id: u64) -> Result<(), String> {
    app.run("ai.referenceRemove", serde_json::json!({"id":id}))?;
    sync(app);
    Ok(())
}

fn available(app: &PhotocraftApp) -> Result<(), String> {
    if app.ai_runtime.running() || app.ai_runtime.reference_job.is_some() {
        return Err(tl!("Wait for the current operation to finish").into());
    }
    if app.session.active().is_some_and(|st| st.doc.ai_project.references.len() >= MAX_FILES) {
        return Err(tl!("Up to 4 reference images").into());
    }
    Ok(())
}

pub(crate) fn choose(app: &mut PhotocraftApp) -> Result<(), String> {
    available(app)?;
    if app.session.active_gui().is_some() {
        app.run("defold.openNodeSource", serde_json::json!({}))?;
    }
    let target = app.session.active().map(|st| (st.doc.id, st.open_id));
    app.ask_file(
        FileDialogRequest::Open {
            multiple: false,
            initial_dir: None,
            extensions: Some(["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"].map(str::to_owned).to_vec()),
        },
        move |app, answer| {
            if app.ai_runtime.running() || app.ai_runtime.reference_job.is_some() {
                return Err(tl!("Wait for the current operation to finish").into());
            }
            if let Some(document) = target {
                check_source(app, document)?;
                app.with_document(document.0, |app| {
                    available(app)?;
                    Ok(serde_json::Value::Null)
                })?;
            }
            prepare(app, target, move || {
                let (name, bytes) = match answer {
                    FileDialogAnswer::Contents(name, bytes) => (name, bytes),
                    FileDialogAnswer::ContentsMany(files) => files.into_iter().next().ok_or("No image selected")?,
                    #[cfg(not(target_arch = "wasm32"))]
                    FileDialogAnswer::Paths(paths) => {
                        use std::io::Read;
                        let path = paths.into_iter().next().ok_or("No image selected")?;
                        let mut bytes = Vec::new();
                        std::fs::File::open(&path).map_err(|e| e.to_string())?.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                        (path, bytes)
                    }
                    _ => return Err("No image selected".into()),
                };
                decode(&name, &bytes)
            });
            Ok(serde_json::Value::Null)
        },
    )?;
    Ok(())
}

pub(crate) fn paste(app: &mut PhotocraftApp) -> Result<bool, String> {
    available(app)?;
    let Some((width, height, bytes)) = app.services.clipboard_get_image.as_mut().and_then(|get| get()) else { return Ok(false) };
    if app.session.active_gui().is_some() {
        app.run("defold.openNodeSource", serde_json::json!({}))?;
    }
    LIMITS.check(width, height, ChannelLayout::Rgba, SampleType::U8).map_err(|e| e.to_string())?;
    let name = tl!("Clipboard image").to_owned();
    prepare(app, app.session.active().map(|st| (st.doc.id, st.open_id)), move || {
        let image = Image::from_raw(width, height, ChannelLayout::Rgba, SampleType::U8, bytes).map_err(|e| e.to_string())?;
        normalize(&name, image)
    });
    Ok(true)
}

fn check_source(app: &PhotocraftApp, source: SourceDocument) -> Result<(), String> {
    if app.session.documents().iter().any(|st| (st.doc.id, st.open_id) == source) {
        Ok(())
    } else {
        Err("The reference image's source document was closed or reopened; add the image to the current document again".into())
    }
}

fn prepare(app: &mut PhotocraftApp, target: Option<SourceDocument>, work: impl FnOnce() -> Result<Attachment, String> + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    app.ai_runtime.reference_job = Some((target, Job { cancel: Default::default(), result: rx }));
    app.ui.ai.status = tl!("Preparing reference image...").into();
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    #[cfg(target_arch = "wasm32")]
    let _ = tx.send(work());
}

pub(crate) fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    sync(app);
    let Some((target, job)) = &app.ai_runtime.reference_job else { return };
    let target = *target;
    let result = match job.result.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        Err(TryRecvError::Disconnected) => Err("Reference image worker stopped".into()),
    };
    app.ai_runtime.reference_job = None;
    match result {
        Ok(image) => {
            let add = |app: &mut PhotocraftApp| app.run("ai.referenceAdd", serde_json::json!({"name":image.image.name,"imageBase64":image.image.png}));
            let result = if let Some(target) = target {
                check_source(app, target).and_then(|()| app.with_document(target.0, add))
            } else {
                app.run("file.new", serde_json::json!({"width":1024,"height":1024})).and_then(|_| add(app))
            };
            match result {
                Ok(_) => app.ui.ai.status.clear(),
                Err(error) => app.ui.ai.status = error,
            }
            sync(app);
        }
        Err(error) => app.ui.ai.status = error,
    }
}

fn decode(name: &str, bytes: &[u8]) -> Result<Attachment, String> {
    if bytes.len() > MAX_BYTES {
        return Err(tl!("Reference image exceeds 32 MB").into());
    }
    let image = photocraft_codecs::decode_with(bytes, &photocraft_codecs::DecodeOptions { limits: LIMITS, ..Default::default() }).map_err(|e| e.to_string())?;
    normalize(name, image)
}

fn normalize(name: &str, mut image: Image) -> Result<Attachment, String> {
    image.meta = Default::default();
    // Import bounded pixels only, then reuse the editor's profile conversion and resampler.
    let bytes = photocraft_codecs::encode(&image, photocraft_codecs::Format::Tiff, &Default::default()).map_err(|e| e.to_string())?;
    let mut doc = photocraft_io::import("reference.tiff", &bytes).map_err(|e| e.to_string())?.document;
    photocraft_engine::color_cmds::convert_document(&mut doc, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true)
        .map_err(|e| e.to_string())?;
    let scale = (1536.0 / f64::from(doc.size.width.max(doc.size.height))).min(1.0);
    let bounds = doc.bounds();
    for layer in &mut doc.layers {
        if let photocraft_doc::LayerContent::Raster(surface) = &mut layer.content {
            *surface = photocraft_algo::resample::resize_surface_in_canvas(surface, scale, scale, photocraft_algo::resample::Resample::Bicubic, bounds);
        }
    }
    doc.size =
        photocraft_geom::Size::new((f64::from(doc.size.width) * scale).round().max(1.0) as u32, (f64::from(doc.size.height) * scale).round().max(1.0) as u32);
    let png = photocraft_io::export(&doc, "reference.png", &photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() })
        .map_err(|e| e.to_string())?
        .bytes;
    let mut image = photocraft_codecs::decode(&png).map_err(|e| e.to_string())?.convert(ChannelLayout::Rgba, SampleType::U8);
    image.meta = Default::default();
    let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.data());
    let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| e.to_string())?;
    let name = name.rsplit(['/', '\\']).next().unwrap_or("Reference").chars().filter(|c| !c.is_control()).take(120).collect();
    Ok(Attachment { id: 0, image: ReferenceImage { name, png: base64::engine::general_purpose::STANDARD.encode(png) }, pixels, texture: None })
}

pub(crate) fn paste_input(app: &mut PhotocraftApp, ctx: &egui::Context, raw: &mut egui::RawInput) {
    if !app.ui.ai.visible || available(app).is_err() || !app.ai_runtime.prompt_id.is_some_and(|id| ctx.memory(|m| m.has_focus(id))) {
        return;
    }
    let mut probe = raw.clone();
    crate::shortcuts::clipboard_keys(ctx, false, &mut probe);
    if !probe.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.command)) {
        return;
    }
    match paste(app) {
        Ok(false) => {} // Keep normal text paste untouched.
        result => {
            raw.events.retain(|e| !matches!(e, egui::Event::Paste(_) | egui::Event::Key { key: egui::Key::V | egui::Key::Insert, .. }));
            if let Err(error) = result {
                app.ui.ai.status = error;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        photocraft_codecs::encode(
            &Image::from_u8(4, 2, ChannelLayout::Rgba, [30, 160, 90, 128].repeat(8)).unwrap(),
            photocraft_codecs::Format::Png,
            &Default::default(),
        )
        .unwrap()
    }

    fn finish(app: &mut PhotocraftApp, ctx: &egui::Context) {
        for _ in 0..400 {
            tick(app, ctx);
            if app.ai_runtime.reference_job.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("reference worker timeout");
    }

    #[test]
    fn references_are_bounded_color_managed_and_strip_paths_and_metadata() {
        for sample in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut image = Image::from_u8(4, 2, ChannelLayout::Rgba, [30, 160, 90, 128].repeat(8)).unwrap().convert(ChannelLayout::Rgba, sample);
            image.meta.text.push(("private".into(), "secret".into()));
            let attachment = normalize("C:\\private\\example.png", image).unwrap();
            assert_eq!(attachment.image.name, "example.png");
            let decoded = photocraft_codecs::decode(&base64::engine::general_purpose::STANDARD.decode(&attachment.image.png).unwrap()).unwrap();
            assert!(decoded.meta.is_empty());
            assert_eq!(decoded.dimensions(), (4, 2));
            assert!(decoded.data().chunks_exact(4).all(|p| p[3].abs_diff(128) <= 1));
        }
        let large = Image::from_u8(2048, 8, ChannelLayout::Rgba, [30, 160, 90, 255].repeat(2048 * 8)).unwrap();
        let small = normalize("wide.png", large).unwrap();
        assert_eq!(small.pixels.size, [1536, 6]);
        assert!(decode("bad.png", b"not an image").is_err());
        assert!(decode("huge.png", &vec![0; MAX_BYTES + 1]).is_err());
    }

    #[test]
    fn picker_and_clipboard_save_project_references_without_changing_layers() {
        let ctx = egui::Context::default();
        let (picker, requests) = crate::file_dialog::fake(vec![Some(FileDialogAnswer::Contents("reference.png".into(), png()))]);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let recorded = calls.clone();
        let mut app = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services {
                file_dialog: Some(picker),
                clipboard_get_image: Some(Box::new(move || {
                    recorded.set(recorded.get() + 1);
                    Some((4, 2, [30, 160, 90, 128].repeat(8)))
                })),
                ..Default::default()
            },
        );
        app.run("file.new", serde_json::json!({"width":32,"height":32})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        tick(&mut app, &ctx);
        assert_eq!(calls.get(), 0);
        choose(&mut app).unwrap();
        app.poll_file_dialog(&ctx, None);
        finish(&mut app, &ctx);
        assert_eq!(requests.borrow().len(), 1);
        assert_eq!(app.ai_runtime.references.len(), 1);
        assert!(paste(&mut app).unwrap());
        finish(&mut app, &ctx);
        assert_eq!(calls.get(), 1);
        assert_eq!(app.ai_runtime.references.len(), 2);
        let saved = snapshot(&app);
        let id = app.ai_runtime.references[0].id;
        remove(&mut app, id).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(before.layers, app.session.active().unwrap().doc.layers);
        assert!(app.session.active().unwrap().is_dirty());
        for n in 1..MAX_FILES {
            app.run("ai.referenceAdd", serde_json::json!({"name":format!("x{n}.png"),"imageBase64":base64::engine::general_purpose::STANDARD.encode(png())}))
                .unwrap();
        }
        assert!(paste(&mut app).is_err());
        assert_eq!(calls.get(), 1, "over-limit paste never reads clipboard");
    }

    #[test]
    fn prompt_pastes_images_once_without_intercepting_text_or_other_fields() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("prompt");
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let recorded = calls.clone();
        let mut app = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services {
                clipboard_get_image: Some(Box::new(move || {
                    recorded.set(recorded.get() + 1);
                    Some((4, 2, [30, 160, 90, 128].repeat(8)))
                })),
                ..Default::default()
            },
        );
        app.ui.ai.visible = true;
        app.ai_runtime.prompt_id = Some(id);
        let key = |pressed| egui::Event::Key { key: egui::Key::V, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::COMMAND };
        let raw = || egui::RawInput { events: vec![key(true), key(false)], ..Default::default() };
        paste_input(&mut app, &ctx, &mut raw());
        assert_eq!(calls.get(), 0);
        ctx.memory_mut(|m| m.request_focus(id));
        let mut input = raw();
        paste_input(&mut app, &ctx, &mut input);
        assert!(input.events.is_empty());
        finish(&mut app, &ctx);
        assert_eq!(calls.get(), 1);
        assert_eq!(app.ai_runtime.references.len(), 1);
        app.services.clipboard_get_image = Some(Box::new(|| None));
        let mut text = egui::RawInput { events: vec![egui::Event::Paste("normal text".into())], ..Default::default() };
        paste_input(&mut app, &ctx, &mut text);
        assert!(matches!(text.events.as_slice(), [egui::Event::Paste(_)]));
        app.services.clipboard_get_image = Some(Box::new(|| Some((u32::MAX, 1, vec![]))));
        assert!(paste(&mut app).is_err());
    }

    #[test]
    fn attachment_strip_fits_compact_and_wide_windows_and_removal_works() {
        use egui_kittest::kittest::Queryable;
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for (width, height) in [(360.0, 480.0), (800.0, 480.0), (1280.0, 720.0)] {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.run("file.new", serde_json::json!({"width":32,"height":32})).unwrap();
            app.ui.ai.visible = true;
            app.ui.ai.prompt =
                "Use reference 1 for the materials, and reference 2 for the control shapes. Add raised buttons and a matching progress bar.".into();
            for n in 1..=4 {
                app.run(
                    "ai.referenceAdd",
                    serde_json::json!({"name":format!("style-{n}.png"),"imageBase64":base64::engine::general_purpose::STANDARD.encode(png())}),
                )
                .unwrap();
            }
            let mut initialized = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                move |ui, app: &mut PhotocraftApp| {
                    if !initialized {
                        egui_extras::install_image_loaders(ui.ctx());
                        crate::theme::install_fonts(ui.ctx());
                        crate::theme::apply(ui.ctx(), crate::theme::ThemeKind::default());
                        initialized = true;
                        return;
                    }
                    app.last_canvas_rect = ui.ctx().content_rect();
                    crate::ai_panel::show(app, ui);
                },
                app,
            );
            harness.run_steps(5);
            for label in ["Prompt", "Generate image", "Reference images", "Model settings", "Collapse AI Studio"] {
                let rect = harness.get_by_label(label).rect();
                assert!(rect.left() >= 0.0 && rect.right() <= width && rect.bottom() <= height, "{label}: {rect:?}");
            }
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_REFERENCE_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("references-{width}.png"))).unwrap();
            }
            harness.get_all_by_label("Remove reference image").next().unwrap().click();
            harness.run_steps(4);
            assert_eq!(harness.state().ai_runtime.references.len(), 3);
            assert!(harness.state().ai_runtime.references_revision >= 2);
            harness.get_by_label("Reference images").click();
            harness.run_steps(4);
            assert!(harness.query_by_label("Choose reference image").is_some());
            assert!(harness.query_by_label("Paste reference image").is_some());
        }
    }

    #[test]
    fn references_roundtrip_switch_documents_and_async_import_stays_with_its_source() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", serde_json::json!({"width":32,"height":32})).unwrap();
        let first = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        prepare(&mut app, Some(first), || decode("saved.png", &png()));
        app.run("file.new", serde_json::json!({"width":16,"height":16})).unwrap();
        finish(&mut app, &ctx);
        assert!(app.ai_runtime.references.is_empty());
        assert!(app.session.active().unwrap().doc.ai_project.references.is_empty());
        app.session.set_active(0);
        sync(&mut app);
        assert_eq!(app.ai_runtime.references.len(), 1);
        let doc = app.session.active().unwrap().doc.clone();
        for extension in ["psd", "pcraft"] {
            let restored = if extension == "psd" {
                let bytes = photocraft_io::export(&doc, "test.psd", &Default::default()).unwrap().bytes;
                photocraft_io::import("test.psd", &bytes).unwrap().document
            } else {
                photocraft_format::load_from_bytes(&photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap()).unwrap()
            };
            let mut reopened = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            reopened.session.add_document(restored, None);
            sync(&mut reopened);
            assert_eq!(reopened.ai_runtime.references[0].image.name, "saved.png");
            assert_eq!(snapshot(&reopened)[0].png, snapshot(&app)[0].png);
        }
    }

    #[test]
    fn delayed_picker_rejects_a_closed_and_reopened_source_document() {
        let ctx = egui::Context::default();
        let reply = std::rc::Rc::new(std::cell::RefCell::new(None::<crate::FileDialogReply>));
        let pending = reply.clone();
        let mut app = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services {
                file_dialog: Some(Box::new(move |_, _, reply| {
                    *pending.borrow_mut() = Some(reply);
                })),
                ..Default::default()
            },
        );
        app.run("file.new", serde_json::json!({"width":32,"height":32})).unwrap();
        let saved = (*app.session.active().unwrap().doc).clone();
        let old_open = app.session.active().unwrap().open_id;
        choose(&mut app).unwrap();
        app.poll_file_dialog(&ctx, None);
        assert!(reply.borrow().is_some());
        app.session.close(0);
        app.session.add_document(saved.clone(), None);
        assert_eq!(app.session.active().unwrap().doc.id, saved.id);
        assert_ne!(app.session.active().unwrap().open_id, old_open);
        let before = app.session.active().unwrap().doc.clone();
        reply.borrow_mut().take().unwrap().send(Some(FileDialogAnswer::Contents("late.png".into(), png())));
        app.poll_file_dialog(&ctx, None);
        assert!(app.ai_runtime.reference_job.is_none(), "a stale picker result must not start decoding for a reopened document");
        tick(&mut app, &ctx);
        assert!(std::sync::Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert!(snapshot(&app).is_empty());
    }

    #[test]
    fn completed_image_decode_rejects_a_closed_and_reopened_source_document() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", serde_json::json!({"width":32,"height":32})).unwrap();
        let saved = (*app.session.active().unwrap().doc).clone();
        let source = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        prepare(&mut app, Some(source), || decode("late.png", &png()));
        assert!(app.ai_runtime.reference_job.is_some());
        app.session.close(0);
        app.session.add_document(saved.clone(), None);
        assert_eq!(app.session.active().unwrap().doc.id, source.0);
        assert_ne!(app.session.active().unwrap().open_id, source.1);
        let before = app.session.active().unwrap().doc.clone();
        finish(&mut app, &ctx);
        assert!(std::sync::Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert!(snapshot(&app).is_empty());
        assert!(app.ui.ai.status.contains("closed or reopened"));
        assert_eq!(app.ai_runtime.reference_document, app.session.active().map(|st| (st.doc.id, st.open_id)));
    }
}
