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
use egui::text::{LayoutJob, TextFormat};

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
    /// Party chat. Its own kind rather than folded into `Other`: a line said
    /// to five people and a line said to everyone in earshot read the same
    /// to a person only if the interface makes them look the same, which is
    /// the one thing a party's privacy assumption cannot survive.
    Party,
    /// Guild chat. Its own kind for the same reason `Party` is, and it is
    /// worth recording *why it was missing*: the wire type existed, the
    /// parser produced it, and both maps from `world::ChatType` to this enum
    /// simply had no arm for it -- so a guild line drew in `Other`'s grey
    /// with no tag, which against `Say`'s near-white is a difference nobody
    /// can see. **A line that renders as a plausible different line is the
    /// chat frame's version of a wrong animation id**: it never errors.
    Guild,
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

#[derive(Debug, Clone, PartialEq)]
struct ChatSpan {
    text: String,
    colour: egui::Color32,
}

fn push_span(spans: &mut Vec<ChatSpan>, text: &mut String, colour: egui::Color32) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.colour == colour {
            last.text.push_str(text);
            text.clear();
            return;
        }
    }
    spans.push(ChatSpan { text: std::mem::take(text), colour });
}

fn markup_spans(text: &str, default: egui::Color32) -> Vec<ChatSpan> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut visible = String::new();
    let mut colour = default;
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'|' || offset + 1 >= bytes.len() {
            let character = text[offset..].chars().next().unwrap();
            visible.push(character);
            offset += character.len_utf8();
            continue;
        }
        match bytes[offset + 1] {
            b'c' if offset + 10 <= bytes.len()
                && bytes[offset + 2..offset + 10]
                    .iter()
                    .all(|byte| byte.is_ascii_hexdigit()) =>
            {
                push_span(&mut spans, &mut visible, colour);
                let value = u32::from_str_radix(std::str::from_utf8(&bytes[offset + 2..offset + 10]).unwrap(), 16).unwrap();
                colour = egui::Color32::from_rgba_unmultiplied(
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    value as u8,
                    (value >> 24) as u8,
                );
                offset += 10;
            }
            b'r' => {
                push_span(&mut spans, &mut visible, colour);
                colour = default;
                offset += 2;
            }
            b'n' => {
                visible.push('\n');
                offset += 2;
            }
            b'|' => {
                visible.push('|');
                offset += 2;
            }
            b'H' => {
                let Some(metadata_end) = text[offset + 2..].find("|h").map(|end| offset + 2 + end) else {
                    visible.push('|');
                    offset += 1;
                    continue;
                };
                let display_start = metadata_end + 2;
                let Some(display_end) = text[display_start..].find("|h").map(|end| display_start + end) else {
                    visible.push('|');
                    offset += 1;
                    continue;
                };
                visible.push_str(&text[display_start..display_end]);
                offset = display_end + 2;
            }
            b'T' => {
                let Some(texture_end) = text[offset + 2..].find("|t").map(|end| offset + 2 + end) else {
                    visible.push('|');
                    offset += 1;
                    continue;
                };
                push_span(&mut spans, &mut visible, colour);
                offset = texture_end + 2;
            }
            b'h' | b't' => offset += 2,
            _ => {
                visible.push('|');
                offset += 1;
            }
        }
    }
    push_span(&mut spans, &mut visible, colour);
    spans
}

fn append_text(job: &mut LayoutJob, text: &str, font: &egui::FontId, colour: egui::Color32) {
    if !text.is_empty() {
        job.append(text, 0.0, TextFormat { font_id: font.clone(), color: colour, ..Default::default() });
    }
}

fn entry_layout(entry: &ChatEntry, font: &egui::FontId, default: egui::Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    if let Some(prefix) = &entry.prefix {
        append_text(&mut job, "[", font, default);
        append_text(&mut job, prefix, font, default);
        append_text(&mut job, "] ", font, default);
    }
    match (&entry.who, entry.kind) {
        (Some(who), ChatKind::Emote) => {
            append_text(&mut job, who, font, default);
            append_text(&mut job, " ", font, default);
        }
        (Some(who), _) => {
            append_text(&mut job, who, font, default);
            append_text(&mut job, ": ", font, default);
        }
        (None, _) => {}
    }
    for span in markup_spans(&entry.text, default) {
        append_text(&mut job, &span.text, font, span.colour);
    }
    job
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
        for span in markup_spans(&self.text, egui::Color32::WHITE) {
            line.push_str(&span.text);
        }
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
        ChatKind::Party => style.chat_party,
        ChatKind::Guild => style.chat_guild,
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
        let default = colour(entry.kind, style).into();
        let galley = painter.layout_job(entry_layout(entry, &font, default, inner.width()));
        bottom -= galley.size().y;
        painter.galley(
            Pos2::new(inner.left(), bottom),
            galley,
            default,
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

    #[test]
    fn a_party_line_names_itself() {
        let entry = ChatEntry {
            kind: ChatKind::Party,
            who: Some("Watcher".into()),
            text: "on my way".into(),
            prefix: Some("party".into()),
        };
        assert_eq!(entry.rendered(), "[party] Watcher: on my way");
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

    #[test]
    fn wow_markup_preserves_link_text_and_colour_runs() {
        let entry = ChatEntry {
            kind: ChatKind::System,
            who: None,
            text: "|cffff0000|Hplayer:Denveous:1:0:0:0:0:0:0|h[Denveous]|h|r's Fly Mode on".into(),
            prefix: None,
        };
        assert_eq!(entry.rendered(), "[Denveous]'s Fly Mode on");
        let spans = markup_spans(&entry.text, egui::Color32::WHITE);
        assert_eq!(spans[0], ChatSpan { text: "[Denveous]".into(), colour: egui::Color32::from_rgb(255, 0, 0) });
        assert_eq!(spans[1], ChatSpan { text: "'s Fly Mode on".into(), colour: egui::Color32::WHITE });
    }

    #[test]
    fn wow_markup_consumes_texture_newline_and_pipe_codes() {
        let spans = markup_spans(r"before|TInterface\Icon\foo:16:16|tafter|nline||tail", egui::Color32::WHITE);
        assert_eq!(spans, vec![ChatSpan { text: "beforeafter\nline|tail".into(), colour: egui::Color32::WHITE }]);
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
            ChatKind::Party,
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
