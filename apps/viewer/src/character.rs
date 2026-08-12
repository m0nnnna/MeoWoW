//! What a player character looks like.
//!
//! A creature's appearance is one `CreatureDisplayInfo` row: a model and up to
//! three texture names. A *player's* is not. Display id 49 is every human male
//! in the world, and its texture columns are empty -- the appearance lives in
//! the five numbers the player picked at character creation, resolved through
//! `CharSections` for textures and `CharHairGeosets` for which geoset to show.
//!
//! Those five numbers come from the character list, which this client already
//! parses and has confirmed against a live realm. That matters: the same
//! values also live in the player object's update fields, at an index nothing
//! here has verified, and reading them from a field whose offset was guessed
//! is the failure this project keeps paying for. The character list is the
//! source that is already known to be right.
//!
//! **A character model expects exactly one geoset per group to be drawn.** All
//! seventeen hairstyles ship in the same model, in geoset group zero, and a
//! client that draws them all puts every haircut on one head at once -- which
//! is what a screenshot of this viewer showed before this module existed.

use dbc::schema::{CharHairGeosets, CharSections, CharacterFacialHairStyles};
use mpq::Chain;

/// The five choices that describe a character, plus who they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Appearance {
    pub race: u8,
    pub gender: u8,
    pub skin: u8,
    pub face: u8,
    pub hair_style: u8,
    pub hair_colour: u8,
    pub facial_hair: u8,
}

impl Appearance {
    /// A stable key for caching a built model.
    ///
    /// Two characters of the same race and gender share a display id but not
    /// an appearance, so a model cache keyed on the display id alone would
    /// hand the second player the first one's skin. Packing the choices means
    /// the cache distinguishes them without needing to understand them.
    pub fn key(&self) -> u64 {
        let bytes = [
            self.race,
            self.gender,
            self.skin,
            self.face,
            self.hair_style,
            self.hair_colour,
            self.facial_hair,
            0,
        ];
        u64::from_le_bytes(bytes)
    }
}

/// Everything the model loader needs to dress one character.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    /// The body texture, already a full archive path.
    pub body: Option<String>,
    /// The hair texture.
    pub hair: Option<String>,
    /// Geosets to draw, one per group. A group absent from this list is left
    /// alone rather than hidden -- see [`Look::shows`].
    pub geosets: Vec<u32>,
}

impl Look {
    /// Whether a submesh with this id should be drawn.
    ///
    /// Geoset ids encode `group * 100 + variant`, and a character model ships
    /// every variant of every group at once expecting the client to choose.
    /// The choice here: **geoset zero, plus exactly what this character's own
    /// numbers name, and nothing else.**
    ///
    /// The "and nothing else" was learned from a screenshot. The first version
    /// let groups it did not manage pass through, on the reasoning that a
    /// client should not silently drop geometry it has not learned about --
    /// and the character rendered wearing a large white sheet. That was geoset
    /// 1501, the cape: equipment geometry, drawn because it exists in the
    /// model, for a cloak the character does not own. Every group above three
    /// is equipment of some kind, and this client does not read equipment yet,
    /// so drawing any of it is drawing gear nobody is wearing.
    ///
    /// The failure mode is now the safe direction. Too little geometry looks
    /// like a character missing a hat; too much looks like a bug, and did.
    pub fn shows(&self, id: u32) -> bool {
        // Geoset zero is the body itself, which is not a choice.
        if id == 0 || self.geosets.contains(&id) {
            return true;
        }
        let (group, variant) = (id / 100, id % 100);
        if MANAGED_GROUPS.contains(&group) {
            // A group this character's own numbers decide: only what they name.
            return false;
        }
        // Everything else is an equipment group, and **variant one is the bare
        // body**, not a piece of gear. Hiding those outright left a character
        // with no forearms, hands, pelvis or legs -- a floating torso with
        // hands and feet nearby, which is what the screenshot showed. The
        // higher variants are the actual gear and stay off until this client
        // reads equipment.
        !HIDDEN_WITHOUT_EQUIPMENT.contains(&group) && variant == 1
    }
}

/// The geoset groups a character's own choices decide. Everything above these
/// is equipment -- see [`Look::shows`].
const MANAGED_GROUPS: [u32; 4] = [0, 1, 2, 3];

/// Equipment groups with no bare-body variant at all, so variant one is a
/// garment rather than a body part.
///
/// Group 15 is here because showing its variant one put the character in a
/// large white sheet -- see [`Look::shows`]. Kept as a list rather than a rule
/// because it is an observation about what these groups contain, and the next
/// group that turns out to behave this way should be added by looking, not by
/// reasoning from this one.
const HIDDEN_WITHOUT_EQUIPMENT: [u32; 1] = [15];

/// Resolves an appearance into textures and geosets.
///
/// Every lookup degrades to `None` rather than failing: without a game
/// installation there are no tables to read, and the character still has to
/// draw -- untextured, as it did before this existed, rather than not at all.
pub fn resolve(chain: &mut Chain, look: Appearance) -> Look {
    let sections = chain
        .read(CharSections::PATH)
        .ok()
        .and_then(|bytes| CharSections::parse(&bytes).ok());

    let (mut body, mut hair) = (None, None);
    if let Some(sections) = &sections {
        // Section type 0 is the base skin, indexed by skin colour. The face,
        // facial hair and underwear are separate *layers* meant to be composed
        // onto this one; composing them needs a texture blit this client does
        // not do yet, so the base skin is used alone and the character has a
        // blank face. Stated rather than hidden: it is the visible limit of
        // this feature.
        body = find(sections, look.race, look.gender, 0, 0, look.skin);
        // Type 3 is hair, indexed by style and colour.
        hair = find(
            sections,
            look.race,
            look.gender,
            3,
            look.hair_style,
            look.hair_colour,
        );
    }

    let mut geosets = Vec::new();
    // Hair. A style names a geoset in group zero; zero itself is the bald
    // scalp, which is a real choice rather than a missing one.
    let hair_geoset = chain
        .read(CharHairGeosets::PATH)
        .ok()
        .and_then(|bytes| CharHairGeosets::parse(&bytes).ok())
        .and_then(|table| {
            table
                .iter()
                .find(|row| {
                    row.race() == u32::from(look.race)
                        && row.gender() == u32::from(look.gender)
                        && row.variation() == u32::from(look.hair_style)
                })
                .map(|row| row.geoset())
        })
        .unwrap_or(0);
    geosets.push(hair_geoset);

    // Facial hair, across three groups at once. The columns are variants in
    // groups 1, 3 and 2 -- in that order, which is not the order they are
    // numbered, and getting it wrong would put a moustache where a beard goes.
    if let Some(table) = chain
        .read(CharacterFacialHairStyles::PATH)
        .ok()
        .and_then(|bytes| CharacterFacialHairStyles::parse(&bytes).ok())
    {
        if let Some(row) = table.iter().find(|row| {
            row.race() == u32::from(look.race)
                && row.gender() == u32::from(look.gender)
                && row.variation() == u32::from(look.facial_hair)
        }) {
            geosets.push(100 + row.geoset_100());
            geosets.push(300 + row.geoset_300());
            geosets.push(200 + row.geoset_200());
        }
    }
    // A group with no row at all still needs one visible entry, or every
    // variant in it is hidden and the character loses a jaw.
    for group in MANAGED_GROUPS {
        if !geosets.iter().any(|id| id / 100 == group) {
            geosets.push(group * 100);
        }
    }

    Look {
        body,
        hair,
        geosets,
    }
}

/// One `CharSections` row's first texture, if it exists.
fn find(
    sections: &CharSections,
    race: u8,
    gender: u8,
    section_type: u32,
    variation: u8,
    colour: u8,
) -> Option<String> {
    sections
        .iter()
        .find(|row| {
            row.race() == u32::from(race)
                && row.gender() == u32::from(gender)
                && row.section_type() == section_type
                && row.variation() == u32::from(variation)
                && row.colour() == u32::from(colour)
        })
        .map(|row| row.texture_0().to_string())
        .filter(|path| !path.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look_with(geosets: Vec<u32>) -> Look {
        Look {
            geosets,
            ..Default::default()
        }
    }

    /// The whole point: one hairstyle, not seventeen.
    #[test]
    fn only_the_chosen_variant_of_a_managed_group_shows() {
        let look = look_with(vec![5, 101, 200, 302]);
        assert!(look.shows(5), "the chosen hairstyle");
        // Geoset zero is excluded deliberately: it is the body and the bald
        // scalp at once, and always draws.
        assert!(look.shows(0), "geoset zero is the body, not a hairstyle");
        for other in [1, 4, 9, 10, 16, 18] {
            assert!(!look.shows(other), "geoset {other} is a hairstyle nobody chose");
        }
        assert!(look.shows(101) && !look.shows(102));
        assert!(look.shows(302) && !look.shows(301));
    }

    /// Equipment geometry stays off until there is equipment to justify it.
    ///
    /// Pinned because the opposite was tried first and rendered the character
    /// in a large white sheet -- geoset 1501, a cape, for a cloak nobody owns.
    #[test]
    fn equipment_geosets_show_the_bare_body_and_not_the_gear() {
        let look = look_with(vec![0, 101, 201, 301]);
        // Variant one of an equipment group is the body part itself. Hiding
        // these left a floating torso with its hands and feet scattered on the
        // grass nearby, which is exactly how it looked.
        for id in [401, 701] {
            assert!(look.shows(id), "geoset {id} is a bare body part");
        }
        // The higher variants are actual gear, which nobody is wearing.
        for id in [402, 403, 404, 702] {
            assert!(!look.shows(id), "geoset {id} is equipment nobody has");
        }
        // Group 8 ships no variant one at all -- `HumanMale.m2` has 802 and
        // 803 and nothing below them -- so its default is to draw nothing,
        // and the rendered character is complete without it. A group's
        // default is whatever the model actually contains, not a number this
        // code assumes every group has.
        for id in [802, 803] {
            assert!(!look.shows(id), "group 8 has no bare variant to fall back to");
        }
        // And group 15 has no bare variant at all: its variant one is the
        // white sheet.
        assert!(!look.shows(1501));
    }

    /// The body is geoset zero of group zero, and a bald character is the
    /// `geoset 0` case -- so an empty choice must not hide the head.
    #[test]
    fn a_bald_character_still_has_a_scalp() {
        let look = look_with(vec![0]);
        assert!(look.shows(0));
    }

    /// Two players of one race and gender share a display id but not a face,
    /// so a cache keyed on the display alone would dress the second in the
    /// first one's skin.
    #[test]
    fn appearances_that_differ_have_different_keys() {
        let base = Appearance {
            race: 1,
            gender: 0,
            skin: 3,
            face: 2,
            hair_style: 4,
            hair_colour: 1,
            facial_hair: 5,
        };
        let mut other = base;
        other.skin = 4;
        assert_ne!(base.key(), other.key());
        assert_eq!(base.key(), base.key());
    }
}
