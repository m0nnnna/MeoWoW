use egui::{Align2, Color32, FontId, Painter};

pub const ACTION_STATUS_FADE_TIME: f32 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct StatusText {
    pub text: String,
    pub elapsed: f32,
}

pub fn draw(painter: &Painter, entry: &StatusText) {
    let alpha = (1.0 - entry.elapsed / ACTION_STATUS_FADE_TIME).clamp(0.0, 1.0);
    if alpha == 0.0 {
        return;
    }
    let alpha = (alpha * 255.0).round() as u8;
    let center = painter.clip_rect().center();
    let font = FontId::proportional(16.0);
    painter.text(
        center + egui::vec2(1.0, -1.0),
        Align2::CENTER_CENTER,
        &entry.text,
        font.clone(),
        Color32::from_rgba_unmultiplied(0, 0, 0, alpha),
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        &entry.text,
        font,
        Color32::from_rgba_unmultiplied(255, 209, 0, alpha),
    );
}
