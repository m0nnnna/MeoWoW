//! Named starting points for [`Style`].
//!
//! A theme here is **not an overlay**, and that is the whole design.
//! [`crate::layout::Profile`] says in its own doc comment that there is no
//! second set of defaults the file only partly overrides, because a user who
//! cannot see why their change had no effect concludes the feature is broken.
//! A theme that lived in the file as a name and silently supplied colours
//! underneath would be exactly that hidden second set.
//!
//! So choosing a theme *writes the colours out*. The file stays the whole
//! truth, and a theme is a way of filling it in that beats typing forty hex
//! codes.
//!
//! The consequence is that nothing stores which theme is in force -- see
//! [`Theme::of`], which answers by comparing. After a hand edit the answer is
//! honestly `None` rather than the name of a theme the colours no longer
//! match, which is the same call [`crate::frames::tracker`] makes about a
//! quest with no distance: **a number nobody can check is worse than a
//! blank.**

use crate::style::{Color, Style};

/// A palette the whole interface can be rebuilt from.
///
/// Cats, because the client is called MeoWoW. [`Theme::Slate`] is the original
/// and stays the default -- a theme system whose arrival silently repainted
/// everybody's client would be a theme system nobody asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Theme {
    /// What this client has always looked like: cold grey-blue over the world.
    Slate,
    /// Pink and cream. The cat on the tin.
    Neko,
    /// A black cat: near-black panels, violet edges, one gold eye for the
    /// things that want answering.
    Void,
    /// Ginger and cream, warm where `Slate` is cold.
    Calico,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::Slate, Theme::Neko, Theme::Void, Theme::Calico];

    /// The word shown on the button.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Slate => "slate",
            Theme::Neko => "neko",
            Theme::Void => "void",
            Theme::Calico => "calico",
        }
    }

    /// Parses [`Theme::name`], case-insensitively.
    pub fn from_name(name: &str) -> Option<Theme> {
        Theme::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name))
    }

    /// Which theme a style *is*, or `None` once it has been edited into
    /// something that is no longer any of them.
    ///
    /// An exact comparison, deliberately. A "closest match" would keep naming
    /// a theme after the user had changed every colour in it, which is a label
    /// that stops meaning anything at the moment it starts mattering.
    pub fn of(style: &Style) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.style() == *style)
    }

    /// The complete style this theme describes.
    ///
    /// Built by overriding [`Style::default`]'s colours rather than by
    /// restating every field: the dimensions -- how wide a frame is, how tall
    /// a bar is -- are not what a theme is about, and a theme that reset them
    /// would throw away tuning a user did for their own screen.
    pub fn style(self) -> Style {
        let base = Style::default();
        match self {
            Theme::Slate => base,
            Theme::Neko => Style {
                background: Color::rgba(38, 26, 36, 214),
                border: Color::rgba(240, 168, 196, 165),
                text: Color::rgb(255, 240, 246),
                bar_backdrop: Color::rgba(18, 10, 16, 150),

                health: Color::rgb(112, 198, 138),
                health_low: Color::rgb(228, 96, 128),
                mana: Color::rgb(146, 152, 236),
                rage: Color::rgb(228, 96, 128),
                energy: Color::rgb(244, 216, 120),

                edit_highlight: Color::rgb(255, 170, 205),
                slot_background: Color::rgba(50, 34, 46, 205),
                slot_empty_border: Color::rgba(240, 168, 196, 70),
                slot_binding: Color::rgba(255, 235, 244, 200),

                spellbook_background: Color::rgba(38, 26, 36, 238),
                spellbook_selected: Color::rgba(255, 158, 196, 90),
                quest_dim: Color::rgba(216, 186, 202, 210),

                world_map_backing: Color::rgba(46, 32, 42, 245),
                world_map_player: Color::rgb(255, 214, 120),
                world_map_objective: Color::rgb(255, 128, 160),
                world_map_party: Color::rgb(160, 200, 255),
                minimap_backing: Color::rgba(46, 32, 42, 245),
                minimap_rim: Color::rgb(58, 34, 50),

                tracker_background: Color::rgba(32, 20, 30, 155),
                quest_complete: Color::rgb(255, 214, 120),
                world_map_questgiver: Color::rgb(255, 214, 120),

                casting: Color::rgb(255, 186, 128),

                chat_background: Color::rgba(30, 18, 28, 165),
                chat_say: Color::rgb(255, 242, 248),
                chat_whisper: Color::rgb(255, 158, 226),
                chat_system: Color::rgb(255, 214, 140),
                chat_other: Color::rgb(214, 186, 200),
                chat_composing: Color::rgb(255, 190, 216),

                target_marker: Color::rgb(255, 190, 216),
                combat_text_damage: Color::rgb(255, 226, 168),
                combat_text_critical: Color::rgb(255, 130, 150),

                party_invite_border: Color::rgb(255, 158, 196),
                party_invite_accept: Color::rgb(126, 214, 160),
                party_invite_decline: Color::rgb(240, 130, 150),

                login_background: Color::rgba(34, 22, 32, 250),
                login_field: Color::rgba(20, 12, 20, 230),
                login_accent: Color::rgb(255, 158, 196),
                login_error: Color::rgb(255, 130, 140),
                ..base
            },
            Theme::Void => Style {
                background: Color::rgba(13, 12, 20, 224),
                border: Color::rgba(150, 122, 214, 160),
                text: Color::rgb(232, 228, 246),
                bar_backdrop: Color::rgba(4, 4, 8, 165),

                health: Color::rgb(92, 190, 130),
                mana: Color::rgb(126, 132, 240),

                edit_highlight: Color::rgb(255, 200, 90),
                slot_background: Color::rgba(20, 18, 30, 208),
                slot_empty_border: Color::rgba(150, 122, 214, 70),

                spellbook_background: Color::rgba(13, 12, 20, 240),
                spellbook_selected: Color::rgba(186, 148, 255, 90),
                quest_dim: Color::rgba(176, 170, 200, 210),

                world_map_backing: Color::rgba(18, 16, 26, 246),
                world_map_player: Color::rgb(255, 200, 90),
                world_map_party: Color::rgb(150, 160, 255),
                minimap_backing: Color::rgba(18, 16, 26, 246),
                minimap_rim: Color::rgb(10, 9, 16),

                tracker_background: Color::rgba(9, 8, 14, 155),
                casting: Color::rgb(186, 148, 255),

                chat_background: Color::rgba(8, 7, 13, 168),
                chat_whisper: Color::rgb(206, 150, 255),
                chat_channel: Color::rgb(130, 210, 220),
                chat_other: Color::rgb(168, 164, 190),
                chat_composing: Color::rgb(186, 148, 255),

                target_marker: Color::rgb(255, 200, 90),
                combat_text_critical: Color::rgb(206, 150, 255),

                party_invite_border: Color::rgb(186, 148, 255),

                login_background: Color::rgba(11, 10, 17, 250),
                login_field: Color::rgba(5, 5, 9, 235),
                login_accent: Color::rgb(186, 148, 255),
                login_error: Color::rgb(240, 120, 120),
                ..base
            },
            Theme::Calico => Style {
                background: Color::rgba(40, 30, 22, 214),
                border: Color::rgba(226, 176, 110, 168),
                text: Color::rgb(250, 242, 230),
                bar_backdrop: Color::rgba(16, 11, 6, 152),

                health: Color::rgb(126, 190, 104),
                mana: Color::rgb(96, 142, 212),

                edit_highlight: Color::rgb(240, 160, 80),
                slot_background: Color::rgba(52, 39, 28, 206),
                slot_empty_border: Color::rgba(226, 176, 110, 70),

                spellbook_background: Color::rgba(40, 30, 22, 238),
                spellbook_selected: Color::rgba(240, 160, 80, 90),
                quest_dim: Color::rgba(206, 190, 168, 210),

                world_map_backing: Color::rgba(46, 35, 24, 245),
                minimap_backing: Color::rgba(46, 35, 24, 245),
                minimap_rim: Color::rgb(38, 26, 16),

                tracker_background: Color::rgba(26, 19, 12, 155),
                casting: Color::rgb(240, 178, 96),

                chat_background: Color::rgba(22, 16, 10, 166),
                chat_other: Color::rgb(196, 182, 162),
                chat_composing: Color::rgb(240, 200, 130),

                target_marker: Color::rgb(250, 214, 140),

                party_invite_border: Color::rgb(240, 160, 80),

                login_background: Color::rgba(36, 27, 19, 250),
                login_field: Color::rgba(19, 14, 9, 232),
                login_accent: Color::rgb(240, 160, 80),
                login_error: Color::rgb(232, 104, 84),
                ..base
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default has to keep being the default. A theme system whose arrival
    /// repainted every existing client is a regression however good the new
    /// palette is.
    #[test]
    fn slate_is_the_untouched_default() {
        assert_eq!(Theme::Slate.style(), Style::default());
        assert_eq!(Theme::of(&Style::default()), Some(Theme::Slate));
    }

    #[test]
    fn every_theme_round_trips_its_name() {
        for theme in Theme::ALL {
            assert_eq!(Theme::from_name(theme.name()), Some(theme));
            assert_eq!(Theme::from_name(&theme.name().to_uppercase()), Some(theme));
        }
        assert_eq!(Theme::from_name("tabby"), None);
    }

    /// Two themes that produce the same style are one theme wearing two names,
    /// and [`Theme::of`] would answer with whichever came first in `ALL` -- a
    /// wrong answer no other test here would notice.
    #[test]
    fn no_two_themes_are_the_same_palette() {
        for (i, a) in Theme::ALL.into_iter().enumerate() {
            for b in Theme::ALL.into_iter().skip(i + 1) {
                assert_ne!(a.style(), b.style(), "{} and {}", a.name(), b.name());
            }
            assert_eq!(Theme::of(&a.style()), Some(a));
        }
    }

    /// A theme sets colours, not dimensions. Resetting how wide somebody's
    /// chat frame is because they liked a different pink is not theming.
    #[test]
    fn a_theme_keeps_every_dimension() {
        let base = Style::default();
        for theme in Theme::ALL {
            let themed = theme.style();
            assert_eq!(themed.frame_width, base.frame_width, "{}", theme.name());
            assert_eq!(themed.chat_width, base.chat_width, "{}", theme.name());
            assert_eq!(themed.slot_size, base.slot_size, "{}", theme.name());
            assert_eq!(themed.minimap_size, base.minimap_size, "{}", theme.name());
            assert_eq!(themed.login_width, base.login_width, "{}", theme.name());
        }
    }

    /// A hand-edited style belongs to no theme, and must say so rather than
    /// keep claiming the one it started as.
    #[test]
    fn an_edited_style_names_no_theme() {
        let mut style = Theme::Neko.style();
        style.health = Color::rgb(1, 2, 3);
        assert_eq!(Theme::of(&style), None);
    }

    /// Every theme has to survive the file, because choosing one writes it
    /// there. A colour that cannot be serialised is a theme that silently
    /// reverts on the next start.
    #[test]
    fn every_theme_survives_the_layout_file() {
        for theme in Theme::ALL {
            let mut profile = crate::layout::Profile::default();
            profile.style = theme.style();
            let text = profile.to_toml().expect("serialise");
            let (parsed, warnings) = crate::layout::Profile::from_toml(&text).expect("parse");
            assert!(warnings.is_empty(), "{}: {warnings:?}", theme.name());
            assert_eq!(Theme::of(&parsed.style), Some(theme));
        }
    }
}
