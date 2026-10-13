use super::*;
use base64::Engine as _;
use egui_kittest::kittest::Queryable;
use photocraft_engine::Session;

fn paints_text(shape: &egui::epaint::Shape, text: &str) -> bool {
    match shape {
        egui::epaint::Shape::Text(shape) => shape.galley.text() == text,
        egui::epaint::Shape::Vec(shapes) => shapes.iter().any(|shape| paints_text(shape, text)),
        _ => false,
    }
}

#[test]
fn long_chinese_results_prompt_and_references_keep_composer_controls_inside_the_viewport() {
    let _language = crate::i18n::language_scope(crate::i18n::Lang::EN);
    for (width, height) in [(360.0, 480.0), (800.0, 480.0), (1280.0, 720.0)] {
        let first = "生成失败：请检查服务连接和当前模型配置，项目内容及参考图片已经保留，可修改设置后重试。".repeat(12);
        let status = format!("{first}\n详细错误：测试信息保持完整，包括模型名称、返回原因和修复建议。");
        let heading = format!("{first}…");
        let mut app = PhotocraftApp::new(Session::new(), Default::default());
        app.run("file.new", json!({"width":32,"height":32})).unwrap();
        let png = photocraft_codecs::encode(
            &photocraft_codecs::Image::from_u8(4, 2, photocraft_codecs::ChannelLayout::Rgba, [30, 160, 90, 128].repeat(8)).unwrap(),
            photocraft_codecs::Format::Png,
            &Default::default(),
        )
        .unwrap();
        for reference in 1..=4 {
            app.run(
                "ai.referenceAdd",
                json!({"name":format!("reference-{reference}.png"),"imageBase64":base64::engine::general_purpose::STANDARD.encode(&png)}),
            )
            .unwrap();
        }
        app.ui.ai.prompt = "保持界面上的文字与图标，将选中的播放控制条改为透明背景并保留柔和阴影。".repeat(30);
        app.ui.ai.status = status.clone();
        let mut ready = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
            move |ui, app: &mut PhotocraftApp| {
                if !ready {
                    egui_extras::install_image_loaders(ui.ctx());
                    theme::install_fonts(ui.ctx());
                    theme::apply(ui.ctx(), theme::ThemeKind::default());
                    ready = true;
                    return;
                }
                app.last_canvas_rect = ui.ctx().content_rect();
                ui.ctx().all_styles_mut(|style| style.animation_time = 0.0);
                show(app, ui);
            },
            app,
        );
        harness.run_steps(5);
        for expanded in [false, true] {
            if expanded {
                harness.get_by_role_and_label(egui::accesskit::Role::Button, heading.as_str()).click();
                harness.run_steps(5);
                assert!(harness.query_by_label(status.as_str()).is_some(), "expanded details retain the complete error");
            }
            if let Some(dir) = std::env::var_os("PHOTOCRAFT_AI_HISTORY_OUT") {
                std::fs::create_dir_all(&dir).unwrap();
                harness.render().unwrap().save(std::path::PathBuf::from(dir).join(format!("chinese-result-{width}-{expanded}.png"))).unwrap();
            }
            assert_eq!(harness.state().ui.ai.status, status);
            // The multiline editor keeps its full intrinsic text height inside its
            // ScrollArea. Check the paint clip, not that intentionally tall node.
            let prompt = harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Prompt").rect();
            let clip = harness
                .output()
                .shapes
                .iter()
                .find(|shape| paints_text(&shape.shape, &harness.state().ui.ai.prompt))
                .map(|shape| shape.clip_rect)
                .expect("the prompt must have a painted text shape");
            let visible_prompt = prompt.intersect(clip);
            assert!(visible_prompt.is_positive(), "the prompt has a visible scroll viewport");
            assert!(
                visible_prompt.left() >= 0.0 && visible_prompt.right() <= width && visible_prompt.top() >= 0.0 && visible_prompt.bottom() <= height,
                "{width}x{height}, expanded={expanded}: prompt viewport {visible_prompt:?}"
            );
            assert!(visible_prompt.height() < prompt.height(), "long prompt text scrolls instead of growing the composer");
            for (role, label) in [
                (egui::accesskit::Role::CheckBox, "AI"),
                (egui::accesskit::Role::Button, "In-place edit"),
                (egui::accesskit::Role::Button, "Transparent component"),
                (egui::accesskit::Role::Button, "Reference images"),
                (egui::accesskit::Role::Button, "Model settings"),
                (egui::accesskit::Role::Button, "Collapse AI Studio"),
                (egui::accesskit::Role::Button, "Generate image"),
                (egui::accesskit::Role::Button, heading.as_str()),
            ] {
                let rect = harness.get_by_role_and_label(role, label).rect();
                assert!(
                    rect.left() >= 0.0 && rect.right() <= width && rect.top() >= 0.0 && rect.bottom() <= height,
                    "{width}x{height}, expanded={expanded}: {label} {rect:?}"
                );
            }
        }
    }
}
