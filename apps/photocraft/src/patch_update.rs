//! Desktop-only update dialog. Network and staging work never run on the UI thread.
use photocraft_ui_egui::PhotocraftApp;
use photocraft_updater::{self as patch, Release};
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

enum Answer {
    Checked(Result<Option<Release>, String>),
    Prepared(Result<PathBuf, String>),
}
pub struct PatchApp {
    pub app: PhotocraftApp,
    requested: Arc<AtomicBool>,
    bundle: Option<PathBuf>,
    open: bool,
    busy: bool,
    status: String,
    release: Option<Release>,
    ticket: Option<PathBuf>,
    rx: Option<mpsc::Receiver<Answer>>,
    // The detached helper waits for all instances using this bundle to release their locks.
    _bundle_lock: Option<File>,
    installing: bool,
}
impl PatchApp {
    pub fn new(mut app: PhotocraftApp) -> Self {
        let requested = Arc::new(AtomicBool::new(std::env::var_os("PHOTOCRAFT_CHECK_UPDATES").is_some()));
        let callback = requested.clone();
        app.services.check_updates = Some(Box::new(move || {
            callback.store(true, Ordering::Relaxed);
        }));
        let bundle = patch::bundle_path().ok();
        let (lock, status) = match bundle.as_ref().map(|path| patch::use_lock(path)).transpose() {
            Ok(lock) => (lock, "Check for incremental updates to PhotoCraft.".into()),
            Err(error) => (None, format!("Updates are unavailable: {error}")),
        };
        Self { app, requested, bundle, open: false, busy: false, status, release: None, ticket: None, rx: None, _bundle_lock: lock, installing: false }
    }
    fn check(&mut self, ctx: &egui::Context) {
        let Some(bundle) = self.bundle.clone() else {
            self.status = "Install a packaged PhotoCraft.app to enable patch updates.".into();
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.release = None;
        self.status = "Checking the installed baseline and update feed…".into();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> patch::Result<_> {
                let config = patch::config(&bundle)?;
                let feed = std::env::var("PHOTOCRAFT_PATCH_FEED").unwrap_or(config.feed);
                patch::check(&bundle, &feed)
            })()
            .map_err(|error| error.to_string());
            let _ = tx.send(Answer::Checked(result));
            ctx.request_repaint();
        });
    }
    fn download(&mut self, ctx: &egui::Context) {
        let (Some(bundle), Some(release)) = (self.bundle.clone(), self.release.clone()) else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.status = "Downloading, verifying and preparing the replacement application…".into();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = patch::prepare(&bundle, &release).map_err(|e| e.to_string());
            let _ = tx.send(Answer::Prepared(result));
            ctx.request_repaint();
        });
    }
    fn ready(&self) -> bool {
        self.app.session.documents().is_empty()
            && !self.app.ui.defold.project_open
            && self.app.jobs.opens.is_empty()
            && self.app.jobs.saves.is_empty()
            && !self.app.session.has_jobs()
            && !self.app.ai_runtime.running()
    }
    fn install(&mut self, ctx: &egui::Context) -> Result<(), String> {
        if !self.ready() {
            return Err("Close all documents and the Defold project, and wait for tasks to finish before installing.".into());
        }
        let ticket = self.ticket.as_ref().ok_or("No prepared patch")?;
        let bundle = self.bundle.as_ref().ok_or("No application bundle")?;
        let parent = ticket.parent().ok_or("No patch workspace")?;
        let helper = parent.join("photocraft-updater");
        std::fs::copy(bundle.join("Contents/MacOS/photocraft-updater"), &helper).map_err(|e| e.to_string())?;
        let log = File::create(parent.join("installer.log")).map_err(|e| e.to_string())?;
        std::process::Command::new(helper)
            .arg("install")
            .arg(ticket)
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| e.to_string())?;
        self.installing = true;
        self.status = "Quitting PhotoCraft to install and restart…".into();
        photocraft_ui_egui::menus::invoke(&mut self.app, ctx, "file.exit", serde_json::json!({}))?;
        Ok(())
    }
    fn dialog(&mut self, ctx: &egui::Context) {
        if self.requested.swap(false, Ordering::Relaxed) {
            self.open = true;
            if !self.busy && self.ticket.is_none() {
                self.check(ctx);
            }
        }
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(answer) => {
                    self.busy = false;
                    self.rx = None;
                    match answer {
                        Answer::Checked(Ok(Some(release))) => {
                            self.status = format!("Patch {} → {} ({} MiB)\n{}", release.from, release.to, release.size.div_ceil(1024 * 1024), release.notes);
                            self.release = Some(release);
                        }
                        Answer::Checked(Ok(None)) => self.status = "PhotoCraft is up to date.".into(),
                        Answer::Prepared(Ok(ticket)) => {
                            self.ticket = Some(ticket);
                            self.status = "The update is verified and ready to install. The previous version will be retained for recovery.".into();
                        }
                        Answer::Checked(Err(error)) | Answer::Prepared(Err(error)) => self.status = format!("Update failed: {error}"),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    self.rx = None;
                    self.status = "The update worker stopped. Try again.".into();
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
        if !self.open {
            self.app.ui.patch_update = Some(serde_json::json!({"open":false}));
            return;
        }
        let mut buttons = serde_json::Map::new();
        egui::Modal::new(egui::Id::new("photocraft-patch-update")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("PhotoCraft Updates");
            if let Some(config) = self.bundle.as_ref().and_then(|path| patch::config(path).ok()) {
                ui.label(format!("{} · patch.{} · {}", config.version, config.revision, config.channel));
            }
            ui.separator();
            ui.label(&self.status);
            if self.busy {
                ui.spinner();
            }
            if self.ticket.is_some() && !self.ready() {
                ui.label("Close all documents and the Defold project, and wait for tasks to finish before installing.");
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if button(ui, &mut buttons, "check", "Check Again", !self.busy && !self.installing && self.ticket.is_none()).clicked() {
                    self.check(ctx);
                }
                if self.release.is_some() && self.ticket.is_none() && button(ui, &mut buttons, "download", "Download Update", !self.busy).clicked() {
                    self.download(ctx);
                }
                if self.ticket.is_some()
                    && button(ui, &mut buttons, "install", "Install and Restart", self.ready() && !self.installing).clicked()
                    && let Err(error) = self.install(ctx)
                {
                    self.status = error;
                }
                if button(ui, &mut buttons, "close", "Close", !self.installing).clicked() {
                    self.open = false;
                }
            });
        });
        self.app.ui.patch_update = Some(
            serde_json::json!({"open":self.open,"busy":self.busy,"status":self.status,"canInstall":self.ready(),"prepared":self.ticket.is_some(),"release":self.release,"buttons":buttons}),
        );
    }
}
fn button(ui: &mut egui::Ui, buttons: &mut serde_json::Map<String, serde_json::Value>, id: &str, label: &str, enabled: bool) -> egui::Response {
    let response = ui.add_enabled(enabled, egui::Button::new(label));
    let rect = response.rect;
    buttons.insert(id.into(), serde_json::json!({"enabled":enabled,"x":rect.center().x,"y":rect.center().y}));
    response
}
impl eframe::App for PatchApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, input);
    }
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        self.dialog(ui.ctx());
    }
    fn on_exit(&mut self) {
        self.app.on_exit();
    }
}

impl Drop for PatchApp {
    fn drop(&mut self) {
        if !self.installing
            && let Some(parent) = self.ticket.as_ref().and_then(|p| p.parent())
        {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installation_requires_closed_documents_projects_and_finished_tasks() {
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let mut shell = PatchApp::new(app);
        assert!(shell.ready());
        shell.app.run("file.new", serde_json::json!({"width":8,"height":8})).unwrap();
        assert!(!shell.ready());
        shell.app.run("file.closeAll", serde_json::json!({})).unwrap();
        assert!(shell.ready());
        shell.app.ui.defold.project_open = true;
        assert!(!shell.ready());
        assert!(shell.install(&egui::Context::default()).unwrap_err().contains("Close all"));
    }
    #[test]
    fn menu_opens_update_dialog_without_an_open_document() {
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let mut shell = PatchApp::new(app);
        let result = photocraft_ui_egui::menus::invoke(&mut shell.app, &egui::Context::default(), "help.checkUpdates", serde_json::json!({})).unwrap();
        assert_eq!(result["opened"], true);
        assert!(shell.requested.load(Ordering::Relaxed));
    }
}
