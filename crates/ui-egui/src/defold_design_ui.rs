//! Defold is a document attachment, never a replacement toolbar/canvas/layer panel.
use crate::{PhotocraftApp, widgets};
use photocraft_defold_format::{PropertyKind, Scene};
use serde_json::{Value, json};

pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.session.active_gui().is_some() {
        crate::defold_ui::properties(app, ui);
    } else if !properties(app, ui) {
        ui.label(tl!("Select a Defold GUI node to edit its properties"));
    }
}

#[derive(Clone)]
struct PublicationStatus {
    message: String,
    error: bool,
    transaction: Option<String>,
    phase: Option<String>,
    revision: Option<u64>,
    retained: std::sync::Arc<Vec<(String, String)>>,
}
fn status_id(open_id: u64) -> egui::Id {
    egui::Id::new(("defold-publication-status", open_id))
}
fn is_project_job(command: &str) -> bool {
    matches!(
        command,
        "defold.saveDesignSource"
            | "defold.syncDesign"
            | "defold.publishProjectDesign"
            | "defold.refreshPublication"
            | "defold.cancelPublication"
            | "defold.reviewProjectChanges"
            | "defold.applyProjectReview"
            | "defold.refreshAssetManagement"
            | "defold.migrateLegacySource"
            | "defold.migrationStatus"
            | "defold.revealProjectResource"
            | "defold.requestProjectEditing"
            | "defold.releaseProjectEditing"
            | "defold.reviewSharedPublication"
            | "defold.shareNodeArtwork"
            | "defold.attachSharedArtwork"
            | "defold.openSharedArtworkSource"
            | "defold.makeArtworkIndependent"
    )
}
/// Keep failure feedback attached to the opening that started it, not another tab with the same persisted document ID.
pub(crate) fn track_jobs(app: &PhotocraftApp, ctx: &egui::Context) {
    for job in app.session.jobs().into_iter().filter(|job| is_project_job(&job.command)) {
        let key = egui::Id::new(("defold-project-job", job.id.0));
        if ctx.data(|data| data.get_temp::<u64>(key)).is_some() {
            continue;
        }
        let mut candidates = app.session.documents().iter().filter(|state| Some(state.doc.id) == job.document);
        if let Some(state) = candidates.next()
            && candidates.next().is_none()
        {
            ctx.data_mut(|data| data.insert_temp(key, state.open_id));
        }
    }
}
fn publication_result(result: &Value) -> PublicationStatus {
    let phase = result["publication"]["phase"].as_str();
    let message = match phase {
        Some("COMMITTED") if result["publication"]["targets"].as_array().is_some_and(|targets| targets.iter().any(|target| target["editorAhead"] == true)) => {
            tl!("Published; Defold has newer unsaved changes").to_owned()
        }
        Some("COMMITTED") => tl!("GUI and artwork published to Defold").to_owned(),
        Some("ABORTED") => tl!("Publication stopped; creative source is preserved").to_owned(),
        Some("RECOVERY_REQUIRED") => tl!("Publication needs recovery; check its status before retrying").to_owned(),
        Some("PREPARING" | "PREPARED" | "APPLYING" | "APPLIED_UNSAVED" | "SAVING") => tl!("Publication is awaiting confirmation; check its status").to_owned(),
        _ if result["publication"].is_null() => tl!("No publication has been recorded for this GUI").to_owned(),
        _ => tl!("Publication was not confirmed").to_owned(),
    };
    let error = matches!(phase, Some("ABORTED" | "RECOVERY_REQUIRED")) || result["publication"]["error"].is_object();
    let message = result["publication"]["error"]["message"].as_str().map_or(message.clone(), |reason| format!("{message}: {reason}"));
    PublicationStatus {
        message,
        error,
        transaction: result["publication"]["transactionId"].as_str().map(str::to_owned),
        phase: phase.map(str::to_owned),
        revision: result["capturedNativeRevision"].as_u64(),
        retained: result["retainedResources"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(result["publication"]["retained"].as_array().into_iter().flatten())
            .filter_map(|resource| Some((resource["path"].as_str()?.into(), resource["reason"].as_str()?.into())))
            .collect::<Vec<_>>()
            .into(),
    }
}
pub(crate) fn publication_event(app: &mut PhotocraftApp, ctx: &egui::Context, event: &photocraft_engine::jobs::JobEvent) -> bool {
    if editing_event(app, ctx, event) {
        return true;
    }
    if assets_event(app, ctx, event) {
        return true;
    }
    if review_event(app, ctx, event) {
        return true;
    }
    if !matches!(event.command.as_str(), "defold.syncDesign" | "defold.publishProjectDesign" | "defold.refreshPublication" | "defold.cancelPublication") {
        return false;
    }
    let key = egui::Id::new(("defold-project-job", event.id.0));
    let tracked = ctx.data_mut(|data| data.remove_temp::<u64>(key));
    let (open_id, status) = match &event.outcome {
        photocraft_engine::jobs::JobOutcome::Done(result) => (result["openId"].as_u64().or(tracked), publication_result(result)),
        photocraft_engine::jobs::JobOutcome::Failed(error) => {
            (tracked, PublicationStatus { message: error.clone(), error: true, transaction: None, phase: None, revision: None, retained: Default::default() })
        }
        photocraft_engine::jobs::JobOutcome::Cancelled => (
            tracked,
            PublicationStatus {
                message: tl!("Publication cancelled; check its status before retrying").into(),
                error: false,
                transaction: None,
                phase: None,
                revision: None,
                retained: Default::default(),
            },
        ),
    };
    app.ui.status = status.message.clone();
    app.ui.status_error = status.error;
    if let Some(open_id) = open_id
        && app.session.documents().iter().any(|state| state.open_id == open_id)
    {
        ctx.data_mut(|data| data.insert_temp(status_id(open_id), status));
    }
    app.sync_views();
    true
}
fn run_publication(app: &mut PhotocraftApp, ui: &mut egui::Ui, open_id: u64, command: &str) {
    run_publication_params(app, ui, open_id, command, json!({}));
}
fn run_publication_params(app: &mut PhotocraftApp, ui: &mut egui::Ui, open_id: u64, command: &str, params: Value) {
    let status = match app.run(command, params) {
        Ok(result) if result["job"].is_number() => {
            let id = result["job"].as_u64().unwrap_or_default();
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(("defold-project-job", id)), open_id));
            PublicationStatus {
                message: tl!("Publishing GUI and artwork...").into(),
                error: false,
                transaction: None,
                phase: Some("RUNNING".into()),
                revision: None,
                retained: Default::default(),
            }
        }
        Ok(result) => publication_result(&result),
        Err(error) => PublicationStatus { message: error, error: true, transaction: None, phase: None, revision: None, retained: Default::default() },
    };
    app.ui.status = status.message.clone();
    app.ui.status_error = status.error;
    ui.ctx().data_mut(|data| data.insert_temp(status_id(open_id), status));
}

#[derive(Clone, Default)]
struct ReviewStatus {
    report: Option<std::sync::Arc<Value>>,
    choices: std::collections::BTreeMap<String, String>,
    message: String,
    error: bool,
}
fn review_id(open_id: u64) -> egui::Id {
    egui::Id::new(("defold-project-review", open_id))
}
fn review_result(result: &Value) -> ReviewStatus {
    if result["applied"] == true {
        ReviewStatus { message: tl!("Reviewed changes applied; save or sync when ready").into(), ..Default::default() }
    } else {
        ReviewStatus { report: Some(std::sync::Arc::new(result.clone())), ..Default::default() }
    }
}
fn review_event(app: &mut PhotocraftApp, ctx: &egui::Context, event: &photocraft_engine::jobs::JobEvent) -> bool {
    if !matches!(event.command.as_str(), "defold.reviewProjectChanges" | "defold.applyProjectReview") {
        return false;
    }
    let tracked = ctx.data_mut(|data| data.remove_temp::<u64>(egui::Id::new(("defold-project-job", event.id.0))));
    let (opening, status) = match &event.outcome {
        photocraft_engine::jobs::JobOutcome::Done(result) => (result["openId"].as_u64().or(tracked), review_result(result)),
        photocraft_engine::jobs::JobOutcome::Failed(error) => (tracked, ReviewStatus { message: error.clone(), error: true, ..Default::default() }),
        photocraft_engine::jobs::JobOutcome::Cancelled => {
            (tracked, ReviewStatus { message: tl!("Review cancelled; the creative source is unchanged").into(), ..Default::default() })
        }
    };
    app.ui.status = status.message.clone();
    app.ui.status_error = status.error;
    if let Some(opening) = opening
        && app.session.documents().iter().any(|state| state.open_id == opening)
    {
        ctx.data_mut(|data| data.insert_temp(review_id(opening), status));
    }
    app.sync_views();
    true
}
fn run_review(app: &mut PhotocraftApp, ui: &mut egui::Ui, opening: u64, command: &str, params: Value) {
    let status = match app.run(command, params) {
        Ok(result) if result["job"].is_number() => {
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(("defold-project-job", result["job"].as_u64().unwrap_or_default())), opening));
            ReviewStatus { message: tl!("Checking project changes...").into(), ..Default::default() }
        }
        Ok(result) => review_result(&result),
        Err(error) => ReviewStatus { message: error, error: true, ..Default::default() },
    };
    ui.ctx().data_mut(|data| data.insert_temp(review_id(opening), status));
}
fn review_ready(report: &Value, revision: u64, choices: &std::collections::BTreeMap<String, String>) -> bool {
    report["blocked"] == false
        && report["capturedNativeRevision"] == revision
        && report["items"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                if item["kind"] != "conflict" {
                    return true;
                }
                let key = item["index"].to_string();
                matches!(choices.get(&key).map(String::as_str), Some("mine") if item["canKeepMine"]==true)
                    || matches!(choices.get(&key).map(String::as_str),Some("theirs") if item["canTakeTheirs"]==true)
            })
        })
}
fn review_value(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let mut output: String = text.chars().take(320).collect();
            if text.chars().nth(320).is_some() {
                output.push_str("...");
            }
            output
        }
        Value::Null => tl!("Absent").into(),
        Value::Object(object) => {
            format!("{{ {}{} }}", object.keys().take(8).map(String::as_str).collect::<Vec<_>>().join(", "), if object.len() > 8 { " ..." } else { "" })
        }
        Value::Array(array) => format!(
            "[ {}{} ]",
            array.iter().take(8).map(|v| if v.is_object() || v.is_array() { "...".into() } else { review_value(v) }).collect::<Vec<_>>().join(", "),
            if array.len() > 8 { " ..." } else { "" }
        ),
        value => value.to_string(),
    }
}
fn review_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui, opening: u64, revision: u64, busy: bool) {
    ui.collapsing(tl!("Review Defold changes"), |ui| {
        ui.add(egui::Label::new(tl!("Compare both editors before accepting independent property changes")).wrap());
        if ui.add_enabled(!busy && app.session.is_enabled("defold.reviewProjectChanges"), egui::Button::new(tl!("Check project changes"))).clicked() {
            run_review(app, ui, opening, "defold.reviewProjectChanges", json!({}));
        }
        let mut status = ui.ctx().data(|data| data.get_temp::<ReviewStatus>(review_id(opening))).unwrap_or_default();
        if !status.message.is_empty() {
            let text = egui::RichText::new(&status.message);
            ui.add(egui::Label::new(if status.error { text.color(ui.visuals().error_fg_color) } else { text }).wrap());
        }
        if let Some(report) = &status.report {
            if report["targetDirty"] == true {
                ui.add(egui::Label::new(tl!("Save this GUI in Defold, then check changes again")).wrap());
            }
            if report["capturedNativeRevision"] != revision {
                ui.add(egui::Label::new(tl!("The draft changed; check project changes again")).wrap());
            }
            egui::ScrollArea::vertical().id_salt(("review-items", opening)).max_height(280.0).show(ui, |ui| {
                for item in report["items"].as_array().into_iter().flatten() {
                    let node = item["node"].as_str().unwrap_or("GUI");
                    ui.separator();
                    ui.add(egui::Label::new(format!("{node} / {}", item["property"].as_str().unwrap_or_default())).wrap());
                    for (label, key) in [(tl!("Common version"), "base"), (tl!("PhotoCraft"), "local"), (tl!("Defold"), "external")] {
                        ui.add(egui::Label::new(format!("{label}: {}", review_value(&item[key]))).wrap());
                    }
                    if let Some(reason) = item["blocker"].as_str() {
                        ui.add(egui::Label::new(egui::RichText::new(reason).color(ui.visuals().error_fg_color)).wrap());
                    } else if item["kind"] == "conflict" {
                        let choice = status.choices.entry(item["index"].to_string()).or_default();
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(item["canKeepMine"] == true, egui::Button::new(tl!("Keep Craft")).selected(choice == "mine")).clicked() {
                                *choice = "mine".into();
                            }
                            if ui.add_enabled(item["canTakeTheirs"] == true, egui::Button::new(tl!("Use Defold")).selected(choice == "theirs")).clicked() {
                                *choice = "theirs".into();
                            }
                        });
                        if item["preserveArtwork"] == true {
                            ui.add(egui::Label::new(tl!("Painted artwork is preserved")).wrap());
                        }
                    }
                }
            });
            if report["items"].as_array().is_some_and(Vec::is_empty) {
                ui.label(tl!("No project differences"));
            }
            if ui
                .add_enabled(
                    !busy && review_ready(report, revision, &status.choices) && app.session.is_enabled("defold.applyProjectReview"),
                    egui::Button::new(tl!("Apply reviewed changes")),
                )
                .clicked()
            {
                let params = json!({"reviewId":report["reviewId"],"choices":status.choices});
                run_review(app, ui, opening, "defold.applyProjectReview", params);
                return;
            }
        }
        ui.ctx().data_mut(|data| data.insert_temp(review_id(opening), status));
    });
}

#[derive(Clone, Default)]
struct AssetFeedback {
    message: String,
    error: bool,
}
fn asset_feedback_id(opening: u64) -> egui::Id {
    egui::Id::new(("defold-assets-feedback", opening))
}
fn shared_review_id(opening: u64) -> egui::Id {
    egui::Id::new(("defold-shared-publication-review", opening))
}
fn assets_result(result: &Value) -> AssetFeedback {
    if result["targets"].is_array() && result["capturedNativeRevision"].is_number() {
        return AssetFeedback { message: tl!("Shared publication scope checked").into(), error: false };
    }
    if let Some(id) = result["sharedAssetId"].as_str() {
        return AssetFeedback { message: format!("{}: {id}", tl!("Shared artwork ID copied")), error: false };
    }
    for (key, message) in [
        ("attachedAssetId", tl!("Shared artwork attached; save and publish to update the game")),
        ("independentAssetId", tl!("Artwork is independent and editable; the shared owner is unchanged")),
        ("sourceOpened", tl!("Shared artwork source opened")),
    ] {
        if !result[key].is_null() {
            return AssetFeedback { message: message.into(), error: false };
        }
    }
    let message = if result["opened"] == true {
        tl!("Resource opened in Defold")
    } else if result["reopenRequired"] == true || result["migration"]["state"] == "sourceCommitted" {
        tl!("Migration saved. Preserve this draft separately, then reopen the GUI to use the migrated source")
    } else if result["migration"].is_object() {
        tl!("Migration needs review; original files are preserved")
    } else {
        tl!("Project asset inventory updated")
    };
    AssetFeedback { message: message.into(), error: false }
}
fn assets_event(app: &mut PhotocraftApp, ctx: &egui::Context, event: &photocraft_engine::jobs::JobEvent) -> bool {
    if !matches!(
        event.command.as_str(),
        "defold.refreshAssetManagement"
            | "defold.migrateLegacySource"
            | "defold.migrationStatus"
            | "defold.revealProjectResource"
            | "defold.shareNodeArtwork"
            | "defold.attachSharedArtwork"
            | "defold.openSharedArtworkSource"
            | "defold.makeArtworkIndependent"
            | "defold.reviewSharedPublication"
    ) {
        return false;
    }
    if let photocraft_engine::jobs::JobOutcome::Done(result) = &event.outcome
        && let Some(id) = result["sharedAssetId"].as_str()
    {
        ctx.copy_text(id.into());
    }
    let tracked = ctx.data_mut(|data| data.remove_temp::<u64>(egui::Id::new(("defold-project-job", event.id.0))));
    let (opening, feedback) = match &event.outcome {
        photocraft_engine::jobs::JobOutcome::Done(result) => (result["openId"].as_u64().or(tracked), assets_result(result)),
        photocraft_engine::jobs::JobOutcome::Failed(error) => (tracked, AssetFeedback { message: error.clone(), error: true }),
        photocraft_engine::jobs::JobOutcome::Cancelled => (tracked, AssetFeedback { message: tl!("Asset inspection cancelled").into(), error: false }),
    };
    if event.command == "defold.reviewSharedPublication"
        && let (Some(opening), photocraft_engine::jobs::JobOutcome::Done(result)) = (opening, &event.outcome)
    {
        ctx.data_mut(|data| data.insert_temp(shared_review_id(opening), std::sync::Arc::new(result.clone())));
    }
    if event.command != "defold.revealProjectResource"
        && matches!(event.outcome, photocraft_engine::jobs::JobOutcome::Done(_))
        && let Some(opening) = opening
    {
        ctx.data_mut(|data| data.remove::<AssetSnapshot>(asset_snapshot_id(opening)));
    }
    app.sync_views();
    app.ui.status = feedback.message.clone();
    app.ui.status_error = feedback.error;
    if let Some(opening) = opening
        && app.session.documents().iter().any(|state| state.open_id == opening)
    {
        ctx.data_mut(|data| data.insert_temp(asset_feedback_id(opening), feedback));
    }
    true
}
fn run_asset(app: &mut PhotocraftApp, ui: &mut egui::Ui, opening: u64, command: &str, params: Value) {
    let feedback = match app.run(command, params) {
        Ok(result) if result["job"].is_number() => {
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(("defold-project-job", result["job"].as_u64().unwrap_or_default())), opening));
            AssetFeedback { message: tl!("Reading project asset inventory...").into(), error: false }
        }
        Ok(result) => {
            if command == "defold.reviewSharedPublication" {
                ui.ctx().data_mut(|data| data.insert_temp(shared_review_id(opening), std::sync::Arc::new(result.clone())));
            }
            if let Some(id) = result["sharedAssetId"].as_str() {
                ui.ctx().copy_text(id.into());
            }
            if command != "defold.revealProjectResource" {
                ui.ctx().data_mut(|data| data.remove::<AssetSnapshot>(asset_snapshot_id(opening)));
            }
            assets_result(&result)
        }
        Err(error) => AssetFeedback { message: error, error: true },
    };
    ui.ctx().data_mut(|data| data.insert_temp(asset_feedback_id(opening), feedback));
}
fn retention_reason(reason: &str) -> &str {
    match reason {
        "sourceVersion" => tl!("Saved creative source"),
        "publication" => tl!("Published version"),
        "sharedReference" => tl!("Used by another scene"),
        "editorHistory" => tl!("Editor history"),
        "unsavedEditor" => tl!("Unsaved editor changes"),
        "publicApi" => tl!("Public game resource"),
        "unknownRoots" => tl!("Incomplete reference inventory"),
        "changedOnDisk" => tl!("Changed outside PhotoCraft"),
        "notTombstoned" => tl!("Deletion has not been requested"),
        "gracePeriod" => tl!("Recovery period"),
        "missingBackup" => tl!("Verified backup required"),
        "transaction" => tl!("Pending publication"),
        _ => reason,
    }
}
#[derive(Clone)]
struct AssetSnapshot {
    authority: Option<photocraft_engine::defold_project_model::ProjectContext>,
    report: std::sync::Arc<Value>,
}
fn asset_snapshot_id(opening: u64) -> egui::Id {
    egui::Id::new(("defold-asset-snapshot", opening))
}
fn asset_snapshot(
    ctx: &egui::Context,
    opening: u64,
    authority: Option<photocraft_engine::defold_project_model::ProjectContext>,
    read: impl FnOnce() -> Value,
) -> std::sync::Arc<Value> {
    let key = asset_snapshot_id(opening);
    if let Some(cached) = ctx.data(|data| data.get_temp::<AssetSnapshot>(key))
        && cached.authority == authority
    {
        return cached.report;
    }
    let report = std::sync::Arc::new(read());
    ctx.data_mut(|data| data.insert_temp(key, AssetSnapshot { authority, report: report.clone() }));
    report
}
fn shared_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui, opening: u64, busy: bool) {
    egui::CollapsingHeader::new(tl!("Shared artwork")).id_salt(("shared-artwork", opening)).show(ui, |ui| {
        ui.add(egui::Label::new(tl!("The owner keeps the editable layers; other GUIs use a read-only reference")).wrap());
        let shared = app.session.active().and_then(|state| {
            let link = photocraft_engine::defold_design::selected(&app.session)?;
            let role = state.doc.defold.as_ref()?.project.as_ref()?.layers.get(&link.layer)?;
            match &role.appearance {
                Some(photocraft_doc::defold::AppearanceSource::Shared { source }) => Some(source.clone()),
                _ => None,
            }
        });
        if let Some(source) = shared {
            ui.add(egui::Label::new(source.asset_id.as_str()).wrap());
            ui.add(egui::Label::new(tl!("Edit the owner or make this node independent before painting")).wrap());
            for (command, label) in
                [("defold.openSharedArtworkSource", tl!("Open artwork source")), ("defold.makeArtworkIndependent", tl!("Make artwork independent"))]
            {
                if ui.add_enabled(!busy && app.session.is_enabled(command), egui::Button::new(label)).clicked() {
                    run_asset(app, ui, opening, command, json!({}));
                }
            }
        } else {
            if ui.add_enabled(!busy && app.session.is_enabled("defold.shareNodeArtwork"), egui::Button::new(tl!("Copy shared artwork ID"))).clicked() {
                run_asset(app, ui, opening, "defold.shareNodeArtwork", json!({}));
            }
            let key = egui::Id::new(("shared-artwork-id", opening));
            let mut asset = ui.ctx().data(|data| data.get_temp::<String>(key)).unwrap_or_default();
            ui.add(egui::TextEdit::singleline(&mut asset).hint_text(tl!("Paste a shared artwork ID")).desired_width(f32::INFINITY));
            if ui
                .add_enabled(
                    !busy && !asset.trim().is_empty() && app.session.is_enabled("defold.attachSharedArtwork"),
                    egui::Button::new(tl!("Use shared artwork for this node")),
                )
                .clicked()
            {
                run_asset(app, ui, opening, "defold.attachSharedArtwork", json!({"assetId":asset.trim()}));
            }
            ui.ctx().data_mut(|data| data.insert_temp(key, asset));
        }
        ui.separator();
        if ui.add_enabled(!busy && app.session.is_enabled("defold.reviewSharedPublication"), egui::Button::new(tl!("Review shared publication"))).clicked() {
            run_asset(app, ui, opening, "defold.reviewSharedPublication", json!({}));
        }
        if let Some(report) = ui.ctx().data(|data| data.get_temp::<std::sync::Arc<Value>>(shared_review_id(opening))) {
            let current =
                app.session.active().is_some_and(|state| state.open_id == opening && report["capturedNativeRevision"].as_u64() == Some(state.revision));
            if !current {
                ui.add(egui::Label::new(tl!("The document changed; review the publication scope again")).wrap());
            }
            egui::ScrollArea::vertical().id_salt(("shared-publication-targets", opening)).max_height(160.0).show(ui, |ui| {
                for target in report["targets"].as_array().into_iter().flatten() {
                    ui.add(egui::Label::new(target["path"].as_str().unwrap_or_default()).wrap());
                    ui.add(egui::Label::new(egui::RichText::new(target["sourceRevision"].as_str().unwrap_or_default()).small()).wrap());
                    if target["unpublishedSource"] == true && target["primary"] != true {
                        ui.add(egui::Label::new(tl!("Saved related changes have not been published")).wrap());
                    }
                }
            });
            if report["requiresRelatedSourceApproval"] == true {
                ui.add(egui::Label::new(tl!("Publishing will include the saved changes in these related GUIs")).wrap());
                if ui
                    .add_enabled(current && !busy && app.session.is_enabled("defold.syncDesign"), egui::Button::new(tl!("Publish these saved related changes")))
                    .clicked()
                {
                    run_publication_params(app, ui, opening, "defold.syncDesign", json!({"sharedReviewId":report["sharedReviewId"]}));
                }
            }
        }
        if let Some(feedback) = ui.ctx().data(|data| data.get_temp::<AssetFeedback>(asset_feedback_id(opening))) {
            ui.add(egui::Label::new(feedback.message).wrap());
        }
    });
}

fn assets_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui, opening: u64, busy: bool) {
    ui.collapsing(tl!("Project assets and legacy sources"), |ui| {
        if ui.add_enabled(!busy && app.session.is_enabled("defold.refreshAssetManagement"), egui::Button::new(tl!("Refresh asset inventory"))).clicked() {
            run_asset(app, ui, opening, "defold.refreshAssetManagement", json!({}));
        }
        if let Some(feedback) = ui.ctx().data(|data| data.get_temp::<AssetFeedback>(asset_feedback_id(opening))) {
            let text = egui::RichText::new(feedback.message);
            ui.add(egui::Label::new(if feedback.error { text.color(ui.visuals().error_fg_color) } else { text }).wrap());
        }
        let authority = photocraft_engine::defold_project_assets_cmds::cached_context(&app.session);
        let status = asset_snapshot(ui.ctx(), opening, authority, || photocraft_engine::defold_project_assets_cmds::cached_status(&app.session));
        let snapshot = &status["snapshot"];
        if status["available"] != true {
            return;
        }
        if snapshot["rootsComplete"] != true {
            ui.add(egui::Label::new(tl!("References are incomplete; retained resources will not be deleted")).wrap());
        }
        let count = snapshot["registry"]["assets"]
            .as_object()
            .map_or(0, serde_json::Map::len)
            .max(snapshot["retained"].as_array().map_or(0, Vec::len))
            .max(snapshot["collection"].as_array().map_or(0, Vec::len));
        let page_key = egui::Id::new(("asset-report-page", opening));
        let mut page = ui.ctx().data(|data| data.get_temp::<usize>(page_key)).unwrap_or_default().min(count.saturating_sub(1) / 50);
        if count > 50 {
            ui.horizontal(|ui| {
                if ui.add_enabled(page > 0, egui::Button::new("<")).clicked() {
                    page = page.saturating_sub(1);
                }
                ui.label(format!("{} / {}", page + 1, count.div_ceil(50)));
                if ui.add_enabled((page + 1) * 50 < count, egui::Button::new(">")).clicked() {
                    page += 1;
                }
            });
        }
        ui.ctx().data_mut(|data| data.insert_temp(page_key, page));
        egui::ScrollArea::vertical().id_salt(("project-assets", opening)).max_height(240.0).show(ui, |ui| {
            for asset in snapshot["registry"]["assets"].as_object().into_iter().flat_map(|assets| assets.values()).skip(page * 50).take(50) {
                let label = if asset["state"] == "active" { tl!("Active resource") } else { tl!("Retained resource") };
                ui.add(egui::Label::new(format!("{label}: {}", asset["displayName"].as_str().unwrap_or_default())).wrap());
                ui.add(egui::Label::new(egui::RichText::new(asset["image"].as_str().unwrap_or_default()).small()).wrap());
                if ui.add_enabled(!busy && app.session.is_enabled("defold.revealProjectResource"), egui::Button::new(tl!("Reveal in Defold"))).clicked() {
                    run_asset(app, ui, opening, "defold.revealProjectResource", json!({"path":asset["image"]}));
                }
            }
            for retained in snapshot["retained"].as_array().into_iter().flatten().skip(page * 50).take(50) {
                ui.add(
                    egui::Label::new(format!("{}: {}", retained["path"].as_str().unwrap_or_default(), retained["reason"].as_str().unwrap_or_default())).wrap(),
                );
            }
            for item in snapshot["collection"].as_array().into_iter().flatten().skip(page * 50).take(50) {
                let name = snapshot["registry"]["assets"][item["assetId"].as_str().unwrap_or_default()]["displayName"].as_str().unwrap_or_default();
                let reasons = item["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).map(retention_reason).collect::<Vec<_>>().join(", ");
                ui.add(egui::Label::new(format!("{name}: {reasons}")).wrap());
            }
            for warning in snapshot["warnings"].as_array().into_iter().flatten() {
                if let Some(text) = warning.as_str() {
                    ui.add(egui::Label::new(text).wrap());
                }
            }
        });
        ui.separator();
        ui.strong(tl!("Legacy source review"));
        ui.add(egui::Label::new(tl!("Migration backs up the selected source and leaves this open draft unchanged")).wrap());
        let selected_id = egui::Id::new(("legacy-source-selection", opening));
        let mut selected = ui.ctx().data(|data| data.get_temp::<String>(selected_id)).unwrap_or_default();
        let inventory = &snapshot["legacyInventory"];
        let entries = inventory["entries"].as_array().map(Vec::as_slice).unwrap_or_default();
        let mut candidates = Vec::new();
        let page_key = egui::Id::new(("legacy-inventory-page", opening));
        let mut legacy_page = ui.ctx().data(|data| data.get_temp::<usize>(page_key)).unwrap_or_default().min(entries.len().saturating_sub(1) / 30);
        if entries.len() > 30 {
            ui.horizontal(|ui| {
                if ui.add_enabled(legacy_page > 0, egui::Button::new("<")).clicked() {
                    legacy_page = legacy_page.saturating_sub(1);
                }
                ui.label(format!("{} / {}", legacy_page + 1, entries.len().div_ceil(30)));
                if ui.add_enabled((legacy_page + 1) * 30 < entries.len(), egui::Button::new(">")).clicked() {
                    legacy_page += 1;
                }
            });
        }
        ui.ctx().data_mut(|data| data.insert_temp(page_key, legacy_page));
        for (index, entry) in entries.iter().enumerate() {
            let path = entry["path"].as_str().unwrap_or_default();
            if matches!(entry["kind"].as_str(), Some("guiNative" | "sharedNative")) {
                candidates.push(path);
                if index / 30 == legacy_page && ui.selectable_label(selected == path, path).clicked() {
                    selected = path.into();
                }
            } else if index / 30 == legacy_page {
                ui.add(egui::Label::new(path).wrap());
            }
        }
        if !candidates.contains(&selected.as_str()) {
            selected.clear();
        }
        ui.ctx().data_mut(|data| data.insert_temp(selected_id, selected.clone()));
        if ui
            .add_enabled(
                !busy && !selected.is_empty() && app.session.is_enabled("defold.migrateLegacySource"),
                egui::Button::new(tl!("Back up and migrate selected source")),
            )
            .clicked()
        {
            run_asset(app, ui, opening, "defold.migrateLegacySource", json!({"input":selected,"inventoryHash":inventory["inventoryHash"]}));
        }
        for migration in snapshot["migrations"].as_array().into_iter().flatten() {
            ui.separator();
            ui.add(
                egui::Label::new(format!("{}: {}", migration["operationId"].as_str().unwrap_or_default(), migration["state"].as_str().unwrap_or_default()))
                    .wrap(),
            );
            for note in migration["notes"].as_array().into_iter().flatten() {
                if let Some(note) = note.as_str() {
                    ui.add(egui::Label::new(note).wrap());
                }
            }
            if ui.add_enabled(!busy && app.session.is_enabled("defold.migrationStatus"), egui::Button::new(tl!("Check migration status"))).clicked() {
                run_asset(app, ui, opening, "defold.migrationStatus", json!({"operationId":migration["operationId"]}));
            }
        }
    });
}

fn editing_message(result: &Value) -> String {
    if result["editing"] == true {
        tl!("Editing permission acquired; saving still checks the project version").into()
    } else {
        tl!("Editing permission released").into()
    }
}
fn editing_event(app: &mut PhotocraftApp, ctx: &egui::Context, event: &photocraft_engine::jobs::JobEvent) -> bool {
    if !matches!(event.command.as_str(), "defold.requestProjectEditing" | "defold.releaseProjectEditing") {
        return false;
    }
    let opening = ctx.data_mut(|data| data.remove_temp::<u64>(egui::Id::new(("defold-project-job", event.id.0))));
    let feedback = match &event.outcome {
        photocraft_engine::jobs::JobOutcome::Done(result) => AssetFeedback { message: editing_message(result), error: false },
        photocraft_engine::jobs::JobOutcome::Failed(error) => AssetFeedback { message: error.clone(), error: true },
        photocraft_engine::jobs::JobOutcome::Cancelled => return true,
    };
    app.ui.status = feedback.message.clone();
    app.ui.status_error = feedback.error;
    if let Some(opening) = opening {
        ctx.data_mut(|data| data.insert_temp(egui::Id::new(("project-editing-feedback", opening)), feedback));
    }
    true
}
fn run_editing(app: &mut PhotocraftApp, ui: &mut egui::Ui, opening: u64, command: &str) {
    let feedback = match app.run(command, json!({})) {
        Ok(result) if result["job"].is_number() => {
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(("defold-project-job", result["job"].as_u64().unwrap_or_default())), opening));
            AssetFeedback { message: tl!("Updating editing permission...").into(), error: false }
        }
        Ok(result) => AssetFeedback { message: editing_message(&result), error: false },
        Err(error) => AssetFeedback { message: error, error: true },
    };
    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(("project-editing-feedback", opening)), feedback));
}
fn connection_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.strong(tl!("Project connection"));
    #[cfg(not(target_arch = "wasm32"))]
    if ui.add_enabled(app.session.is_enabled("defold.connectProjectHost"), egui::Button::new(tl!("Connect project host..."))).clicked() {
        use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
        if let Err(error) =
            app.ask_file(FileDialogRequest::Open { multiple: false, initial_dir: None, extensions: Some(vec!["json".into()]) }, |app, answer| {
                let FileDialogAnswer::Paths(paths) = answer else { return Err(tl!("Choose a project pairing file on this computer").into()) };
                let path = paths.into_iter().next().ok_or_else(|| tl!("Choose a project pairing file on this computer").to_owned())?;
                // Credentials remain inside the worker. Only the chosen file path enters the command log.
                app.run("defold.connectProjectHost", json!({"pairingFile":path}))
            })
        {
            crate::notices::error(app, error);
        }
    }
    let connection = photocraft_engine::defold_project_client::connection_status(&app.session);
    if connection["connected"] == true {
        ui.label(tl!("Project host connected"));
        let key = egui::Id::new("defold-project-gui-path");
        let mut path = ui.ctx().data(|data| data.get_temp::<String>(key)).unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut path).hint_text("/main/ui.gui").desired_width(ui.available_width()));
        ui.ctx().data_mut(|data| data.insert_temp(key, path.clone()));
        if ui.add_enabled(!path.trim().is_empty() && app.session.is_enabled("defold.openProjectGui"), egui::Button::new(tl!("Open project GUI"))).clicked() {
            let _ = app.run("defold.openProjectGui", json!({"path":path.trim()}));
        }
    }
}
pub fn sync_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some((open_id, revision, gui, root)) =
        app.session.active().and_then(|state| state.doc.defold.as_ref().map(|link| (state.open_id, state.revision, link.gui.clone(), link.root.clone())))
    else {
        connection_panel(app, ui);
        ui.collapsing(tl!("Defold editor connection"), |ui| crate::peer_binding_ui::panel(app, ui));
        ui.label(tl!("Open a linked Defold GUI to synchronize"));
        return;
    };
    let busy = app.session.active_job().is_some() || photocraft_engine::defold_project_client::project_task_running(&app.session);
    let width = ui.available_width();
    if ui.add_enabled_ui(!busy && app.session.is_enabled("defold.syncDesign"), |ui| widgets::primary_button(ui, tl!("Sync to Defold"), width)).inner.clicked() {
        run_publication(app, ui, open_id, "defold.syncDesign");
    }
    ui.add(egui::Label::new(tl!("Saves the creative source, then publishes GUI and artwork")).wrap());
    ui.separator();
    let source = photocraft_engine::defold_project_client::status(&app.session);
    ui.strong(tl!("Creative source"));
    ui.label(if source["sourceKnownSaved"] != true {
        tl!("Creative source has not been saved to project")
    } else if source["unsaved"] == true {
        tl!("Source has unsaved edits")
    } else {
        tl!("Creative source saved to project")
    });
    if ui.add_enabled(!busy && app.session.is_enabled("defold.saveDesignSource"), egui::Button::new(tl!("Save creative source"))).clicked()
        && let Ok(result) = app.run("defold.saveDesignSource", json!({}))
    {
        app.ui.status = if result["job"].is_number() { tl!("Saving creative source...") } else { tl!("Creative source saved to project") }.into();
        app.ui.status_error = false;
    }
    if ui.add_enabled(!busy && app.session.is_enabled("defold.requestProjectEditing"), egui::Button::new(tl!("Request editing permission"))).clicked() {
        run_editing(app, ui, open_id, "defold.requestProjectEditing");
    }
    if ui
        .add_enabled(!busy && app.session.is_enabled("defold.releaseProjectEditing"), egui::Button::new(tl!("Release editing permission")))
        .on_hover_text(tl!("Save the creative source before handing editing to another computer"))
        .clicked()
    {
        run_editing(app, ui, open_id, "defold.releaseProjectEditing");
    }
    if let Some(feedback) = ui.ctx().data(|data| data.get_temp::<AssetFeedback>(egui::Id::new(("project-editing-feedback", open_id)))) {
        let text = egui::RichText::new(feedback.message);
        ui.add(egui::Label::new(if feedback.error { text.color(ui.visuals().error_fg_color) } else { text }).wrap());
    }
    ui.add_space(8.0);
    ui.separator();
    if let Some(status) = ui.ctx().data(|data| data.get_temp::<PublicationStatus>(status_id(open_id))) {
        let text = egui::RichText::new(&status.message);
        ui.add(egui::Label::new(if status.error { text.color(ui.visuals().error_fg_color) } else { text }).wrap());
        if status.phase.as_deref() == Some("COMMITTED") && status.revision.is_some_and(|published| published != revision) {
            ui.label(tl!("New edits have not been published"));
        }
        if matches!(status.phase.as_deref(), Some("PREPARING" | "PREPARED"))
            && ui.add_enabled(!busy && app.session.is_enabled("defold.cancelPublication"), egui::Button::new(tl!("Cancel prepared publication"))).clicked()
        {
            run_publication(app, ui, open_id, "defold.cancelPublication");
        }
        if let Some(transaction) = status.transaction {
            ui.collapsing(tl!("Publication details"), |ui| {
                ui.add(egui::Label::new(transaction).wrap());
                for (path, reason) in status.retained.iter() {
                    ui.add(egui::Label::new(format!("{path}: {reason}")).wrap());
                }
            });
        }
    } else {
        ui.add(egui::Label::new(tl!("Edits stay local until you sync")).wrap());
    }
    if ui.add_enabled(!busy && app.session.is_enabled("defold.refreshPublication"), egui::Button::new(tl!("Check publication status"))).clicked() {
        run_publication(app, ui, open_id, "defold.refreshPublication");
    }
    review_panel(app, ui, open_id, revision, busy);
    shared_panel(app, ui, open_id, busy);
    assets_panel(app, ui, open_id, busy);
    ui.add_space(8.0);
    let resource = std::path::Path::new(&gui).strip_prefix(&root).unwrap_or_else(|_| std::path::Path::new(&gui));
    ui.strong(tl!("GUI resource"));
    ui.add(egui::Label::new(format!("/{}", resource.to_string_lossy().replace('\\', "/"))).wrap()).on_hover_text(&gui);
    ui.collapsing(tl!("Defold Project"), |ui| {
        ui.add(egui::Label::new(&root).wrap());
        connection_panel(app, ui);
        ui.collapsing(tl!("Defold editor connection"), |ui| crate::peer_binding_ui::panel(app, ui));
    });
    ui.add_space(8.0);
    ui.separator();
    ui.strong(tl!("GUI nodes"));
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(app.session.is_enabled("defold.addDesignNode"), egui::Button::new(tl!("Add child node"))).clicked() {
            let _ = app.run("defold.addDesignNode", json!({"child":true}));
        }
        if ui.add_enabled(app.session.is_enabled("defold.addDesignNode"), egui::Button::new(tl!("Add GUI node"))).clicked() {
            let _ = app.run("defold.addDesignNode", json!({"child":false}));
        }
    });
    if ui.button(tl!("Defold GUI Properties")).clicked() {
        let _ = app.run("window.panel.defoldProperties", json!({}));
    }
}

fn property_resource(scene: &Scene, node: &str, key: &str) -> Option<String> {
    let collection = match key {
        "texture" => "textures",
        "font" => "fonts",
        "material" => "materials",
        _ => return None,
    };
    let value = scene.property(node, key).ok()?;
    let alias = value.as_str()?;
    let alias = if key == "texture" { alias.split('/').next()? } else { alias };
    let field = scene.proto.fields.iter().find(|field| field.name == collection && scene.proto.text(&field.children, "name") == alias)?;
    let path = scene.proto.text(&field.children, key);
    (!path.is_empty()).then_some(path)
}
fn number(ui: &mut egui::Ui, key: &str, base: f64, width: f32, range: std::ops::RangeInclusive<f32>) -> Option<Value> {
    let id = ui.id().with(key);
    let mut value = ui.ctx().data(|d| d.get_temp::<(f64, f32)>(id)).filter(|(v, _)| *v == base).map_or(base as f32, |(_, v)| v);
    let r = widgets::value_field_prec(ui, &mut value, range, "", width.max(30.0), 2);
    ui.ctx().data_mut(|d| d.insert_temp(id, (base, value)));
    ((r.drag_stopped() || (r.changed() && !r.dragged())) && (value as f64 - base).abs() > 1e-7).then(|| json!(value))
}
fn string(ui: &mut egui::Ui, key: &str, base: &str, multiline: bool) -> Option<Value> {
    let id = ui.id().with(key);
    let mut draft = ui.ctx().data(|d| d.get_temp::<(String, String)>(id)).filter(|(b, _)| b == base).map_or_else(|| base.to_string(), |(_, v)| v);
    let r = if multiline {
        ui.add(egui::TextEdit::multiline(&mut draft).desired_rows(3).desired_width(ui.available_width()))
    } else {
        ui.add(egui::TextEdit::singleline(&mut draft).desired_width(ui.available_width()))
    };
    ui.ctx().data_mut(|d| d.insert_temp(id, (base.to_string(), draft.clone())));
    (r.lost_focus() && draft != base).then(|| json!(draft))
}
fn value(scene: &Scene, node: &str, key: &str) -> Value {
    scene.property(node, key).unwrap_or(Value::Null)
}
fn enum_label(value: &str) -> String {
    let short = ["BLEND_MODE_", "PIVOT_", "XANCHOR_", "YANCHOR_", "ADJUST_MODE_", "SIZE_MODE_", "CLIPPING_MODE_", "PIEBOUNDS_"]
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .unwrap_or(value);
    if short.len() <= 2 {
        return short.into();
    }
    short
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or(String::new(), |c| format!("{}{}", c, chars.as_str().to_lowercase()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn color(ui: &mut egui::Ui, scene: &Scene, node: &str, key: &str, out: &mut serde_json::Map<String, Value>) {
    let mut rgb: [u8; 3] = std::array::from_fn(|i| {
        let component = ["x", "y", "z"].get(i).copied().unwrap_or("x");
        (value(scene, node, &format!("{key}.{component}")).as_f64().unwrap_or(0.0) * 255.0).round().clamp(0.0, 255.0) as u8
    });
    let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.allocate_ui(egui::vec2((ui.available_width() - 35.0).max(40.0), 24.0), |ui| {
            if let Some(v) = string(ui, key, &hex, false)
                && let Some(v) = v.as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches('#'), 16).ok())
            {
                rgb = [((v >> 16) & 255) as u8, ((v >> 8) & 255) as u8, (v & 255) as u8];
                changed = true;
            }
        });
        changed |= ui.color_edit_button_srgb(&mut rgb).changed();
    });
    if changed {
        for (k, c) in ["x", "y", "z"].into_iter().zip(rgb) {
            out.insert(format!("{key}.{k}"), json!(c as f64 / 255.0));
        }
    }
}
// Pointer motion and painting do not alter the authored GUI schema. Parse it only
// when its source or native text changes, rather than on every panel repaint.
#[derive(Clone)]
struct PropertyScene {
    source: String,
    text: Vec<(u64, String)>,
    scene: std::sync::Arc<Scene>,
}
fn property_scene(app: &PhotocraftApp, ctx: &egui::Context) -> Option<std::sync::Arc<Scene>> {
    let doc = &app.session.active()?.doc;
    let link = doc.defold.as_ref()?;
    let text: Vec<_> = link
        .nodes
        .iter()
        .filter_map(|b| match &doc.layer(photocraft_doc::LayerId(b.artwork))?.content {
            photocraft_doc::LayerContent::Text(t) => Some((b.artwork, t.text.clone())),
            _ => None,
        })
        .collect();
    let key = egui::Id::new(("defold-property-scene", doc.id.0));
    if let Some(c) = ctx.data(|d| d.get_temp::<PropertyScene>(key))
        && c.source == link.source
        && c.text == text
    {
        return Some(c.scene);
    }
    let scene = std::sync::Arc::new(photocraft_engine::defold_design::scene(&app.session).ok()?);
    ctx.data_mut(|d| d.insert_temp(key, PropertyScene { source: link.source.clone(), text, scene: scene.clone() }));
    Some(scene)
}
pub fn properties(app: &mut PhotocraftApp, ui: &mut egui::Ui) -> bool {
    let Some(binding) = photocraft_engine::defold_design::selected(&app.session).cloned() else { return false };
    let Some(scene) = property_scene(app, ui.ctx()) else { return false };
    let Some(node) = scene.nodes.iter().find(|n| n.id == binding.node) else { return false };
    let mut updates = serde_json::Map::new();
    let schema = photocraft_defold_format::properties(&node.kind);
    let mut rows = vec![
        ("id", tl!("Id")),
        ("position", tl!("Position")),
        ("rotation", tl!("Rotation")),
        ("scale", tl!("Scale")),
        ("size", tl!("Size")),
        ("enabled", tl!("Enabled")),
        ("visible", tl!("Visible")),
    ];
    if node.kind == "TYPE_TEXT" {
        rows.extend([("text", tl!("Text")), ("line_break", tl!("Line break")), ("font", tl!("Font"))]);
    }
    if matches!(node.kind.as_str(), "TYPE_BOX" | "TYPE_PIE") {
        rows.push(("texture", tl!("Texture")));
    }
    rows.extend([("material", tl!("Material")), ("color", tl!("Color")), ("alpha", tl!("Opacity")), ("inherit_alpha", tl!("Inherit alpha"))]);
    if node.kind == "TYPE_TEXT" {
        rows.extend([
            ("text_leading", tl!("Text leading")),
            ("text_tracking", tl!("Text tracking")),
            ("outline", tl!("Outline")),
            ("outline_alpha", tl!("Outline opacity")),
            ("shadow", tl!("Shadow")),
            ("shadow_alpha", tl!("Shadow opacity")),
        ]);
    }
    rows.extend([
        ("layer", tl!("Layer")),
        ("blend_mode", tl!("Blend Mode")),
        ("pivot", tl!("Pivot")),
        ("xanchor", tl!("X anchor")),
        ("yanchor", tl!("Y anchor")),
        ("adjust_mode", tl!("Adjust mode")),
    ]);
    for (key, label) in [
        ("size_mode", tl!("Size mode")),
        ("slice9", tl!("Nine slice")),
        ("clipping_mode", tl!("Clipping mode")),
        ("clipping_visible", tl!("Clipping visible")),
        ("clipping_inverted", tl!("Clipping inverted")),
        ("outerBounds", tl!("Outer bounds")),
        ("innerRadius", tl!("Inner radius")),
        ("perimeterVertices", tl!("Perimeter vertices")),
        ("pieFillAngle", tl!("Fill angle")),
        ("particlefx", tl!("Particle FX")),
        ("template", tl!("Template")),
    ] {
        if schema.iter().any(|p| p.path == key || p.path.starts_with(&format!("{key}."))) {
            rows.push((key, label));
        }
    }
    ui.push_id(("defold-native-properties", binding.layer), |ui| {
        egui::Grid::new("fields").num_columns(2).spacing(egui::vec2(8.0, 6.0)).show(ui, |ui| {
            for (key, label) in rows {
                ui.label(label);
                ui.push_id(key, |ui| {
                    let width = (ui.available_width()).max(120.0);
                    ui.allocate_ui(egui::vec2(width, 22.0), |ui| {
                        if matches!(key, "position" | "rotation" | "scale" | "size" | "slice9") {
                            ui.horizontal(|ui| {
                                let components = if key == "slice9" { "xyzw" } else { "xyz" };
                                let w = ((width - 18.0 * components.len() as f32) / components.len() as f32 - 4.0).max(28.0);
                                for c in components.chars() {
                                    ui.label(c.to_ascii_uppercase().to_string());
                                    let path = format!("{key}.{c}");
                                    let min = if key == "size" || key == "slice9" { 0.0 } else { -1_000_000.0 };
                                    if let Some(v) = number(ui, &path, value(&scene, &node.id, &path).as_f64().unwrap_or(0.0), w, min..=1_000_000.0) {
                                        updates.insert(path, v);
                                    }
                                }
                            });
                            return;
                        }
                        if matches!(key, "color" | "outline" | "shadow") {
                            color(ui, &scene, &node.id, key, &mut updates);
                            return;
                        }
                        if key == "id" {
                            if let Some(v) = string(ui, key, &node.id, false) {
                                updates.insert(key.into(), v);
                            }
                            return;
                        }
                        let v = value(&scene, &node.id, key);
                        if let Some(p) = schema.iter().find(|p| p.path == key)
                            && let PropertyKind::Enum(choices) = &p.kind
                        {
                            egui::ComboBox::from_id_salt(key).width(width).selected_text(enum_label(v.as_str().unwrap_or_default())).show_ui(ui, |ui| {
                                for choice in *choices {
                                    if ui.selectable_label(v.as_str() == Some(*choice), enum_label(choice)).clicked() {
                                        updates.insert(key.into(), json!(choice));
                                    }
                                }
                            });
                            return;
                        }
                        if matches!(key, "font" | "material" | "texture" | "layer") {
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt(key).width((width - 30.0).max(60.0)).selected_text(v.as_str().unwrap_or_default()).show_ui(
                                    ui,
                                    |ui| {
                                        for choice in photocraft_engine::defold_design::resource_choices(&app.session, key).unwrap_or_default() {
                                            if ui.selectable_label(v.as_str() == Some(choice.as_str()), if choice.is_empty() { "—" } else { &choice }).clicked()
                                            {
                                                updates.insert(key.into(), json!(choice));
                                            }
                                        }
                                    },
                                );
                                let path = property_resource(&scene, &binding.node, key);
                                if ui
                                    .add_enabled(
                                        path.is_some() && app.session.active_job().is_none() && app.session.is_enabled("defold.revealProjectResource"),
                                        egui::Button::new("↗").small(),
                                    )
                                    .on_hover_text(tl!("Reveal in Defold"))
                                    .clicked()
                                    && let Some(path) = path
                                    && let Some(opening) = app.session.active().map(|state| state.open_id)
                                {
                                    run_asset(app, ui, opening, "defold.revealProjectResource", json!({"path":path}));
                                }
                            });
                            return;
                        }
                        match v {
                            Value::Bool(mut b) => {
                                if ui.checkbox(&mut b, "").changed() {
                                    updates.insert(key.into(), json!(b));
                                }
                            }
                            Value::Number(n) => {
                                let base = n.as_f64().unwrap_or(0.0);
                                ui.horizontal(|ui| {
                                    if let Some(v) = number(
                                        ui,
                                        key,
                                        base,
                                        if key.ends_with("alpha") { 54.0 } else { width },
                                        if key.ends_with("alpha") { 0.0..=1.0 } else { -1_000_000.0..=1_000_000.0 },
                                    ) {
                                        updates.insert(key.into(), v);
                                    }
                                    if key.ends_with("alpha") {
                                        let mut alpha = base as f32;
                                        if widgets::slider(ui, &mut alpha, 0.0..=1.0, None).changed() {
                                            updates.insert(key.into(), json!(alpha));
                                        }
                                    }
                                });
                            }
                            _ => {
                                if let Some(v) = string(ui, key, v.as_str().unwrap_or_default(), key == "text") {
                                    updates.insert(key.into(), v);
                                }
                            }
                        }
                    });
                });
                ui.end_row();
            }
        });
    });
    if !updates.is_empty() {
        let _ = app.run("defold.setDesignProperties", json!({"values":updates}));
    }
    true
}

#[cfg(test)]
mod project_sync_tests {
    use super::*;
    #[test]
    fn only_confirmed_receipt_is_reported_as_published() {
        let committed = publication_result(&json!({"publication":{"phase":"COMMITTED"},"capturedNativeRevision":8}));
        assert_eq!(committed.phase.as_deref(), Some("COMMITTED"));
        assert!(!committed.error);
        assert_eq!(committed.revision, Some(8));
        for phase in ["PREPARED", "APPLYING", "APPLIED_UNSAVED", "SAVING", "RECOVERY_REQUIRED", "ABORTED"] {
            let pending = publication_result(&json!({"publication":{"phase":phase}}));
            assert_ne!(pending.message, committed.message, "{phase}");
            assert_eq!(pending.error, matches!(phase, "RECOVERY_REQUIRED" | "ABORTED"));
        }
    }
    #[test]
    fn dock_repaint_needs_no_project_files_and_preserves_native_document() {
        use photocraft_doc::{ColorMode, Document, SampleType, Size, defold::Design};
        let mut document = Document::new("GUI", Size::new(96, 96), ColorMode::Rgb, SampleType::U8);
        document.defold = Some(Design {
            version: 1,
            root: "X:/does-not-exist".into(),
            gui: "X:/does-not-exist/main.gui".into(),
            source: String::new(),
            disk_hash: String::new(),
            nodes: vec![],
            remote: None,
            canvas_origin: [0; 2],
            project: None,
        });
        let mut session = photocraft_engine::Session::new();
        session.add_document(document, None);
        let mut app = PhotocraftApp::new(session, Default::default());
        let before = app.session.active().unwrap().doc.clone();
        let revision = app.session.active().unwrap().revision;
        let context = egui::Context::default();
        PhotocraftApp::setup_context(&context, Default::default());
        let mut output = context
            .run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 700.0))), ..Default::default() }, |ui| {
                sync_panel(&mut app, ui)
            });
        assert!(!output.shapes.is_empty());
        assert_eq!(&*app.session.active().unwrap().doc, &*before);
        assert_eq!(app.session.active().unwrap().revision, revision);
        assert!(!app.session.has_jobs());
        output.textures_delta.clear();
    }
    #[test]
    fn publication_status_is_scoped_to_each_opening() {
        let context = egui::Context::default();
        let first = status_id(1);
        let second = status_id(2);
        context.data_mut(|data| data.insert_temp(first, publication_result(&json!({"publication":{"phase":"COMMITTED"}}))));
        assert!(context.data(|data| data.get_temp::<PublicationStatus>(first)).is_some());
        assert!(context.data(|data| data.get_temp::<PublicationStatus>(second)).is_none());
    }

    #[test]
    fn large_asset_reports_are_serialized_once_and_review_clones_share_storage() {
        let context = egui::Context::default();
        let reads = std::cell::Cell::new(0);
        let mut first = None;
        for _ in 0..30 {
            let report = asset_snapshot(&context, 1, None, || {
                reads.set(reads.get() + 1);
                json!({"available":true,"snapshot":{"registry":{"assets":(0..5000).map(|i|(i.to_string(),json!({"displayName":format!("Image {i}"),"state":"active"}))).collect::<serde_json::Map<_,_>>()}}})
            });
            if let Some(first) = &first {
                assert!(std::sync::Arc::ptr_eq(first, &report));
            } else {
                first = Some(report);
            }
        }
        assert_eq!(reads.get(), 1);
        asset_snapshot(&context, 2, None, || {
            reads.set(reads.get() + 1);
            json!({})
        });
        assert_eq!(reads.get(), 2);
        context.data_mut(|data| data.remove::<AssetSnapshot>(asset_snapshot_id(1)));
        asset_snapshot(&context, 1, None, || {
            reads.set(reads.get() + 1);
            json!({})
        });
        assert_eq!(reads.get(), 3);
        let review = review_result(&json!({"items":(0..512).map(|_|json!({"local":"painted node"})).collect::<Vec<_>>()}));
        assert!(std::sync::Arc::ptr_eq(review.report.as_ref().unwrap(), review.clone().report.as_ref().unwrap()));
    }
    #[test]
    fn review_controls_require_fresh_resolved_unblocked_report() {
        let mut report = json!({"blocked":false,"capturedNativeRevision":8,"items":[{"index":0,"kind":"conflict","canKeepMine":true,"canTakeTheirs":false}]});
        let mut choices = std::collections::BTreeMap::new();
        assert!(!review_ready(&report, 8, &choices));
        choices.insert("0".into(), "theirs".into());
        assert!(!review_ready(&report, 8, &choices));
        choices.insert("0".into(), "mine".into());
        assert!(review_ready(&report, 8, &choices));
        assert!(!review_ready(&report, 9, &choices));
        report["blocked"] = json!(true);
        assert!(!review_ready(&report, 8, &choices));
    }

    #[test]
    #[ignore = "Offscreen shared publication consent visual acceptance"]
    fn render_shared_publication_review_preserves_cached_scope() {
        use egui_kittest::kittest::Queryable;
        let mut session = photocraft_engine::Session::new();
        session.add_document(
            photocraft_doc::Document::new("GUI", photocraft_doc::Size::new(96, 96), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8),
            None,
        );
        let opening = session.active().unwrap().open_id;
        let revision = session.active().unwrap().revision;
        let app = PhotocraftApp::new(session, Default::default());
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(360.0, 700.0)).wgpu().build_ui_state(
            move |ui, app| {
                if !ready {
                    PhotocraftApp::setup_context(ui.ctx(), Default::default());
                    ready = true;
                    return;
                }
                shared_panel(app, ui, opening, false);
            },
            app,
        );
        let report = std::sync::Arc::new(
            json!({"openId":opening,"capturedNativeRevision":revision,"sharedReviewId":"review-fixture","requiresRelatedSourceApproval":true,
            "targets":[{"path":"/gui/shop.gui","sourceRevision":"7c17-owner-saved-version","primary":true,"unpublishedSource":false},
                {"path":"/gui/reward_window.gui","sourceRevision":"f938-related-saved-version","primary":false,"unpublishedSource":true}]}),
        );
        harness.ctx.data_mut(|data| data.insert_temp(shared_review_id(opening), report.clone()));
        harness.run_steps(3);
        harness.get_by_label(crate::i18n::t("Shared artwork")).click();
        harness.run_steps(5);
        let cached = harness.ctx.data(|data| data.get_temp::<std::sync::Arc<Value>>(shared_review_id(opening))).unwrap();
        assert!(std::sync::Arc::ptr_eq(&report, &cached));
        assert!(!harness.state().session.has_jobs());
        let folder = std::path::PathBuf::from(std::env::var("PHOTOCRAFT_SYNC_REVIEW_DIR").unwrap_or_else(|_| "target/sync-panel-review".into()));
        std::fs::create_dir_all(&folder).unwrap();
        harness.render().unwrap().save(folder.join("defold-shared-publication-review.png")).unwrap();
    }

    #[test]
    #[ignore = "Offscreen three-way review visual acceptance"]
    fn render_project_review_panel() {
        use egui_kittest::kittest::Queryable;
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(360.0, 650.0)).wgpu().build_ui_state(
            move |ui, app| {
                if !ready {
                    PhotocraftApp::setup_context(ui.ctx(), Default::default());
                    ready = true;
                    return;
                }
                review_panel(app, ui, 42, 8, false);
            },
            app,
        );
        harness.ctx.data_mut(|data|data.insert_temp(review_id(42),review_result(&json!({"reviewId":"fixture","blocked":false,"capturedNativeRevision":8,"items":[
            {"index":0,"node":"Reward","property":"position.x","base":48,"local":58,"external":68,"kind":"conflict","canKeepMine":true,"canTakeTheirs":true,"preserveArtwork":true},
            {"index":1,"node":"root","property":"enabled","base":true,"local":true,"external":false,"kind":"externalOnly","canTakeTheirs":true}
        ]}))));
        harness.run_steps(3);
        harness.get_by_label(crate::i18n::t("Review Defold changes")).click();
        harness.run_steps(5);
        let folder = std::path::PathBuf::from(std::env::var("PHOTOCRAFT_SYNC_REVIEW_DIR").unwrap_or_else(|_| "target/sync-panel-review".into()));
        std::fs::create_dir_all(&folder).unwrap();
        harness.render().unwrap().save(folder.join("defold-three-way-review.png")).unwrap();
    }
    #[test]
    #[ignore = "Offscreen project sync panel visual review"]
    fn render_project_sync_panel() {
        use photocraft_doc::{ColorMode, Document, SampleType, Size, defold::Design};
        let mut document = Document::new("GUI", Size::new(96, 96), ColorMode::Rgb, SampleType::U8);
        document.defold = Some(Design {
            version: 1,
            root: "X:/Game/Project".into(),
            gui: "X:/Game/Project/ui/window_character.gui".into(),
            source: String::new(),
            disk_hash: String::new(),
            nodes: vec![],
            remote: None,
            canvas_origin: [0; 2],
            project: None,
        });
        let mut session = photocraft_engine::Session::new();
        session.add_document(document, None);
        let app = PhotocraftApp::new(session, Default::default());
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(360.0, 700.0)).wgpu().build_ui_state(
            move |ui, app| {
                if !ready {
                    PhotocraftApp::setup_context(ui.ctx(), Default::default());
                    ready = true;
                    return;
                }
                sync_panel(app, ui);
            },
            app,
        );
        let open_id = harness.state().session.active().unwrap().open_id;
        let folder = std::path::PathBuf::from(std::env::var("PHOTOCRAFT_SYNC_REVIEW_DIR").unwrap_or_else(|_| "target/sync-panel-review".into()));
        std::fs::create_dir_all(&folder).unwrap();
        for phase in ["COMMITTED", "APPLYING", "RECOVERY_REQUIRED"] {
            harness.ctx.data_mut(|data| {
                data.insert_temp(
                    status_id(open_id),
                    publication_result(
                        &json!({"publication":{"phase":phase,"transactionId":"ec3796a7-cb2b-4e68-885b-d96cdf14fe3d"},"capturedNativeRevision":0}),
                    ),
                )
            });
            harness.run_steps(3);
            harness.render().unwrap().save(folder.join(format!("defold-sync-{phase}.png"))).unwrap();
        }
    }
}
