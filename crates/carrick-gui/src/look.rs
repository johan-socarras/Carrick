//! How the window looks: sizes, colours and the few widgets of its own.

use eframe::egui::{
    self, pos2, vec2, Color32, FontFamily, FontId, Rect, Response, RichText, Role, Sense, Stroke, StrokeKind, TextStyle,
    Ui, WidgetInfo,
};

/// Text a little larger than egui's default, and roomier buttons.
pub fn install(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Button, FontId::new(15.0, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Small, FontId::new(12.5, FontFamily::Proportional));
        style.text_styles.insert(TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace));
        style.spacing.button_padding = vec2(12.0, 6.0);
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.interact_size.y = 30.0;
    });
}

pub fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

pub fn good(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(0x5f, 0xd0, 0x8a)
    } else {
        Color32::from_rgb(0x1f, 0x8a, 0x4c)
    }
}

pub fn caution(ui: &Ui) -> Color32 {
    ui.visuals().warn_fg_color
}

pub fn bad(ui: &Ui) -> Color32 {
    ui.visuals().error_fg_color
}

pub fn weak(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

/// One of the three tabs at the top. `label` is its name for screen
/// readers and tests; `text` what it shows.
pub fn tab(ui: &mut Ui, selected: bool, label: &str, text: &str) -> Response {
    let font = FontId::new(16.0, FontFamily::Proportional);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + 28.0, 38.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, label));
    let visuals = ui.visuals();
    let painter = ui.painter();
    if response.hovered() && !selected {
        painter.rect_filled(rect, 6, visuals.widgets.hovered.weak_bg_fill);
    }
    let color = if selected { visuals.strong_text_color() } else { visuals.text_color() };
    let at = pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0);
    painter.galley(at, galley, color);
    if selected {
        let bar = Rect::from_min_size(pos2(rect.left() + 8.0, rect.bottom() - 3.0), vec2(rect.width() - 16.0, 3.0));
        painter.rect_filled(bar, 2, accent(ui));
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The button that does the main thing of a screen.
pub fn primary(ui: &mut Ui, enabled: bool, text: &str) -> Response {
    let fill = accent(ui);
    let button = egui::Button::new(RichText::new(text).strong().color(Color32::WHITE).size(16.0))
        .fill(fill)
        .min_size(vec2(170.0, 38.0));
    ui.add_enabled(enabled, button)
}

/// The area files can be dropped on, with what is inside it.
pub fn drop_zone<R>(ui: &mut Ui, hovering: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let stroke_color = if hovering { accent(ui) } else { ui.visuals().widgets.noninteractive.bg_stroke.color };
    let fill = if hovering { accent(ui).gamma_multiply(0.12) } else { ui.visuals().faint_bg_color };
    let frame = egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(if hovering { 2.0 } else { 1.0 }, stroke_color))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(16, 22));
    frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(add).inner
        })
        .inner
}

/// The box around one transfer.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A small coloured dot before a line of text.
pub fn dot_label(ui: &mut Ui, color: Color32, text: impl Into<RichText>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(text.into().color(color));
    });
}

/// Paints a translucent cover saying files can be dropped.
pub fn drop_cover(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-cover")));
    let accent = ctx.global_style().visuals.selection.bg_fill;
    painter.rect(screen.shrink(6.0), 12, Color32::from_black_alpha(90), Stroke::new(3.0, accent), StrokeKind::Inside);
    painter.text(
        screen.center(),
        egui::Align2::CENTER_CENTER,
        "Drop to send",
        FontId::new(28.0, FontFamily::Proportional),
        Color32::WHITE,
    );
}
