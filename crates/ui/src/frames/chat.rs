//! The chat scrollback.
//!
//! Unlike a unit frame, this one has to *wrap*, which is the first thing in
//! this crate that cannot be drawn from rectangles and single-line text. egui's
//! galley layout does the wrapping; everything else -- what colour a line is,
//! how many are kept, where the box sits -- stays this crate's decision, so a
//! user who wants pink whispers edits a file rather than patching a binary.
//!
//! Lines are drawn **from the bottom up**. Chat grows downward and the newest
//! line matters most, so laying out from the bottom means a partially visible
//! line is always the *oldest* one on screen, which is the one you can afford
//! to lose.

use egui::{Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::{Color, Style};

/// What kind of line this is, for colouring.
///
/// This crate's own categories rather than the protocol's: several wire types
/// read the same way to a person (a creature's say and a player's say are both
/// "say"), and the interface has no business knowing which opcode carried
/// them. The caller maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Say,
    Yell,
    Whisper,
    Emote,
    System,
    Channel,
    /// Damage this character dealt or took. Its own kind because combat is
    /// the one category that arrives faster than it can be read, so it wants
    /// a colour that recedes rather than one that competes with a whisper.
    Combat,
    Other,
}

/// One line of scrollback.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatEntry {
    pub kind: ChatKind,
    /// `None` for a line with no speaker, such as a system notice.
    pub who: Option<String>,
    pub text: String,
    /// Shown before the speaker: a channel name, or `yell` for a yell.
    pub prefix: Option<String>,
}

impl ChatEntry {
    /// The line as it reads on screen.
    pub fn rendered(&self) -> String {
        let mut line = String::new();
        if let Some(prefix) = &self.prefix {
            line.push('[');
            line.push_str(prefix);
            line.push_str("] ");
        }
        match (&self.who, self.kind) {
            // An emote reads as prose about the speaker rather than as
            // something they said, so it takes no colon.
            (Some(who), ChatKind::Emote) => {
                line.push_str(who);
                line.push(' ');
            }
            (Some(who), _) => {
                line.push_str(who);
                line.push_str(": ");
            }
            (None, _) => {}
        }
        line.push_str(&self.text);
        line
    }
}

/// How much room the chat box wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    Vec2::new(style.chat_width, style.chat_height) * scale
}

/// The colour a kind of line is drawn in.
pub fn colour(kind: ChatKind, style: &Style) -> Color {
    match kind {
        ChatKind::Say => style.chat_say,
        ChatKind::Yell => style.chat_yell,
        ChatKind::Whisper => style.chat_whisper,
        ChatKind::Emote => style.chat_emote,
        ChatKind::System => style.chat_system,
        ChatKind::Channel => style.chat_channel,
        ChatKind::Combat => style.chat_combat,
        ChatKind::Other => style.chat_other,
    }
}

/// Paints the scrollback into `rect`, newest at the bottom.
///
/// `composing` is the line currently being typed, drawn under the scrollback
/// with a caret. It is `None` when the user is not typing.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    lines: &[ChatEntry],
    composing: Option<&str>,
    style: &Style,
    scale: f32,
) {
    let corner = egui::CornerRadius::same((style.corner * scale).round().clamp(0.0, 255.0) as u8);
    painter.rect_filled(rect, corner, style.chat_background);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let inner = rect.shrink(style.padding * scale);
    let painter = painter.with_clip_rect(inner);
    let font = egui::FontId::proportional(style.font_size * scale);
    let mut bottom = inner.bottom();

    // The line being typed sits below everything, where it does not push the
    // scrollback around as it grows.
    if let Some(text) = composing {
        let galley = painter.layout(
            format!("> {text}_"),
            font.clone(),
            style.chat_composing.into(),
            inner.width(),
        );
        bottom -= galley.size().y;
        painter.galley(
            Pos2::new(inner.left(), bottom),
            galley,
            style.chat_composing.into(),
        );
        bottom -= style.gap * scale;
    }

    // Newest first, walking upward, stopping as soon as the box is full. A
    // long scrollback therefore costs no layout for the lines nobody can see.
    for entry in lines.iter().rev() {
        if bottom <= inner.top() {
            break;
        }
        let galley = painter.layout(
            entry.rendered(),
            font.clone(),
            colour(entry.kind, style).into(),
            inner.width(),
        );
        bottom -= galley.size().y;
        painter.galley(
            Pos2::new(inner.left(), bottom),
            galley,
            colour(entry.kind, style).into(),
        );
    }
}

/// Placeholder scrollback, so the box can be positioned before anyone speaks.
pub fn placeholder() -> Vec<ChatEntry> {
    vec![
        ChatEntry {
            kind: ChatKind::System,
            who: None,
            text: "Welcome to MeoWoW.".into(),
            prefix: None,
        },
        ChatEntry {
            kind: ChatKind::Say,
            who: Some("Testwolf".into()),
            text: "drag me somewhere better".into(),
            prefix: None,
        },
        ChatEntry {
            kind: ChatKind::Emote,
            who: Some("Watcher".into()),
            text: "waves.".into(),
            prefix: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spoken_line_reads_as_speech() {
        let entry = ChatEntry {
            kind: ChatKind::Say,
            who: Some("Testwolf".into()),
            text: "hello".into(),
            prefix: None,
        };
        assert_eq!(entry.rendered(), "Testwolf: hello");
    }

    /// An emote is prose about the speaker, not something they said, so it
    /// must not gain a colon.
    #[test]
    fn an_emote_reads_as_prose() {
        let entry = ChatEntry {
            kind: ChatKind::Emote,
            who: Some("Watcher".into()),
            text: "waves.".into(),
            prefix: None,
        };
        assert_eq!(entry.rendered(), "Watcher waves.");
    }

    #[test]
    fn a_channel_line_names_its_channel() {
        let entry = ChatEntry {
            kind: ChatKind::Channel,
            who: Some("Watcher".into()),
            text: "anyone selling?".into(),
            prefix: Some("General".into()),
        };
        assert_eq!(entry.rendered(), "[General] Watcher: anyone selling?");
    }

    /// A system line has no speaker and must not render a stray separator.
    #[test]
    fn a_speakerless_line_has_no_separator() {
        let entry = ChatEntry {
            kind: ChatKind::System,
            who: None,
            text: "Server restarting.".into(),
            prefix: None,
        };
        assert_eq!(entry.rendered(), "Server restarting.");
    }

    /// Every kind must have a colour of its own, or two kinds are
    /// indistinguishable and one of them looks broken.
    #[test]
    fn every_kind_has_its_own_colour() {
        let style = Style::default();
        let kinds = [
            ChatKind::Say,
            ChatKind::Yell,
            ChatKind::Whisper,
            ChatKind::Emote,
            ChatKind::System,
            ChatKind::Channel,
            ChatKind::Other,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(
                    colour(*a, &style),
                    colour(*b, &style),
                    "{a:?} and {b:?} are the same colour"
                );
            }
        }
    }
}
