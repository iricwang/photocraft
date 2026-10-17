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
    operation: Option<(String, u64)>,
    #[cfg(not(target_arch = "wasm32"))]
    auto_at: Option<f64>,
    #[cfg(not(target_arch = "wasm32"))]
    auto_attempt: bool,
    #[cfg(not(target_arch = "wasm32"))]
    auto_cursor: usize,
    #[cfg(not(target_arch = "wasm32"))]
    attempt_address: Option<String>,
    address: String,
    resource: String,
    resources: Vec<String>,
}
#[cfg(not(target_arch = "wasm32"))]
enum Event {
    Ready(Peer),
    Quiet,
    Connected(Peer),
    Bound(Peer),
    Mirrored(Peer),
    Cancelled,
}
#[cfg(not(target_arch = "wasm32"))]
fn worker(
    ui: &mut BindingUi,
    ctx: &egui::Context,
    address: String,
    expected_epoch: Option<u64>,
    run: impl FnOnce(u64) -> link_result::Result<Event> + Send + 'static,
) -> bool {
    let epoch = {
        let mut state = link::state();
        let epoch = state.epoch(&address);
        if expected_epoch.is_some_and(|expected| expected != epoch) {
            state.working = false;
            state.frozen = false;
            ui.operation = None;
            return false;
        }
        state.working = true;
        epoch
    };
    ui.operation = Some((address.clone(), epoch));
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = ensure_current(&address, epoch).and_then(|()| run(epoch));
        let result = if ensure_current(&address, epoch).is_err() {
            if let Ok(event) = result {
                cancel_event(event);
            }
            Ok(Event::Cancelled)
        } else {
            result
        };
        let _ = tx.send(result.map_err(|e| e.to_string()));
        ctx.request_repaint();
    });
    ui.worker = Some(rx);
    true
}
#[cfg(not(target_arch = "wasm32"))]
fn ensure_current(address: &str, epoch: u64) -> link_result::Result<()> {
    if link::state().epoch(address) != epoch {
        return Err(photocraft_engine::EngineError::Other("Connection operation was cancelled".into()));
    }
    Ok(())
}
#[cfg(not(target_arch = "wasm32"))]
fn cancel_event(event: Event) {
    if let Event::Ready(peer) | Event::Bound(peer) | Event::Mirrored(peer) | Event::Connected(peer) = event {
        cancel_peer(&peer);
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn cancel_peer(peer: &Peer) {
    {
        let mut state = link::state();
        if state.outgoing_peer.as_ref().is_some_and(|pending| pending.token == peer.token) {
            state.outgoing_peer = None;
        }
    }
    let _ = native::json_request(&peer.address, "/binding/cancel", &peer.token, &json!({}));
    let _ = native::json_request(&peer.address, "/binding/release", &peer.token, &json!({}));
}
#[cfg(not(target_arch = "wasm32"))]
mod link_result {
    pub type Result<T> = photocraft_engine::Result<T>;
}
#[cfg(not(target_arch = "wasm32"))]
fn request(target: String, automatic: bool, epoch: u64) -> link_result::Result<Event> {
    ensure_current(&target, epoch)?;
    let (id, port) = {
        let s = link::state();
        if automatic && s.disconnected.contains(&target) {
            return Ok(Event::Quiet);
        }
        (s.id.clone(), s.port)
    };
    let status = native::json_request(&target, "/binding/status", "", &json!({}))?;
    if status["app"] != "Defold" {
        return Err(photocraft_engine::EngineError::Other("This address is not a Defold binding service".into()));
    }
    let existing = { link::state().connections().into_iter().find(|p| status["id"] == p.id).cloned() };
    let bound = status["boundPeers"].as_array().is_some_and(|ids| ids.iter().any(|peer| peer == &id)) || status["bound"] == id;
    if let Some(existing) = existing.as_ref().filter(|_| bound) {
        return Ok(Event::Connected(existing.clone()));
    }
    if automatic && status["bindingVersion"] != 2 && !status["bound"].is_null() && status["bound"] != id {
        return Ok(Event::Quiet);
    }
    let reply = native::json_request(&target, "/binding/request", "", &json!({"id":id,"address":native::own_address(&target,port)?}))?;
    let token = reply["ticket"].as_str().ok_or_else(|| photocraft_engine::EngineError::Other("No binding ticket".into()))?.to_owned();
    let mut peer = Peer { id: reply["id"].as_str().unwrap_or("").into(), address: target, token, project: String::new(), root: String::new() };
    let result = (|| {
        {
            let mut state = link::state();
            if state.epoch(&peer.address) != epoch {
                return Err(photocraft_engine::EngineError::Other("Connection operation was cancelled".into()));
            }
            state.outgoing_peer = Some(peer.clone());
        }
        let deadline = Instant::now() + Duration::from_secs(170);
        while Instant::now() < deadline {
            ensure_current(&peer.address, epoch)?;
            let result = native::json_request(&peer.address, "/binding/result", &peer.token, &json!({}))?;
            if result["phase"] == "ready" {
                native::resolve_project(&mut peer)?;
                ensure_current(&peer.address, epoch)?;
                return Ok(Event::Ready(peer.clone()));
            }
            if result["phase"] == "rejected" {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Err(photocraft_engine::EngineError::Other("Binding was cancelled or timed out".into()))
    })();
    if result.is_err() {
        cancel_peer(&peer);
    }
    result
}
#[cfg(not(target_arch = "wasm32"))]
fn different_project(app: &PhotocraftApp, peer: &Peer) -> bool {
    if link::state().connections().into_iter().any(|known| known.address == peer.address && link::same_project(&known.project, &peer.project)) {
        return false;
    }
    if let Some(old) = &link::state().active
        && !old.project.is_empty()
        && !link::same_project(&old.project, &peer.project)
    {
        return true;
    }
    app.session.documents().iter().filter_map(|d| d.doc.defold.as_ref()).any(|d| !link::same_project(&d.root, &peer.root))
        || app.session.gui.documents.iter().any(|d| !link::same_project(&d.root, &peer.root))
}
#[cfg(not(target_arch = "wasm32"))]
fn incoming_needs_consent(peer: &Peer, known: Option<&Peer>) -> bool {
    !link::address(&peer.address, link::DEFOLD_PORT).is_ok_and(|a| a.ip().is_loopback())
        && known.is_none_or(|old| old.address != peer.address || !link::same_project(&old.project, &peer.project))
}
#[cfg(not(target_arch = "wasm32"))]
fn commit(peer: Peer, ui: &mut BindingUi, ctx: &egui::Context) {
    ui.ready = None;
    let expected_epoch = ui.operation.as_ref().filter(|(address, _)| address == &peer.address).map(|(_, epoch)| *epoch);
    let cancelled = peer.clone();
    if !worker(ui, ctx, peer.address.clone(), expected_epoch, move |epoch| {
        let mut peer = peer;
        let result = (|| {
            ensure_current(&peer.address, epoch)?;
            let _ = native::json_request(&peer.address, "/binding/commit", &peer.token, &json!({}));
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                ensure_current(&peer.address, epoch)?;
                let v = native::json_request(&peer.address, "/binding/committed", &peer.token, &json!({}))?;
                if v["phase"] == "bound" {
                    break;
                }
                if Instant::now() > deadline {
                    return Err(photocraft_engine::EngineError::Other("Binding commit timed out".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            native::prepare_project(&mut peer)?;
            Ok(Event::Bound(peer.clone()))
        })();
        if result.is_err() {
            cancel_peer(&peer);
        }
        result
    }) {
        std::thread::spawn(move || cancel_peer(&cancelled));
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let mut ui = std::mem::take(&mut app.peer_binding);
    if ui.operation.as_ref().is_some_and(|(address, epoch)| ensure_current(address, *epoch).is_err()) {
        if let Some(rx) = ui.worker.take()
            && let Ok(Ok(event)) = rx.try_recv()
        {
            std::thread::spawn(move || cancel_event(event));
        }
        ui.ready = None;
        ui.operation = None;
        ui.auto_attempt = false;
        let mut state = link::state();
        state.working = false;
        state.frozen = false;
    }
    if let Some(rx) = &ui.worker {
        match rx.try_recv() {
            Ok(Ok(Event::Cancelled)) => {
                ui.worker = None;
                ui.ready = None;
                ui.operation = None;
                ui.auto_attempt = false;
                let mut state = link::state();
                state.working = false;
                state.frozen = false;
            }
            Ok(Ok(Event::Mirrored(peer))) if link::state().peer_for_token(&peer.token).is_none() => {
                ui.worker = None;
                ui.operation = None;
                let mut state = link::state();
                state.working = false;
                state.frozen = false;
            }
            Ok(Ok(Event::Connected(peer))) => {
                ui.worker = None;
                ui.auto_attempt = false;
                let mut state = link::state();
                state.working = false;
                state.mark_address(&peer.address, true);
                state.error = None;
            }
            Ok(Ok(Event::Quiet)) => {
                ui.worker = None;
                ui.auto_attempt = false;
                link::state().working = false;
                if let Some(address) = &ui.attempt_address {
                    link::state().mark_address(address, false);
                }
            }
            Ok(Ok(Event::Ready(peer))) => {
                ui.auto_attempt = false;
                ui.worker = None;
                ui.ready = Some(peer);
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
                if !std::path::Path::new(&peer.root).join(".photocraft-remote").is_file() {
                    let mut directories = vec![std::path::PathBuf::from(&peer.root)];
                    let mut visited = 0;
                    while let Some(directory) = directories.pop() {
                        let Ok(entries) = std::fs::read_dir(directory) else { continue };
                        for entry in entries.flatten() {
                            visited += 1;
                            if visited > 20000 {
                                directories.clear();
                                break;
                            }
                            let name = entry.file_name();
                            let name = name.to_string_lossy();
                            if name.starts_with('.') || matches!(name.as_ref(), "build" | "target") {
                                continue;
                            }
                            let Ok(kind) = entry.file_type() else { continue };
                            if kind.is_dir() {
                                directories.push(entry.path());
                            } else if kind.is_file()
                                && name.ends_with(".gui")
                                && let Ok(path) = entry.path().strip_prefix(&peer.root)
                            {
                                ui.resources.push(format!("/{}", path.to_string_lossy().replace('\\', "/")));
                            }
                        }
                    }
                    ui.resources.sort();
                }
                if ui.resource.is_empty() || !ui.resources.contains(&ui.resource) {
                    ui.resource = ui.resources.first().cloned().unwrap_or_default();
                }
                let epoch = ui.operation.as_ref().filter(|(address, _)| address == &peer.address).map(|(_, epoch)| *epoch);
                let accepted = epoch.is_some_and(|epoch| native::replace(peer.clone(), epoch));
                if !accepted {
                    ui.resources.clear();
                    ui.resource.clear();
                    let mut state = link::state();
                    state.refresh = state.active.is_some();
                    drop(state);
                    std::thread::spawn(move || cancel_peer(&peer));
                }
                link::state().working = false;
                link::state().frozen = false;
                ui.worker = None;
                ui.ready = None;
                app.ui.status = if accepted { tl!("Defold bound; project resources are ready") } else { tl!("Disconnected") }.into();
            }
            Ok(Err(e)) => {
                if ui.auto_attempt
                    && let Some(address) = &ui.attempt_address
                {
                    link::state().mark_address(address, false);
                }
                link::state().working = false;
                link::state().frozen = false;
                ui.worker = None;
                ui.ready = None;
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
            if s.incoming.is_none() && s.outgoing.is_none() && !s.working {
                ui.auto_attempt = true;
                let addresses = s.automatic_targets(&format!("127.0.0.1:{}", link::DEFOLD_PORT));
                s.outgoing = if addresses.is_empty() { None } else { addresses.get(ui.auto_cursor % addresses.len()).cloned() };
                ui.auto_attempt = s.outgoing.is_some();
                ui.auto_cursor = ui.auto_cursor.wrapping_add(1);
            }
        }
    }
    if ui.worker.is_none() && ui.ready.is_none() {
        let target = link::state().outgoing.take();
        if let Some(target) = target {
            let automatic = ui.auto_attempt;
            ui.attempt_address = Some(target.clone());
            worker(&mut ui, ctx, target.clone(), None, move |epoch| request(target, automatic, epoch));
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
        if offer.phase == "commit" && ui.worker.is_none() && ui.ready.is_none() {
            let Ok(mut peer) = native::accept_commit(&offer.ticket) else {
                app.peer_binding = ui;
                return;
            };
            worker(&mut ui, ctx, peer.address.clone(), None, move |_| {
                native::prepare_project(&mut peer)?;
                Ok(Event::Mirrored(peer))
            });
        } else if offer.phase == "confirm" && ui.worker.is_none() && ui.ready.is_none() {
            // Metadata is authenticated with the pending ticket before project identity is used.
            let mut peer = offer.peer.clone();
            if peer.root.is_empty() {
                worker(&mut ui, ctx, peer.address.clone(), None, move |_| {
                    native::resolve_project(&mut peer)?;
                    Ok(Event::Ready(peer))
                });
                // Ready for an incoming request is handled without sending a reciprocal commit.
            }
        }
    }
    if let Some(peer) = ui.ready.clone() {
        let incoming_ticket = incoming.as_ref().filter(|o| o.ticket == peer.token).map(|o| o.ticket.clone());
        let known = { link::state().connections().into_iter().find(|known| known.address == peer.address).cloned() };
        let mut accepted = !(different_project(app, &peer) || incoming_ticket.is_some() && incoming_needs_consent(&peer, known.as_ref()));
        let mut cancelled = false;
        if !accepted {
            egui::Modal::new(egui::Id::new("peer-binding-confirm")).show(ctx, |view| {
                view.heading(tl!("Connect project"));
                view.label(&peer.project);
                view.label(tl!("Open documents and unsaved edits stay open. Only manual sync publishes changes to their own project."));
                view.horizontal(|view| {
                    accepted = view.button(tl!("Connect project")).clicked();
                    cancelled = view.button(tl!("Cancel")).clicked();
                });
            });
        }
        if accepted {
            if let Some(ticket) = incoming_ticket {
                if let Some(o) = &mut link::state().incoming
                    && o.ticket == ticket
                {
                    o.peer = peer;
                    o.phase = "ready".into();
                }
                ui.ready = None;
                link::state().working = false;
            } else {
                commit(peer, &mut ui, ctx);
            }
        } else if cancelled {
            ui.ready = None;
            ui.operation = None;
            {
                let mut state = link::state();
                if state.outgoing_peer.as_ref().is_some_and(|pending| pending.token == peer.token) {
                    state.outgoing_peer = None;
                }
            }
            if incoming_ticket.is_some() {
                link::state().incoming = None;
            } else {
                std::thread::spawn(move || {
                    let _ = native::json_request(&peer.address, "/binding/cancel", &peer.token, &json!({}));
                });
            }
            link::state().working = false;
        }
    }
    if link::state().port > 0 || ui.worker.is_some() || incoming.is_some() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    let refresh_ready = {
        let state = link::state();
        state.refresh && state.incoming.is_none()
    };
    if ui.worker.is_none() && ui.ready.is_none() && refresh_ready {
        link::state().refresh = false;
        let peer = { link::state().active.clone() };
        if let Some(mut peer) = peer {
            worker(&mut ui, ctx, peer.address.clone(), None, move |_| {
                native::mirror(&mut peer)?;
                Ok(Event::Mirrored(peer))
            });
        }
    }
    if ui.worker.is_none() {
        let open = {
            let mut s = link::state();
            if !s.opens.is_empty() {
                let (peer, path) = s.opens.remove(0);
                s.peer_for_token(&peer.token).filter(|current| !current.root.is_empty()).map(|current| (current.root.clone(), path))
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
    ui.label(format!("PhotoCraft · {}", status["port"]));
    if let Some(address) = status["peer"]["address"].as_str() {
        ui.label(format!("{} {address}", if status["connected"] == true { tl!("Bound to") } else { tl!("Reconnecting to") }));
        if let Some(project) = status["peer"]["project"].as_str() {
            ui.label(project);
        }
    } else {
        let paused = link::state().disconnected.contains(&format!("127.0.0.1:{}", link::DEFOLD_PORT));
        ui.label(if paused { tl!("Disconnected") } else { tl!("Not bound; trying the local Defold editor") });
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
    let peers = status["peers"].as_array().cloned().unwrap_or_default();
    let mut selected = status["peer"]["id"].as_str().unwrap_or("").to_owned();
    let previous = selected.clone();
    ui.add_enabled_ui(status["working"] != true, |ui| {
        egui::ComboBox::from_id_salt("bound-defold-connection")
            .selected_text(status["peer"]["project"].as_str().unwrap_or("—"))
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for item in &peers {
                    let peer = &item["peer"];
                    if let Some(id) = peer["id"].as_str() {
                        ui.selectable_value(
                            &mut selected,
                            id.to_owned(),
                            format!("{} · {}", peer["project"].as_str().unwrap_or(""), peer["address"].as_str().unwrap_or("")),
                        );
                    }
                }
            });
    });
    if selected != previous {
        if let Err(error) = app.run("defold.selectBinding", json!({"id":selected})) {
            app.ui.status = error;
            app.ui.status_error = true;
        } else {
            app.peer_binding.resources.clear();
            app.peer_binding.resource.clear();
            link::state().refresh = true;
        }
    }
    if !selected.is_empty() && ui.add_enabled(status["working"] != true, egui::Button::new(tl!("Disconnect selected Defold"))).clicked() {
        if let Err(error) = app.run("defold.unbind", json!({"id":selected})) {
            app.ui.status = error;
            app.ui.status_error = true;
        } else {
            app.peer_binding.resource.clear();
            app.peer_binding.resources.clear();
            app.ui.status = tl!("Disconnected").into();
            app.ui.status_error = false;
            let refresh = link::inspect()["peer"].is_object();
            link::state().refresh = refresh;
        }
    }
    let bound_peer = { link::state().active.clone() };
    if let Some(peer) = bound_peer
        && !peer.root.is_empty()
    {
        ui.label(tl!("GUI resource path"));
        egui::ComboBox::from_id_salt("bound-defold-resource").selected_text(&app.peer_binding.resource).width(ui.available_width()).show_ui(ui, |ui| {
            for path in &app.peer_binding.resources {
                ui.selectable_value(&mut app.peer_binding.resource, path.clone(), path);
            }
        });
        ui.text_edit_singleline(&mut app.peer_binding.resource);
        if ui.button(tl!("Open GUI in both editors")).clicked() {
            let resource = app.peer_binding.resource.clone();
            let _ = app.run("defold.openBoundResource", json!({"path":resource,"id":peer.id}));
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
        assert!(app.session.execute("file.new", json!({"width":8,"height":8})).is_ok());
        assert!(app.session.execute("layer.new.layer", json!({})).is_ok());
        assert!(app.session.active().is_some_and(|doc| doc.is_dirty()));
        (app, saved)
    }
    fn frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
        let mut output = ctx.run_ui(Default::default(), |ui| tick(app, ui.ctx()));
        output.textures_delta.clear();
    }
    #[test]
    fn reconnect_renews_credentials_when_the_editor_still_names_this_client() {
        use std::io::{BufRead, Read, Write};
        let _serial = SERIAL.lock().unwrap();
        let saved = std::mem::take(&mut *link::state());
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("game.project"), "project").unwrap();
        let project = root.path().to_string_lossy().into_owned();
        link::state().id = "craft-client".into();
        let mut previous = peer("previous-editor");
        previous.project = project.clone();
        link::state().active = Some(previous);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            for response in [
                json!({"app":"Defold","id":"restarted-editor","bound":"craft-client"}),
                json!({"ticket":"renewed-ticket","id":"restarted-editor"}),
                json!({"phase":"ready"}),
                json!({"project":project}),
            ] {
                let mut stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    if Instant::now() > deadline {
                        return;
                    }
                    std::thread::park_timeout(Duration::from_millis(5));
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        size = value.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; size];
                reader.read_exact(&mut bytes).unwrap();
                let bytes = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n", bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        let result = request(target, true, 0);
        *link::state() = saved;
        server.join().unwrap();
        assert!(matches!(result, Ok(Event::Ready(peer)) if peer.token == "renewed-ticket" && peer.id == "restarted-editor"));
    }
    #[test]
    fn unknown_remote_connections_need_consent_but_known_project_reconnections_do_not() {
        let local = peer("local");
        assert!(!incoming_needs_consent(&local, None));
        let mut remote = peer("remote");
        remote.address = "192.0.2.10:17654".into();
        remote.project = "remote-project".into();
        assert!(incoming_needs_consent(&remote, None));
        assert!(!incoming_needs_consent(&remote, Some(&remote)));
        let mut another = remote.clone();
        another.project = "another-project".into();
        assert!(incoming_needs_consent(&remote, Some(&another)));
    }
    #[test]
    fn known_project_reconnection_does_not_switch_or_close_other_project_documents() {
        let _serial = SERIAL.lock().unwrap();
        let (app, saved) = fixture("confirm");
        let mut a = peer("a");
        a.address = "127.0.0.1:19001".into();
        a.root = "/game/a".into();
        a.project = a.root.clone();
        let mut b = peer("b");
        b.address = "127.0.0.1:19002".into();
        b.root = "/game/b".into();
        b.project = b.root.clone();
        link::state().put(a.clone());
        link::state().put(b);
        assert!(!different_project(&app, &a));
        assert!(app.session.documents()[0].is_dirty());
        *link::state() = saved;
    }
    #[test]
    fn ordinary_dirty_documents_do_not_require_closing_for_a_project_connection() {
        let _serial = SERIAL.lock().unwrap();
        let (app, saved) = fixture("confirm");
        let mut next = peer("new");
        next.project = "project".into();
        next.root = "project".into();
        assert!(!different_project(&app, &next));
        assert_eq!(app.session.documents().len(), 1);
        assert!(app.session.documents()[0].is_dirty());
        *link::state() = saved;
    }
    #[test]
    fn same_project_incoming_connection_preserves_dirty_gui_and_image_documents() {
        let _serial = SERIAL.lock().unwrap();
        let (mut app, saved) = fixture("confirm");
        let root = tempfile::tempdir().unwrap();
        let root = root.path().to_string_lossy().into_owned();
        let mut next = peer("new");
        next.project = root.clone();
        next.root = root.clone();
        next.token = "next-ticket".into();
        std::sync::Arc::make_mut(&mut app.session.active_mut().unwrap().doc).defold = Some(photocraft_doc::defold::Design {
            version: 1,
            root,
            gui: "/main/test.gui".into(),
            source: String::new(),
            disk_hash: "unchanged".into(),
            nodes: vec![],
            remote: None,
            canvas_origin: [0, 0],
            project: None,
        });
        app.session.execute("file.new", json!({"width":4,"height":4})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        let ids: Vec<_> = app.session.documents().iter().map(|d| d.doc.id).collect();
        app.peer_binding.ready = Some(next);
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        assert_eq!(link::state().incoming.as_ref().unwrap().phase, "ready");
        assert!(app.discard.is_none());
        assert_eq!(app.session.documents().iter().map(|d| d.doc.id).collect::<Vec<_>>(), ids);
        assert!(app.session.documents().iter().all(|d| d.is_dirty()));
        assert_eq!(app.session.documents()[0].doc.defold.as_ref().unwrap().disk_hash, "unchanged");
        *link::state() = saved;
    }
    #[test]
    fn switching_projects_preserves_all_documents_and_cancelling_preserves_the_peer() {
        let _serial = SERIAL.lock().unwrap();
        let (mut app, saved) = fixture("ready");
        link::state().active.as_mut().unwrap().project = "original".into();
        let mut next = peer("new");
        next.project = "different".into();
        next.root = "different".into();
        app.peer_binding.ready = Some(next);
        let ctx = egui::Context::default();
        frame(&mut app, &ctx);
        assert!(app.discard.is_none());
        assert_eq!(link::state().active.as_ref().unwrap().id, "old");
        assert!(app.session.documents()[0].is_dirty());
        assert_eq!(app.session.documents().len(), 1);
        *link::state() = saved;
    }

    // Review reproduction: a refresh that finishes after release must not restore its revoked session.
    #[test]
    fn revoked_refresh_must_not_resurrect_connection() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::mem::take(&mut *link::state());
        let root = tempfile::tempdir().unwrap();
        let mut revoked = peer("revoked");
        revoked.address = "127.0.0.1:29001".into();
        revoked.root = root.path().to_string_lossy().into_owned();
        revoked.project = revoked.root.clone();
        let mut other = peer("other");
        other.address = "127.0.0.1:29002".into();
        other.root = revoked.root.clone();
        other.project = revoked.project.clone();
        link::state().put(revoked.clone());
        link::state().put(other.clone());
        let (tx, rx) = channel();
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        app.peer_binding.worker = Some(rx);
        link::state().working = true;
        // The authenticated /binding/release route calls this exact transition while a mirror runs.
        link::state().remove_token(&revoked.token);
        tx.send(Ok(Event::Mirrored(revoked.clone()))).unwrap();
        frame(&mut app, &egui::Context::default());
        let resurrected = link::state().peer_for_token(&revoked.token).is_some();
        let other_survived = link::state().peer_for_token(&other.token).is_some();
        *link::state() = saved;
        assert!(other_survived, "the unrelated connection must remain usable");
        assert!(!resurrected, "late mirror completion resurrected a released session and its revoked token");
    }

    // Review reproduction: releasing a peer must cancel an already prepared replacement for that endpoint.
    #[test]
    fn release_must_cancel_same_endpoint_pending_commit() {
        let mut state = Binding::default();
        let old = peer("old");
        state.put(old.clone());
        let mut other = peer("other");
        other.address = "127.0.0.1:29002".into();
        state.put(other.clone());
        state.incoming = Some(Offer {
            ticket: "replacement-ticket".into(),
            peer: Peer { token: "replacement-ticket".into(), ..old.clone() },
            phase: "ready".into(),
            expires: Instant::now() + Duration::from_secs(180),
        });
        state.remove_token(&old.token);
        assert!(state.peer_for_token(&other.token).is_some());
        assert!(state.incoming.is_none(), "a pending commit at the disconnected endpoint can still recreate the connection");
    }

    // A released endpoint invalidates every local completion stage, without cancelling the other endpoint.
    #[test]
    fn released_operation_discards_ready_bound_and_mirrored_events() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::mem::take(&mut *link::state());
        for event_kind in 0..3 {
            let mut state = link::state();
            *state = Binding::default();
            let mut old = peer("old");
            old.address = "127.0.0.1:29001".into();
            let mut other = peer("other");
            other.address = "127.0.0.1:29002".into();
            state.put(old.clone());
            state.put(other.clone());
            let epoch = state.epoch(&old.address);
            let (tx, rx) = channel();
            let mut app = PhotocraftApp::new(Session::new(), Services::default());
            app.peer_binding.worker = Some(rx);
            app.peer_binding.operation = Some((old.address.clone(), epoch));
            state.working = true;
            state.remove_token(&old.token);
            drop(state);
            // UI-thread cancellation drains a completed channel before applying any event.
            let event = match event_kind {
                0 => Event::Ready(old.clone()),
                1 => Event::Bound(old.clone()),
                _ => Event::Mirrored(old.clone()),
            };
            tx.send(Ok(event)).unwrap();
            frame(&mut app, &egui::Context::default());
            assert!(app.peer_binding.ready.is_none());
            assert!(app.peer_binding.worker.is_none());
            assert!(link::state().peer_for_token(&old.token).is_none());
            assert!(link::state().peer_for_token(&other.token).is_some());
            assert!(!link::state().working);
        }
        *link::state() = saved;
    }

    // Removing the last/default peer must leave automatic discovery idle, including the empty-target case.
    #[test]
    fn manually_disconnected_default_does_not_restart_discovery() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::mem::take(&mut *link::state());
        let old = peer("old");
        link::state().put(old.clone());
        link::state().remove_token(&old.token);
        link::state().port = 19199;
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        app.peer_binding.auto_at = Some(0.0);
        frame(&mut app, &egui::Context::default());
        let idle = app.peer_binding.worker.is_none() && link::state().outgoing.is_none() && !link::state().working && !app.peer_binding.auto_attempt;
        *link::state() = saved;
        assert!(idle, "manual disconnect must suppress the default candidate until an explicit binding succeeds");
    }
    // An epoch checked earlier in the callback cannot authorize the final mutation after release.
    #[test]
    fn final_connection_commit_rechecks_epoch_atomically() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::mem::take(&mut *link::state());
        let old = peer("old");
        link::state().put(old.clone());
        let epoch = link::state().epoch(&old.address);
        assert!(ensure_current(&old.address, epoch).is_ok());
        link::state().remove_token(&old.token);
        let accepted = native::replace(old.clone(), epoch);
        let revived = link::state().peer_for_token(&old.token).is_some();
        *link::state() = saved;
        assert!(!accepted);
        assert!(!revived);
    }
    // A continuation must not acquire a fresh epoch after its original operation was cancelled.
    #[test]
    fn cancelled_handshake_cannot_restart_with_the_new_epoch() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::mem::take(&mut *link::state());
        let old = peer("old");
        link::state().put(old.clone());
        let epoch = link::state().epoch(&old.address);
        link::state().remove_token(&old.token);
        let mut ui = BindingUi::default();
        let started = worker(&mut ui, &egui::Context::default(), old.address, Some(epoch), |_| panic!("cancelled continuation must not execute"));
        *link::state() = saved;
        assert!(!started);
        assert!(ui.worker.is_none());
    }
}
