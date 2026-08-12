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

/// A skin composed from its layers, ready to upload.
#[derive(Clone, PartialEq)]
pub struct Skin {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Skin {
    /// Without this a `Look` prints a megabyte of pixels into the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Skin({}x{})", self.width, self.height)
    }
}

/// Everything the model loader needs to dress one character.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    /// The composed body texture: base skin with the face, facial hair and
    /// underwear blended into it. `None` without a game installation.
    pub skin: Option<Skin>,
    /// The base body texture's path, kept for logs and for the case where
    /// composition could not run.
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

    let (mut body, mut hair, mut skin) = (None, None, None);
    if let Some(sections) = &sections {
        skin = compose(chain, sections, look);
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
        skin,
        body,
        hair,
        geosets,
    }
}


/// Where each layer goes on the composed skin, in the base texture's own
/// pixels.
///
/// **Derived from the textures, not transcribed.** 3.3.5a has no
/// `CharComponentTextureSections` table -- the layout lives in the client --
/// so the regions below are the classic 256-unit character layout doubled to
/// the 512x512 base skin this build ships. What makes that a measurement
/// rather than a guess is that the overlay textures are *exactly* the sizes it
/// predicts, in three independent places:
///
/// | layer | region says | the file is |
/// |---|---|---|
/// | face upper | 256x64 | 256x64 |
/// | face lower | 256x128 | 256x128 |
/// | pelvis | 256x128 | 256x128 |
///
/// Three exact agreements between a layout and files that know nothing about
/// it is the same kind of evidence as two parsers arriving at the same rage
/// value. A wrong layout would have to be wrong by zero pixels three times.
mod region {
    /// `(x, y, width, height)` on a 512x512 base skin.
    pub type Rect = (u32, u32, u32, u32);
    pub const FACE_UPPER: Rect = (0, 320, 256, 64);
    pub const FACE_LOWER: Rect = (0, 384, 256, 128);
    /// Underwear sits on the upper legs.
    pub const PELVIS: Rect = (256, 192, 256, 128);
}

/// Reads a texture and expands it to RGBA.
fn layer(chain: &mut Chain, path: &str) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = chain.read(path).ok()?;
    let image = blp::Blp::parse(&bytes).ok()?;
    let rgba = image.decode_rgba(0)?;
    Some((image.width(), image.height(), rgba))
}

/// Blends one layer into a region of the skin.
///
/// Nearest-neighbour where the layer and the region disagree on size, which
/// happens for facial hair: it ships at half the face's resolution and is
/// meant to be stretched over it. Alpha is honoured rather than assumed --
/// the skin and face layers are opaque and simply overwrite, but facial hair
/// carries an 8-bit alpha and a beard drawn as an opaque rectangle would be a
/// box on the chin.
fn blend(skin: &mut Skin, rect: region::Rect, layer: (u32, u32, Vec<u8>)) {
    let (rx, ry, rw, rh) = rect;
    let (lw, lh, pixels) = layer;
    if lw == 0 || lh == 0 {
        return;
    }
    for y in 0..rh {
        let sy = ry + y;
        if sy >= skin.height {
            break;
        }
        // Sampled from the layer by proportion, so a half-size layer stretches
        // to fill its region instead of covering a quarter of it.
        let ly = (y * lh / rh).min(lh - 1);
        for x in 0..rw {
            let sx = rx + x;
            if sx >= skin.width {
                break;
            }
            let lx = (x * lw / rw).min(lw - 1);
            let src = ((ly * lw + lx) * 4) as usize;
            let dst = ((sy * skin.width + sx) * 4) as usize;
            let (Some(s), Some(d)) = (pixels.get(src..src + 4), skin.rgba.get(dst..dst + 4)) else {
                continue;
            };
            let alpha = s[3] as u32;
            if alpha == 0 {
                continue;
            }
            let blended = [
                ((s[0] as u32 * alpha + d[0] as u32 * (255 - alpha)) / 255) as u8,
                ((s[1] as u32 * alpha + d[1] as u32 * (255 - alpha)) / 255) as u8,
                ((s[2] as u32 * alpha + d[2] as u32 * (255 - alpha)) / 255) as u8,
                255,
            ];
            skin.rgba[dst..dst + 4].copy_from_slice(&blended);
        }
    }
}

/// Builds one character's skin out of its layers.
///
/// The base body first, then the face, then facial hair over the face, then
/// underwear. Order matters and is the order a person would paint them: a
/// beard belongs on top of the chin it grows from, not under it.
fn compose(chain: &mut Chain, sections: &CharSections, look: Appearance) -> Option<Skin> {
    let base = find(sections, look.race, look.gender, 0, 0, look.skin)?;
    let (width, height, rgba) = layer(chain, &base)?;
    let mut skin = Skin {
        width,
        height,
        rgba,
    };

    // The face is two halves in the first two texture columns of one row.
    if let Some(row) = row_for(sections, look.race, look.gender, 1, look.face, look.skin) {
        if let Some(l) = layer(chain, row.0.as_str()) {
            blend(&mut skin, region::FACE_LOWER, l);
        }
        if let Some(l) = layer(chain, row.1.as_str()) {
            blend(&mut skin, region::FACE_UPPER, l);
        }
    }

    // Facial hair, over the face rather than under it. Indexed by hair colour,
    // not skin colour -- a beard matches the hair on the head.
    if let Some(row) = row_for(
        sections,
        look.race,
        look.gender,
        2,
        look.facial_hair,
        look.hair_colour,
    ) {
        if let Some(l) = layer(chain, row.0.as_str()) {
            blend(&mut skin, region::FACE_LOWER, l);
        }
        if let Some(l) = layer(chain, row.1.as_str()) {
            blend(&mut skin, region::FACE_UPPER, l);
        }
    }

    // Underwear. Not modesty for its own sake: the base skin has no smallclothes
    // painted on, so without this the character is nude.
    if let Some(row) = row_for(sections, look.race, look.gender, 4, 0, look.skin) {
        if let Some(l) = layer(chain, row.0.as_str()) {
            blend(&mut skin, region::PELVIS, l);
        }
    }

    Some(skin)
}

/// A section row's first two texture columns.
fn row_for(
    sections: &CharSections,
    race: u8,
    gender: u8,
    section_type: u32,
    variation: u8,
    colour: u8,
) -> Option<(String, String)> {
    sections
        .iter()
        .find(|row| {
            row.race() == u32::from(race)
                && row.gender() == u32::from(gender)
                && row.section_type() == section_type
                && row.variation() == u32::from(variation)
                && row.colour() == u32::from(colour)
        })
        .map(|row| (row.texture_0().to_string(), row.texture_1().to_string()))
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
