//! Compact connection and model controls shared by docked and floating AI settings.

use crate::{PhotocraftApp, icons, theme};

pub(crate) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let before = settings_values(app);
    let tokens = theme::Tokens::get(ui.ctx());
    let label_width = [tl!("Provider"), tl!("Model")]
        .into_iter()
        .map(|text| ui.painter().layout_no_wrap(text.into(), egui::TextStyle::Body.resolve(ui.style()), tokens.text).size().x)
        .fold(0.0_f32, f32::max);
    ui.add_enabled_ui(!app.ai_runtime.running(), |ui| {
        ui.horizontal(|ui| {
            let label = ui.add_sized([label_width, ui.spacing().interact_size.y], egui::Label::new(tl!("Provider")));
            if app.ui.ai.provider == "local-test" {
                ui.label(tl!("Local Test Provider"));
            } else {
                egui::ComboBox::from_id_salt("ai-provider")
                    .selected_text(if app.ui.ai.provider == "codex" { "Codex" } else { "Images API" })
                    .width(ui.available_width())
                    .truncate()
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.ui.ai.provider, "codex".into(), "Codex");
                        ui.selectable_value(&mut app.ui.ai.provider, "images-api".into(), "Images API");
                    })
                    .response
                    .labelled_by(label.id);
            }
        });
        if app.ui.ai.provider == "local-test" {
            return;
        }
        ui.horizontal(|ui| {
            let label = ui.add_sized([label_width, ui.spacing().interact_size.y], egui::Label::new(tl!("Model")));
            if app.ui.ai.provider == "codex" {
                model_picker(app, ui, label.id);
            } else {
                ui.add(egui::TextEdit::singleline(&mut app.ui.ai.model).desired_width(ui.available_width())).labelled_by(label.id);
            }
        });
        egui::CollapsingHeader::new(tl!("Connection settings")).id_salt("ai-settings-connection").default_open(false).show(ui, |ui| {
            if app.ui.ai.provider == "images-api" {
                let label = ui.label(tl!("Base URL"));
                if ui.add(egui::TextEdit::singleline(&mut app.ui.ai.base_url).desired_width(f32::INFINITY)).labelled_by(label.id).changed() {
                    app.ai_runtime.api_key.clear();
                }
                let label = ui.label(tl!("API key (session only)"));
                ui.add(egui::TextEdit::singleline(&mut app.ai_runtime.api_key).password(true).desired_width(f32::INFINITY)).labelled_by(label.id);
            } else {
                let label = ui.label(tl!("Codex executable (optional)"));
                ui.add(egui::TextEdit::singleline(&mut app.ui.ai.codex_path).desired_width(f32::INFINITY)).labelled_by(label.id);
                reasoning_picker(app, ui);
            }
        });
    });
    if app.ui.ai.provider == "codex" && !app.ai_runtime.models_error.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(&app.ai_runtime.models_error).color(tokens.warning)).wrap());
    }
    if before != settings_values(app)
        && let Err(error) = crate::ai_service::save_settings(app)
    {
        app.ui.ai.status = error;
    }
}

fn settings_values(app: &PhotocraftApp) -> [String; 6] {
    [
        app.ui.ai.provider.clone(),
        app.ui.ai.base_url.clone(),
        app.ui.ai.model.clone(),
        app.ui.ai.codex_path.clone(),
        app.ui.ai.codex_model.clone(),
        app.ui.ai.codex_reasoning_effort.clone(),
    ]
}

fn model_picker(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: egui::Id) {
    let before = app.ui.ai.codex_model.clone();
    let selected = if before.is_empty() {
        let automatic = tl!("Automatic");
        crate::ai_service::selected_model(app).map_or_else(|| automatic.to_owned(), |model| format!("{automatic} ({})", model.name))
    } else {
        app.ai_runtime.models.iter().find(|model| model.id == before).map_or_else(|| before.clone(), |model| model.name.clone())
    };
    let width = (ui.available_width() - 24.0 - ui.spacing().item_spacing.x).max(40.0);
    egui::ComboBox::from_id_salt("ai-codex-model")
        .selected_text(&selected)
        .width(width)
        .truncate()
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut app.ui.ai.codex_model, String::new(), tl!("Automatic"));
            for model in &app.ai_runtime.models {
                let name = if model.hidden { format!("{} ({})", model.name, tl!("Hidden")) } else { model.name.clone() };
                ui.selectable_value(&mut app.ui.ai.codex_model, model.id.clone(), name).on_hover_text(&model.id);
            }
        })
        .response
        .labelled_by(label)
        .on_hover_text(selected);
    if app.ai_runtime.models_loading() {
        ui.add_sized([24.0, 24.0], egui::Spinner::new().size(16.0));
    } else if icons::button(ui, "rotate-cw", 24.0, false, "Refresh models").clicked() {
        crate::ai_service::refresh_models(app);
    }
    if before != app.ui.ai.codex_model {
        crate::ai_service::normalize_reasoning(app);
    }
}

fn reasoning_label(effort: &str) -> &str {
    match effort {
        "none" => tl!("None"),
        "minimal" => tl!("Minimal"),
        "low" => tl!("Low"),
        "medium" => tl!("Medium"),
        "high" => tl!("High"),
        "xhigh" => tl!("Extra high"),
        "max" => tl!("Maximum"),
        "ultra" => tl!("Ultra"),
        _ => effort,
    }
}

fn reasoning_picker(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let model = crate::ai_service::selected_model(app);
    let efforts = model.map(|model| model.supported_reasoning_efforts.clone()).unwrap_or_default();
    let default = model.map(|model| model.default_reasoning_effort.as_str()).unwrap_or_default();
    let automatic =
        if efforts.iter().any(|effort| effort == default) { format!("{} ({})", tl!("Automatic"), reasoning_label(default)) } else { tl!("Automatic").into() };
    let selected = if app.ui.ai.codex_reasoning_effort.is_empty() { automatic.clone() } else { reasoning_label(&app.ui.ai.codex_reasoning_effort).to_owned() };
    let label = ui.label(tl!("Reasoning effort"));
    ui.add_enabled_ui(!app.ai_runtime.models_loading(), |ui| {
        egui::ComboBox::from_id_salt("ai-codex-reasoning")
            .selected_text(&selected)
            .width(ui.available_width())
            .truncate()
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.ui.ai.codex_reasoning_effort, String::new(), automatic);
                for effort in &efforts {
                    ui.selectable_value(&mut app.ui.ai.codex_reasoning_effort, effort.clone(), reasoning_label(effort));
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(selected);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};
    use photocraft_engine::Session;

    fn setup(ui: &mut egui::Ui, ready: &mut bool) -> bool {
        if *ready {
            return true;
        }
        egui_extras::install_image_loaders(ui.ctx());
        theme::install_fonts(ui.ctx());
        theme::apply(ui.ctx(), theme::ThemeKind::default());
        *ready = true;
        false
    }

    fn model(id: &str, name: &str, efforts: &[&str], default: &str) -> crate::ai_service::CodexModel {
        crate::ai_service::CodexModel {
            id: id.into(),
            name: name.into(),
            is_default: id == "fast",
            hidden: false,
            supported_reasoning_efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
            default_reasoning_effort: default.into(),
        }
    }

    #[test]
    fn rail_flyout_grows_for_connection_controls_and_shrinks_when_collapsed() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for width in [360.0, 800.0] {
            let mut app = PhotocraftApp::new(Session::new(), Default::default());
            app.ui.panels.ai_settings = true;
            app.ai_runtime.models = vec![model("fast", "Example model", &["high", "xhigh"], "high")];
            let mut ready = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, 480.0)).with_max_steps(64).build_ui_state(
                move |ui, app| {
                    if setup(ui, &mut ready) {
                        crate::panels::ai_settings_flyout(app, ui, None);
                    }
                },
                app,
            );
            harness.run();
            let area_id = egui::Id::new("ai-settings-rail-flyout");
            let collapsed = egui::containers::AreaState::load(&harness.ctx, area_id).unwrap().rect();
            harness.get_by_label("Connection settings").click();
            harness.run();
            let expanded = egui::containers::AreaState::load(&harness.ctx, area_id).unwrap().rect();
            assert!(expanded.height() > collapsed.height() + 40.0, "the remembered collapsed area height must not cap its scroll body");
            assert!(expanded.right() <= width && expanded.bottom() <= 480.0);
            let reasoning = harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Reasoning effort");
            assert!(expanded.contains(reasoning.rect().center()), "the reasoning control must be inside the visible flyout");
            reasoning.click();
            harness.run();
            harness.get_by_label("Extra high").click();
            harness.run();
            assert_eq!(harness.state().ui.ai.codex_reasoning_effort, "xhigh");
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_SETTINGS_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("settings-flyout-expanded-{width}.png"))).unwrap();
            }
            harness.get_by_label("Connection settings").click();
            harness.run();
            let restored = egui::containers::AreaState::load(&harness.ctx, area_id).unwrap().rect();
            assert!((restored.height() - collapsed.height()).abs() < 1.0, "collapsing returns the model panel to its compact height");
        }
    }

    #[test]
    fn compact_settings_keep_model_visible_and_reasoning_available_without_exposing_secrets() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for width in [250.0, 360.0] {
            let saved = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
            let settings = saved.clone();
            let services = crate::Services {
                save_ai_settings: Some(Box::new(move |text| {
                    *settings.borrow_mut() = text.into();
                    Ok(())
                })),
                ..Default::default()
            };
            let mut app = PhotocraftApp::new(Session::new(), services);
            app.ui.ai.provider = "codex".into();
            app.ui.ai.prompt = "private prompt".into();
            app.ai_runtime.api_key = "private key".into();
            app.ai_runtime.models =
                vec![model("fast", "Example Fast", &["low", "medium"], "medium"), model("precise", "Example Precise", &["high", "xhigh"], "high")];
            let mut ready = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, 420.0)).build_ui_state(
                move |ui, app| {
                    if setup(ui, &mut ready) {
                        show(app, ui);
                    }
                },
                app,
            );
            harness.run_steps(4);
            assert!(harness.query_by_label("Codex executable (optional)").is_none());
            assert!(harness.query_by_label("Reasoning effort").is_none());
            for label in ["Provider", "Model"] {
                let field = harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, label);
                assert!(field.rect().left() >= 0.0 && field.rect().right() <= width, "{label} fits the compact panel");
            }
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_SETTINGS_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("settings-collapsed-{width}.png"))).unwrap();
            }
            harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Model").click();
            harness.run_steps(3);
            harness.get_by_label("Example Precise").click();
            harness.run_steps(3);
            assert_eq!(harness.state().ui.ai.codex_model, "precise");
            harness.get_by_label("Connection settings").click();
            harness.run_steps(4);
            let reasoning = harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Reasoning effort");
            assert_eq!(reasoning.value().as_deref(), Some("Automatic (High)"));
            reasoning.click();
            harness.run_steps(3);
            assert!(harness.query_by_label("Low").is_none());
            harness.get_by_label("Extra high").click();
            harness.run_steps(3);
            let settings: serde_json::Value = serde_json::from_str(&saved.borrow()).unwrap();
            assert_eq!(settings["codexModel"], "precise");
            assert_eq!(settings["codexReasoningEffort"], "xhigh");
            assert!(!saved.borrow().contains("private"));
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_SETTINGS_OUT") {
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("settings-expanded-{width}.png"))).unwrap();
            }
            harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Model").click();
            harness.run_steps(3);
            harness.get_by_label("Example Fast").click();
            harness.run_steps(3);
            assert!(harness.state().ui.ai.codex_reasoning_effort.is_empty(), "unsupported effort resets when the model changes");
        }
    }

    #[test]
    fn provider_fields_persist_and_endpoint_changes_clear_the_session_key() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        let saved = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let settings = saved.clone();
        let services = crate::Services {
            save_ai_settings: Some(Box::new(move |text| {
                *settings.borrow_mut() = text.into();
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Session::new(), services);
        app.ui.ai.base_url.clear();
        app.ui.ai.model.clear();
        app.ai_runtime.api_key = "private key".into();
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(280.0, 420.0)).build_ui_state(
            move |ui, app| {
                if setup(ui, &mut ready) {
                    show(app, ui);
                }
            },
            app,
        );
        harness.run_steps(4);
        harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Provider").click();
        harness.run_steps(3);
        harness.get_by_label("Images API").click();
        harness.run_steps(3);
        harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Model").click();
        harness.run_steps(2);
        harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Model").type_text("custom-image-model");
        harness.run_steps(3);
        harness.get_by_label("Connection settings").click();
        harness.run_steps(4);
        harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Base URL").click();
        harness.run_steps(2);
        harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Base URL").type_text("https://images.example/v1");
        harness.run_steps(3);
        assert!(harness.state().ai_runtime.api_key.is_empty());
        let settings: serde_json::Value = serde_json::from_str(&saved.borrow()).unwrap();
        assert_eq!(settings["provider"], "images-api");
        assert_eq!(settings["model"], "custom-image-model");
        assert_eq!(settings["baseUrl"], "https://images.example/v1");
        assert!(!saved.borrow().contains("private"));
    }

    #[test]
    fn active_generation_locks_runtime_configuration_and_keeps_model_errors_visible() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        let (_tx, rx) = std::sync::mpsc::channel();
        let rx = std::cell::RefCell::new(Some(rx));
        let services = crate::Services {
            ai_generate: Some(Box::new(move |_| Ok(crate::ai_service::Job { cancel: Default::default(), result: rx.borrow_mut().take().unwrap() }))),
            ..Default::default()
        };
        let mut session = Session::new();
        session.execute("file.new", serde_json::json!({"width": 32, "height": 24})).unwrap();
        let mut app = PhotocraftApp::new(session, services);
        app.ui.ai.prompt = "Test only".into();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Repaint;
        crate::ai_service::start(&mut app).unwrap();
        app.ai_runtime.models_error = "Test model discovery error".into();
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(280.0, 300.0)).build_ui_state(
            move |ui, app| {
                if setup(ui, &mut ready) {
                    show(app, ui);
                }
            },
            app,
        );
        harness.run_steps(4);
        for label in ["Provider", "Model"] {
            assert!(harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, label).accesskit_node().is_disabled());
        }
        assert!(harness.get_by_label("Refresh models").accesskit_node().is_disabled());
        assert!(harness.query_by_label("Test model discovery error").is_some());
        crate::ai_service::cancel(harness.state_mut());
        harness.run_steps(3);
        assert!(!harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Provider").accesskit_node().is_disabled());
    }
}
