//! Binding consent is handled on the UI thread; network and transfers use workers.
use crate::PhotocraftApp;
#[cfg(not(target_arch = "wasm32"))]
use link::Peer;
#[cfg(not(target_arch = "wasm32"))]
use link::native;
use photocraft_engine::peer_binding as link;
use serde_json::json;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct BindingUi {
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<Receiver<std::result::Result<Event, String>>>,
    #[cfg(not(target_arch = "wasm32"))]
    ready: Option<Peer>,
    #[cfg(not(target_arch = "wasm32"))]
    closing: bool,
    #[cfg(not(target_arch = "wasm32"))]
    started_close: bool,
    #[cfg(not(target_arch = "wasm32"))]
    auto_at: Option<f64>,
    #[cfg(not(target_arch = "wasm32"))]
    auto_attempt: bool,
    address: String,
    resource: String,
    resources: Vec<String>,
}
#[cfg(not(target_arch = "wasm32"))]
enum Event {
    Ready(Peer),
    Quiet,
    Bound(Peer),
    Mirrored(Peer),
}
#[cfg(not(target_arch = "wasm32"))]
fn worker(ui: &mut BindingUi, ctx: &egui::Context, run: impl FnOnce() -> link_result::Result<Event> + Send + 'static) {
    link::state().working = true;
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run().map_err(|e| e.to_string()));
        ctx.request_repaint();
    });
    ui.worker = Some(rx);
}
#[cfg(not(target_arch = "wasm32"))]
mod link_result {
    pub type Result<T> = photocraft_engine::Result<T>;
}
#[cfg(not(target_arch = "wasm32"))]
fn request(target: String, automatic: bool) -> link_result::Result<Event> {
    let (id, port) = {
        let s = link::state();
        (s.id.clone(), s.port)
    };
    let status = native::json_request(&target, "/binding/status", "", &json!({}))?;
    if status["app"] != "Defold" {
        return Err(photocraft_engine::EngineError::Other("This address is not a Defold binding service".into()));
    }
    let existing = { link::state().active.clone() };
    if let Some(peer) = existing.filter(|p| status["id"] == p.id && status["bound"] == id) {
        return Ok(Event::Bound(peer));
    }
    if automatic && !status["bound"].is_null() {
        return Ok(Event::Quiet);
    }
    let reply = native::json_request(&target, "/binding/request", "", &json!({"id":id,"address":native::own_address(&target,port)?}))?;
    let token = reply["ticket"].as_str().ok_or_else(|| photocraft_engine::EngineError::Other("No binding ticket".into()))?.to_owned();
    let peer = Peer { id: reply["id"].as_str().unwrap_or("").into(), address: target, token, project: String::new(), root: String::new() };
    let deadline = Instant::now() + Duration::from_secs(170);
    while Instant::now() < deadline {
        let result = native::json_request(&peer.address, "/binding/result", &peer.token, &json!({}))?;
        if result["phase"] == "ready" {
            return Ok(Event::Ready(peer));
        }
        if result["phase"] == "rejected" {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = native::json_request(&peer.address, "/binding/cancel", &peer.token, &json!({}));
    Err(photocraft_engine::EngineError::Other("Binding was cancelled or timed out".into()))
}
#[cfg(not(target_arch = "wasm32"))]
fn busy(app: &PhotocraftApp) -> bool {
    app.saving() || app.printing() || !app.jobs.opens.is_empty() || !app.session.jobs().is_empty() || app.ai_runtime.running()
}
#[cfg(not(target_arch = "wasm32"))]
fn empty(app: &PhotocraftApp) -> bool {
    app.session.documents().is_empty() && app.session.gui.documents.is_empty() && !busy(app)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let mut ui = std::mem::take(&mut app.peer_binding);
    if let Some(rx) = &ui.worker {
        match rx.try_recv() {
            Ok(Ok(Event::Quiet)) => {
                ui.worker = None;
                ui.auto_attempt = false;
                link::state().working = false;
            }
            Ok(Ok(Event::Ready(peer))) => {
                ui.auto_attempt = false;
                ui.worker = None;
                ui.ready = Some(peer);
                ui.closing = link::state().active.is_none() && empty(app);
                ui.started_close = ui.closing;
            }
            Ok(Ok(Event::Bound(peer))) | Ok(Ok(Event::Mirrored(peer))) => {
                ui.resources = std::fs::read(std::path::Path::new(&peer.root).join(".photocraft-resources.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .and_then(|manifest| {
                        manifest["files"]
                            .as_array()
                            .map(|files| files.iter().filter_map(|f| f["path"].as_str()).filter(|p| p.ends_with(".gui")).map(|p| format!("/{p}")).collect())
                    })
                    .unwrap_or_default();
                if ui.resource.is_empty() {
                    ui.resource = ui.resources.first().cloned().unwrap_or_default();
                }
                native::replace(peer);
                link::state().working = false;
                link::state().frozen = false;
                ui.worker = None;
                ui.ready = None;
                ui.closing = false;
                ui.started_close = false;
                app.ui.status = tl!("Defold bound; project resources are ready").into();
            }
            Ok(Err(e)) => {
                link::state().working = false;
                link::state().frozen = false;
                ui.worker = None;
                ui.ready = None;
                ui.closing = false;
                ui.started_close = false;
                if !ui.auto_attempt {
                    link::state().error = Some(e.clone());
                    app.ui.status = e;
                    app.ui.status_error = true;
                }
                ui.auto_attempt = false;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                link::state().working = false;
                link::state().frozen = false;
                ui.worker = None;
                link::state().error = Some("Binding worker stopped".into());
            }
            Err(_) => {}
        }
    }
    if link::state().port > 0 {
        let now = ctx.input(|i| i.time);
        let at = *ui.auto_at.get_or_insert(now + 1.0);
        if now >= at && ui.worker.is_none() && ui.ready.is_none() {
            ui.auto_at = Some(now + 5.0);
            let mut s = link::state();
            if s.active.is_none() && s.incoming.is_none() && s.outgoing.is_none() && !s.working {
                ui.auto_attempt = true;
                s.outgoing = Some(format!("127.0.0.1:{}", link::DEFOLD_PORT));
            }
        }
    }
    if ui.worker.is_none() && ui.ready.is_none() {
        let target = link::state().outgoing.take();
        if let Some(target) = target {
            let automatic = ui.auto_attempt;
            worker(&mut ui, ctx, move || request(target, automatic));
        }
    }
    let incoming = {
        let mut s = link::state();
        if s.incoming.as_ref().is_some_and(|o| o.expires < Instant::now()) {
            s.incoming = None;
        }
        s.incoming.clone()
    };
    if let Some(offer) = &incoming {
        if offer.phase == "commit" {
            if empty(app) {
                let Ok(mut peer) = native::accept_commit(&offer.ticket) else {
                    app.peer_binding = ui;
                    return;
                };
                worker(&mut ui, ctx, move || {
                    native::mirror(&mut peer)?;
                    Ok(Event::Mirrored(peer))
                });
            } else {
                link::state().incoming = None;
                link::state().error = Some("Workspace is busy; binding was not switched".into());
            }
        } else if offer.phase == "confirm"
            && link::state().active.is_none()
            && empty(app)
            && link::address(&offer.peer.address, link::DEFOLD_PORT).is_ok_and(|a| a.ip().is_loopback())
        {
            if let Some(o) = &mut link::state().incoming {
                o.phase = "ready".into();
            }
        }
    }
    let incoming = { link::state().incoming.clone() };
    let needs_prompt = ui.ready.is_some() || incoming.as_ref().is_some_and(|o| o.phase == "confirm" || o.phase == "closing");
    if needs_prompt && !ui.closing {
        let target = ui.ready.as_ref().map(|p| p.address.clone()).or_else(|| incoming.as_ref().map(|o| o.peer.address.clone())).unwrap_or_default();
        egui::Modal::new(egui::Id::new("peer-binding-confirm")).show(ctx, |view| {
            view.heading(tl!("Switch application binding?"));
            view.label(format!("{} {target}", tl!("Requested peer:")));
            if let Some(peer) = &link::state().active {
                view.label(format!("{} {}", tl!("Current peer:"), peer.address));
            }
            view.label(tl!("All open files must be saved or closed before switching. Unsaved files will ask for confirmation."));
            view.horizontal(|view| {
                if view.add_enabled(!busy(app), egui::Button::new(tl!("Continue and close files"))).clicked() {
                    ui.closing = true;
                    ui.started_close = false;
                    if let Some(o) = &mut link::state().incoming {
                        o.phase = "closing".into();
                    }
                }
                if view.button(tl!("Cancel")).clicked() {
                    if let Some(peer) = ui.ready.take() {
                        std::thread::spawn(move || {
                            let _ = native::json_request(&peer.address, "/binding/cancel", &peer.token, &json!({}));
                        });
                    }
                    link::state().incoming = None;
                    link::state().working = false;
                    ui.closing = false;
                }
            });
        });
    }
    if ui.closing {
        if !ui.started_close {
            ui.started_close = true;
            let _ = crate::menus::invoke(app, ctx, "file.closeAll", json!({}));
        } else if empty(app) {
            if let Some(mut peer) = ui.ready.take() {
                link::state().frozen = true;
                worker(&mut ui, ctx, move || {
                    // A lost HTTP acknowledgement must be reconciled before deciding whether the switch failed.
                    let _ = native::json_request(&peer.address, "/binding/commit", &peer.token, &json!({}));
                    let deadline = Instant::now() + Duration::from_secs(15);
                    loop {
                        let v = native::json_request(&peer.address, "/binding/committed", &peer.token, &json!({}))?;
                        if v["phase"] == "bound" {
                            break;
                        }
                        if Instant::now() > deadline {
                            return Err(photocraft_engine::EngineError::Other("Binding commit timed out".into()));
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    native::replace(peer.clone());
                    native::mirror(&mut peer)?;
                    Ok(Event::Bound(peer))
                });
            } else if let Some(o) = &mut link::state().incoming {
                o.phase = "ready".into();
            }
            ui.closing = false;
            ui.started_close = false;
        } else if app.discard.is_none() && app.ui.defold.close.is_none() && !busy(app) && !app.file_dialog_open() {
            if let Some(peer) = ui.ready.take() {
                std::thread::spawn(move || {
                    let _ = native::json_request(&peer.address, "/binding/cancel", &peer.token, &json!({}));
                });
            }
            link::state().incoming = None;
            link::state().working = false;
            ui.closing = false;
            ui.started_close = false;
        }
    }
    if link::state().port > 0 || ui.worker.is_some() || incoming.is_some() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    if ui.worker.is_none() && link::state().refresh {
        link::state().refresh = false;
        let peer = { link::state().active.clone() };
        if let Some(mut peer) = peer {
            worker(&mut ui, ctx, move || {
                native::mirror(&mut peer)?;
                Ok(Event::Mirrored(peer))
            });
        }
    }
    if ui.worker.is_none() {
        let open = {
            let mut s = link::state();
            if s.active.as_ref().is_some_and(|p| !p.root.is_empty()) && !s.opens.is_empty() {
                let path = s.opens.remove(0);
                s.active.as_ref().map(|p| (p.root.clone(), path))
            } else {
                None
            }
        };
        if let Some((root, path)) = open {
            match native::safe_relative(&path) {
                Ok(rel) => {
                    if let Err(e) = app.run("defold.openDesign", json!({"path":std::path::Path::new(&root).join(rel).to_string_lossy()})) {
                        app.ui.status = e;
                        app.ui.status_error = true;
                    }
                }
                Err(e) => {
                    app.ui.status = e.to_string();
                    app.ui.status_error = true;
                }
            }
        }
    }
    app.peer_binding = ui;
}
#[cfg(target_arch = "wasm32")]
pub fn tick(_: &mut PhotocraftApp, _: &egui::Context) {}

pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.strong(tl!("Defold binding"));
    let status = link::inspect();
    if let Some(address) = status["peer"]["address"].as_str() {
        ui.label(format!("{} {address}", tl!("Bound to")));
    } else {
        ui.label(tl!("Not bound; trying the local Defold editor"));
    }
    ui.label(tl!("Defold IP address"));
    if app.peer_binding.address.is_empty() {
        app.peer_binding.address = format!("127.0.0.1:{}", link::DEFOLD_PORT);
    }
    ui.text_edit_singleline(&mut app.peer_binding.address);
    if ui.button(tl!("Bind Defold")).clicked() {
        let target = app.peer_binding.address.clone();
        let _ = app.run("defold.bind", json!({"address":target}));
    }
    if let Some(error) = status["error"].as_str() {
        ui.add(egui::Label::new(error).wrap());
    }
    let bound_peer = { link::state().active.clone() };
    if let Some(peer) = bound_peer {
        if !peer.root.is_empty() {
            ui.label(tl!("GUI resource path"));
            egui::ComboBox::from_id_salt("bound-defold-resource").selected_text(&app.peer_binding.resource).width(ui.available_width()).show_ui(ui, |ui| {
                for path in &app.peer_binding.resources {
                    ui.selectable_value(&mut app.peer_binding.resource, path.clone(), path);
                }
            });
            ui.text_edit_singleline(&mut app.peer_binding.resource);
            if ui.button(tl!("Open GUI in both editors")).clicked() {
                let resource = app.peer_binding.resource.clone();
                let _ = app.run("defold.openBoundResource", json!({"path":resource}));
            }
        }
    }
    if status["pending"]["phase"] == "ready" {
        ui.label(tl!("Waiting for the other application to confirm"));
        if ui.button(tl!("Cancel binding request")).clicked() {
            link::state().incoming = None;
        }
    }
    if status["peer"].is_object() && ui.button(tl!("Refresh project resources")).clicked() {
        link::state().refresh = true;
    }
    if status["working"] == true {
        ui.label(tl!("Waiting for confirmation or transferring project resources…"));
    }
    ui.separator();
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::Services;
    use photocraft_engine::{
        Session,
        peer_binding::{Binding, Offer},
    };
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn peer(id: &str) -> Peer {
        Peer { id: id.into(), address: "127.0.0.1:17654".into(), token: format!("{id}-secret"), project: String::new(), root: String::new() }
    }
    fn fixture(phase: &str) -> (PhotocraftApp, Binding) {
        let saved = std::mem::take(&mut *link::state());
        link::state().active = Some(peer("old"));
        link::state().incoming =
            Some(Offer { ticket: "next-ticket".into(), peer: peer("new"), phase: phase.into(), expires: Instant::now() + Duration::from_secs(180) });
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        app.session.execute("file.new", json!({"width":8,"height":8})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        assert!(app.session.active().unwrap().is_dirty());
        (app, saved)
    }
    fn frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
        let mut output = ctx.run_ui(Default::default(), |ui| tick(app, ui.ctx()));
        output.textures_delta.clear();
    }
    #[test]
    fn an_open_document_prevents_a_queued_peer_switch() {
        let _serial = SERIAL.lock().unwrap();
        let (mut app, saved) = fixture("commit");
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        assert_eq!(link::state().active.as_ref().unwrap().id, "old");
        assert!(link::state().incoming.is_none());
        assert_eq!(app.session.documents().len(), 1);
        *link::state() = saved;
    }
    #[test]
    fn cancelling_the_save_prompt_preserves_documents_and_original_peer() {
        let _serial = SERIAL.lock().unwrap();
        let (mut app, saved) = fixture("closing");
        app.peer_binding.closing = true;
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        assert!(app.discard.is_some(), "The existing per-document save prompt must be used");
        assert_eq!(link::state().active.as_ref().unwrap().id, "old");
        app.discard = None; // Existing Save / Discard / Cancel UI dismisses its prompt on Cancel.
        frame(&mut app, &ctx);
        assert!(link::state().incoming.is_none());
        assert!(!link::state().working);
        assert_eq!(link::state().active.as_ref().unwrap().id, "old");
        assert_eq!(app.session.documents().len(), 1);
        assert!(app.session.documents()[0].is_dirty());
        *link::state() = saved;
    }
}
