//! What the interface looks like.
//!
//! Every dimension and colour a frame draws with comes from here, and this
//! whole struct is written to the layout file. That is the point: the client
//! has no fixed appearance to reverse-engineer or patch around, and retheming
//! it is editing a text file rather than replacing art.
//!
//! Colours are stored as `#rrggbb` or `#rrggbbaa` strings rather than as
//! arrays of numbers, because the audience for this file is a person with a
//! colour picker open.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A straight (non-premultiplied) sRGB colour with alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([r, g, b, a])
    }

    /// Parses `#rrggbb`, `#rrggbbaa`, or either without the `#`.
    pub fn from_hex(text: &str) -> Result<Self, String> {
        let digits = text.strip_prefix('#').unwrap_or(text);
        if digits.len() != 6 && digits.len() != 8 {
            return Err(format!(
                "{text:?} is not a colour: expected #rrggbb or #rrggbbaa"
            ));
        }
        let mut bytes = [255u8; 4];
        for (i, pair) in digits.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(pair).map_err(|_| format!("{text:?} is not a colour"))?;
            bytes[i] = u8::from_str_radix(pair, 16)
                .map_err(|_| format!("{text:?} is not a colour: {pair:?} is not hex"))?;
        }
        Ok(Self(bytes))
    }

    /// Round-trips [`Color::from_hex`], omitting a fully opaque alpha so the
    /// common case reads as an ordinary six-digit colour.
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.0;
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    /// Mixes towards `other`, for the low-health tint and the edit highlight.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let mut out = [0u8; 4];
        for i in 0..4 {
            out[i] = (self.0[i] as f32 + (other.0[i] as f32 - self.0[i] as f32) * t).round() as u8;
        }
        Color(out)
    }
}

impl From<Color> for egui::Color32 {
    fn from(c: Color) -> Self {
        let [r, g, b, a] = c.0;
        egui::Color32::from_rgba_unmultiplied(r, g, b, a)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Color::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// What a unit spends to act. The id is the value packed into the unit's
/// `BYTES_0` field, and it selects both which power field holds the current
/// value and what colour the bar is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PowerType {
    #[default]
    Mana,
    Rage,
    Focus,
    Energy,
    Happiness,
    Runes,
    RunicPower,
    /// Anything the server reports that this client has no name for. Kept
    /// rather than folded into mana, so an unfamiliar value shows up as an
    /// unfamiliar bar instead of a plausible wrong one.
    Other(u8),
}

impl PowerType {
    pub fn from_id(id: u8) -> Self {
        match id {
            0 => PowerType::Mana,
            1 => PowerType::Rage,
            2 => PowerType::Focus,
            3 => PowerType::Energy,
            4 => PowerType::Happiness,
            5 => PowerType::Runes,
            6 => PowerType::RunicPower,
            other => PowerType::Other(other),
        }
    }

    /// Which of the seven power fields holds this type's current value.
    ///
    /// The powers are a parallel array indexed by the type itself, which is
    /// why the id has to be read before the value means anything: reading
    /// `POWER1` unconditionally reports a rogue's mana, which is always zero.
    pub fn field_index(self) -> u16 {
        match self {
            PowerType::Mana => 0,
            PowerType::Rage => 1,
            PowerType::Focus => 2,
            PowerType::Energy => 3,
            PowerType::Happiness => 4,
            PowerType::Runes => 5,
            PowerType::RunicPower => 6,
            PowerType::Other(id) => id as u16,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PowerType::Mana => "mana",
            PowerType::Rage => "rage",
            PowerType::Focus => "focus",
            PowerType::Energy => "energy",
            PowerType::Happiness => "happiness",
            PowerType::Runes => "runes",
            PowerType::RunicPower => "runic power",
            PowerType::Other(_) => "power",
        }
    }
}

/// Every dimension and colour the frames draw with.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
// Container-level default: a layout file may override one colour and inherit
// the rest, which is what makes hand-editing this bearable.
#[serde(default, deny_unknown_fields)]
pub struct Style {
    /// Width of a unit frame at scale 1.0.
    pub frame_width: f32,
    pub bar_height: f32,
    pub padding: f32,
    /// Gap between the name line and the bars, and between the bars.
    pub gap: f32,
    pub corner: f32,
    pub font_size: f32,
    pub border_width: f32,

    pub background: Color,
    pub border: Color,
    pub text: Color,
    /// Behind the filled part of every bar, so an empty bar still reads as a
    /// bar rather than as a gap in the frame.
    pub bar_backdrop: Color,

    pub health: Color,
    /// Blended into `health` as the fraction falls below `low_health`.
    pub health_low: Color,
    pub low_health: f32,

    pub mana: Color,
    pub rage: Color,
    pub focus: Color,
    pub energy: Color,
    pub happiness: Color,
    pub runes: Color,
    pub runic_power: Color,
    pub other_power: Color,

    /// Draws `1234 / 5678` across each bar.
    pub show_values: bool,
    /// Draws the percentage at the right-hand end of each bar.
    pub show_percent: bool,

    /// Outline drawn around every element while the layout is being edited.
    pub edit_highlight: Color,

    /// Side of one action slot, before scale.
    pub slot_size: f32,
    pub slot_gap: f32,
    pub slot_background: Color,
    /// Outline of a slot with nothing in it, so an empty bar still reads as a
    /// bar rather than as a gap.
    pub slot_empty_border: Color,
    /// The key printed in the slot's corner.
    pub slot_binding: Color,

    /// The spellbook panel at scale 1.0, and the height of one spell in it.
    /// Its own row height rather than the action bar's `slot_size`: the two
    /// are independently tunable, and a book of bar-sized icons would show
    /// four spells at a time.
    /// Room beside a character-panel slot for its name. Its own dimension
    /// rather than a multiple of the slot size: the longest label
    /// ('Shoulders') sets it, and slot size is tuned for icons.
    pub character_label_width: f32,
    /// How wide the loot window is. Its own dimension because loot rows hold
    /// an item name and a count where a spellbook row holds a name and a rank.
    pub loot_width: f32,
    pub spellbook_width: f32,
    pub spellbook_height: f32,
    pub spellbook_row: f32,
    pub spellbook_background: Color,
    /// Behind the spell currently picked up, waiting to be put on a bar.
    pub spellbook_selected: Color,

    /// Width of the cast bar at scale 1.0. Reuses `bar_height` for its
    /// thickness -- a cast bar is one bar, the same shape as a health bar
    /// turned wide, and does not need a dimension of its own for that.
    pub cast_bar_width: f32,
    /// Fill colour of the cast bar as it progresses.
    pub casting: Color,

    pub chat_width: f32,
    pub chat_height: f32,
    pub chat_background: Color,
    /// How many lines of scrollback to keep. A chat log is the one thing here
    /// that grows without bound if nobody caps it.
    pub chat_scrollback: usize,
    pub chat_say: Color,
    pub chat_yell: Color,
    pub chat_whisper: Color,
    pub chat_emote: Color,
    pub chat_system: Color,
    /// The name of a unit with no health left. See `UnitView::is_dead` for
    /// why an empty bar alone is not enough to say so.
    pub text_dead: Color,
    pub chat_channel: Color,
    /// Damage dealt and taken. Deliberately dimmer than speech: combat fills
    /// the scrollback faster than anything else, and a colour that competes
    /// with a whisper makes the whisper unreadable during a fight.
    pub chat_combat: Color,
    pub chat_other: Color,
    /// The line currently being typed.
    pub chat_composing: Color,

    /// Corner ticks drawn around the selected unit, out in the world.
    pub target_marker: Color,
    pub target_marker_width: f32,
    /// Whether to draw them at all. A `Style` toggle rather than an element,
    /// because the marker follows a creature rather than sitting at an anchor
    /// -- see [`crate::frames::marker`].
    pub show_target_marker: bool,

    /// Font size of a floating damage number at spawn, before scale.
    pub combat_text_size: f32,
    /// How far a number travels upward over its lifetime, in points at
    /// scale 1.0. Fading is derived from the same age, not a separate value:
    /// one number rising and dimming together reads as one animation rather
    /// than two that happen to overlap.
    pub combat_text_rise: f32,
    /// How long a number takes to rise and fade completely. A `Style` field
    /// rather than a constant for the same reason `chat_scrollback` is: it
    /// governs behaviour a user might want to retune, not just a colour.
    pub combat_text_lifetime_ms: u64,
    pub combat_text_damage: Color,
    /// A critical hit's number, drawn larger as well as in this colour --
    /// see [`crate::frames::combat_text::draw`].
    pub combat_text_critical: Color,
    pub combat_text_miss: Color,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            frame_width: 230.0,
            bar_height: 18.0,
            padding: 6.0,
            gap: 4.0,
            corner: 4.0,
            font_size: 13.0,
            border_width: 1.0,

            // Dark and slightly transparent, so the frame sits over the world
            // without hiding it.
            background: Color::rgba(16, 18, 24, 210),
            border: Color::rgba(120, 130, 150, 160),
            text: Color::rgb(232, 236, 244),
            bar_backdrop: Color::rgba(0, 0, 0, 150),

            health: Color::rgb(58, 168, 74),
            health_low: Color::rgb(190, 58, 48),
            low_health: 0.35,

            mana: Color::rgb(58, 106, 200),
            rage: Color::rgb(190, 58, 48),
            focus: Color::rgb(190, 130, 60),
            energy: Color::rgb(214, 200, 70),
            happiness: Color::rgb(90, 190, 120),
            runes: Color::rgb(120, 180, 200),
            runic_power: Color::rgb(80, 160, 220),
            other_power: Color::rgb(140, 140, 150),

            show_values: true,
            show_percent: false,

            edit_highlight: Color::rgb(240, 190, 70),

            slot_size: 38.0,
            slot_gap: 4.0,
            slot_background: Color::rgba(24, 27, 34, 200),
            slot_empty_border: Color::rgba(120, 130, 150, 70),
            slot_binding: Color::rgba(230, 235, 245, 200),

            character_label_width: 62.0,
            loot_width: 200.0,
            spellbook_width: 250.0,
            spellbook_height: 320.0,
            spellbook_row: 30.0,
            spellbook_background: Color::rgba(16, 18, 24, 235),
            spellbook_selected: Color::rgba(240, 190, 70, 90),

            cast_bar_width: 260.0,
            casting: Color::rgb(220, 170, 60),

            chat_width: 440.0,
            chat_height: 170.0,
            chat_background: Color::rgba(8, 10, 14, 160),
            chat_scrollback: 200,
            chat_say: Color::rgb(236, 240, 246),
            chat_yell: Color::rgb(240, 90, 70),
            chat_whisper: Color::rgb(226, 120, 226),
            chat_emote: Color::rgb(240, 150, 80),
            chat_system: Color::rgb(240, 220, 100),
            text_dead: Color::rgb(140, 130, 130),
            chat_channel: Color::rgb(120, 210, 190),
            chat_combat: Color::rgb(190, 155, 120),
            chat_other: Color::rgb(170, 176, 190),
            chat_composing: Color::rgb(150, 235, 150),

            target_marker: Color::rgb(240, 225, 130),
            target_marker_width: 2.0,
            show_target_marker: true,

            combat_text_size: 20.0,
            combat_text_rise: 40.0,
            combat_text_lifetime_ms: 1200,
            combat_text_damage: Color::rgb(255, 226, 120),
            combat_text_critical: Color::rgb(255, 110, 40),
            combat_text_miss: Color::rgb(200, 202, 214),
        }
    }
}

impl Style {
    pub fn power_color(&self, power: PowerType) -> Color {
        match power {
            PowerType::Mana => self.mana,
            PowerType::Rage => self.rage,
            PowerType::Focus => self.focus,
            PowerType::Energy => self.energy,
            PowerType::Happiness => self.happiness,
            PowerType::Runes => self.runes,
            PowerType::RunicPower => self.runic_power,
            PowerType::Other(_) => self.other_power,
        }
    }

    /// The health colour at a given fraction, reddening as it falls.
    pub fn health_color(&self, fraction: f32) -> Color {
        if self.low_health <= 0.0 || fraction >= self.low_health {
            return self.health;
        }
        // Fully `health_low` at empty, fully `health` at the threshold.
        let t = 1.0 - (fraction / self.low_health).clamp(0.0, 1.0);
        self.health.mix(self.health_low, t)
    }

    /// Clamps hand-edited dimensions to something drawable. Returns whether
    /// anything had to change.
    pub fn sanitise(&mut self) -> bool {
        let before = *self;
        self.frame_width = self.frame_width.clamp(60.0, 1200.0);
        self.bar_height = self.bar_height.clamp(4.0, 200.0);
        self.padding = self.padding.clamp(0.0, 60.0);
        self.gap = self.gap.clamp(0.0, 60.0);
        self.corner = self.corner.clamp(0.0, 40.0);
        self.font_size = self.font_size.clamp(6.0, 72.0);
        self.border_width = self.border_width.clamp(0.0, 12.0);
        self.low_health = self.low_health.clamp(0.0, 1.0);
        self.target_marker_width = self.target_marker_width.clamp(0.5, 12.0);
        self.slot_size = self.slot_size.clamp(12.0, 200.0);
        self.slot_gap = self.slot_gap.clamp(0.0, 40.0);
        self.cast_bar_width = self.cast_bar_width.clamp(60.0, 1200.0);
        self.character_label_width = self.character_label_width.clamp(0.0, 400.0);
        self.loot_width = self.loot_width.clamp(100.0, 900.0);
        self.spellbook_width = self.spellbook_width.clamp(120.0, 1200.0);
        self.spellbook_height = self.spellbook_height.clamp(60.0, 1600.0);
        self.spellbook_row = self.spellbook_row.clamp(10.0, 200.0);
        self.chat_width = self.chat_width.clamp(120.0, 2000.0);
        self.chat_height = self.chat_height.clamp(40.0, 1200.0);
        self.chat_scrollback = self.chat_scrollback.clamp(10, 10_000);
        self.combat_text_size = self.combat_text_size.clamp(6.0, 72.0);
        self.combat_text_rise = self.combat_text_rise.clamp(0.0, 400.0);
        self.combat_text_lifetime_ms = self.combat_text_lifetime_ms.clamp(100, 10_000);
        before != *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file is meant to be hand-edited, so every form a person would
    /// plausibly type has to parse, and what we write back has to be one of
    /// them.
    #[test]
    fn colours_round_trip_through_hex() {
        for (text, expected) in [
            ("#3aa84a", Color::rgb(58, 168, 74)),
            ("3aa84a", Color::rgb(58, 168, 74)),
            ("#101218d2", Color::rgba(16, 18, 24, 210)),
            ("#FFFFFF", Color::rgb(255, 255, 255)),
        ] {
            let parsed = Color::from_hex(text).unwrap();
            assert_eq!(parsed, expected, "{text}");
            assert_eq!(
                Color::from_hex(&parsed.to_hex()).unwrap(),
                expected,
                "{text} did not survive being written back"
            );
        }
    }

    #[test]
    fn nonsense_colours_are_rejected_rather_than_guessed() {
        for text in ["", "#abc", "#gggggg", "red", "#1234567890"] {
            assert!(Color::from_hex(text).is_err(), "{text:?} was accepted");
        }
    }

    /// The whole style must survive a trip through the file it lives in,
    /// because that trip happens on every save.
    #[test]
    fn the_default_style_round_trips_through_toml() {
        let style = Style::default();
        let text = toml::to_string_pretty(&style).unwrap();
        let parsed: Style = toml::from_str(&text).unwrap();
        assert_eq!(parsed, style);
    }

    #[test]
    fn a_partial_style_inherits_the_rest() {
        let style: Style = toml::from_str(r##"health = "#ff00ff""##).unwrap();
        assert_eq!(style.health, Color::rgb(255, 0, 255));
        assert_eq!(style.frame_width, Style::default().frame_width);
    }

    /// Reading `POWER1` regardless of type is the obvious mistake here: it
    /// would report every rogue and warrior as having zero of a resource they
    /// do not use, which looks like a replication failure rather than a
    /// misread field.
    #[test]
    fn power_types_index_their_own_field() {
        assert_eq!(PowerType::from_id(0), PowerType::Mana);
        assert_eq!(PowerType::Mana.field_index(), 0);
        assert_eq!(PowerType::from_id(3), PowerType::Energy);
        assert_eq!(PowerType::Energy.field_index(), 3);
        assert_eq!(PowerType::from_id(6), PowerType::RunicPower);
        assert_eq!(PowerType::RunicPower.field_index(), 6);
        // An id this client has no name for keeps its number rather than
        // being folded into mana.
        assert_eq!(PowerType::from_id(9), PowerType::Other(9));
        assert_eq!(PowerType::Other(9).field_index(), 9);
    }

    #[test]
    fn health_reddens_as_it_falls() {
        let style = Style::default();
        assert_eq!(style.health_color(1.0), style.health);
        assert_eq!(style.health_color(style.low_health), style.health);
        assert_eq!(style.health_color(0.0), style.health_low);
        let middle = style.health_color(style.low_health / 2.0);
        assert_ne!(middle, style.health);
        assert_ne!(middle, style.health_low);
    }
}
