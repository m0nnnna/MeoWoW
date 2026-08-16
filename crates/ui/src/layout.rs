//! The layout as a whole, and the file it lives in.
//!
//! A [`Profile`] is the complete description of the interface: one [`Element`]
//! per thing that can be drawn, plus the [`Style`] they all draw with. It is
//! the entire customisation surface -- there is no second, hidden set of
//! defaults compiled in somewhere that the file only partly overrides, because
//! a user who cannot see why their change had no effect will conclude the
//! feature does not work.
//!
//! Reading a profile is deliberately forgiving in one direction and strict in
//! the other. Fields a user left out are filled from defaults, and elements
//! this build has never heard of are *reported and dropped* rather than
//! refused -- a layout written by a later build should not stop this one from
//! starting. But a malformed value is an error, because silently substituting
//! something for `scale = "big"` teaches the user that the file is being read
//! when it is not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::element::Element;
use crate::frames::action_bar;
use crate::style::Style;
use crate::Error;

/// Everything the interface can draw, one variant per element.
///
/// Adding a variant here is what adding a piece of interface looks like: it
/// gains a default position, an entry in the file, and a row in the edit
/// window without any of those being written again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ElementId {
    PlayerFrame,
    TargetFrame,
    ChatFrame,
    CastBar,
    ActionBar1,
    ActionBar2,
    ActionBar3,
    Spellbook,
    Bags,
    Character,
    Loot,
    QuestLog,
    ReleasePrompt,
}

impl ElementId {
    pub const ALL: [ElementId; 13] = [
        ElementId::PlayerFrame,
        ElementId::TargetFrame,
        ElementId::ChatFrame,
        ElementId::CastBar,
        ElementId::ActionBar1,
        ElementId::ActionBar2,
        ElementId::ActionBar3,
        ElementId::Spellbook,
        ElementId::Bags,
        ElementId::Character,
        ElementId::Loot,
        ElementId::QuestLog,
        ElementId::ReleasePrompt,
    ];

    /// Which action bar this element is, if it is one.
    pub fn action_bar(self) -> Option<usize> {
        match self {
            ElementId::ActionBar1 => Some(0),
            ElementId::ActionBar2 => Some(1),
            ElementId::ActionBar3 => Some(2),
            _ => None,
        }
    }

    /// The key this element takes in the layout file.
    pub fn key(self) -> &'static str {
        match self {
            ElementId::PlayerFrame => "player-frame",
            ElementId::TargetFrame => "target-frame",
            ElementId::ChatFrame => "chat-frame",
            ElementId::CastBar => "cast-bar",
            ElementId::ActionBar1 => "action-bar-1",
            ElementId::ActionBar2 => "action-bar-2",
            ElementId::ActionBar3 => "action-bar-3",
            ElementId::Spellbook => "spellbook",
            ElementId::Bags => "bags",
            ElementId::Character => "character",
            ElementId::Loot => "loot",
            ElementId::QuestLog => "quest-log",
            ElementId::ReleasePrompt => "release-prompt",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        ElementId::ALL.into_iter().find(|id| id.key() == key)
    }

    /// What the edit window calls it.
    pub fn label(self) -> &'static str {
        match self {
            ElementId::PlayerFrame => "Player frame",
            ElementId::TargetFrame => "Target frame",
            ElementId::ChatFrame => "Chat",
            ElementId::CastBar => "Cast bar",
            ElementId::ActionBar1 => "Action bar (no modifier)",
            ElementId::ActionBar2 => "Action bar (Shift)",
            ElementId::ActionBar3 => "Action bar (Ctrl)",
            ElementId::Spellbook => "Spellbook",
            ElementId::Bags => "Bags",
            ElementId::Character => "Character",
            ElementId::Loot => "Loot",
            ElementId::QuestLog => "Quest log",
            ElementId::ReleasePrompt => "Release-spirit prompt",
        }
    }

    /// Where it sits before anyone has moved it.
    pub fn default_element(self) -> Element {
        use crate::element::Anchor;
        match self {
            ElementId::PlayerFrame => Element {
                anchor: Anchor::TopLeft,
                offset: [24.0, 24.0],
                ..Default::default()
            },
            // Beside the player frame rather than under it, so the two read as
            // a pair and neither covers the other at any scale below 2.
            ElementId::TargetFrame => Element {
                anchor: Anchor::TopLeft,
                offset: [286.0, 24.0],
                ..Default::default()
            },
            // Bottom left, where a chat log has lived in every game that has
            // one -- and anchored there so it stays put when the window
            // resizes rather than drifting off the bottom.
            ElementId::ChatFrame => Element {
                anchor: Anchor::BottomLeft,
                offset: [16.0, -16.0],
                ..Default::default()
            },
            // Centred above the first action bar, with enough clearance that
            // it cannot touch the bar even at the largest hand-edited scale
            // either one is likely to use.
            ElementId::CastBar => Element {
                anchor: Anchor::Bottom,
                offset: [0.0, -74.0],
                ..Default::default()
            },
            // Stacked upward from the bottom centre. Only the unmodified bar
            // is shown to begin with -- three rows of mostly empty slots is a
            // worse first impression than one, and the other two are a
            // checkbox away in the edit window.
            ElementId::ActionBar1 => Element {
                anchor: Anchor::Bottom,
                offset: [0.0, -16.0],
                ..Default::default()
            },
            ElementId::ActionBar2 => Element {
                anchor: Anchor::Bottom,
                offset: [0.0, -68.0],
                visible: false,
                ..Default::default()
            },
            ElementId::ActionBar3 => Element {
                anchor: Anchor::Bottom,
                offset: [0.0, -120.0],
                visible: false,
                ..Default::default()
            },
            // Against the right edge, clear of everything else the default
            // layout draws -- the book is opened over the world mid-play and
            // covering the chat log or the bars with it would be a poor trade.
            // `visible` stays true because it means "this element may be
            // drawn", not "it is on screen now": the book only appears while
            // it is open, the same way the target frame only appears with a
            // target. Unticking it in the edit window switches the book off
            // altogether.
            ElementId::Spellbook => Element {
                anchor: Anchor::Right,
                offset: [-24.0, 0.0],
                ..Default::default()
            },
            // Bottom right, the one corner the default layout leaves free, and
            // the corner a bag window has traditionally occupied. Deliberately
            // not beside the spellbook: both are opened by a keypress and a
            // player may well have both open, so overlapping defaults would
            // make the first thing anyone does with this window be moving it.
            ElementId::Bags => Element {
                anchor: Anchor::BottomRight,
                offset: [-24.0, -16.0],
                ..Default::default()
            },
            // Left of centre, clear of the bag window it is most often opened
            // beside: comparing a worn item against one in the bags is the
            // whole reason both would be on screen at once, so overlapping
            // defaults would defeat the pairing.
            ElementId::Character => Element {
                anchor: Anchor::Left,
                offset: [24.0, 0.0],
                ..Default::default()
            },
            // Near the middle, where the eye already is. Unlike every other
            // window here this one is not opened by a keypress -- it appears
            // because the player clicked a corpse -- so it has to be somewhere
            // they are already looking rather than somewhere they have learned
            // to check.
            ElementId::Loot => Element {
                anchor: Anchor::Center,
                offset: [0.0, -40.0],
                ..Default::default()
            },
            // Also centred, like the loot window it can in principle share
            // the screen with (a creature's corpse looted moments before this
            // character's own death) -- offset the other way so the two do
            // not start out overlapping.
            ElementId::ReleasePrompt => Element {
                anchor: Anchor::Center,
                offset: [0.0, 60.0],
                ..Default::default()
            },
            // Top right, the one edge nothing else claims -- the spellbook is
            // centred on the right edge and the bags sit in the corner below
            // it. A quest log is read *while* moving rather than stopped, so
            // it wants a corner and not the middle of the view.
            ElementId::QuestLog => Element {
                anchor: Anchor::TopRight,
                offset: [-24.0, 24.0],
                ..Default::default()
            },
        }
    }
}

/// What is on the action bars.
///
/// Part of the layout rather than of game state, because this client has
/// nowhere else to keep it: the server stores action bars per character, and
/// this one does not speak that yet. A spell id of zero means an empty slot --
/// plain integer arrays read far better in a hand-edited file than a column of
/// `Option`s would.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActionBars {
    pub bars: Vec<Vec<u32>>,
}

impl Default for ActionBars {
    fn default() -> Self {
        Self {
            bars: vec![vec![0; action_bar::SLOTS]; action_bar::BARS],
        }
    }
}

impl ActionBars {
    pub fn get(&self, bar: usize, slot: usize) -> Option<u32> {
        self.bars.get(bar)?.get(slot).copied().filter(|id| *id != 0)
    }

    pub fn set(&mut self, bar: usize, slot: usize, spell: Option<u32>) {
        if let Some(row) = self.bars.get_mut(bar) {
            if let Some(cell) = row.get_mut(slot) {
                *cell = spell.unwrap_or(0);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bars.iter().flatten().all(|id| *id == 0)
    }

    /// Forces the grid to the shape this build expects.
    ///
    /// A hand-written file can hold any shape at all, and every lookup here
    /// would otherwise need to cope with a ragged one. Returns whether
    /// anything had to change, so the caller can say so.
    pub fn sanitise(&mut self) -> bool {
        let before = self.clone();
        self.bars.resize(action_bar::BARS, Vec::new());
        for row in &mut self.bars {
            row.resize(action_bar::SLOTS, 0);
        }
        before != *self
    }
}

/// The complete interface layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub style: Style,
    pub bars: ActionBars,
    /// How the camera behaves. Not a frame, and not drawn by this crate at
    /// all -- it lives here because this profile is the one thing written to
    /// `ui.toml`, and a setting the player changes is one they expect to still
    /// be there tomorrow.
    pub camera: crate::camera::Camera,
    elements: BTreeMap<ElementId, Element>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            style: Style::default(),
            bars: ActionBars::default(),
            camera: crate::camera::Camera::default(),
            elements: ElementId::ALL
                .into_iter()
                .map(|id| (id, id.default_element()))
                .collect(),
        }
    }
}

/// The on-disk shape. Separate from [`Profile`] because the file keys elements
/// by string, and this build has to survive reading a string it does not know.
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Stored {
    style: Style,
    bars: ActionBars,
    camera: crate::camera::Camera,
    elements: BTreeMap<String, Element>,
}

impl Default for Stored {
    fn default() -> Self {
        Self {
            style: Style::default(),
            bars: ActionBars::default(),
            camera: crate::camera::Camera::default(),
            elements: BTreeMap::new(),
        }
    }
}

impl Profile {
    /// The element's placement, falling back to its default if the profile
    /// somehow lacks it.
    ///
    /// Returns by value rather than by reference so there is no lookup that
    /// can fail: a missing entry means "never customised", which is a normal
    /// state and not one worth a panic or an `Option` at every call site.
    pub fn get(&self, id: ElementId) -> Element {
        self.elements
            .get(&id)
            .copied()
            .unwrap_or_else(|| id.default_element())
    }

    pub fn set(&mut self, id: ElementId, element: Element) {
        self.elements.insert(id, element);
    }

    /// Edits an element in place, inserting its default first if it is absent.
    pub fn edit(&mut self, id: ElementId) -> &mut Element {
        self.elements
            .entry(id)
            .or_insert_with(|| id.default_element())
    }

    pub fn reset(&mut self) {
        *self = Profile::default();
    }

    pub fn reset_element(&mut self, id: ElementId) {
        self.set(id, id.default_element());
    }

    pub fn to_toml(&self) -> Result<String, Error> {
        let stored = Stored {
            style: self.style,
            bars: self.bars.clone(),
            camera: self.camera,
            elements: self
                .elements
                .iter()
                .map(|(id, element)| (id.key().to_string(), *element))
                .collect(),
        };
        Ok(toml::to_string_pretty(&stored)?)
    }

    /// Parses a layout, returning it alongside anything that had to be
    /// corrected or ignored.
    ///
    /// The warnings are the point of the second return value: an element this
    /// build does not know about, or a scale that had to be clamped, is a
    /// disagreement between the file and what is on screen. Silently winning
    /// that disagreement is how a customisation feature earns a reputation for
    /// not working.
    pub fn from_toml(text: &str) -> Result<(Self, Vec<String>), Error> {
        let stored: Stored = toml::from_str(text)?;
        let mut warnings = Vec::new();

        let mut style = stored.style;
        if style.sanitise() {
            warnings.push("some style dimensions were out of range and were clamped".to_string());
        }

        let mut bars = stored.bars;
        if bars.sanitise() {
            warnings.push("the action bars were not the expected shape and were resized".into());
        }

        // Sanitised on the way *out* rather than here -- see
        // `Camera::radians_per_pixel`, which clamps every time it is asked.
        // A camera setting is read once a frame from a file a person can type
        // into, so the guard belongs at the point of use, where it cannot be
        // skipped by a caller that built the struct some other way.
        let mut profile = Profile {
            style,
            bars,
            camera: stored.camera,
            elements: BTreeMap::new(),
        };
        for (key, mut element) in stored.elements {
            let Some(id) = ElementId::from_key(&key) else {
                warnings.push(format!("ignored unknown element {key:?}"));
                continue;
            };
            if element.sanitise() {
                warnings.push(format!("{key}: out-of-range values were clamped"));
            }
            profile.elements.insert(id, element);
        }
        Ok((profile, warnings))
    }

    pub fn load(path: &Path) -> Result<(Self, Vec<String>), Error> {
        let text = std::fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    /// Writes the layout, creating the directory if needed.
    ///
    /// Written to a sibling file and renamed over the target rather than
    /// truncated in place. This project has already destroyed one file by
    /// writing it non-atomically and failing partway (see `CLAUDE.md`); the
    /// same failure here would cost a user their layout at the moment they
    /// tried to save it.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = self.to_toml()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("toml.new");
        std::fs::write(&temporary, text.as_bytes())?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }
}

/// Where the layout file lives.
///
/// `%APPDATA%\open-wow\ui.toml` on Windows, `$XDG_CONFIG_HOME/open-wow` or
/// `~/.config/open-wow` elsewhere. Probed by environment rather than by
/// `cfg!(windows)` so the same code answers correctly under either.
pub fn default_path() -> Result<PathBuf, Error> {
    let directory = if let Some(appdata) = env_path("APPDATA") {
        appdata
    } else if let Some(xdg) = env_path("XDG_CONFIG_HOME") {
        xdg
    } else if let Some(home) = env_path("HOME") {
        home.join(".config")
    } else {
        return Err(Error::NoConfigDirectory);
    };
    Ok(directory.join("open-wow").join("ui.toml"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Anchor;

    /// Save then load is the round trip every customisation depends on. It
    /// runs on every edit, and a field that silently fails to survive it looks
    /// exactly like a setting that does not take effect.
    #[test]
    fn a_customised_profile_survives_the_file() {
        let mut profile = Profile::default();
        profile.style.health = crate::style::Color::rgb(1, 2, 3);
        profile.style.show_percent = true;
        profile.style.frame_width = 300.0;
        profile.edit(ElementId::TargetFrame).anchor = Anchor::BottomRight;
        profile.edit(ElementId::TargetFrame).offset = [-40.0, -120.0];
        profile.edit(ElementId::TargetFrame).scale = 1.25;
        profile.edit(ElementId::PlayerFrame).visible = false;

        let text = profile.to_toml().unwrap();
        let (parsed, warnings) = Profile::from_toml(&text).unwrap();
        assert_eq!(parsed, profile);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Camera preferences survive a save and a reload.
    ///
    /// The whole reason they live in this profile rather than in the viewer is
    /// that they are written to disk, so the round trip *is* the feature. A
    /// field added to `Profile` and forgotten in `Stored` compiles, runs, and
    /// silently resets every time the client restarts.
    #[test]
    fn camera_settings_round_trip() {
        let mut profile = Profile::default();
        profile.camera.turn_per_window = 320.0;
        profile.camera.distance = 14.5;
        profile.camera.invert_pitch = true;

        let text = profile.to_toml().unwrap();
        assert!(
            text.contains("turn_per_window"),
            "the camera was not written to the file at all:\n{text}"
        );
        let (parsed, warnings) = Profile::from_toml(&text).unwrap();
        assert_eq!(parsed.camera, profile.camera);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A file from before the camera existed still loads, and gets the
    /// defaults rather than an error.
    ///
    /// Every `ui.toml` already on a disk somewhere is one of these.
    #[test]
    fn a_file_without_a_camera_section_still_loads() {
        let (parsed, warnings) = Profile::from_toml("[style]\nfont_size = 18.0\n").unwrap();
        assert_eq!(parsed.camera, crate::camera::Camera::default());
        assert_eq!(parsed.style.font_size, 18.0);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A file written by a later build must not stop this one from starting,
    /// but the element it could not place has to be reported rather than
    /// quietly dropped.
    #[test]
    fn an_unknown_element_is_reported_and_skipped() {
        // **A key chosen so it can never become real.** This test has been
        // broken twice by its own example getting built: it used `spellbook`
        // until the spellbook existed, then `quest-log` until the quest log
        // did. Naming a planned feature guarantees a third time, so the
        // example is now a key nothing will ever claim.
        let text = r#"
            [elements.player-frame]
            offset = [10.0, 10.0]

            [elements.no-such-element]
            offset = [0.0, 0.0]
        "#;
        let (profile, warnings) = Profile::from_toml(text).unwrap();
        assert_eq!(profile.get(ElementId::PlayerFrame).offset, [10.0, 10.0]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no-such-element"), "{warnings:?}");
    }

    /// An element the file never mentions falls back to where it belongs,
    /// rather than to the origin.
    #[test]
    fn an_absent_element_keeps_its_default_place() {
        let (profile, warnings) = Profile::from_toml("").unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            profile.get(ElementId::TargetFrame),
            ElementId::TargetFrame.default_element()
        );
        assert_eq!(profile.style, Style::default());
    }

    /// Forgiving about omissions, strict about nonsense: a value that cannot
    /// mean anything is an error the user gets told about, not a default
    /// substituted behind their back.
    #[test]
    fn a_malformed_value_is_an_error() {
        let text = r#"
            [elements.player-frame]
            scale = "big"
        "#;
        assert!(Profile::from_toml(text).is_err());
    }

    /// A hand-typed scale that would fill the screen is clamped, and said so.
    #[test]
    fn an_out_of_range_value_is_clamped_and_reported() {
        let text = r#"
            [elements.player-frame]
            scale = 100.0
        "#;
        let (profile, warnings) = Profile::from_toml(text).unwrap();
        assert_eq!(profile.get(ElementId::PlayerFrame).scale, crate::element::MAX_SCALE);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    /// Every id must be reachable from its own key, or an element saves to a
    /// name that never loads back.
    #[test]
    fn every_element_key_round_trips() {
        for id in ElementId::ALL {
            assert_eq!(ElementId::from_key(id.key()), Some(id), "{}", id.key());
        }
    }

    #[test]
    fn the_default_profile_writes_every_element() {
        let text = Profile::default().to_toml().unwrap();
        for id in ElementId::ALL {
            assert!(text.contains(id.key()), "{} is missing from {text}", id.key());
        }
    }
}
