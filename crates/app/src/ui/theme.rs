//! Fesilent 官网视觉语言：雾白背景、紫色细节、黑色胶囊按钮及柔和粉蓝光晕。

use egui::{Color32, FontId, RichText, Stroke, Vec2};

pub(super) const INK: Color32 = Color32::from_rgb(33, 31, 39);
pub(super) const MUTED: Color32 = Color32::from_rgb(119, 116, 127);
pub(super) const PURPLE: Color32 = Color32::from_rgb(117, 91, 198);
pub(super) const PURPLE_WASH: Color32 = Color32::from_rgb(240, 235, 251);
pub(super) const BORDER: Color32 = Color32::from_rgb(231, 227, 233);
pub(super) const BACKGROUND: Color32 = Color32::from_rgb(248, 247, 249);
pub(super) const PRIMARY: Color32 = Color32::from_rgb(29, 28, 34);
pub(super) const GREEN: Color32 = Color32::from_rgb(38, 121, 105);
pub(super) const ERROR: Color32 = Color32::from_rgb(166, 57, 72);
pub(super) const AMBER: Color32 = Color32::from_rgb(150, 105, 37);

pub(super) fn install(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.faint_bg_color = BACKGROUND;
    style.visuals.extreme_bg_color = Color32::WHITE;
    style.visuals.selection.bg_fill = PURPLE_WASH;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, PURPLE);
    style.visuals.hyperlink_color = PURPLE;
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.noninteractive,
    ] {
        widget.bg_fill = Color32::WHITE;
        widget.weak_bg_fill = Color32::WHITE;
        widget.bg_stroke = Stroke::new(1.0_f32, BORDER);
        widget.fg_stroke = Stroke::new(1.0_f32, INK);
        widget.corner_radius = 10.into();
    }
    for widget in [
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        widget.bg_fill = PURPLE_WASH;
        widget.weak_bg_fill = PURPLE_WASH;
        widget.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(203, 190, 233));
        widget.fg_stroke = Stroke::new(1.0_f32, INK);
        widget.corner_radius = 10.into();
    }
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 8.0);
    style.spacing.interact_size.y = 34.0;
    style.spacing.slider_width = 210.0;
    for (kind, size) in [
        (egui::TextStyle::Heading, 25.0),
        (egui::TextStyle::Body, 14.0),
        (egui::TextStyle::Button, 13.0),
        (egui::TextStyle::Small, 12.0),
    ] {
        style.text_styles.insert(kind, FontId::proportional(size));
    }
    ctx.set_style(style);
}

pub(super) fn eyebrow(ui: &mut egui::Ui, value: &str) {
    ui.label(
        RichText::new(value)
            .size(10.0)
            .color(PURPLE)
            .extra_letter_spacing(1.4),
    );
}

pub(super) fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::WHITE))
            .fill(PRIMARY)
            .stroke(Stroke::NONE)
            .corner_radius(30)
            .min_size(Vec2::new(112.0, 37.0)),
    )
}

pub(super) fn secondary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(label).corner_radius(30))
}

pub(super) fn card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    content: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 235))
        .stroke(Stroke::new(1.0_f32, Color32::WHITE))
        .corner_radius(22)
        .inner_margin(20)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).size(16.0).strong());
            }
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).small().color(MUTED));
            }
            if !title.is_empty() || !subtitle.is_empty() {
                ui.add_space(7.0);
            }
            content(ui);
        });
}

/// 使用原生顶点透明度绘制柔和光斑，保持缩放清晰，无额外位图资源。
pub(super) fn glow(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    const STEPS: u32 = 48;
    for index in 0..=STEPS {
        let angle = std::f32::consts::TAU * index as f32 / STEPS as f32;
        mesh.colored_vertex(center + Vec2::angled(angle) * radius, Color32::TRANSPARENT);
    }
    for index in 0..STEPS {
        mesh.add_triangle(0, index + 1, index + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
}

pub(super) fn page_background(ui: &egui::Ui) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    glow(
        painter,
        rect.left_top() + Vec2::new(50.0, 20.0),
        390.0,
        Color32::from_rgba_unmultiplied(221, 227, 255, 165),
    );
    glow(
        painter,
        rect.right_top() + Vec2::new(-35.0, 95.0),
        330.0,
        Color32::from_rgba_unmultiplied(253, 222, 239, 150),
    );
}

pub(super) fn notice(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.075))
        .corner_radius(12)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(text).color(color));
        });
}
