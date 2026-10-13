//! Native candidate review. All document changes go through engine commands.

use serde_json::json;

use crate::{PhotocraftApp, icons, theme, widgets};

pub(crate) fn show(app: &mut PhotocraftApp, host: &mut egui::Ui) {
    sync_history(app);
    crate::ai_project::sync_prompt(app);
    crate::ai_references::sync(app);
    let ctx = host.ctx().clone();
    let content = ctx.content_rect();
    let canvas = app.last_canvas_rect.intersect(content);
    let canvas = if canvas.is_positive() { canvas } else { content };
    let width = (canvas.width() - 24.0).clamp(180.0, 680.0).min(content.width());
    let tokens = theme::Tokens::get(&ctx);
    egui::Area::new(egui::Id::new("ai-floating-prompt"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(canvas.center().x, canvas.bottom() - 18.0))
        .constrain_to(canvas)
        .movable(false)
        .show(&ctx, |ui| {
            // Reset the remembered Area height so a longer draft can grow its viewport.
            ui.set_max_height((canvas.height() - 36.0).max(120.0));
            egui::Frame::popup(ui.style()).fill(tokens.dock).corner_radius(tokens.radius as u8).inner_margin(10.0).show(ui, |ui| {
                if !app.ui.ai.visible {
                    if icons::button(ui, "sparkles", 28.0, app.ui.ai.enabled, "Show AI Studio").clicked() {
                        app.ui.ai.visible = true;
                    }
                    return;
                }
                ui.set_width((width - 20.0).max(120.0));
                ui.spacing_mut().item_spacing.y = 8.0;
                composer_header(app, ui);
                prompt_input(app, ui, canvas.height());
                reference_strip(app, ui);
                composer_actions(app, ui);
                result_summary(app, ui);
            });
        });
    crate::ai_project::remember_prompt(app);
}

fn composer_header(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let compact = ui.available_width() < 440.0;
    ui.horizontal(|ui| {
        ai_switch(app, ui);
        if compact {
            let status = if app.ai_runtime.running() {
                "Generating"
            } else if !app.ui.ai.enabled {
                "AI disabled"
            } else {
                "Ready"
            };
            ui.label(egui::RichText::new(tl!(status)).color(theme::Tokens::get(ui.ctx()).text_dim));
        }
        if !compact {
            ui.add_space(8.0);
            mode_selector(app, ui);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, "minus", 28.0, false, "Collapse AI Studio").clicked() {
                app.ui.ai.visible = false;
            }
            let shown = crate::panel_docking::ai_settings_shown(app);
            if icons::button(ui, "sliders-horizontal", 28.0, shown, "Model settings").clicked()
                && let Err(error) = crate::panel_docking::toggle_ai_settings(app)
            {
                app.ui.ai.status = error;
            }
        });
    });
    if compact {
        ui.horizontal_wrapped(|ui| mode_selector(app, ui));
    }
}

fn ai_switch(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = theme::Tokens::get(ui.ctx());
    ui.label("AI");
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(36.0, 28.0), egui::Sense::click());
    if response.clicked() {
        app.ui.ai.enabled = !app.ui.ai.enabled;
        response.mark_changed();
        if let Err(error) = crate::ai_service::save_settings(app) {
            app.ui.ai.status = error;
        }
    }
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(36.0, 20.0));
    ui.painter().rect_filled(track, 10.0, if app.ui.ai.enabled { t.accent } else { t.field_border });
    let position = ui.ctx().animate_bool(response.id, app.ui.ai.enabled);
    let x = egui::lerp((track.left() + 10.0)..=(track.right() - 10.0), position);
    ui.painter().circle_filled(egui::pos2(x, track.center().y), 7.0, t.accent_text);
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), t.radius_sm, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Outside);
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, app.ui.ai.enabled, "AI"));
    response.on_hover_text(tl!("Enable AI"));
}

fn mode_selector(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    use photocraft_doc::ai::GenerationMode;
    let saved = app.session.active().and_then(|st| st.doc.ai_generations.iter().find(|g| Some(g.id) == app.ui.ai.history_group)).map(|g| g.mode);
    let mut mode = saved.unwrap_or(app.ui.ai.mode);
    let before = mode;
    ui.add_enabled_ui(app.ui.ai.enabled && !app.ai_runtime.running(), |ui| {
        let t = theme::Tokens::get(ui.ctx());
        egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.field_border)).corner_radius(t.radius_sm as u8).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (value, label) in [(GenerationMode::Repaint, "In-place edit"), (GenerationMode::Component, "Transparent component")] {
                    let selected = mode == value;
                    if ui
                        .add(
                            egui::Button::selectable(selected, tl!(label))
                                .fill(if selected { t.primary_bg } else { t.field })
                                .corner_radius(t.radius_sm as u8)
                                .min_size(egui::vec2(98.0, 28.0)),
                        )
                        .clicked()
                    {
                        mode = value;
                    }
                }
            });
        });
    });
    if before != mode {
        // A mode belongs to its generation record. Switching modes starts a new generation,
        // retaining the draft instead of silently retrying the old record in its old mode.
        app.ui.ai.history_group = None;
        app.ui.ai.mode = mode;
        if let Err(error) = crate::ai_service::save_settings(app) {
            app.ui.ai.status = error;
        }
    }
}

fn prompt_input(app: &mut PhotocraftApp, ui: &mut egui::Ui, canvas_height: f32) {
    let t = theme::Tokens::get(ui.ctx());
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text_height = ui.fonts_mut(|fonts| fonts.layout(app.ui.ai.prompt.clone(), font, t.text, (ui.available_width() - 18.0).max(80.0))).size().y;
    let height = (text_height + 8.0).clamp(56.0, (canvas_height * 0.24).clamp(56.0, 126.0));
    egui::Frame::NONE.fill(t.field).stroke(egui::Stroke::new(1.0, t.field_border)).corner_radius(t.radius_sm as u8).inner_margin(8.0).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        let label = ui.label(egui::RichText::new(tl!("Prompt")).small().color(t.text_dim));
        egui::ScrollArea::vertical().id_salt("ai-prompt-scroll").min_scrolled_height(height).max_height(height).show(ui, |ui| {
            let response = ui
                .add(
                    egui::TextEdit::multiline(&mut app.ui.ai.prompt)
                        .id_salt("ai-prompt-input")
                        .hint_text(tl!("Describe your edit"))
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3)
                        .char_limit(4096),
                )
                .labelled_by(label.id);
            app.ai_runtime.prompt_id = Some(response.id);
            response.widget_info(|| {
                let mut info = egui::WidgetInfo::text_edit(true, &app.ui.ai.prompt, &app.ui.ai.prompt, tl!("Describe your edit"));
                info.label = Some(tl!("Prompt").into());
                info
            });
        });
    });
}

fn composer_actions(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = theme::Tokens::get(ui.ctx());
    let compact = ui.available_width() < 440.0;
    ui.horizontal(|ui| {
        reference_button(app, ui, compact);
        let target = app
            .session
            .active()
            .map(|st| {
                let name = st.active_layer.and_then(|id| st.doc.layer(id)).map_or_else(|| tl!("None").to_owned(), |layer| layer.name.clone());
                format!("{}: {name}", tl!("Layer"))
            })
            .unwrap_or_else(|| tl!("New document").to_owned());
        let reserved = if compact { 112.0 } else { 177.0 };
        ui.allocate_ui_with_layout(egui::vec2((ui.available_width() - reserved).max(20.0), 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&target).color(t.text_dim)).truncate()).on_hover_text(&target);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            generate_button(app, ui);
            if !compact {
                let status = if app.ai_runtime.running() {
                    "Generating"
                } else if !app.ui.ai.enabled {
                    "AI disabled"
                } else {
                    "Ready"
                };
                let response = ui.add(egui::Button::new(tl!(status)).frame(false)).on_hover_text(tl!("Generation details"));
                egui::Popup::menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.set_max_width(420.0);
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| result_details(app, ui));
                });
            }
        });
    });
}

fn reference_button(app: &mut PhotocraftApp, ui: &mut egui::Ui, compact: bool) {
    ui.add_enabled_ui(!app.ai_runtime.running() && app.ai_runtime.reference_job.is_none(), |ui| {
        let response = if compact {
            icons::button(ui, "image", 28.0, !app.ai_runtime.references.is_empty(), "Reference images")
        } else {
            let t = theme::Tokens::get(ui.ctx());
            ui.add(egui::Button::image_and_text(icons::image("image", 16.0, t.icon), tl!("Reference images")).frame(false).min_size(egui::vec2(0.0, 28.0)))
        };
        egui::Popup::menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            if ui.button(tl!("Choose reference image")).clicked() {
                if let Err(error) = crate::ai_references::choose(app) {
                    app.ui.ai.status = error;
                }
                ui.close();
            }
            if ui.button(tl!("Paste reference image")).clicked() {
                match crate::ai_references::paste(app) {
                    Ok(true) => {}
                    Ok(false) => app.ui.ai.status = tl!("No image on the clipboard").into(),
                    Err(error) => app.ui.ai.status = error,
                }
                ui.close();
            }
            if app.ui.ai.history_group.is_none()
                && !(app.ui.ai.provider == "codex" && app.ui.ai.mode == photocraft_doc::ai::GenerationMode::Component)
                && app.session.active().is_some_and(|st| st.doc.selection.is_none())
            {
                ui.separator();
                widgets::checkbox(ui, &mut app.ui.ai.reference, "Use current layer as reference");
            }
            let legacy_component_retry = app.ui.ai.provider == "codex"
                && app.session.active().is_some_and(|st| {
                    st.doc
                        .ai_generations
                        .iter()
                        .any(|group| Some(group.id) == app.ui.ai.history_group && group.mode == photocraft_doc::ai::GenerationMode::Component)
                });
            if app.ui.ai.provider == "images-api" || legacy_component_retry {
                egui::CollapsingHeader::new(tl!("Advanced")).show(ui, |ui| mode_controls(app, ui));
            }
        });
    });
}

fn reference_strip(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.ai_runtime.references.is_empty() {
        return;
    }
    let enabled = !app.ai_runtime.running() && app.ai_runtime.reference_job.is_none();
    let mut remove = None;
    egui::ScrollArea::horizontal().id_salt("ai-references").max_height(66.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (index, reference) in app.ai_runtime.references.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    let name = format!("{}: {}", index + 1, reference.image.name);
                    let texture = reference.texture(ui.ctx());
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(64.0, 52.0), egui::Sense::hover());
                    let size = texture.size_vec2();
                    let scale = (rect.width() / size.x).min(rect.height() / size.y);
                    egui::Image::new(texture).paint_at(ui, egui::Rect::from_center_size(rect.center(), size * scale));
                    response.on_hover_text(name);
                    ui.add_enabled_ui(enabled, |ui| {
                        if icons::button(ui, "x", 22.0, false, "Remove reference image").clicked() {
                            remove = Some(reference.id);
                        }
                    });
                });
            }
        });
    });
    if let Some(id) = remove
        && let Err(error) = crate::ai_references::remove(app, id)
    {
        app.ui.ai.status = error;
    }
}

fn sync_history(app: &mut PhotocraftApp) {
    let Some(doc) = app.session.active().map(|st| st.doc.clone()) else {
        app.ui.ai.history_document = None;
        app.ui.ai.history_group = None;
        return;
    };
    if app.ui.ai.history_document != Some(doc.id.0) {
        app.ui.ai.history_document = Some(doc.id.0);
        app.ui.ai.history_group = doc.ai_generations.last().map(|g| g.id);
        if !app.ai_runtime.running() {
            app.ai_runtime.optimized_prompt.clear();
            if let Some(group) = doc.ai_generations.last()
                && let Some(record) =
                    group.versions.iter().rev().find(|v| v.layer.and_then(|id| doc.layer(id)).is_some_and(|l| l.visible)).or_else(|| group.versions.last())
            {
                load_record(app, record);
            }
        }
    }
    if app.ui.ai.history_group.is_some_and(|id| !doc.ai_generations.iter().any(|g| g.id == id)) {
        app.ui.ai.history_group = None;
    }
}

fn generate_button(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.ai_runtime.running() {
        ui.spinner();
        if widgets::secondary_button(ui, "Cancel", 68.0).clicked() {
            crate::ai_service::cancel(app);
        }
        return;
    }
    let local_test = app.ui.ai.provider == "local-test";
    let command = if local_test {
        "ai.prepareTestCandidate"
    } else if app.ui.ai.history_group.is_some() {
        "ai.retryGeneration"
    } else if app.ui.ai.provider == "codex" && app.ui.ai.mode == photocraft_doc::ai::GenerationMode::Component {
        "ai.assetBegin"
    } else {
        "ai.beginGeneration"
    };
    let reason = if app.session.active().is_some() { app.session.disabled_reason(command) } else { None };
    let label = if local_test {
        "Create Test Candidate"
    } else if app.ui.ai.history_group.is_some() {
        "Generate again"
    } else {
        "Generate image"
    };
    let response = ui
        .add_enabled_ui(
            app.ui.ai.enabled && (local_test || !app.ui.ai.prompt.trim().is_empty()) && app.ai_runtime.reference_job.is_none() && reason.is_none(),
            |ui| {
                let t = theme::Tokens::get(ui.ctx());
                ui.add(
                    egui::Button::new(egui::RichText::new(tl!(label)).color(t.primary_text))
                        .fill(t.primary_bg)
                        .corner_radius(t.radius_sm as u8)
                        .min_size(egui::vec2(100.0, 30.0)),
                )
            },
        )
        .inner;
    let clicked = response.clicked();
    if let Some(reason) = reason {
        response.on_hover_text(reason);
    }
    if clicked {
        if local_test {
            execute(app, command, json!({"prompt":app.ui.ai.prompt}));
        } else {
            let result = match app.ui.ai.history_group {
                Some(id) => crate::ai_service::retry(app, id),
                None => crate::ai_service::start(app),
            };
            if let Err(e) = result {
                app.ui.ai.status = e;
            }
        }
        app.ui.ai.preview = true;
    }
}

pub(crate) fn settings_content(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::ai_settings::show(app, ui);
}

fn result_summary(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let has_history = app.session.active().is_some_and(|st| !st.doc.ai_generations.is_empty());
    if app.ui.ai.status.is_empty()
        && app.ai_runtime.project_sync_error.is_empty()
        && !has_history
        && app.ai_runtime.saved_images.is_empty()
        && app.session.ai.candidate().is_none()
    {
        return;
    }
    let message = if !app.ai_runtime.project_sync_error.is_empty() { &app.ai_runtime.project_sync_error } else { &app.ui.ai.status };
    let first = message.lines().find(|line| !line.trim().is_empty()).unwrap_or(tl!("Generation details"));
    let mut summary = first.to_owned();
    if message.lines().count() > 1 {
        summary.push('…');
    }
    // Bound the text after reserving the icon and padding, including unbroken CJK text.
    let width = (ui.available_width() - 12.0 - ui.spacing().icon_spacing - 2.0 * ui.spacing().button_padding.x).max(1.0);
    let mut job = egui::text::LayoutJob::simple(summary, egui::TextStyle::Button.resolve(ui.style()), theme::Tokens::get(ui.ctx()).text, width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let summary = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let content = ui.ctx().content_rect();
    let details_width = ui.available_width().min(520.0);
    let t = theme::Tokens::get(ui.ctx());
    let response = ui.add(egui::Button::image_and_text(icons::image("chevron-down", 12.0, t.icon), summary).frame(false));
    // Details open above the bottom-anchored composer when space is tight. Keeping them
    // in a bounded popup prevents expansion from moving input or hiding action buttons.
    egui::Popup::menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_width(details_width);
        egui::ScrollArea::vertical()
            .id_salt("ai-results-scroll")
            .max_height((content.height() - 60.0).clamp(60.0, 300.0))
            .show(ui, |ui| result_details(app, ui));
    });
}

fn result_details(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if !app.ui.ai.status.is_empty() {
        ui.add(egui::Label::new(&app.ui.ai.status).wrap().selectable(true));
    }
    if !app.ai_runtime.project_sync_error.is_empty() {
        ui.colored_label(theme::Tokens::get(ui.ctx()).warning, &app.ai_runtime.project_sync_error);
    }
    history_controls(app, ui);
    for path in app.ai_runtime.saved_images.clone() {
        let label = if path.ends_with("design.png") { "Open intermediate design" } else { "Open transparent asset" };
        if ui.button(tl!(label)).clicked()
            && let Err(error) = app.open_path(&path)
        {
            app.ui.ai.status = error;
        }
    }
    if !app.ai_runtime.optimized_prompt.is_empty() {
        egui::CollapsingHeader::new(tl!("Optimized prompt")).show(ui, |ui| {
            ui.add(egui::Label::new(&app.ai_runtime.optimized_prompt).wrap().selectable(true));
        });
    }
    if let Some((id, current)) = app.session.ai.candidate().map(|candidate| (candidate.id, candidate.is_current(&app.session))) {
        ui.label(format!("{} #{id}", tl!("Candidate")));
        if current {
            widgets::checkbox(ui, &mut app.ui.ai.preview, "Preview");
        } else {
            ui.colored_label(theme::Tokens::get(ui.ctx()).warning, tl!("Document changed"));
        }
        ui.horizontal(|ui| {
            if ui.add_enabled_ui(current, |ui| widgets::primary_button(ui, "Apply", 68.0)).inner.clicked() {
                execute(app, "ai.applyCandidate", json!({"candidateId":id}));
            }
            if widgets::secondary_button(ui, "Discard", 68.0).clicked() {
                execute(app, "ai.discardCandidate", json!({"candidateId":id}));
            }
        });
    }
}

fn load_record(app: &mut PhotocraftApp, record: &photocraft_doc::ai::GenerationVersion) {
    app.ui.ai.prompt = record.prompt.clone();
    if ["codex", "images-api"].contains(&record.provider.as_str()) {
        app.ui.ai.provider = record.provider.clone();
        if record.provider == "codex" {
            app.ui.ai.codex_model = record.model.clone();
            crate::ai_service::normalize_reasoning(app);
        } else {
            app.ui.ai.model = record.model.clone();
        }
    }
    if !app.ai_runtime.running() {
        app.ai_runtime.optimized_prompt.clear();
    }
}

fn mode_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    use photocraft_doc::ai::GenerationMode;
    let saved = app.session.active().and_then(|st| st.doc.ai_generations.iter().find(|g| Some(g.id) == app.ui.ai.history_group)).map(|g| g.mode);
    let mode = saved.unwrap_or(app.ui.ai.mode);
    if mode == GenerationMode::Component {
        if app.ui.ai.provider == "codex" && saved.is_none() {
            return;
        }
        let mut batch_changed = false;
        ui.add_enabled_ui(!app.ai_runtime.running(), |ui| {
            batch_changed |= widgets::checkbox(ui, &mut app.ui.ai.batch_components, "Component set (1 image)").changed();
            if app.ui.ai.batch_components {
                ui.horizontal(|ui| {
                    if ui.button(tl!("Player preset")).clicked() {
                        let defaults = crate::state::AiPanelState::default();
                        app.ui.ai.component_buttons = defaults.component_buttons;
                        app.ui.ai.component_bars = defaults.component_bars;
                        batch_changed = true;
                    }
                });
                let label = ui.label(tl!("Buttons"));
                batch_changed |= ui
                    .add(egui::TextEdit::multiline(&mut app.ui.ai.component_buttons).desired_rows(4).desired_width(f32::INFINITY).char_limit(2048))
                    .labelled_by(label.id)
                    .changed();
                let label = ui.label(tl!("Tracks"));
                batch_changed |= ui
                    .add(egui::TextEdit::multiline(&mut app.ui.ai.component_bars).desired_rows(2).desired_width(f32::INFINITY).char_limit(2048))
                    .labelled_by(label.id)
                    .changed();
                if let Err(error) = photocraft_engine::ai_assets::parse(&app.ui.ai.component_buttons, &app.ui.ai.component_bars) {
                    ui.colored_label(ui.visuals().error_fg_color, tl!(&error));
                }
            }
        });
        if batch_changed && let Err(e) = crate::ai_service::save_settings(app) {
            app.ui.ai.status = e;
        }
        if app.ui.ai.batch_components {
            return;
        }
        let changed = ui
            .add_enabled_ui(!app.ai_runtime.running(), |ui| widgets::checkbox(ui, &mut app.ui.ai.expand_component, "Allow component outside selection"))
            .inner
            .changed();
        if changed && let Err(e) = crate::ai_service::save_settings(app) {
            app.ui.ai.status = e;
        }
        let changed = ui
            .add_enabled_ui(!app.ai_runtime.running(), |ui| widgets::checkbox(ui, &mut app.ui.ai.design_first, "Design first, then extract (2 images)"))
            .inner
            .changed();
        if changed && let Err(e) = crate::ai_service::save_settings(app) {
            app.ui.ai.status = e;
        }
    }
}

fn history_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(doc) = app.session.active().map(|st| st.doc.clone()) else {
        app.ui.ai.history_document = None;
        app.ui.ai.history_group = None;
        return;
    };
    let changed_document = app.ui.ai.history_document != Some(doc.id.0);
    if changed_document {
        app.ui.ai.history_document = Some(doc.id.0);
        app.ui.ai.history_group = doc.ai_generations.last().map(|g| g.id);
        if !app.ai_runtime.running() {
            app.ai_runtime.optimized_prompt.clear();
        }
    }
    if app.ui.ai.history_group.is_some_and(|id| !doc.ai_generations.iter().any(|g| g.id == id)) {
        app.ui.ai.history_group = None;
    }
    if doc.ai_generations.is_empty() {
        return;
    }
    ui.add_space(8.0);
    let label = ui.label(tl!("Generation history"));
    let previous = app.ui.ai.history_group;
    let name = |id: u64| format!("#{}", id);
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("ai-history-group")
            .selected_text(previous.map_or_else(|| tl!("New generation").to_owned(), name))
            .width((ui.available_width() - 32.0).max(80.0))
            .truncate()
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.ui.ai.history_group, None, tl!("New generation"));
                for group in doc.ai_generations.iter().rev() {
                    let title = group.versions.first().map_or(String::new(), |v| v.prompt.chars().take(24).collect());
                    ui.selectable_value(&mut app.ui.ai.history_group, Some(group.id), format!("{}  {}", name(group.id), title));
                }
            })
            .response
            .labelled_by(label.id);
        if ui
            .add_enabled_ui(previous.is_some() && !app.ai_runtime.running(), |ui| icons::button(ui, "trash", 24.0, false, "Remove generation record"))
            .inner
            .clicked()
            && let Some(id) = previous
        {
            if let Err(e) = app.run("ai.forgetHistory", json!({"groupId":id})) {
                app.ui.ai.status = e;
            } else {
                app.ui.ai.history_group = None;
            }
        }
    });
    if previous != app.ui.ai.history_group && !app.ai_runtime.running() {
        app.ai_runtime.optimized_prompt.clear();
    }
    let Some(group) = app.ui.ai.history_group.and_then(|id| doc.ai_generations.iter().find(|g| g.id == id)) else { return };
    let visible = group.versions.iter().rev().find(|v| v.layer.and_then(|id| doc.layer(id)).is_some_and(|l| l.visible));
    let record = visible.or_else(|| group.versions.last());
    if (changed_document || previous != app.ui.ai.history_group)
        && let Some(record) = record
    {
        load_record(app, record);
    }
    egui::ScrollArea::vertical().id_salt("ai-versions").max_height(80.0).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(visible.is_none(), tl!("Original")).clicked()
                && let Err(e) = app.run("ai.selectVersion", json!({"groupId":group.id,"layerId":null}))
            {
                app.ui.ai.status = e;
            }
            for (index, version) in group.versions.iter().enumerate() {
                let available = version.layer.and_then(|id| doc.layer(id)).is_some();
                let selected = visible.is_some_and(|v| v.layer == version.layer);
                if ui.add_enabled(available, egui::Button::selectable(selected, format!("{} {}", tl!("Version"), index + 1))).clicked() {
                    match app.run("ai.selectVersion", json!({"groupId":group.id,"layerId":version.layer.map(|l|l.0)})) {
                        Ok(_) => load_record(app, version),
                        Err(e) => app.ui.ai.status = e,
                    }
                }
            }
        });
    });
    if let Some(record) = record {
        egui::CollapsingHeader::new(tl!("Properties")).id_salt("ai-record-properties").show(ui, |ui| {
            ui.label(format!("{}: {}", tl!("Provider"), record.provider));
            ui.label(format!("{}: {}", tl!("Model"), record.model));
            ui.label(format!("{}: {}, {} / {} x {}", tl!("Selection"), group.area.x0, group.area.y0, group.area.width(), group.area.height()));
            egui::ScrollArea::vertical().id_salt("ai-record-text").max_height(160.0).show(ui, |ui| {
                ui.label(tl!("Prompt"));
                ui.add(egui::Label::new(&record.prompt).wrap().selectable(true));
                if !record.optimized_prompt.is_empty() && record.optimized_prompt != record.prompt {
                    ui.label(tl!("Optimized prompt"));
                    ui.add(egui::Label::new(&record.optimized_prompt).wrap().selectable(true));
                }
            });
        });
    }
}

fn execute(app: &mut PhotocraftApp, id: &str, params: serde_json::Value) {
    app.ui.ai.status = match app.run(id, params) {
        Ok(_) => tl!(match id {
            "ai.prepareTestCandidate" => "Candidate ready",
            "ai.applyCandidate" => "Candidate applied",
            _ => "Candidate discarded",
        })
        .to_owned(),
        Err(error) => error,
    };
}

#[cfg(test)]
#[path = "ai_layout_regression_tests.rs"]
mod layout_regression_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};
    use photocraft_engine::Session;

    fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        // Applying a production theme replaces the harness's animation-free styles. Keep
        // collapsing bodies settled before the next real click targets their controls.
        ui.ctx().all_styles_mut(|style| {
            style.animation_time = 0.0;
            style.scroll_animation = egui::style::ScrollAnimation::none();
        });
        app.last_canvas_rect = ui.ctx().content_rect();
        super::show(app, ui);
        crate::panels::ai_settings_flyout(app, ui, None);
    }

    #[test]
    fn floating_prompt_grows_upward_and_settings_are_collapsed_by_default() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        assert!(icons::exists("sliders-horizontal"));
        for (width, height) in [(360.0, 480.0), (800.0, 480.0), (1280.0, 720.0)] {
            let mut app = PhotocraftApp::new(Session::new(), Default::default());
            app.ui.ai.visible = true;
            app.ui.ai.prompt = "Redesign the playback bar".into();
            let mut initialized = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                move |ui, app: &mut PhotocraftApp| {
                    if !initialized {
                        egui_extras::install_image_loaders(ui.ctx());
                        theme::install_fonts(ui.ctx());
                        theme::apply(ui.ctx(), theme::ThemeKind::default());
                        initialized = true;
                        return;
                    }
                    app.last_canvas_rect = ui.ctx().content_rect();
                    show(app, ui);
                },
                app,
            );
            harness.run_steps(5);
            assert!(harness.query_by_label("Provider").is_none());
            assert!(harness.query_by_label("In-place edit").is_some());
            assert!(harness.query_by_label("Transparent component").is_some());
            assert!(harness.state().ui.ai.enabled && harness.state().ui.ai.visible);
            assert!(!harness.state().ui.ai.design_first);
            harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Prompt").click();
            harness.run_steps(2);
            harness.get_by_label("Prompt").type_text(" Add depth.");
            harness.run_steps(4);
            assert!(harness.state().ui.ai.prompt.contains("Add depth."));
            let short = harness.get_by_label("Prompt").rect();
            harness.state_mut().ui.ai.prompt = "Redesign the playback bar with raised controls and clear hierarchy. ".repeat(30);
            harness.run_steps(5);
            let long = harness.get_by_label("Prompt").rect();
            assert!(long.top() < short.top(), "the input grows upward");
            let button = harness.get_by_label("Generate image").rect();
            assert!(button.left() >= 0.0 && button.right() <= width && button.bottom() <= height);
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("floating-long-{width}.png"))).unwrap();
            }
            harness.get_by_role_and_label(egui::accesskit::Role::Button, "Model settings").click();
            harness.run_steps(5);
            assert!(harness.query_by_label("Provider").is_some());
        }
    }

    #[test]
    fn ai_switch_preserves_draft_and_is_independent_of_both_surfaces() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        let mut app = PhotocraftApp::new(Session::new(), Default::default());
        app.ui.ai.prompt = "Preserve this unfinished request".into();
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(
            move |ui, app| {
                if !ready {
                    egui_extras::install_image_loaders(ui.ctx());
                    theme::install_fonts(ui.ctx());
                    theme::apply(ui.ctx(), theme::ThemeKind::default());
                    ready = true;
                    return;
                }
                show(app, ui);
            },
            app,
        );
        harness.run_steps(4);
        harness.get_by_role_and_label(egui::accesskit::Role::CheckBox, "AI").click();
        harness.run_steps(4);
        assert!(!harness.state().ui.ai.enabled && harness.state().ui.ai.visible);
        assert_eq!(harness.state().ui.ai.prompt, "Preserve this unfinished request");
        for label in ["Generate image", "In-place edit", "Transparent component"] {
            assert!(harness.get_by_label(label).accesskit_node().is_disabled(), "{label} cannot start work while AI is off");
        }
        harness.get_by_role_and_label(egui::accesskit::Role::Button, "Model settings").click();
        harness.run_steps(4);
        assert!(crate::panel_docking::ai_settings_shown(harness.state()));
        assert!(!harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Provider").accesskit_node().is_disabled());
        harness.get_by_label("Collapse AI Studio").click();
        harness.run_steps(4);
        assert!(!harness.state().ui.ai.visible && !harness.state().ui.ai.enabled);
        assert!(crate::panel_docking::ai_settings_shown(harness.state()));
        harness.get_by_label("Show AI Studio").click();
        harness.run_steps(4);
        harness.get_by_role_and_label(egui::accesskit::Role::CheckBox, "AI").click();
        harness.run_steps(4);
        assert!(harness.state().ui.ai.enabled && harness.state().ui.ai.visible);
        harness.get_by_label("Hide model settings").click();
        harness.run_steps(4);
        assert!(!crate::panel_docking::ai_settings_shown(harness.state()));
        assert!(harness.state().ui.ai.enabled && harness.state().ui.ai.visible);
        assert_eq!(harness.state().ui.ai.prompt, "Preserve this unfinished request");
    }

    #[test]
    fn changing_mode_from_saved_history_starts_new_work_with_the_same_prompt() {
        use base64::Engine as _;
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        let mut session = Session::new();
        session.execute("file.new", json!({"width":16,"height":16})).unwrap();
        let image = photocraft_codecs::Image::from_u8(2, 2, photocraft_codecs::ChannelLayout::Rgba, [40, 80, 120, 255].repeat(4)).unwrap();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap());
        let pending = session.execute("ai.beginGeneration", json!({"prompt":"Saved image request","provider":"codex","destination":"layer"})).unwrap();
        session.execute("ai.receiveCandidate", json!({"candidateId":pending["candidateId"],"imageBase64":encoded})).unwrap();
        let requested = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured = requested.clone();
        let mut app = PhotocraftApp::new(
            session,
            crate::Services {
                ai_generate: Some(Box::new(move |request| {
                    *captured.borrow_mut() = Some((request.asset_job, request.transparent, request.prompt));
                    let (_tx, rx) = std::sync::mpsc::channel();
                    Ok(crate::ai_service::Job { cancel: Default::default(), result: rx })
                })),
                ..Default::default()
            },
        );
        sync_history(&mut app);
        assert!(app.ui.ai.history_group.is_some());
        app.ui.ai.prompt = "Keep the revised draft when changing mode".into();
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(
            move |ui, app| {
                if !ready {
                    theme::install_fonts(ui.ctx());
                    theme::apply(ui.ctx(), theme::ThemeKind::default());
                    ready = true;
                    return;
                }
                show(app, ui);
            },
            app,
        );
        harness.run_steps(4);
        assert!(harness.query_by_label("Generate again").is_some());
        harness.get_by_label("Transparent component").click();
        harness.run_steps(4);
        assert!(harness.state().ui.ai.history_group.is_none());
        assert_eq!(harness.state().ui.ai.mode, photocraft_doc::ai::GenerationMode::Component);
        assert_eq!(harness.state().ui.ai.prompt, "Keep the revised draft when changing mode");
        harness.get_by_label("Generate image").click();
        harness.run_steps(4);
        let requested = requested.borrow();
        let (asset_job, transparent, prompt) = requested.as_ref().unwrap();
        assert!(asset_job.is_some() && *transparent, "the component request must not retry the saved repaint generation");
        assert_eq!(prompt, "Keep the revised draft when changing mode");
        assert!(harness.state().ai_runtime.running());
        assert_eq!(harness.state().session.active().unwrap().doc.ai_generations.len(), 1);
    }

    #[test]
    fn browsing_history_during_generation_preserves_pending_prompt_metadata() {
        let (tx, rx) = std::sync::mpsc::channel();
        let rx = std::cell::RefCell::new(Some(rx));
        let mut app = PhotocraftApp::new(
            Session::new(),
            crate::Services {
                ai_generate: Some(Box::new(move |_| Ok(crate::ai_service::Job { cancel: Default::default(), result: rx.borrow_mut().take().unwrap() }))),
                ..Default::default()
            },
        );
        app.ui.ai.prompt = "running request".into();
        crate::ai_service::start(&mut app).unwrap();
        app.ai_runtime.optimized_prompt = "running optimized prompt".into();
        let record = photocraft_doc::ai::GenerationVersion {
            layer: None,
            prompt: "old prompt".into(),
            optimized_prompt: "old optimized prompt".into(),
            provider: "codex".into(),
            model: "old model".into(),
        };
        load_record(&mut app, &record);
        assert_eq!(app.ai_runtime.optimized_prompt, "running optimized prompt");
        let mut harness = egui_kittest::Harness::builder().build_ui_state(|ui, app| history_controls(app, ui), app);
        harness.run_steps(2);
        assert_eq!(harness.state().ai_runtime.optimized_prompt, "running optimized prompt");
        tx.send(Err("cancel fixture".into())).unwrap();
        crate::ai_service::cancel(harness.state_mut());
    }

    #[test]
    fn component_set_settings_hide_two_pass_controls_and_allow_editing() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        let mut app = PhotocraftApp::new(Session::new(), Default::default());
        app.ui.ai.provider = "images-api".into();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Component;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(420.0, 650.0)).build_ui_state(|ui, app| mode_controls(app, ui), app);
        harness.run_steps(3);
        harness.get_by_label("Component set (1 image)").click();
        harness.run_steps(3);
        assert!(harness.state().ui.ai.batch_components);
        assert!(harness.query_by_label("Design first, then extract (2 images)").is_none());
        assert!(harness.query_by_label("Allow component outside selection").is_none());
        harness.state_mut().ui.ai.component_buttons = "Custom\nButtons".into();
        harness.get_by_label("Player preset").click();
        harness.run_steps(3);
        assert!(harness.state().ui.ai.component_buttons.contains("Pause"));
        if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
            std::fs::create_dir_all(&dir).unwrap();
            harness.render().unwrap().save(std::path::PathBuf::from(dir).join("component-set.png")).unwrap();
        }
    }

    #[test]
    fn transparent_component_mode_can_be_selected_on_compact_panels() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for (width, height) in [(800.0, 480.0), (1280.0, 720.0)] {
            let mut session = Session::new();
            session.execute("file.new", json!({"width":1920,"height":1080})).unwrap();
            session.execute("select.rect", json!({"x":0,"y":948,"width":1920,"height":132})).unwrap();
            let mut app = PhotocraftApp::new(session, Default::default());
            app.ui.ai.visible = true;
            app.ui.ai.prompt = "Raised playback bar with transparent surroundings".into();
            let mut initialized = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                move |ui, app: &mut PhotocraftApp| {
                    if !initialized {
                        egui_extras::install_image_loaders(ui.ctx());
                        theme::install_fonts(ui.ctx());
                        theme::apply(ui.ctx(), theme::ThemeKind::default());
                        initialized = true;
                        return;
                    }
                    show(app, ui);
                    egui::CentralPanel::default().show(ui, |_| {});
                },
                app,
            );
            harness.run_steps(4);
            assert!(harness.query_by_label("Provider").is_none(), "generation mode stays available with settings closed");
            harness.get_by_label("Transparent component").click();
            harness.run_steps(4);
            assert_eq!(harness.state().ui.ai.mode, photocraft_doc::ai::GenerationMode::Component);
            assert!(harness.get_by_label("Generate image").rect().bottom() <= height);
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("component-{width}.png"))).unwrap();
            }
        }
    }

    #[test]
    fn saved_versions_can_be_compared_and_retried_from_original() {
        use base64::Engine as _;
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for (width, height) in [(800.0, 480.0), (1280.0, 720.0)] {
            let mut session = Session::new();
            session.execute("file.new", json!({"width":80,"height":48})).unwrap();
            session.execute("select.rect", json!({"x":0,"y":40,"width":80,"height":8})).unwrap();
            let image = photocraft_codecs::Image::from_u8(4, 4, photocraft_codecs::ChannelLayout::Rgba, [30, 170, 80, 255].repeat(16)).unwrap();
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap());
            let one = session
                .execute(
                    "ai.beginGeneration",
                    json!({"prompt":"Subtle raised playback controls","provider":"codex","destination":"layer","model":"example-model"}),
                )
                .unwrap();
            let first = session
                .execute(
                    "ai.receiveCandidate",
                    json!({"candidateId":one["candidateId"],"imageBase64":encoded,"optimizedPrompt":"Preserve positions and add restrained highlights."}),
                )
                .unwrap();
            let two = session
                .execute("ai.retryGeneration", json!({"groupId":first["groupId"],"prompt":"Softer shadows","provider":"codex","model":"example-model"}))
                .unwrap();
            session.execute("ai.receiveCandidate", json!({"candidateId":two["candidateId"],"imageBase64":encoded})).unwrap();
            let native = photocraft_format::save_to_bytes(&session.active().unwrap().doc, &Default::default()).unwrap();
            let mut reopened = Session::new();
            reopened.add_document(photocraft_format::load_from_bytes(&native).unwrap(), None);
            let reference = one["sourcePng"].as_str().unwrap().to_owned();
            let services = crate::Services {
                ai_generate: Some(Box::new(move |req| {
                    assert_eq!(req.source_png.as_deref(), Some(reference.as_str()));
                    let (tx, rx) = std::sync::mpsc::channel();
                    tx.send(Ok(encoded.clone())).unwrap();
                    Ok(crate::ai_service::Job { cancel: Default::default(), result: rx })
                })),
                ..Default::default()
            };
            let mut app = PhotocraftApp::new(reopened, services);
            app.ui.ai.visible = true;
            let mut initialized = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                move |ui, app: &mut PhotocraftApp| {
                    if !initialized {
                        egui_extras::install_image_loaders(ui.ctx());
                        theme::install_fonts(ui.ctx());
                        theme::apply(ui.ctx(), theme::ThemeKind::default());
                        initialized = true;
                        return;
                    }
                    crate::ai_service::tick(app, ui.ctx());
                    show(app, ui);
                    egui::CentralPanel::default().show(ui, |_| {});
                },
                app,
            );
            harness.run_steps(4);
            assert_eq!(harness.state().ui.ai.prompt, "Softer shadows");
            harness.get_by_role_and_label(egui::accesskit::Role::Button, "Generation details").click();
            harness.run_steps(4);
            assert!(harness.get_by_label("Version 2").rect().right() <= width);
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("history-{width}.png"))).unwrap();
            }
            harness.get_by_label("Original").click();
            harness.run_steps(4);
            assert!(harness.state().session.active().unwrap().doc.layers.iter().skip(1).all(|l| !l.visible));
            assert!(harness.query_by_label("Version 1").is_some(), "comparing versions keeps the details popup open");
            harness.get_by_label("Version 1").click();
            harness.run_steps(4);
            assert_eq!(harness.state().ui.ai.prompt, "Subtle raised playback controls");
            assert!(harness.state().session.active().unwrap().doc.layers[1].visible);
            let retry = harness.get_by_label("Generate again");
            assert!(retry.rect().bottom() <= height, "retry must be visible on a compact screen");
            retry.click();
            harness.run_steps(6);
            assert_eq!(harness.state().session.active().unwrap().doc.ai_generations[0].versions.len(), 3);
            assert!(harness.query_by_label("Properties").is_none(), "generating outside the popup closes its details");
            let summary = harness.state().ui.ai.status.clone();
            harness.get_by_role_and_label(egui::accesskit::Role::Button, &summary).click();
            harness.run_steps(4);
            harness.get_by_label("Properties").click();
            harness.run_steps(4);
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("history-details-{width}.png"))).unwrap();
            }
        }
    }

    #[test]
    fn codex_model_picker_is_visible_persists_and_forwards_selection() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for (width, height) in [(800.0, 480.0), (1280.0, 720.0)] {
            let saved = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
            let settings = saved.clone();
            let requested = std::rc::Rc::new(std::cell::RefCell::new((String::new(), String::new())));
            let selected = requested.clone();
            let services = crate::Services {
                ai_models: Some(Box::new(|_| {
                    let (tx, rx) = std::sync::mpsc::channel();
                    tx.send(Ok(vec![
                        crate::ai_service::CodexModel {
                            id: "fast".into(),
                            name: "Example Fast".into(),
                            is_default: true,
                            hidden: false,
                            supported_reasoning_efforts: vec!["low".into(), "medium".into()],
                            default_reasoning_effort: "medium".into(),
                        },
                        crate::ai_service::CodexModel {
                            id: "precise".into(),
                            name: "Example Precise".into(),
                            is_default: false,
                            hidden: false,
                            supported_reasoning_efforts: vec!["medium".into(), "high".into(), "xhigh".into()],
                            default_reasoning_effort: "high".into(),
                        },
                        crate::ai_service::CodexModel {
                            id: "extra".into(),
                            name: "Example Extra".into(),
                            is_default: false,
                            hidden: true,
                            supported_reasoning_efforts: vec![],
                            default_reasoning_effort: String::new(),
                        },
                    ]))
                    .unwrap();
                    Ok(crate::ai_service::Job { cancel: Default::default(), result: rx })
                })),
                save_ai_settings: Some(Box::new(move |text| {
                    *settings.borrow_mut() = text.into();
                    Ok(())
                })),
                ai_generate: Some(Box::new(move |req| {
                    *selected.borrow_mut() = (req.codex_model, req.codex_reasoning_effort);
                    let (_tx, rx) = std::sync::mpsc::channel();
                    Ok(crate::ai_service::Job { cancel: Default::default(), result: rx })
                })),
                ..Default::default()
            };
            let mut app = PhotocraftApp::new(Session::new(), services);
            app.ui.ai.visible = true;
            app.ui.ai.provider = "codex".into();
            app.ui.ai.prompt = "private prompt".into();
            app.ui.panels.ai_settings = true;
            app.ai_runtime.api_key = "private key".into();
            let mut initialized = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                move |ui, app: &mut PhotocraftApp| {
                    if !initialized {
                        egui_extras::install_image_loaders(ui.ctx());
                        theme::install_fonts(ui.ctx());
                        theme::apply(ui.ctx(), theme::ThemeKind::default());
                        initialized = true;
                        return;
                    }
                    crate::ai_service::tick_models(app, ui.ctx());
                    show(app, ui);
                    egui::CentralPanel::default().show(ui, |_| {});
                },
                app,
            );
            harness.run_steps(4);
            let picker = harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Model");
            assert_eq!(picker.value().as_deref(), Some("Automatic (Example Fast)"));
            assert!(picker.rect().right() <= width && picker.rect().bottom() <= height);
            picker.click();
            harness.run_steps(4);
            assert!(harness.query_by_label("Example Extra (Hidden)").is_some());
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_MODELS_OUT") {
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("models-{width}.png"))).unwrap();
            }
            harness.get_by_label("Example Precise").click();
            harness.run_steps(4);
            assert_eq!(harness.state().ui.ai.codex_model, "precise");
            assert!(harness.query_by_label("Reasoning effort").is_none());
            harness.get_by_label("Connection settings").click();
            harness.run_steps(4);
            let reasoning = harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Reasoning effort");
            assert_eq!(reasoning.value().as_deref(), Some("Automatic (High)"));
            assert!(reasoning.rect().right() <= width && reasoning.rect().bottom() <= height);
            reasoning.click();
            harness.run_steps(4);
            assert!(harness.query_by_label("Low").is_none(), "do not invent efforts absent from the chosen model");
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_REASONING_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("reasoning-{width}.png"))).unwrap();
            }
            harness.get_by_label("Extra high").click();
            harness.run_steps(4);
            assert_eq!(harness.state().ui.ai.codex_reasoning_effort, "xhigh");
            let settings: serde_json::Value = serde_json::from_str(&saved.borrow()).unwrap();
            assert_eq!(settings["codexModel"], "precise");
            assert_eq!(settings["codexReasoningEffort"], "xhigh");
            assert!(!saved.borrow().contains("private"));
            harness.get_by_label("Hide model settings").click();
            harness.run_steps(4);
            harness.get_by_label("Generate image").click();
            harness.run_steps(4);
            assert_eq!(*requested.borrow(), ("precise".into(), "xhigh".into()));
            crate::ai_service::cancel(harness.state_mut());
            harness.get_by_role_and_label(egui::accesskit::Role::Button, "Model settings").click();
            harness.run_steps(4);
            harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Model").click();
            harness.run_steps(4);
            harness.get_by_label("Automatic").click();
            harness.run_steps(4);
            assert!(harness.state().ui.ai.codex_model.is_empty());
            assert!(harness.state().ui.ai.codex_reasoning_effort.is_empty(), "the default model does not support xhigh");
            harness.state_mut().ai_runtime.optimized_prompt =
                "A centered red circle with crisp edges and generous white space. Preserve the requested colors; no text or extra objects.".into();
            harness.get_by_label("Hide model settings").click();
            harness.run_steps(4);
            harness.get_by_role_and_label(egui::accesskit::Role::Button, "Generation cancelled").click();
            harness.run_steps(4);
            harness.get_by_role_and_label(egui::accesskit::Role::Button, "Optimized prompt").click();
            harness.run_steps(4);
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_MODELS_OUT") {
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("optimized-prompt-{width}.png"))).unwrap();
            }
        }
    }

    #[test]
    fn asset_generate_works_with_an_active_group_and_no_selection() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        let mut session = Session::new();
        session.execute("file.new", json!({"width":320,"height":160})).unwrap();
        session
            .edit("Group", |doc, active| {
                let layer = photocraft_doc::Layer::group("Existing assets", Vec::new());
                *active = Some(layer.id);
                doc.layers.push(layer);
                Ok(())
            })
            .unwrap();
        let (_tx, rx) = std::sync::mpsc::channel();
        let rx = std::cell::RefCell::new(Some(rx));
        let services = crate::Services {
            ai_generate: Some(Box::new(move |request| {
                assert!(request.asset_job.is_some());
                Ok(crate::ai_service::Job { cancel: Default::default(), result: rx.borrow_mut().take().unwrap() })
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(session, services);
        app.ui.ai.provider = "codex".into();
        app.ui.ai.mode = photocraft_doc::ai::GenerationMode::Component;
        app.ui.ai.prompt = "Make transparent player controls".into();
        let mut initialized = false;
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            move |ui, app| {
                if !initialized {
                    theme::install_fonts(ui.ctx());
                    initialized = true;
                    return;
                }
                generate_button(app, ui);
            },
            app,
        );
        h.run_steps(3);
        h.get_by_label("Generate image").click();
        h.run_steps(3);
        assert!(h.state().ai_runtime.running());
    }

    #[test]
    fn real_generate_and_cancel_buttons_work_without_a_document_and_on_large_canvases() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for (width, height, canvas) in [(800.0, 480.0, None), (1280.0, 720.0, Some((1920, 1080))), (800.0, 480.0, Some((3840, 2160)))] {
            let (_tx, rx) = std::sync::mpsc::channel();
            let rx = std::cell::RefCell::new(Some(rx));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = cancel.clone();
            let services = crate::Services {
                ai_generate: Some(Box::new(move |_| Ok(crate::ai_service::Job { cancel: flag.clone(), result: rx.borrow_mut().take().unwrap() }))),
                ..Default::default()
            };
            let mut session = Session::new();
            if let Some((width, height)) = canvas {
                session.execute("file.new", json!({"width":width,"height":height})).unwrap();
            }
            let mut app = PhotocraftApp::new(session, services);
            app.ui.ai.visible = true;
            app.ui.ai.prompt = "A geometric pattern".into();
            let mut initialized = false;
            let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                move |ui, app| {
                    if !initialized {
                        theme::install_fonts(ui.ctx());
                        initialized = true;
                        return;
                    }
                    show(app, ui);
                    egui::CentralPanel::default().show(ui, |_| {});
                },
                app,
            );
            harness.run_steps(4);
            let button = harness.get_by_label("Generate image");
            assert!(button.rect().bottom() <= height);
            button.click();
            harness.run_steps(4);
            assert!(harness.state().ai_runtime.running());
            assert!(harness.state().session.active().is_some());
            harness.get_by_label("Cancel").click();
            harness.run_steps(4);
            assert!(!harness.state().ai_runtime.running());
            assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
            assert!(harness.state().session.ai.candidate().is_none());
        }
    }

    #[test]
    fn preview_and_menu_do_not_commit_document() {
        let mut session = Session::new();
        session.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
        session.execute("select.rect", json!({"x": 8, "y": 6, "width": 12, "height": 10})).unwrap();
        let mut app = PhotocraftApp::new(session, Default::default());
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "window.ai", json!({"show": true})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        execute(&mut app, "ai.prepareTestCandidate", json!({"prompt": "test"}));
        let (shown, key) = crate::canvas::display_doc(&mut app, 0);
        assert_ne!(key, 0);
        assert!(!std::sync::Arc::ptr_eq(&shown, &before));
        assert!(std::sync::Arc::ptr_eq(&app.session.active().unwrap().doc, &before));
        crate::menus::invoke(&mut app, &ctx, "window.ai", json!({"show":false})).unwrap();
        let (collapsed, collapsed_key) = crate::canvas::display_doc(&mut app, 0);
        assert_eq!(collapsed_key, key, "collapsing the composer must preserve candidate preview");
        assert!(std::sync::Arc::ptr_eq(&collapsed, &shown));
        assert!(app.ui.ai.enabled);
        app.ui.ai.preview = false;
        assert_eq!(crate::canvas::display_doc(&mut app, 0).1, 0);
        assert!(crate::menus::invoke(&mut app, &ctx, "window.ai", json!({"show": "invalid"})).is_err());
    }

    #[test]
    fn candidate_buttons_work_across_themes_and_sizes() {
        let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
        for kind in theme::ThemeKind::ALL {
            for (width, height) in [(800.0, 480.0), (1280.0, 720.0)] {
                let mut session = Session::new();
                session.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
                session.execute("select.rect", json!({"x": 8, "y": 6, "width": 12, "height": 10})).unwrap();
                let steps = session.active().unwrap().history.past_len();
                let mut app = PhotocraftApp::new(session, Default::default());
                app.ui.ai.visible = true;
                app.ui.ai.provider = "local-test".into();
                app.ui.ai.prompt = "Prepare a preview candidate".into();
                let mut ready = false;
                let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
                    move |ui, app: &mut PhotocraftApp| {
                        if !ready {
                            egui_extras::install_image_loaders(ui.ctx());
                            theme::install_fonts(ui.ctx());
                            theme::apply(ui.ctx(), kind);
                            ready = true;
                            return;
                        }
                        show(app, ui);
                        egui::CentralPanel::default().show(ui, |_| {});
                    },
                    app,
                );
                harness.run_steps(4);
                let button = harness.get_by_label("Create Test Candidate");
                let rect = button.rect();
                assert!(rect.left() >= 0.0 && rect.right() <= width && rect.top() >= 0.0 && rect.bottom() <= height, "{kind:?}: {rect:?}");
                button.click();
                harness.run_steps(4);
                assert!(harness.state().session.ai.candidate().is_some());
                assert_eq!(harness.state().session.active().unwrap().history.past_len(), steps);
                harness.get_by_role_and_label(egui::accesskit::Role::Button, "Candidate ready").click();
                harness.run_steps(4);
                harness.get_by_label("Preview").click();
                harness.run_steps(4);
                assert!(!harness.state().ui.ai.preview);
                assert!(harness.query_by_label("Apply").is_some(), "previewing keeps the candidate popup open");
                harness.get_by_label("Apply").click();
                harness.run_steps(4);
                assert!(harness.state().session.ai.candidate().is_none());
                assert_eq!(harness.state().session.active().unwrap().history.past_len(), steps + 1);
                harness.get_by_label("Create Test Candidate").click();
                harness.run_steps(4);
                assert!(harness.state().session.ai.candidate().is_some());
                assert!(harness.query_by_label("Discard").is_none(), "creating another candidate closes the previous details popup");
                harness.get_by_role_and_label(egui::accesskit::Role::Button, "Candidate ready").click();
                harness.run_steps(4);
                if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
                    std::fs::create_dir_all(&dir).unwrap();
                    harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("candidate-discard-{}-{width}.png", kind.id()))).unwrap();
                }
                harness.get_by_label("Discard").click();
                harness.run_steps(4);
                assert!(harness.state().session.ai.candidate().is_none());
                assert_eq!(harness.state().session.active().unwrap().history.past_len(), steps + 1);
                harness.get_by_label("Collapse AI Studio").click();
                harness.run_steps(4);
                assert!(!harness.state().ui.ai.visible);
            }
        }
    }
}
