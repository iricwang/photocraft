//! Host-owned, cancellable generation. Credentials never enter serialized UI state.
use crate::PhotocraftApp;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError},
};

#[derive(Clone)]
pub struct ReferenceImage {
    pub name: String,
    pub png: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProjectContext {
    pub id: String,
    pub open_id: u64,
    pub name: String,
    pub path: Option<String>,
    pub memory: String,
    pub events: Vec<photocraft_doc::ai::ProjectEvent>,
}

pub type ProjectSyncFn = Box<dyn FnMut(ProjectContext) -> Result<(), String>>;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CanvasSnapshot {
    pub image_base64: String,
    pub document: Value,
}

fn canvas_snapshot(context: &Value, image_key: &str) -> Option<CanvasSnapshot> {
    let image_base64 = context.get(image_key)?.as_str()?.to_owned();
    let mut document = context.clone();
    if let Some(metadata) = document.as_object_mut() {
        for key in ["sourcePng", "contextPng", "canvasPng", "imageBase64", "detailBase64"] {
            metadata.remove(key);
        }
    }
    Some(CanvasSnapshot { image_base64, document })
}

#[derive(Clone)]
pub struct Request {
    pub project: Option<ProjectContext>,
    pub asset_job: Option<u64>,
    pub components: Vec<photocraft_engine::ai_assets::AssetSpec>,
    pub design_first: bool,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub codex_path: String,
    pub codex_model: String,
    pub codex_reasoning_effort: String,
    pub prompt: String,
    pub source_png: Option<String>,
    pub context_png: Option<String>,
    pub canvas_snapshot: Option<CanvasSnapshot>,
    pub references: Vec<ReferenceImage>,
    pub transparent: bool,
    pub edit_instructions: Option<String>,
    pub progress: Option<std::sync::mpsc::Sender<GenerationProgress>>,
}

pub enum GenerationProgress {
    ToolCall { job: u64, command: String, params: Value, reply: std::sync::mpsc::Sender<Result<Value, String>> },
    Stage(String),
    SavedImage(String),
    PromptReady(String),
    ModelResolved(String),
}

pub struct Job<T = String> {
    pub cancel: Arc<AtomicBool>,
    pub result: Receiver<Result<T, String>>,
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub type GenerateFn = Box<dyn Fn(Request) -> Result<Job, String>>;

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct CodexModel {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub hidden: bool,
    pub supported_reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: String,
}

pub type ModelsFn = Box<dyn Fn(String) -> Result<Job<Vec<CodexModel>>, String>>;

pub(crate) fn selected_model(app: &PhotocraftApp) -> Option<&CodexModel> {
    if app.ai_runtime.models_path.as_ref().is_some_and(|path| path != &app.ui.ai.codex_path) {
        return None;
    }
    if app.ui.ai.codex_model.is_empty() {
        app.ai_runtime.models.iter().find(|model| model.is_default).or_else(|| app.ai_runtime.models.first())
    } else {
        app.ai_runtime.models.iter().find(|model| model.id == app.ui.ai.codex_model)
    }
}

pub(crate) fn normalize_reasoning(app: &mut PhotocraftApp) -> bool {
    if app.ai_runtime.models.is_empty()
        || app.ui.ai.codex_reasoning_effort.is_empty()
        || app.ai_runtime.models_path.as_ref().is_some_and(|path| path != &app.ui.ai.codex_path)
    {
        return false;
    }
    if selected_model(app).is_some_and(|model| model.supported_reasoning_efforts.contains(&app.ui.ai.codex_reasoning_effort)) {
        return false;
    }
    app.ui.ai.codex_reasoning_effort.clear();
    true
}

#[derive(Default)]
pub struct Runtime {
    pub(crate) project_document: Option<(photocraft_doc::DocId, u64)>,
    pub(crate) project_drafts: std::collections::HashMap<u64, String>,
    pub(crate) project_synced: std::collections::HashMap<photocraft_doc::DocId, (u64, u64, Option<String>)>,
    pub(crate) project_sync_error: String,
    pub(crate) project_sync_retry_at: f64,
    pub(crate) project_record_error: String,
    pub(crate) project_events: std::collections::VecDeque<crate::ai_project::PendingEvent>,
    job_document: Option<(photocraft_doc::DocId, u64)>,
    asset_job: Option<u64>,
    asset_prompt: String,
    asset_references: Vec<photocraft_doc::ai::ProjectReference>,
    pub(crate) references_revision: u64,
    pub(crate) references: Vec<crate::ai_references::Attachment>,
    pub(crate) reference_document: Option<crate::ai_references::SourceDocument>,
    pub(crate) reference_sources: Vec<(u64, Arc<Vec<u8>>)>,
    pub(crate) reference_job: Option<(Option<crate::ai_references::SourceDocument>, Job<crate::ai_references::Attachment>)>,
    pub(crate) prompt_id: Option<egui::Id>,
    components: Vec<photocraft_engine::ai_assets::AssetSpec>,
    expand_component: bool,
    pub saved_images: Vec<String>,
    pub api_key: String,
    job: Option<(u64, Job)>,
    pub models: Vec<CodexModel>,
    pub models_error: String,
    pub optimized_prompt: String,
    pub resolved_model: String,
    progress: Option<Receiver<GenerationProgress>>,
    models_job: Option<Job<Vec<CodexModel>>>,
    models_path: Option<String>,
    models_due: Option<f64>,
}

impl Runtime {
    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    pub fn models_loading(&self) -> bool {
        self.models_job.is_some()
    }
}

pub fn refresh_models(app: &mut PhotocraftApp) {
    app.ai_runtime.models_due = None;
    app.ai_runtime.models_job = None;
    app.ai_runtime.models.clear();
    app.ai_runtime.models_error.clear();
    app.ai_runtime.models_path = Some(app.ui.ai.codex_path.clone());
    match app
        .services
        .ai_models
        .as_ref()
        .ok_or_else(|| "Model discovery requires the desktop application".to_owned())
        .and_then(|list| list(app.ui.ai.codex_path.clone()))
    {
        Ok(job) => app.ai_runtime.models_job = Some(job),
        Err(error) => app.ai_runtime.models_error = error,
    }
}

pub fn tick_models(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    if (app.ui.ai.visible || crate::panel_docking::ai_settings_shown(app))
        && app.ui.ai.provider == "codex"
        && app.services.ai_models.is_some()
        && app.ai_runtime.models_path.as_ref() != Some(&app.ui.ai.codex_path)
    {
        if app.ai_runtime.models_path.is_none() {
            refresh_models(app);
        } else {
            app.ai_runtime.models_job = None;
            app.ai_runtime.models.clear();
            app.ai_runtime.models_error.clear();
            app.ai_runtime.models_path = Some(app.ui.ai.codex_path.clone());
            app.ai_runtime.models_due = Some(now + 0.4);
        }
    }
    if let Some(due) = app.ai_runtime.models_due {
        if now >= due {
            refresh_models(app);
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let Some(job) = &app.ai_runtime.models_job else { return };
    let result = match job.result.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(TryRecvError::Disconnected) => Err("Codex model discovery stopped".into()),
    };
    app.ai_runtime.models_job = None;
    match result {
        Ok(models) => {
            app.ai_runtime.models = models;
            if normalize_reasoning(app)
                && let Err(error) = save_settings(app)
            {
                app.ui.ai.status = error;
            }
        }
        Err(error) => app.ai_runtime.models_error = error,
    }
}

pub fn save_settings(app: &mut PhotocraftApp) -> Result<(), String> {
    if let Some(save) = &mut app.services.save_ai_settings {
        let settings = json!({"enabled":app.ui.ai.enabled,"provider":app.ui.ai.provider,"baseUrl":app.ui.ai.base_url,"model":app.ui.ai.model,"codexPath":app.ui.ai.codex_path,"codexModel":app.ui.ai.codex_model,"codexReasoningEffort":app.ui.ai.codex_reasoning_effort,"mode":app.ui.ai.mode,"designFirst":app.ui.ai.design_first,"expandComponent":app.ui.ai.expand_component,"batchComponents":app.ui.ai.batch_components,"componentButtons":app.ui.ai.component_buttons,"componentBars":app.ui.ai.component_bars});
        save(&settings.to_string())?;
    }
    Ok(())
}

pub fn load_settings(app: &mut PhotocraftApp) {
    let Some(text) = app.services.load_ai_settings.as_mut().and_then(|load| load()) else { return };
    if text.len() > 32 * 1024 {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
    if let Some(enabled) = v.get("enabled").and_then(Value::as_bool) {
        app.ui.ai.enabled = enabled;
    }
    app.ui.ai.design_first = v.get("designFirst").and_then(Value::as_bool).unwrap_or(false);
    app.ui.ai.batch_components = v.get("batchComponents").and_then(Value::as_bool).unwrap_or(false);
    app.ui.ai.expand_component = v.get("expandComponent").and_then(Value::as_bool).unwrap_or(false);
    if let Some(mode) = v.get("mode").and_then(|v| serde_json::from_value(v.clone()).ok()) {
        app.ui.ai.mode = mode;
    }
    for (name, field) in [
        ("provider", &mut app.ui.ai.provider),
        ("baseUrl", &mut app.ui.ai.base_url),
        ("model", &mut app.ui.ai.model),
        ("codexPath", &mut app.ui.ai.codex_path),
        ("codexModel", &mut app.ui.ai.codex_model),
        ("codexReasoningEffort", &mut app.ui.ai.codex_reasoning_effort),
        ("componentButtons", &mut app.ui.ai.component_buttons),
        ("componentBars", &mut app.ui.ai.component_bars),
    ] {
        if let Some(value) = v.get(name).and_then(Value::as_str).filter(|s| s.len() <= 4096) {
            *field = value.into();
        }
    }
    if !["codex", "images-api"].contains(&app.ui.ai.provider.as_str()) {
        app.ui.ai.provider = "codex".into();
    }
}

pub fn start(app: &mut PhotocraftApp) -> Result<Value, String> {
    start_generation(app, None)
}

pub fn retry(app: &mut PhotocraftApp, group_id: u64) -> Result<Value, String> {
    start_generation(app, Some(group_id))
}

fn start_generation(app: &mut PhotocraftApp, group_id: Option<u64>) -> Result<Value, String> {
    if !app.ui.ai.enabled {
        return Err(tl!("Enable AI to generate images").into());
    }
    normalize_reasoning(app);
    if app.ai_runtime.reference_job.is_some() {
        return Err(tl!("Preparing reference image...").into());
    }
    if app.ai_runtime.running() {
        return Err("A generation is already running".into());
    }
    if app.services.ai_generate.is_none() {
        return Err("Image generation requires the desktop application".into());
    }
    if app.ui.ai.prompt.trim().is_empty() {
        return Err("Enter a prompt first".into());
    }
    if app.ui.ai.provider == "images-api" && app.ai_runtime.api_key.trim().is_empty() {
        return Err("Enter an API key first".into());
    }
    if app.session.active_gui().is_some() {
        if group_id.is_some() { return Err("Open the node source to retry its generation".into()); }
        app.run("defold.openNodeSource", json!({}))?;
    }
    if group_id.is_none() && app.ui.ai.provider == "codex" && app.ui.ai.mode == photocraft_doc::ai::GenerationMode::Component {
        return start_asset_agent(app);
    }
    if group_id.is_none() && app.ui.ai.mode == photocraft_doc::ai::GenerationMode::Component && app.session.active().is_none_or(|st| st.doc.selection.is_none())
    {
        return Err("Select a component area first".into());
    }
    let mode = group_id.and_then(|id| app.session.active()?.doc.ai_generations.iter().find(|g| g.id == id).map(|g| g.mode)).unwrap_or(app.ui.ai.mode);
    let components = if mode == photocraft_doc::ai::GenerationMode::Component && app.ui.ai.batch_components {
        photocraft_engine::ai_assets::parse(&app.ui.ai.component_buttons, &app.ui.ai.component_bars)?
    } else {
        Vec::new()
    };
    save_settings(app)?;
    if app.session.active().is_none() && group_id.is_none() {
        app.run("file.new", json!({"width":1024,"height":1024}))?;
    }
    let project = crate::ai_project::prepare(app)?;
    let model = if app.ui.ai.provider == "codex" { app.ui.ai.codex_model.clone() } else { app.ui.ai.model.clone() };
    let mut params = json!({"prompt":app.ui.ai.prompt,"provider":app.ui.ai.provider,"model":model});
    let command = if let Some(id) = group_id {
        params["groupId"] = json!(id);
        "ai.retryGeneration"
    } else {
        params["reference"] = json!(app.ui.ai.reference);
        params["destination"] = json!("layer");
        params["mode"] = json!(app.ui.ai.mode);
        "ai.beginGeneration"
    };
    let begin = app.run(command, params)?;
    let id = begin["candidateId"].as_u64().ok_or("Missing generation identifier")?;
    let (progress, updates) = std::sync::mpsc::channel();
    app.ai_runtime.optimized_prompt.clear();
    app.ai_runtime.saved_images.clear();
    app.ai_runtime.expand_component = begin["mode"] == "component" && app.ui.ai.expand_component;
    app.ai_runtime.resolved_model = model;
    let request = Request {
        project: Some(project),
        asset_job: None,
        design_first: components.is_empty() && begin["mode"] == "component" && app.ui.ai.design_first,
        components: components.clone(),
        provider: app.ui.ai.provider.clone(),
        base_url: app.ui.ai.base_url.clone(),
        model: app.ui.ai.model.clone(),
        api_key: if app.ui.ai.provider == "images-api" { app.ai_runtime.api_key.clone() } else { String::new() },
        codex_path: app.ui.ai.codex_path.clone(),
        codex_model: app.ui.ai.codex_model.clone(),
        codex_reasoning_effort: app.ui.ai.codex_reasoning_effort.clone(),
        prompt: app.ui.ai.prompt.clone(),
        source_png: begin["sourcePng"].as_str().map(str::to_owned),
        context_png: begin["contextPng"].as_str().map(str::to_owned),
        canvas_snapshot: canvas_snapshot(&begin, "canvasPng"),
        references: crate::ai_references::snapshot(app),
        transparent: begin["mode"] == "component",
        edit_instructions: begin["editInstructions"].as_str().map(str::to_owned),
        progress: Some(progress),
    };
    let result = app.services.ai_generate.as_ref().ok_or("Image service unavailable")?(request);
    match result {
        Ok(job) => {
            app.ai_runtime.asset_job = None;
            app.ai_runtime.job_document = app.session.active().map(|st| (st.doc.id, st.open_id));
            app.ai_runtime.components = components;
            app.ai_runtime.job = Some((id, job));
            app.ai_runtime.progress = Some(updates);
            app.ui.ai.status = tl!("Generating image...").into();
            Ok(json!({"candidateId":id,"pending":true}))
        }
        Err(e) => {
            let _ = app.run("ai.cancelGeneration", json!({"candidateId":id}));
            Err(e)
        }
    }
}

fn start_asset_agent(app: &mut PhotocraftApp) -> Result<Value, String> {
    save_settings(app)?;
    if app.session.active().is_none() {
        app.run("file.new", json!({"width":1024,"height":1024}))?;
    }
    let project = crate::ai_project::prepare(app)?;
    let references = app.session.active().map(|st| st.doc.ai_project.references.clone()).unwrap_or_default();
    let resumable = app
        .ai_runtime
        .asset_job
        .filter(|_| app.ai_runtime.asset_prompt == app.ui.ai.prompt)
        .filter(|_| app.ai_runtime.asset_references == references)
        .and_then(|job| app.run("ai.assetStatus", json!({"jobId":job})).ok().map(|status| (job, status)))
        .filter(|(_, status)| status["receipt"].is_null() && app.session.active().is_some_and(|st| Some(st.doc.id.0) == status["documentId"].as_u64()));
    let resuming = resumable.is_some();
    let context = if let Some((job, _)) = resumable { app.run("ai.assetRebase", json!({"jobId":job}))? } else { app.run("ai.assetBegin", json!({}))? };
    let id = context.get("jobId").and_then(Value::as_u64).ok_or("Missing asset task identifier")?;
    let (progress, updates) = std::sync::mpsc::channel();
    let request = Request {
        project: Some(project),
        asset_job: Some(id),
        components: Vec::new(),
        design_first: false,
        provider: "codex".into(),
        base_url: app.ui.ai.base_url.clone(),
        model: app.ui.ai.model.clone(),
        api_key: String::new(),
        codex_path: app.ui.ai.codex_path.clone(),
        codex_model: app.ui.ai.codex_model.clone(),
        codex_reasoning_effort: app.ui.ai.codex_reasoning_effort.clone(),
        prompt: app.ui.ai.prompt.clone(),
        source_png: context["detailBase64"].as_str().map(str::to_owned),
        context_png: context["imageBase64"].as_str().map(str::to_owned),
        canvas_snapshot: canvas_snapshot(&context, "imageBase64"),
        references: crate::ai_references::snapshot(app),
        transparent: true,
        edit_instructions: None,
        progress: Some(progress),
    };
    match app.services.ai_generate.as_ref().ok_or("Image service unavailable")?(request) {
        Ok(job) => {
            app.ai_runtime.asset_job = Some(id);
            app.ai_runtime.job_document = app.session.active().map(|st| (st.doc.id, st.open_id));
            app.ai_runtime.job = Some((id, job));
            app.ai_runtime.progress = Some(updates);
            app.ai_runtime.asset_prompt = app.ui.ai.prompt.clone();
            app.ai_runtime.asset_references = references;
            if !resuming {
                app.ai_runtime.saved_images.clear();
            }
            app.ai_runtime.optimized_prompt.clear();
            app.ai_runtime.components.clear();
            app.ui.ai.status = tl!("Arranging asset layers...").into();
            Ok(json!({"jobId":id,"pending":true}))
        }
        Err(e) => {
            if !resuming {
                let _ = app.run("ai.assetCancel", json!({"jobId":id}));
            }
            Err(e)
        }
    }
}

pub fn cancel(app: &mut PhotocraftApp) {
    app.ai_runtime.progress = None;
    if let Some((id, job)) = app.ai_runtime.job.take() {
        job.cancel.store(true, Ordering::Relaxed);
        if app.ai_runtime.asset_job.is_none() {
            let _ = app.run("ai.cancelGeneration", json!({"candidateId":id}));
        }
        app.ui.ai.status = tl!("Generation cancelled").into();
        let document = app.ai_runtime.job_document.take();
        crate::ai_project::record(app, document, "cancelled", "Image generation cancelled");
    }
}

fn read_progress(app: &mut PhotocraftApp) {
    let updates: Vec<_> = app.ai_runtime.progress.as_ref().map(|updates| updates.try_iter().take(64).collect()).unwrap_or_default();
    for progress in updates {
        match progress {
            GenerationProgress::ToolCall { job, command, params, reply } => {
                let permitted = [
                    "ai.designContext",
                    "ai.assetReserve",
                    "ai.assetRegister",
                    "ai.assetInspect",
                    "ai.assetExtract",
                    "ai.layersPreview",
                    "ai.layersApply",
                    "ai.assetStatus",
                ]
                .contains(&command.as_str());
                let result = if permitted && app.ai_runtime.asset_job == Some(job) && params.get("jobId").and_then(Value::as_u64) == Some(job) {
                    app.run(&command, params)
                } else {
                    Err("Asset task is no longer active".into())
                };
                let _ = reply.send(result);
            }
            GenerationProgress::Stage(stage) => app.ui.ai.status = tl!(&stage).into(),
            GenerationProgress::SavedImage(path) => app.ai_runtime.saved_images.push(path),
            GenerationProgress::PromptReady(prompt) => {
                app.ai_runtime.optimized_prompt = prompt;
                app.ui.ai.status = tl!("Generating image...").into();
            }
            GenerationProgress::ModelResolved(model) => app.ai_runtime.resolved_model = model,
        }
    }
}

pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    crate::ai_references::tick(app, ctx);
    crate::ai_project::sync(app, ctx);
    tick_models(app, ctx);
    read_progress(app);
    let Some((id, job)) = &app.ai_runtime.job else { return };
    let id = *id;
    let result = match job.result.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(TryRecvError::Disconnected) => Err("Image worker stopped before returning a result".into()),
    };
    app.ai_runtime.job = None;
    // Drain again: a completed result can race the first progress-channel read.
    read_progress(app);
    app.ai_runtime.progress = None;
    let agent_message = if app.ai_runtime.asset_job.is_some() { result.as_ref().ok().filter(|message| !message.is_empty()).cloned() } else { None };
    let received = if let Some(job) = app.ai_runtime.asset_job {
        let status = app.run("ai.assetStatus", json!({"jobId":job}));
        match status {
            Ok(status) if status.get("receipt").is_some_and(|v| !v.is_null()) => Ok(status["receipt"].clone()),
            _ => result.and_then(|message| {
                let status = "The asset agent did not commit a layout; generated files are retained. No automatic retry was started.";
                Err(if message.trim().is_empty() { status.into() } else { format!("{status}\n{message}") })
            }),
        }
    } else {
        result.and_then(|image| {
        app.run(
            "ai.receiveCandidate",
            json!({"candidateId":id,"imageBase64":image,
        "optimizedPrompt":app.ai_runtime.optimized_prompt,"model":app.ai_runtime.resolved_model,"expandComponent":app.ai_runtime.expand_component,"components":app.ai_runtime.components}),
        )
    })
    };
    app.ui.ai.status = match received {
        Ok(receipt) => {
            if app.session.active().is_some_and(|st| Some(st.doc.id.0) == receipt["documentId"].as_u64()) {
                app.ui.ai.history_document = receipt["documentId"].as_u64();
                app.ui.ai.history_group = receipt["groupId"].as_u64();
            }
            let mut label = format!("{}: {}", tl!("Layer"), receipt["name"].as_str().unwrap_or("AI"));
            if receipt["placement"]["hasMargins"].as_bool() == Some(true) {
                label = format!("{label}\n{}", tl!("Image fitted proportionally; original canvas remains visible outside the result."));
            }
            agent_message.map_or(label.clone(), |message| format!("{label}\n{message}"))
        }
        Err(e) => {
            if app.ai_runtime.asset_job.is_none() {
                let _ = app.run("ai.cancelGeneration", json!({"candidateId":id}));
            }
            e
        }
    };
    let document = app.ai_runtime.job_document.take();
    crate::ai_project::record(app, document, "assistant", &app.ui.ai.status.clone());
    crate::ai_project::sync(app, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_preference_defaults_on_and_round_trips_without_visibility() {
        let saved = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let written = saved.clone();
        let mut app = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services {
                load_ai_settings: Some(Box::new(|| Some(r#"{"model":"saved-model"}"#.into()))),
                save_ai_settings: Some(Box::new(move |text| {
                    *written.borrow_mut() = text.to_owned();
                    Ok(())
                })),
                ..Default::default()
            },
        );
        assert!(app.ui.ai.enabled && app.ui.ai.visible, "older connection settings must not opt out of AI");
        app.ui.ai.enabled = false;
        app.ui.ai.visible = false;
        save_settings(&mut app).unwrap();
        let value: Value = serde_json::from_str(&saved.borrow()).unwrap();
        assert_eq!(value["enabled"], false);
        assert!(value.get("visible").is_none(), "collapsing the prompt bar is independent of AI preferences");
        let restored = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services { load_ai_settings: Some(Box::new(move || Some(saved.borrow().clone()))), ..Default::default() },
        );
        assert!(!restored.ui.ai.enabled);
        assert!(restored.ui.ai.visible);
        assert_eq!(restored.ui.ai.model, "saved-model");
    }

    #[test]
    fn disabled_ai_rejects_start_and_retry_without_creating_work() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let invoked = calls.clone();
        let mut app = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services {
                ai_generate: Some(Box::new(move |_| {
                    invoked.set(invoked.get() + 1);
                    Err("unexpected provider call".into())
                })),
                ..Default::default()
            },
        );
        app.ui.ai.enabled = false;
        app.ui.ai.prompt = "Draw a small icon".into();
        assert!(start(&mut app).is_err());
        assert!(retry(&mut app, 1).is_err());
        assert_eq!(calls.get(), 0);
        assert!(app.session.active().is_none());
        assert!(!app.ai_runtime.running());
    }

    #[test]
    fn control_configures_ai_enablement_and_visibility_independently() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"enabled":false,"visible":true}));
        let _ = crate::control::handle(&mut app, &ctx, &req);
        assert!(!app.ui.ai.enabled && app.ui.ai.visible);
        let before = app.ui.ai.clone();
        let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"enabled":true,"visible":"no"}));
        let _ = crate::control::handle(&mut app, &ctx, &req);
        assert_eq!(app.ui.ai, before, "invalid state must not partially apply");
        let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"enabled":true,"visible":false}));
        let _ = crate::control::handle(&mut app, &ctx, &req);
        assert!(app.ui.ai.enabled && !app.ui.ai.visible);
    }

    #[test]
    fn reasoning_capabilities_drive_selection_without_inventing_fallback_levels() {
        let saved = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let written = saved.clone();
        let mut app = PhotocraftApp::new(
            photocraft_engine::Session::new(),
            crate::Services {
                load_ai_settings: Some(Box::new(|| Some(r#"{"codexModel":"precise","codexReasoningEffort":"xhigh"}"#.into()))),
                save_ai_settings: Some(Box::new(move |text| {
                    *written.borrow_mut() = text.into();
                    Ok(())
                })),
                ..Default::default()
            },
        );
        load_settings(&mut app);
        assert_eq!(app.ui.ai.codex_reasoning_effort, "xhigh");
        assert!(!normalize_reasoning(&mut app), "no metadata must not erase a persisted choice during loading");
        app.ai_runtime.models = vec![
            CodexModel {
                id: "precise".into(),
                name: "Precise".into(),
                is_default: false,
                hidden: false,
                supported_reasoning_efforts: vec!["high".into(), "xhigh".into(), "future-effort".into()],
                default_reasoning_effort: "high".into(),
            },
            CodexModel {
                id: "fast".into(),
                name: "Fast".into(),
                is_default: true,
                hidden: false,
                supported_reasoning_efforts: vec!["low".into(), "medium".into()],
                default_reasoning_effort: "medium".into(),
            },
            CodexModel {
                id: "legacy".into(),
                name: "Legacy".into(),
                is_default: false,
                hidden: false,
                supported_reasoning_efforts: vec![],
                default_reasoning_effort: String::new(),
            },
        ];
        assert!(!normalize_reasoning(&mut app));
        app.ui.ai.codex_reasoning_effort = "future-effort".into();
        assert!(!normalize_reasoning(&mut app), "support must come from metadata, not a hardcoded effort list");
        save_settings(&mut app).unwrap();
        let value: serde_json::Value = serde_json::from_str(&saved.borrow()).unwrap();
        assert_eq!(value["codexReasoningEffort"], "future-effort");
        app.ui.ai.codex_model.clear();
        assert!(normalize_reasoning(&mut app));
        assert!(app.ui.ai.codex_reasoning_effort.is_empty(), "unsupported choices must use the model default, never a fixed high level");
        assert_eq!(selected_model(&app).unwrap().default_reasoning_effort, "medium");
        app.ui.ai.codex_model = "legacy".into();
        app.ui.ai.codex_reasoning_effort = "high".into();
        assert!(normalize_reasoning(&mut app));
        app.ui.ai.codex_model = "missing".into();
        app.ui.ai.codex_reasoning_effort = "high".into();
        assert!(normalize_reasoning(&mut app));
        app.ai_runtime.models_path = Some("previous-codex".into());
        app.ui.ai.codex_path = "new-codex".into();
        app.ui.ai.codex_reasoning_effort = "xhigh".into();
        assert!(!normalize_reasoning(&mut app), "stale metadata from another Codex executable must not change a saved choice");
        assert!(selected_model(&app).is_none());
        let state = serde_json::to_value(&app.ui.ai).unwrap();
        let restored: crate::state::AiPanelState = serde_json::from_value(state).unwrap();
        assert_eq!(restored.codex_reasoning_effort, "xhigh");
        assert!(serde_json::from_value::<crate::state::AiPanelState>(json!({})).unwrap().codex_reasoning_effort.is_empty());
    }

    #[test]
    fn reasoning_automatic_uses_first_model_when_no_default_is_reported() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.ai_runtime.models = vec![CodexModel {
            id: "first".into(),
            name: "First".into(),
            is_default: false,
            hidden: false,
            supported_reasoning_efforts: vec!["medium".into(), "high".into()],
            default_reasoning_effort: "medium".into(),
        }];
        app.ui.ai.codex_reasoning_effort = "high".into();
        assert_eq!(selected_model(&app).unwrap().id, "first");
        assert_eq!(selected_model(&app).unwrap().default_reasoning_effort, "medium");
        assert!(!normalize_reasoning(&mut app));
        app.ai_runtime.models.push(CodexModel {
            id: "default".into(),
            name: "Default".into(),
            is_default: true,
            hidden: false,
            supported_reasoning_efforts: vec!["low".into()],
            default_reasoning_effort: "low".into(),
        });
        assert_eq!(selected_model(&app).unwrap().id, "default");
        assert!(normalize_reasoning(&mut app));
        assert!(app.ui.ai.codex_reasoning_effort.is_empty());
        app.ui.ai.codex_model = "missing".into();
        assert!(selected_model(&app).is_none(), "an explicit missing model must not fall back");
    }

    #[test]
    fn reasoning_configuration_uses_string_values_and_preserves_transactionality() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"codexReasoningEffort":"high"}));
        let _ = crate::control::handle(&mut app, &ctx, &req);
        assert_eq!(app.ui.ai.codex_reasoning_effort, "high");
        let before = app.ui.ai.clone();
        let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"codexReasoningEffort":true}));
        let _ = crate::control::handle(&mut app, &ctx, &req);
        assert_eq!(app.ui.ai, before);
    }

    #[test]
    fn asset_agent_needs_no_checklist_resumes_budget_and_rejects_foreign_tools() {
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Request>::new()));
        let captured = requests.clone();
        let services = crate::Services {
            ai_generate: Some(Box::new(move |request| {
                captured.borrow_mut().push(request);
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Err("Test interruption".into())).unwrap();
                Ok(Job { cancel: Arc::new(AtomicBool::new(false)), result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":100,"height":100})).unwrap();
        app.ui.ai.provider = "codex".into();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Component;
        app.ui.ai.prompt = "Make a coherent set of player controls".into();
        let first = start(&mut app).unwrap();
        let id = first["jobId"].as_u64().unwrap();
        assert!(requests.borrow()[0].components.is_empty());
        assert!(!requests.borrow()[0].design_first);
        assert!(requests.borrow()[0].context_png.is_some());
        let (reply, answer) = std::sync::mpsc::channel();
        requests.borrow()[0]
            .progress
            .as_ref()
            .unwrap()
            .send(GenerationProgress::ToolCall { job: id, command: "file.new".into(), params: json!({"jobId":id}), reply })
            .unwrap();
        tick(&mut app, &egui::Context::default());
        assert!(answer.recv().unwrap().is_err());
        app.run("ai.assetReserve", json!({"jobId":id})).unwrap();
        app.run("layer.new.layer", json!({"name":"User edit"})).unwrap();
        let second = start(&mut app).unwrap();
        assert_eq!(first["jobId"], second["jobId"]);
        assert!(app.run("ai.assetReserve", json!({"jobId":id})).is_err());
        cancel(&mut app);
        assert!(app.run("ai.assetStatus", json!({"jobId":id})).is_ok());
        // Switching the thumbnail cache does not invalidate already generated assets.
        app.ai_runtime.references_revision += 1;
        assert_eq!(start(&mut app).unwrap()["jobId"], first["jobId"]);
        cancel(&mut app);
        let image = photocraft_codecs::Image::from_u8(1, 1, photocraft_codecs::ChannelLayout::Rgba, vec![100, 200, 20, 255]).unwrap();
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        app.run("ai.referenceAdd", json!({"name":"changed.png","imageBase64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png)}))
            .unwrap();
        let changed = start(&mut app).unwrap();
        assert_ne!(changed["jobId"], first["jobId"], "different references must not reuse the previous atlas");
        cancel(&mut app);
        app.ui.ai.prompt = "A different explicit request".into();
        assert_ne!(start(&mut app).unwrap()["jobId"], first["jobId"]);
    }

    #[test]
    fn model_discovery_is_lazy_cancellable_and_retryable() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let recorded = calls.clone();
        let senders = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let pending = senders.clone();
        let flags = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let cancelled = flags.clone();
        let services = crate::Services {
            ai_models: Some(Box::new(move |_| {
                recorded.set(recorded.get() + 1);
                let (tx, rx) = std::sync::mpsc::channel();
                let flag = Arc::new(AtomicBool::new(false));
                pending.borrow_mut().push(tx);
                cancelled.borrow_mut().push(flag.clone());
                Ok(Job { cancel: flag, result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        app.ui.ai.visible = false;
        tick_models(&mut app, &ctx);
        assert_eq!(calls.get(), 0);
        app.ui.ai.visible = true;
        app.ui.ai.provider = "images-api".into();
        tick_models(&mut app, &ctx);
        assert_eq!(calls.get(), 0);
        app.ui.ai.visible = false;
        app.ui.ai.enabled = false;
        crate::panel_docking::set_ai_settings_visible(&mut app, true).unwrap();
        app.ui.ai.provider = "codex".into();
        tick_models(&mut app, &ctx);
        assert!(app.ai_runtime.models_loading());
        assert_eq!(calls.get(), 1, "settings discovery stays available while AI and the prompt bar are off");
        refresh_models(&mut app);
        assert!(flags.borrow()[0].load(Ordering::Relaxed));
        assert!(senders.borrow()[0].send(Ok(Vec::new())).is_err());
        senders.borrow()[1].send(Err("offline".into())).unwrap();
        tick_models(&mut app, &ctx);
        assert_eq!(app.ai_runtime.models_error, "offline");
        tick_models(&mut app, &ctx);
        assert_eq!(calls.get(), 2);
        refresh_models(&mut app);
        senders.borrow()[2]
            .send(Ok(vec![CodexModel {
                id: "m".into(),
                name: "Example".into(),
                is_default: true,
                hidden: false,
                supported_reasoning_efforts: vec![],
                default_reasoning_effort: String::new(),
            }]))
            .unwrap();
        tick_models(&mut app, &ctx);
        assert_eq!(app.ai_runtime.models[0].id, "m");
        assert!(app.ai_runtime.models_error.is_empty());
    }

    fn encoded_image() -> String {
        use base64::Engine as _;
        let image = photocraft_codecs::Image::from_u8(2, 2, photocraft_codecs::ChannelLayout::Rgba, [220, 40, 10, 255].repeat(4)).unwrap();
        base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap())
    }

    #[test]
    fn full_canvas_is_available_to_tools_without_automatic_attachment() {
        let services = crate::Services {
            ai_generate: Some(Box::new(|req| {
                assert!(req.source_png.is_none());
                assert!(req.context_png.is_none());
                assert_eq!(req.references.len(), 1);
                let canvas = req.canvas_snapshot.unwrap();
                assert!(!canvas.image_base64.is_empty());
                assert_eq!(canvas.document["canvas"], json!([128, 80]));
                for key in ["canvasPng", "sourcePng", "contextPng"] {
                    assert!(canvas.document.get(key).is_none());
                }
                let (_, rx) = std::sync::mpsc::channel();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":128,"height":80})).unwrap();
        app.run("ai.referenceAdd", json!({"name":"example.png","imageBase64":encoded_image()})).unwrap();
        crate::ai_references::sync(&mut app);
        app.ui.ai.provider = "codex".into();
        app.ui.ai.reference = false;
        app.ui.ai.prompt = "Redraw the canvas using the example".into();
        start(&mut app).unwrap();
    }

    #[test]
    fn selected_generation_passes_context_even_when_reference_toggle_is_off() {
        let services = crate::Services {
            ai_generate: Some(Box::new(|req| {
                assert!(req.source_png.is_some());
                assert!(req.edit_instructions.as_deref().unwrap().contains("entire square frame"));
                assert!(!req.design_first, "component workflow must not change repaint output sizing");
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Ok(encoded_image())).unwrap();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            ..Default::default()
        };
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", json!({"width":128,"height":80})).unwrap();
        session.execute("select.rect", json!({"x":0,"y":70,"width":128,"height":10})).unwrap();
        let mut app = PhotocraftApp::new(session, services);
        app.ui.ai.reference = false;
        app.ui.ai.prompt = "3D playback bar".into();
        app.ui.ai.design_first = true;
        start(&mut app).unwrap();
        tick(&mut app, &egui::Context::default());
        assert_eq!(app.session.active().unwrap().doc.layers.last().unwrap().name, "AI 1");
    }

    #[test]
    fn component_set_snapshots_names_and_disables_two_step_generation() {
        let services = crate::Services {
            ai_generate: Some(Box::new(|req| {
                assert!(req.transparent);
                assert!(!req.design_first);
                assert_eq!(req.components.len(), 10);
                assert_eq!(req.components[0].name, "Play");
                let (_, rx) = std::sync::mpsc::channel();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.ui.ai.provider = "images-api".into();
        app.ai_runtime.api_key = "test-only".into();
        app.run("file.new", json!({"width":80,"height":60})).unwrap();
        app.run("select.rect", json!({"x":0,"y":40,"width":80,"height":16})).unwrap();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Component;
        app.ui.ai.prompt = "3D player controls".into();
        app.ui.ai.batch_components = true;
        app.ui.ai.design_first = true;
        app.ui.ai.component_buttons = "Play\nPlay".into();
        assert!(start(&mut app).is_err());
        assert!(!app.ai_runtime.running());
        assert!(app.session.ai.candidate().is_none());
        app.ui.ai.component_buttons = crate::state::AiPanelState::default().component_buttons;
        start(&mut app).unwrap();
        app.ui.ai.component_buttons = "New\nNames".into();
        assert_eq!(app.ai_runtime.components[0].name, "Play");
        assert_eq!(app.ai_runtime.components.len(), 10);
        cancel(&mut app);
    }

    #[test]
    fn transparent_component_request_preserves_two_references_and_rejects_opaque_output() {
        let services = crate::Services {
            ai_generate: Some(Box::new(|req| {
                assert!(req.transparent);
                assert!(req.source_png.is_some());
                assert!(req.context_png.is_some());
                assert!(req.edit_instructions.as_deref().unwrap().contains("IDENTIFY the component"));
                assert!(!req.design_first, "single-image redesign is the default");
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Ok(encoded_image())).unwrap();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.ui.ai.provider = "images-api".into();
        app.ai_runtime.api_key = "test-only".into();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Component;
        app.ui.ai.prompt = "raised controls".into();
        assert!(start(&mut app).is_err());
        assert!(app.session.active().is_none());
        app.run("file.new", json!({"width":80,"height":60})).unwrap();
        app.run("select.rect", json!({"x":0,"y":40,"width":80,"height":16})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        start(&mut app).unwrap();
        tick(&mut app, &egui::Context::default());
        assert!(!app.ai_runtime.running());
        assert!(app.ui.ai.status.contains("transparent component"));
        assert_eq!(app.session.active().unwrap().doc.layers, before.layers);
    }

    #[test]
    fn proportional_placement_reports_margins_without_another_generation() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let recorded = calls.clone();
        let services = crate::Services {
            ai_generate: Some(Box::new(move |_| {
                recorded.set(recorded.get() + 1);
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Ok(encoded_image())).unwrap();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":128,"height":64})).unwrap();
        app.ui.ai.prompt = "Change only the requested content".into();
        start(&mut app).unwrap();
        let ctx = egui::Context::default();
        tick(&mut app, &ctx);
        assert!(app.ui.ai.status.contains("original canvas remains visible"));
        tick(&mut app, &ctx);
        assert_eq!(calls.get(), 1);
        assert!(!app.ai_runtime.running());
    }

    #[test]
    fn asset_clarification_is_shown_without_committing_or_retrying() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let recorded = calls.clone();
        let services = crate::Services {
            ai_generate: Some(Box::new(move |_| {
                recorded.set(recorded.get() + 1);
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Ok("Which reference should supply the layout?".into())).unwrap();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":100,"height":100})).unwrap();
        app.ui.ai.provider = "codex".into();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Component;
        app.ui.ai.prompt = "Use the reference layout".into();
        let original = app.session.active().unwrap().doc.clone();
        start(&mut app).unwrap();
        let ctx = egui::Context::default();
        tick(&mut app, &ctx);
        assert!(app.ui.ai.status.contains("Which reference should supply the layout?"));
        assert!(app.ui.ai.status.contains("No automatic retry"));
        tick(&mut app, &ctx);
        assert_eq!(calls.get(), 1);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), original.layers.len());
        assert!(app.session.active().unwrap().doc.ai_generations.is_empty());
    }

    #[test]
    fn completed_worker_adds_a_persistent_layer_and_settings_exclude_prompt() {
        let encoded = encoded_image();
        let services = crate::Services {
            ai_generate: Some(Box::new(move |req| {
                req.progress.as_ref().unwrap().send(GenerationProgress::ModelResolved("actual-model".into())).unwrap();
                req.progress.as_ref().unwrap().send(GenerationProgress::PromptReady("Art-directed image prompt".into())).unwrap();
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Ok(encoded.clone())).unwrap();
                Ok(Job { cancel: Default::default(), result: rx })
            })),
            save_ai_settings: Some(Box::new(|s| {
                assert!(!s.contains("private-prompt"));
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.ui.ai.prompt = "private-prompt".into();
        start(&mut app).unwrap();
        let original = app.session.active().unwrap().doc.clone();
        app.ui.ai.visible = false;
        app.ui.panels.ai_settings = false;
        assert!(app.ai_runtime.running(), "hiding both AI surfaces must not cancel the worker");
        tick(&mut app, &egui::Context::default());
        assert_eq!(app.ai_runtime.optimized_prompt, "Art-directed image prompt");
        assert_eq!(app.ui.ai.prompt, "private-prompt");
        assert!(!app.ai_runtime.running());
        assert!(app.session.ai.candidate().is_none());
        assert!(!Arc::ptr_eq(&original, &app.session.active().unwrap().doc));
        let doc = &app.session.active().unwrap().doc;
        assert_eq!(doc.layers.len(), original.layers.len() + 1);
        assert_eq!(doc.layers.last().unwrap().name, "AI 1");
        assert_eq!(doc.ai_generations[0].versions[0].model, "actual-model");
        assert_eq!(doc.ai_generations[0].versions[0].optimized_prompt, "Art-directed image prompt");
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layers.len(), original.layers.len());
    }

    #[test]
    fn completed_worker_keeps_the_layer_after_user_edits_and_switches_tabs() {
        let (tx, rx) = std::sync::mpsc::channel();
        let rx = std::cell::RefCell::new(Some(rx));
        let services = crate::Services {
            ai_generate: Some(Box::new(move |_| Ok(Job { cancel: Default::default(), result: rx.borrow_mut().take().unwrap() }))),
            ..Default::default()
        };
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", json!({"width":32,"height":24})).unwrap();
        let mut app = PhotocraftApp::new(session, services);
        app.ui.ai.prompt = "red".into();
        start(&mut app).unwrap();
        app.run("edit.fill", json!({"contents":"color","color":[0.0,1.0,0.0,1.0]})).unwrap();
        let source = app.session.active().unwrap().doc.clone();
        app.run("file.new", json!({"width":16,"height":16})).unwrap();
        let other = app.session.active().unwrap().doc.clone();
        tx.send(Ok(encoded_image())).unwrap();
        tick(&mut app, &egui::Context::default());
        assert!(!app.ai_runtime.running());
        assert!(Arc::ptr_eq(&app.session.active().unwrap().doc, &other));
        let saved = app.session.documents().iter().find(|st| st.doc.id == source.id).unwrap();
        assert_eq!(saved.doc.layers.len(), source.layers.len() + 1);
        assert_eq!(saved.doc.layers.last().unwrap().name, "AI 1");
        assert_eq!(saved.doc.layers[0].surface().unwrap().pixel(5, 5), source.layers[0].surface().unwrap().pixel(5, 5));
        assert!(saved.is_dirty());
    }
    #[test]
    fn asynchronous_candidate_and_cancel_do_not_leak_keys_or_commit() {
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", json!({"width":16,"height":16})).unwrap();
        let before = session.active().unwrap().doc.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let rx = std::cell::RefCell::new(Some(rx));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let services = crate::Services {
            ai_generate: Some(Box::new(move |r| {
                assert_eq!(r.api_key, "private-test-key");
                Ok(Job { cancel: flag.clone(), result: rx.borrow_mut().take().unwrap() })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(session, services);
        app.ui.ai.prompt = "red".into();
        app.ui.ai.provider = "images-api".into();
        app.ai_runtime.api_key = "private-test-key".into();
        start(&mut app).unwrap();
        assert!(app.ai_runtime.running());
        let mut expected = before.as_ref().clone();
        expected.ai_project = app.session.active().unwrap().doc.ai_project.clone();
        assert_eq!(&expected, app.session.active().unwrap().doc.as_ref());
        assert!(!serde_json::to_string(&app.ui).unwrap().contains("private-test-key"));
        assert!(!serde_json::to_string(&expected.ai_project.events).unwrap().contains("private-test-key"));
        assert_eq!(expected.ai_project.last_prompt, "red");
        cancel(&mut app);
        assert!(stop.load(Ordering::Relaxed));
        assert!(!app.ai_runtime.running());
        assert!(tx.send(Ok("late-image".into())).is_err());
        assert!(app.session.execute("ai.inspect", json!({})).unwrap()["pending"].is_null());
        expected.ai_project = app.session.active().unwrap().doc.ai_project.clone();
        assert_eq!(&expected, app.session.active().unwrap().doc.as_ref());
        assert_eq!(expected.ai_project.events.last().unwrap().kind, "cancelled");
    }

    #[test]
    fn changing_provider_address_cannot_forward_an_existing_key() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.ai_runtime.api_key = "old-provider-secret".into();
        let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"baseUrl":"https://different-provider.example/v1"}));
        let _ = crate::control::handle(&mut app, &egui::Context::default(), &req);
        assert!(app.ai_runtime.api_key.is_empty());
        assert_eq!(app.ui.ai.base_url, "https://different-provider.example/v1");
    }

    #[test]
    fn component_mode_is_configurable_and_invalid_modes_do_not_mutate_settings() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        for mode in ["component", "unknown"] {
            let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"mode":mode}));
            let _ = crate::control::handle(&mut app, &egui::Context::default(), &req);
            assert_eq!(app.ui.ai.mode, photocraft_doc::ai::GenerationMode::Component);
        }
        for value in [json!(true), json!("invalid")] {
            let (req, _) = crate::control::ControlRequest::new("ai.configure", json!({"designFirst":value,"expandComponent":value}));
            let _ = crate::control::handle(&mut app, &egui::Context::default(), &req);
            assert!(app.ui.ai.design_first);
            assert!(app.ui.ai.expand_component);
        }
    }
}
