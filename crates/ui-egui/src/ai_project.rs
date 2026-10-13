//! Per-document AI context and incremental host-owned project journal.
use crate::{PhotocraftApp, ai_service::ProjectContext};
use serde_json::json;

const MAX_PENDING_EVENTS: usize = 128;
const PENDING_NOTICE: &str = "AI project events are waiting for the document operation to finish";

pub(crate) struct PendingEvent {
    source: (photocraft_doc::DocId, u64),
    kind: String,
    summary: String,
}

pub(crate) fn sync_prompt(app: &mut PhotocraftApp) {
    let active = app.session.active();
    let document = active.map(|st| (st.doc.id, st.open_id));
    if app.ai_runtime.project_document == document {
        return;
    }
    if let Some(st) = active {
        if let Some(draft) = app.ai_runtime.project_drafts.get(&st.open_id) {
            app.ui.ai.prompt = draft.clone();
        } else if !st.doc.ai_project.last_prompt.is_empty() {
            app.ui.ai.prompt = st.doc.ai_project.last_prompt.clone();
        } else if app.ai_runtime.project_document.is_some() && st.doc.ai_generations.is_empty() {
            app.ui.ai.prompt.clear();
        }
    } else if app.ai_runtime.project_document.is_some() {
        app.ui.ai.prompt.clear();
    }
    app.ai_runtime.project_document = document;
}

pub(crate) fn remember_prompt(app: &mut PhotocraftApp) {
    if let Some(st) = app.session.active() {
        app.ai_runtime.project_drafts.insert(st.open_id, app.ui.ai.prompt.clone());
    }
}

pub(crate) fn context(st: &photocraft_engine::DocState) -> Option<ProjectContext> {
    let project = &st.doc.ai_project;
    if project.id.is_empty() {
        return None;
    }
    let layers: Vec<_> = st
        .doc
        .layers
        .iter()
        .take(128)
        .map(|l| {
            json!({
                "id":l.id.0,"name":l.name.chars().take(160).collect::<String>(),"visible":l.visible
            })
        })
        .collect();
    let recent: Vec<_> = project
        .events
        .iter()
        .rev()
        .take(24)
        .rev()
        .map(|e| {
            json!({
                "sequence":e.sequence,"kind":e.kind,"summary":e.summary.chars().take(768).collect::<String>()
            })
        })
        .collect();
    let memory = json!({"projectId":project.id,"document":st.doc.name,"canvas":{"width":st.doc.size.width,"height":st.doc.size.height},
        "activeLayer":st.active_layer.map(|id|id.0),"layers":layers,"selection":st.doc.selection.as_ref().map(|s|s.content_bounds()),
        "lastPrompt":project.last_prompt,"references":project.references.iter().map(|r|json!({"id":r.id,"name":r.name})).collect::<Vec<_>>(),
        "recentEvents":recent,"eventCount":project.next_sequence})
    .to_string();
    Some(ProjectContext {
        id: project.id.clone(),
        open_id: st.open_id,
        name: st.doc.name.clone(),
        path: st.path.clone(),
        memory,
        events: project.events.clone(),
    })
}

pub(crate) fn prepare(app: &mut PhotocraftApp) -> Result<ProjectContext, String> {
    flush_records(app);
    if app.ai_runtime.project_events.len() >= MAX_PENDING_EVENTS - 1 {
        return Err("Finish pending document operations before starting another generation; its event could not be safely queued".into());
    }
    app.run("ai.projectEnsure", json!({}))?;
    let prompt = app.ui.ai.prompt.clone();
    app.run("ai.projectRecord", json!({"kind":"user","summary":prompt.chars().take(512).collect::<String>(),"prompt":prompt}))?;
    let context = app.session.active().and_then(context).ok_or("AI project is unavailable")?;
    if let Some(sync) = &mut app.services.project_sync {
        sync(context.clone())?;
    }
    Ok(context)
}

pub(crate) fn record(app: &mut PhotocraftApp, source: Option<(photocraft_doc::DocId, u64)>, kind: &str, summary: &str) {
    let Some(source) = source else { return };
    if app.ai_runtime.project_events.len() >= MAX_PENDING_EVENTS {
        app.ai_runtime.project_record_error = "AI project event queue is full; this completion could not be recorded".into();
        app.ai_runtime.project_sync_error.clone_from(&app.ai_runtime.project_record_error);
        return;
    }
    app.ai_runtime.project_events.push_back(PendingEvent {
        source,
        kind: photocraft_doc::ai::bounded_text(kind, 64),
        summary: photocraft_doc::ai::bounded_text(summary, 2048),
    });
    flush_records(app);
}

fn flush_records(app: &mut PhotocraftApp) {
    let pending = std::mem::take(&mut app.ai_runtime.project_events);
    let mut blocked = std::collections::HashSet::new();
    for event in pending {
        let (doc, open_id) = event.source;
        if !app.session.documents().iter().any(|st| st.doc.id == doc && st.open_id == open_id) {
            app.ai_runtime.project_record_error = "An AI project event could not be recorded because its source document was closed".into();
            continue;
        }
        if blocked.contains(&event.source) {
            app.ai_runtime.project_events.push_back(event);
            continue;
        }
        // Preserve per-document order and retry after save/filter/floating-selection gates release.
        if app.with_document(doc, |app| app.run("ai.projectRecord", json!({"kind":event.kind,"summary":event.summary}))).is_err() {
            blocked.insert(event.source);
            app.ai_runtime.project_events.push_back(event);
        }
    }
    if !app.ai_runtime.project_record_error.is_empty() {
        app.ai_runtime.project_sync_error.clone_from(&app.ai_runtime.project_record_error);
    } else if !app.ai_runtime.project_events.is_empty() {
        app.ai_runtime.project_sync_error = PENDING_NOTICE.into();
    } else if app.ai_runtime.project_sync_error == PENDING_NOTICE {
        app.ai_runtime.project_sync_error.clear();
    }
}

fn sync_succeeded(app: &mut PhotocraftApp) {
    app.ai_runtime.project_sync_error = if !app.ai_runtime.project_record_error.is_empty() {
        app.ai_runtime.project_record_error.clone()
    } else if !app.ai_runtime.project_events.is_empty() {
        PENDING_NOTICE.into()
    } else {
        String::new()
    };
}

pub(crate) fn sync(app: &mut PhotocraftApp, ctx: &egui::Context) {
    flush_records(app);
    if !app.ai_runtime.project_events.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    app.ai_runtime.project_synced.retain(|id, (open_id, _, _)| app.session.documents().iter().any(|st| st.doc.id == *id && st.open_id == *open_id));
    app.ai_runtime.project_drafts.retain(|open_id, _| app.session.documents().iter().any(|st| st.open_id == *open_id));
    if app.services.project_sync.is_none() || ctx.input(|i| i.time) < app.ai_runtime.project_sync_retry_at {
        return;
    }
    if let Some(status) = &mut app.services.project_sync_status
        && let Err(error) = status()
    {
        app.ai_runtime.project_sync_error = error;
        app.ai_runtime.project_synced.clear();
    }
    let pending: Vec<_> = app
        .session
        .documents()
        .iter()
        .filter_map(|st| {
            let signature = (st.open_id, st.doc.ai_project.next_sequence, st.path.clone());
            if app.ai_runtime.project_synced.get(&st.doc.id) == Some(&signature) {
                return None;
            }
            context(st).map(|context| (st.doc.id, signature, context))
        })
        .collect();
    for (doc, signature, context) in pending {
        let Some(sync) = &mut app.services.project_sync else { return };
        match sync(context) {
            Ok(()) => {
                app.ai_runtime.project_synced.insert(doc, signature);
                sync_succeeded(app);
            }
            Err(error) => {
                app.ai_runtime.project_sync_error = error;
                app.ai_runtime.project_sync_retry_at = ctx.input(|i| i.time) + 2.0;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
    };

    fn app(services: crate::Services) -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":32,"height":24})).unwrap();
        app
    }

    fn at(ctx: &egui::Context, time: f64) {
        ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |_| {}).textures_delta.clear();
    }

    #[test]
    fn prompt_drafts_are_isolated_and_reopening_restores_the_saved_prompt() {
        let mut app = app(Default::default());
        app.run("ai.projectEnsure", json!({})).unwrap();
        app.run("ai.projectRecord", json!({"kind":"user","summary":"Saved A","prompt":"Saved A"})).unwrap();
        let saved = (*app.session.active().unwrap().doc).clone();
        sync_prompt(&mut app);
        assert_eq!(app.ui.ai.prompt, "Saved A");
        app.ui.ai.prompt = "Unsent draft A".into();
        remember_prompt(&mut app);

        app.run("file.new", json!({"width":32,"height":24})).unwrap();
        sync_prompt(&mut app);
        assert!(app.ui.ai.prompt.is_empty(), "a new project must not inherit another project's prompt");
        app.ui.ai.prompt = "Draft B".into();
        remember_prompt(&mut app);
        app.session.set_active(0);
        sync_prompt(&mut app);
        assert_eq!(app.ui.ai.prompt, "Unsent draft A");
        app.session.set_active(1);
        sync_prompt(&mut app);
        assert_eq!(app.ui.ai.prompt, "Draft B");

        let mut reopened = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        reopened.session.add_document(saved, None);
        sync_prompt(&mut reopened);
        assert_eq!(reopened.ui.ai.prompt, "Saved A", "only the persisted prompt survives a fresh app session");
        reopened.session.close(0);
        sync_prompt(&mut reopened);
        assert!(reopened.ui.ai.prompt.is_empty());
        assert!(reopened.ai_runtime.project_document.is_none());
    }

    #[test]
    fn context_includes_selection_layers_reference_names_and_bounded_recent_history() {
        let mut app = app(Default::default());
        assert!(context(app.session.active().unwrap()).is_none());
        app.run("select.rect", json!({"x":2,"y":3,"width":12,"height":7})).unwrap();
        app.run("ai.projectEnsure", json!({})).unwrap();
        let st = app.session.active_mut().unwrap();
        st.path = Some("project.psd".into());
        let doc = Arc::make_mut(&mut st.doc);
        doc.layers.clear();
        for index in 0..130 {
            let mut layer = photocraft_doc::Layer::new(
                format!("{index}-{}", "L".repeat(200)),
                photocraft_doc::LayerContent::Raster(photocraft_raster::Surface::new(doc.pixel_format())),
            );
            layer.visible = index != 1;
            doc.layers.push(layer);
        }
        st.active_layer = doc.layers.get(2).map(|layer| layer.id);
        let image = photocraft_codecs::Image::from_u8(1, 1, photocraft_codecs::ChannelLayout::Rgba, vec![20, 80, 160, 255]).unwrap();
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        doc.ai_project.references.push(photocraft_doc::ai::ProjectReference { id: 1, name: "Materials.png".into(), png: Arc::new(png) });
        doc.ai_project.events.clear();
        doc.ai_project.next_sequence = 1;
        for index in 0..40 {
            doc.ai_project.record("edit", &format!("{index}:{}", "x".repeat(1500)));
        }
        doc.ai_project.last_prompt = "Keep the complete playback bar".into();
        let expected_selection = serde_json::to_value(doc.selection.as_ref().map(|selection| selection.content_bounds())).unwrap();
        let expected_active = st.active_layer.map(|id| id.0);
        let snapshot = context(st).unwrap();
        let memory: serde_json::Value = serde_json::from_str(&snapshot.memory).unwrap();
        assert_eq!(snapshot.open_id, st.open_id);
        assert_eq!(snapshot.path.as_deref(), Some("project.psd"));
        assert_eq!(memory["canvas"], json!({"width":32,"height":24}));
        assert_eq!(memory["selection"], expected_selection);
        assert_eq!(memory["activeLayer"], json!(expected_active));
        let layers = memory["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 128);
        assert!(layers.iter().all(|layer| layer["name"].as_str().unwrap().chars().count() <= 160));
        assert_eq!(layers[1]["visible"], false);
        assert_eq!(memory["references"], json!([{"id":1,"name":"Materials.png"}]));
        assert!(
            !snapshot.memory.contains("imageBase64") && !snapshot.memory.contains("iVBOR"),
            "image bytes belong to the reference channel, not the text memory"
        );
        assert_eq!(memory["lastPrompt"], "Keep the complete playback bar");
        let recent = memory["recentEvents"].as_array().unwrap();
        assert_eq!(recent.len(), 24);
        assert_eq!(recent.first().unwrap()["sequence"], 17);
        assert_eq!(recent.last().unwrap()["sequence"], 40);
        assert!(recent.iter().all(|event| event["summary"].as_str().unwrap().chars().count() <= 768));
        assert_eq!(snapshot.events.len(), 40, "the host journal retains events outside the short model context");
    }

    #[test]
    fn failed_sync_retries_without_mutating_the_document_or_starting_generation() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let recorded = calls.clone();
        let generated = Rc::new(Cell::new(0));
        let generation_calls = generated.clone();
        let mut app = app(crate::Services {
            project_sync: Some(Box::new(move |context| {
                let mut calls = recorded.borrow_mut();
                calls.push(context);
                if calls.len() == 1 { Err("temporary journal failure".into()) } else { Ok(()) }
            })),
            ai_generate: Some(Box::new(move |_| {
                generation_calls.set(generation_calls.get() + 1);
                Err("unexpected generation".into())
            })),
            ..Default::default()
        });
        app.run("ai.projectEnsure", json!({})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        let revision = app.session.active().unwrap().revision;
        let ctx = egui::Context::default();
        at(&ctx, 1.0);
        sync(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(app.ai_runtime.project_sync_error, "temporary journal failure");
        assert!(app.ai_runtime.project_synced.is_empty());
        at(&ctx, 2.0);
        sync(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 1, "do not retry before the backoff expires");
        at(&ctx, 3.1);
        sync(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 2);
        assert!(app.ai_runtime.project_sync_error.is_empty());
        assert_eq!(app.ai_runtime.project_synced.len(), 1);
        sync(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 2, "unchanged context is not submitted twice after success");
        assert_eq!(calls.borrow()[0].id, calls.borrow()[1].id);
        assert_eq!(calls.borrow()[0].memory, calls.borrow()[1].memory);
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().revision, revision);
        assert_eq!(generated.get(), 0);
    }

    #[test]
    fn absent_host_service_is_a_read_only_noop() {
        let mut app = app(Default::default());
        let before = app.session.active().unwrap().doc.clone();
        let revision = app.session.active().unwrap().revision;
        let ctx = egui::Context::default();
        for _ in 0..3 {
            sync(&mut app, &ctx);
        }
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().revision, revision);
        assert!(app.ai_runtime.project_synced.is_empty());
        assert!(app.ai_runtime.project_sync_error.is_empty());
        assert!(!app.ai_runtime.running());
    }

    #[test]
    fn completion_records_to_its_source_document_without_switching_the_selected_tab() {
        let mut app = app(Default::default());
        app.run("ai.projectEnsure", json!({})).unwrap();
        let source = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        app.run("file.new", json!({"width":32,"height":24})).unwrap();
        let selected = app.session.active().unwrap().doc.id;
        let other = app.session.active().unwrap().doc.clone();
        record(&mut app, Some(source), "assistant", "Added the complete toolbar");
        assert_eq!(app.session.active().unwrap().doc.id, selected);
        assert!(Arc::ptr_eq(&other, &app.session.active().unwrap().doc));
        assert_eq!(app.session.documents()[0].doc.ai_project.events.last().unwrap().summary, "Added the complete toolbar");
        app.session.close(0);
        record(&mut app, Some(source), "assistant", "Late result");
        assert_eq!(app.session.active().unwrap().doc.id, selected);
        assert!(Arc::ptr_eq(&other, &app.session.active().unwrap().doc));
        assert!(!app.ai_runtime.project_sync_error.is_empty());
    }

    #[test]
    fn queued_events_retry_after_floating_selection_finishes_in_source_order() {
        let mut app = app(Default::default());
        app.run("ai.projectEnsure", json!({})).unwrap();
        app.run("select.rect", json!({"x":2,"y":3,"width":12,"height":7})).unwrap();
        app.run("select.float", json!({"dx":1,"dy":0})).unwrap();
        let source = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        record(&mut app, Some(source), "assistant", "Finished first result");
        record(&mut app, Some(source), "cancelled", "Cancelled the next result");
        assert_eq!(app.ai_runtime.project_events.len(), 2);
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        let ctx = egui::Context::default();
        sync(&mut app, &ctx);
        assert_eq!(app.ai_runtime.project_events.len(), 2);
        app.run("select.drop", json!({})).unwrap();
        sync(&mut app, &ctx);
        assert!(app.ai_runtime.project_events.is_empty());
        let events = &app.session.active().unwrap().doc.ai_project.events;
        let completed: Vec<_> =
            events.iter().filter(|event| ["assistant", "cancelled"].contains(&event.kind.as_str())).map(|event| event.summary.as_str()).collect();
        assert_eq!(completed, ["Finished first result", "Cancelled the next result"]);
        let sequence = app.session.active().unwrap().doc.ai_project.next_sequence;
        sync(&mut app, &ctx);
        assert_eq!(app.session.active().unwrap().doc.ai_project.next_sequence, sequence, "flushing twice must not duplicate records");
        assert!(app.ai_runtime.project_sync_error.is_empty());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn completed_background_save_releases_queued_event_without_mutating_its_snapshot() {
        let mut app = app(Default::default());
        app.run("ai.projectEnsure", json!({})).unwrap();
        let source = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        let started = app.session.start_job("file.save", json!({}), "Save fixture", true, |_| Ok(()), |_, ()| Ok(json!({}))).unwrap();
        let photocraft_engine::jobs::Started::Job(job) = started else { panic!("native fixture must be asynchronous") };
        record(&mut app, Some(source), "assistant", "Generation finished during save");
        assert_eq!(app.ai_runtime.project_events.len(), 1);
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        app.session.wait_job(job).unwrap();
        sync(&mut app, &egui::Context::default());
        assert!(app.ai_runtime.project_events.is_empty());
        assert_eq!(app.session.active().unwrap().doc.ai_project.events.last().unwrap().summary, "Generation finished during save");
    }

    #[test]
    fn reopen_uses_new_prompt_and_sync_identity_and_rejects_the_old_completion() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let recorded = calls.clone();
        let mut app = app(crate::Services {
            project_sync: Some(Box::new(move |context| {
                recorded.borrow_mut().push(context);
                Ok(())
            })),
            ..Default::default()
        });
        app.run("ai.projectEnsure", json!({})).unwrap();
        app.run("ai.projectRecord", json!({"kind":"user","summary":"saved","prompt":"Saved prompt"})).unwrap();
        let saved = (*app.session.active().unwrap().doc).clone();
        let source = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        sync_prompt(&mut app);
        app.ui.ai.prompt = "Unsaved old draft".into();
        remember_prompt(&mut app);
        let ctx = egui::Context::default();
        sync(&mut app, &ctx);
        app.session.close(0);
        app.session.add_document(saved, None);
        assert_eq!(app.session.active().unwrap().doc.id, source.0);
        assert_ne!(app.session.active().unwrap().open_id, source.1);
        sync_prompt(&mut app);
        assert_eq!(app.ui.ai.prompt, "Saved prompt");
        let before = app.session.active().unwrap().doc.clone();
        record(&mut app, Some(source), "assistant", "Late result from closed project");
        sync(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 2, "reopened documents must be resubmitted even with identical persisted sequence numbers");
        assert_ne!(calls.borrow()[0].open_id, calls.borrow()[1].open_id);
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert!(app.ai_runtime.project_events.is_empty());
        assert!(app.ai_runtime.project_sync_error.contains("closed"), "successful host sync must not hide an unrecordable completion");
        assert!(!app.ai_runtime.project_drafts.contains_key(&source.1));
    }

    #[test]
    fn generation_does_not_overflow_a_blocked_event_queue() {
        let mut app = app(Default::default());
        app.run("ai.projectEnsure", json!({})).unwrap();
        app.run("select.rect", json!({"x":2,"y":3,"width":12,"height":7})).unwrap();
        app.run("select.float", json!({"dx":1,"dy":0})).unwrap();
        let source = app.session.active().map(|st| (st.doc.id, st.open_id)).unwrap();
        for _ in 0..MAX_PENDING_EVENTS - 1 {
            record(&mut app, Some(source), "assistant", "Blocked completion");
        }
        assert_eq!(app.ai_runtime.project_events.len(), MAX_PENDING_EVENTS - 1);
        let before = app.session.active().unwrap().doc.clone();
        assert!(prepare(&mut app).unwrap_err().contains("pending document operations"));
        assert_eq!(app.ai_runtime.project_events.len(), MAX_PENDING_EVENTS - 1);
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
    }
}
