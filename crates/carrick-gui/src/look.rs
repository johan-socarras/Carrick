//! How the window looks: sizes, colours and the few widgets of its own.

use eframe::egui::{
    self, pos2, vec2, Color32, FontFamily, FontId, Rect, Response, RichText, Role, Sense, Stroke, StrokeKind, TextStyle,
    Ui, WidgetInfo,
};

/// The two colours of the icon: the knot goes from this blue…
pub const BLUE: Color32 = Color32::from_rgb(0x1e, 0x6b, 0xff);
/// …to this cyan.
pub const CYAN: Color32 = Color32::from_rgb(0x23, 0xc4, 0xf0);
/// The blue, a little lighter, for dark backgrounds.
const BLUE_ON_DARK: Color32 = Color32::from_rgb(0x2f, 0x80, 0xff);

/// Text a little larger than egui's default, roomier buttons, and the blue of
/// the icon wherever egui marks something as chosen.
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
        let dark = style.visuals.dark_mode;
        let blue = if dark { BLUE_ON_DARK } else { BLUE };
        style.visuals.selection.bg_fill = blue;
        style.visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        style.visuals.hyperlink_color = if dark { Color32::from_rgb(0x7c, 0xc4, 0xff) } else { BLUE };
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, blue.gamma_multiply(0.7));
        style.visuals.text_edit_bg_color = Some(if dark { Color32::from_rgb(0x12, 0x14, 0x19) } else { Color32::WHITE });
    });
}

pub fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

/// The blue-to-cyan of the icon, as a texture that paints rounded shapes.
fn gradient(ctx: &egui::Context) -> egui::TextureId {
    let id = egui::Id::unique("carrick-gradient");
    if let Some(texture) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return texture.id();
    }
    let width = 256;
    let pixels = (0..width)
        .map(|x| {
            let t = x as f32 / (width - 1) as f32;
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            Color32::from_rgb(mix(BLUE.r(), CYAN.r()), mix(BLUE.g(), CYAN.g()), mix(BLUE.b(), CYAN.b()))
        })
        .collect();
    let image = egui::ColorImage::new([width, 1], pixels);
    let texture = ctx.load_texture("carrick-gradient", image, egui::TextureOptions::LINEAR);
    let texture_id = texture.id();
    ctx.data_mut(|d| d.insert_temp(id, texture));
    texture_id
}

/// A rounded shape filled with the icon's blue-to-cyan, from `from` to `to`
/// of the way across (so a bar shows the part of the gradient it has reached).
fn gradient_shape(ctx: &egui::Context, rect: Rect, radius: u8, from: f32, to: f32, tint: Color32) -> egui::Shape {
    let uv = Rect::from_min_max(pos2(from, 0.0), pos2(to, 1.0));
    egui::epaint::RectShape::filled(rect, radius, tint).with_texture(gradient(ctx), uv).into()
}

/// The icon itself, small, where the name used to be.
pub fn knot(ui: &mut Ui) {
    const N: usize = 64;
    let id = egui::Id::unique("carrick-knot");
    let texture = match ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        Some(texture) => texture,
        None => {
            // The same 64 x 64 picture as the window's icon.
            let image = egui::ColorImage::from_rgba_unmultiplied([N, N], include_bytes!("icon-64.rgba"));
            let texture = ui.ctx().load_texture("carrick-knot", image, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, texture.clone()));
            texture
        }
    };
    let (rect, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
}

/// A button painted with the icon's gradient: the main thing of a screen.
fn gradient_button(ui: &mut Ui, enabled: bool, text: RichText, min_size: egui::Vec2) -> Response {
    let behind = ui.painter().add(egui::Shape::Noop);
    let button = egui::Button::new(text.color(Color32::WHITE)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE).min_size(min_size);
    let response = ui.add_enabled(enabled, button);
    let tint = if !enabled {
        Color32::WHITE.gamma_multiply(0.4)
    } else if response.hovered() {
        Color32::from_gray(235)
    } else {
        Color32::WHITE
    };
    ui.painter().set(behind, gradient_shape(ui.ctx(), response.rect, 8, 0.0, 1.0, tint));
    response
}

/// A smaller gradient button, for the action next to a code.
pub fn brand(ui: &mut Ui, text: &str) -> Response {
    gradient_button(ui, true, RichText::new(text), vec2(0.0, 30.0))
}

/// How far a transfer is: a track with the part done in the icon's gradient,
/// and the percentage on it when asked.
pub fn progress(ui: &mut Ui, fraction: f32, height: f32, percentage: bool) {
    let fraction = fraction.clamp(0.0, 1.0);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::ProgressIndicator, true, format!("{}%", (fraction * 100.0).round())));
    let radius = (height / 2.0).min(255.0) as u8;
    let track = if ui.visuals().dark_mode { Color32::from_gray(0x2a) } else { Color32::from_gray(0xe2) };
    let painter = ui.painter();
    painter.rect_filled(rect, radius, track);
    if fraction > 0.0 {
        let done = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * fraction.max(height / rect.width()), rect.bottom()));
        painter.add(gradient_shape(ui.ctx(), done, radius, 0.0, fraction.max(0.02), Color32::WHITE));
    }
    if percentage {
        let text = format!("{}%", (fraction * 100.0).floor());
        let color = if fraction > 0.12 { Color32::WHITE } else { ui.visuals().text_color() };
        painter.text(pos2(rect.left() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, text, FontId::new(11.5, FontFamily::Proportional), color);
    }
}

/// A short word in a coloured pill: how data travels, a state.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
}

/// The four words, or the long code, in a tinted box of their own.
pub fn code_frame<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (fill, stroke) = if ui.visuals().dark_mode {
        (Color32::from_rgb(0x11, 0x22, 0x3d), Color32::from_rgb(0x2a, 0x3a, 0x5c))
    } else {
        (Color32::from_rgb(0xe8, 0xf1, 0xff), Color32::from_rgb(0xbf, 0xd6, 0xff))
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// The colour of the code itself, on its tinted box.
pub fn code_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(0x7c, 0xc4, 0xff)
    } else {
        Color32::from_rgb(0x14, 0x50, 0xc8)
    }
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
        painter.add(gradient_shape(ui.ctx(), bar, 2, 0.0, 1.0, Color32::WHITE));
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The button that does the main thing of a screen.
pub fn primary(ui: &mut Ui, enabled: bool, text: &str) -> Response {
    gradient_button(ui, enabled, RichText::new(text).strong().size(16.0), vec2(170.0, 38.0))
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
    let (fill, stroke) = if ui.visuals().dark_mode {
        (Color32::from_rgb(0x1a, 0x1d, 0x24), Color32::from_rgb(0x2a, 0x2e, 0x38))
    } else {
        (Color32::WHITE, Color32::from_rgb(0xdd, 0xe1, 0xea))
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(12)
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
