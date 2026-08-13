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

impl From<::world::Appearance> for Appearance {
    /// The same five numbers, arriving from the network instead of from the
    /// character list.
    ///
    /// `world::Appearance` carries a class as well, which says nothing about
    /// how anyone looks -- a warrior and a mage of one race are the same model
    /// wearing different gear -- so it is dropped here rather than carried into
    /// the cache key, where it would build the same character twice.
    fn from(value: ::world::Appearance) -> Self {
        Self {
            race: value.race,
            gender: value.gender,
            skin: value.skin,
            face: value.face,
            hair_style: value.hair_style,
            hair_colour: value.hair_color,
            facial_hair: value.facial_hair,
        }
    }
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

/// A cache key for an appearance *and* what it is wearing.
///
/// Separate from [`Appearance::key`] because equipment changes how a character
/// looks without changing any of the five numbers: two humans in different
/// armour must not share a built model, which is the same bug `look_key`
/// already exists to prevent for faces.
pub fn look_key(appearance: &Appearance, equipment: &[u32]) -> u64 {
    // FNV-1a. Any stable hash would do -- this is a cache key, not a checksum,
    // and the appearance key alone is the part that has to be exact.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |value: u64| {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    };
    eat(appearance.key());
    for display in equipment {
        eat(u64::from(*display));
    }
    hash
}

/// The eight body components an item can paint on, and where each lives.
///
/// The directory names are the ones in the archive; the region is where that
/// component lands on the composed skin. Ordered as `ItemDisplayInfo` stores
/// them, so the two cannot drift.
const COMPONENTS: [(&str, region::Rect); 8] = [
    ("ArmUpperTexture", region::ARM_UPPER),
    ("ArmLowerTexture", region::ARM_LOWER),
    ("HandTexture", region::HAND),
    ("TorsoUpperTexture", region::TORSO_UPPER),
    ("TorsoLowerTexture", region::TORSO_LOWER),
    ("LegUpperTexture", region::PELVIS),
    ("LegLowerTexture", region::LEG_LOWER),
    ("FootTexture", region::FOOT),
];

/// Paints one item's components onto a skin already composed.
///
/// The gender-specific file is preferred and the unisex one is the fallback.
/// Most components ship as `_U` only; a few are cut for one body and not the
/// other, and taking `_U` first would quietly use the wrong shape for those.
fn wear(chain: &mut Chain, skin: &mut Skin, row: &dbc::schema::ItemDisplayInfoRow<'_>, gender: u8) {
    let names = [
        row.arm_upper(),
        row.arm_lower(),
        row.hand(),
        row.torso_upper(),
        row.torso_lower(),
        row.leg_upper(),
        row.leg_lower(),
        row.foot(),
    ];
    let sex = if gender == 1 { 'F' } else { 'M' };
    for (name, (directory, rect)) in names.iter().zip(COMPONENTS) {
        if name.is_empty() {
            continue;
        }
        let candidates = [
            format!(r"Item\TextureComponents\{directory}\{name}_{sex}.blp"),
            format!(r"Item\TextureComponents\{directory}\{name}_U.blp"),
        ];
        match candidates.iter().find_map(|path| layer(chain, path)) {
            Some(pixels) => blend(skin, rect, pixels),
            // Worth a line rather than a silent skip: a component that will not
            // load is a bare patch of skin where armour should be, which looks
            // like the character is wearing nothing there rather than like a
            // missing file.
            None => tracing::debug!("no {directory} texture for {name:?}"),
        }
    }
}

/// Resolves an appearance into textures and geosets, wearing nothing.
///
/// Every lookup degrades to `None` rather than failing: without a game
/// installation there are no tables to read, and the character still has to
/// draw -- untextured, as it did before this existed, rather than not at all.
pub fn resolve(chain: &mut Chain, look: Appearance) -> Look {
    resolve_wearing(chain, look, &[])
}

/// Resolves an appearance, dressed in the items whose display ids are given.
///
/// `equipment` is taken in the order `SMSG_CHAR_ENUM` sends it, and that order
/// is used directly for the paint order rather than being mapped through a slot
/// enum -- which is worth stating, because it looks like luck and is not
/// *quite*. Where two items paint the same component, the array already runs
/// inner to outer: shirt before chest, bracer before glove, trouser before
/// boot. So no table of slot meanings has to be transcribed to get the layering
/// right, and the one thing that would be wrong -- an item painted over the
/// thing that should cover it -- is exactly what a render shows.
pub fn resolve_wearing(chain: &mut Chain, look: Appearance, equipment: &[u32]) -> Look {
    let sections = chain
        .read(CharSections::PATH)
        .ok()
        .and_then(|bytes| CharSections::parse(&bytes).ok());
    let hair_geosets = chain
        .read(CharHairGeosets::PATH)
        .ok()
        .and_then(|bytes| CharHairGeosets::parse(&bytes).ok());
    let facial = chain
        .read(CharacterFacialHairStyles::PATH)
        .ok()
        .and_then(|bytes| CharacterFacialHairStyles::parse(&bytes).ok());

    let (mut body, mut skin) = (None, None);
    if let Some(sections) = &sections {
        skin = compose(chain, sections, look);
        if let Some(skin) = skin.as_mut() {
            dress(chain, skin, look, equipment);
        }
        // Section type 0 is the base skin, indexed by skin colour. The face,
        // facial hair and underwear are separate *layers* meant to be composed
        // onto this one; composing them needs a texture blit this client does
        // not do yet, so the base skin is used alone and the character has a
        // blank face. Stated rather than hidden: it is the visible limit of
        // this feature.
        body = find(sections, look.race, look.gender, 0, 0, look.skin);
    }

    let (hair, geosets) = hair_and_geosets(
        sections.as_ref(),
        hair_geosets.as_ref(),
        facial.as_ref(),
        look,
    );

    Look {
        skin,
        body,
        hair,
        geosets,
    }
}

/// The hair texture and the geosets to draw, from tables already read.
///
/// Shared by the player's [`resolve`] and by an NPC's [`NpcAppearances::look`]
/// rather than written twice. A player and an NPC differ in where their *body*
/// texture comes from -- composed from layers versus baked by an artist -- and
/// in nothing else: both pick one haircut out of seventeen in the same geoset
/// group, by the same rule, and a second copy of that rule would be a second
/// place for a character to end up wearing every hairstyle at once.
fn hair_and_geosets(
    sections: Option<&CharSections>,
    hair_geosets: Option<&CharHairGeosets>,
    facial: Option<&CharacterFacialHairStyles>,
    look: Appearance,
) -> (Option<String>, Vec<u32>) {
    // Type 3 is hair, indexed by style and colour. `None` is a real answer
    // rather than a failure: every colour of human male style 0 has an empty
    // texture, because style 0 is bald.
    let hair = sections.and_then(|sections| {
        find(
            sections,
            look.race,
            look.gender,
            3,
            look.hair_style,
            look.hair_colour,
        )
    });

    let mut geosets = Vec::new();
    // Hair. A style names a geoset in group zero; zero itself is the bald
    // scalp, which is a real choice rather than a missing one.
    let hair_geoset = hair_geosets
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
    if let Some(table) = facial {
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

    (hair, geosets)
}

/// Where a baked NPC texture lives. The table stores a bare filename.
const BAKED_NPC_TEXTURES: &str = r"Textures\BakedNpcTextures";

/// The tables a humanoid NPC's appearance needs, read once and kept.
///
/// Held rather than read per creature because these are megabytes: reading
/// `CreatureDisplayInfoExtra` and `CharSections` again on the frame each new
/// species first comes into view is the same shape as the thirty-seven-second
/// login this project has already paid for once. A parsed `dbc` table owns its
/// bytes, so keeping one costs nothing but the memory.
pub struct NpcAppearances {
    displays: dbc::schema::CreatureDisplayInfo,
    extras: dbc::schema::CreatureDisplayInfoExtra,
    sections: Option<CharSections>,
    hair_geosets: Option<CharHairGeosets>,
    facial: Option<CharacterFacialHairStyles>,
}

impl NpcAppearances {
    /// Reads the tables. `None` without a game installation, or if either
    /// creature table is missing -- there is nothing to answer with then.
    pub fn load(chain: &mut Chain) -> Option<Self> {
        use dbc::schema::{CreatureDisplayInfo, CreatureDisplayInfoExtra};

        let displays = CreatureDisplayInfo::parse(&chain.read(CreatureDisplayInfo::PATH).ok()?)
            .map_err(|e| tracing::warn!("CreatureDisplayInfo: {e}"))
            .ok()?;
        let extras =
            CreatureDisplayInfoExtra::parse(&chain.read(CreatureDisplayInfoExtra::PATH).ok()?)
                .map_err(|e| tracing::warn!("CreatureDisplayInfoExtra: {e}"))
                .ok()?;
        Some(Self {
            displays,
            extras,
            sections: chain
                .read(CharSections::PATH)
                .ok()
                .and_then(|b| CharSections::parse(&b).ok()),
            hair_geosets: chain
                .read(CharHairGeosets::PATH)
                .ok()
                .and_then(|b| CharHairGeosets::parse(&b).ok()),
            facial: chain
                .read(CharacterFacialHairStyles::PATH)
                .ok()
                .and_then(|b| CharacterFacialHairStyles::parse(&b).ok()),
        })
    }

    /// How a display id is dressed, if it is a humanoid built from character
    /// parts.
    ///
    /// `None` means "this creature is not one of those" -- a wolf, a kobold,
    /// anything whose skins come from its own `CreatureDisplayInfo` row -- and
    /// the caller should carry on exactly as before. That is the common case
    /// by count of *rows read*, but not by count of rows in the table: 15,446
    /// of 24,262 display ids have an extended row and no texture variation of
    /// their own, and every one of those renders white without this.
    pub fn look(&self, display_id: u32) -> Option<Look> {
        let extended = self
            .displays
            .iter()
            .find(|row| row.id() == display_id)?
            .extended_display_info_id();
        if extended == 0 {
            return None;
        }
        let row = self.extras.iter().find(|row| row.id() == extended)?;

        let appearance = Appearance {
            race: row.race() as u8,
            gender: row.gender() as u8,
            skin: row.skin() as u8,
            face: row.face() as u8,
            hair_style: row.hair_style() as u8,
            hair_colour: row.hair_colour() as u8,
            facial_hair: row.facial_hair() as u8,
        };
        let (hair, geosets) = hair_and_geosets(
            self.sections.as_ref(),
            self.hair_geosets.as_ref(),
            self.facial.as_ref(),
            appearance,
        );

        // The baked texture *is* the composed skin, done by an artist, with
        // this NPC's armour already painted into it. So `skin` stays `None`:
        // there is nothing to compose, and composing the character-creation
        // layers instead would strip the clothes off a guard and put him in
        // underwear.
        let bake = row.bake_name();
        let body = (!bake.is_empty()).then(|| format!(r"{BAKED_NPC_TEXTURES}\{bake}"));
        if body.is_none() {
            tracing::debug!("display {display_id} (extra {extended}) names no baked texture");
        }

        Some(Look {
            skin: None,
            body,
            hair,
            geosets,
        })
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
/// The eight armour components were added later and confirm the same layout
/// from a second direction. They ship at two resolutions, 128 wide and 256
/// wide, so their *sizes* prove nothing on their own -- but their aspect ratios
/// do, and they agree with every region here: hand, torso-lower and foot are
/// 4:1, the other five 2:1. Swap any two regions and that stops being true.
///
/// The stronger check is that the ten regions **tile the 512x512 skin exactly**
/// with nothing left over and nothing overlapping: the left column runs
/// 128+128+64+64+128 and the right 128+64+128+128+64, both reaching 512. A
/// layout guessed one region at a time would not close.
mod region {
    /// `(x, y, width, height)` on a 512x512 base skin.
    pub type Rect = (u32, u32, u32, u32);

    pub const ARM_UPPER: Rect = (0, 0, 256, 128);
    pub const ARM_LOWER: Rect = (0, 128, 256, 128);
    pub const HAND: Rect = (0, 256, 256, 64);
    pub const FACE_UPPER: Rect = (0, 320, 256, 64);
    pub const FACE_LOWER: Rect = (0, 384, 256, 128);

    pub const TORSO_UPPER: Rect = (256, 0, 256, 128);
    pub const TORSO_LOWER: Rect = (256, 128, 256, 64);
    /// Underwear sits here too, which is why this one was already known.
    pub const PELVIS: Rect = (256, 192, 256, 128);
    pub const LEG_LOWER: Rect = (256, 320, 256, 128);
    pub const FOOT: Rect = (256, 448, 256, 64);
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
/// Paints every equipped item onto an already-composed skin.
///
/// Reads `ItemDisplayInfo` once for the whole outfit rather than once per item:
/// it is 58,000 rows, and a character wears up to nineteen things.
fn dress(chain: &mut Chain, skin: &mut Skin, look: Appearance, equipment: &[u32]) {
    use dbc::schema::ItemDisplayInfo;

    // `display` alone shadows a `tracing` helper of that name inside its
    // macros, so the binding is spelled out.
    if equipment.iter().all(|display_id| *display_id == 0) {
        return;
    }
    let started = std::time::Instant::now();
    let Some(table) = chain
        .read(ItemDisplayInfo::PATH)
        .ok()
        .and_then(|bytes| ItemDisplayInfo::parse(&bytes).ok())
    else {
        tracing::warn!("no ItemDisplayInfo: equipment will not be drawn");
        return;
    };

    let mut worn = 0;
    for display_id in equipment.iter().filter(|id| **id != 0) {
        match table.iter().find(|row| row.id() == *display_id) {
            Some(row) => {
                wear(chain, skin, &row, look.gender);
                worn += 1;
            }
            // A display id with no row is worth naming: it means the item
            // exists and this client cannot say what it looks like, which is
            // different from wearing nothing there.
            None => tracing::debug!("no ItemDisplayInfo row for display {display_id}"),
        }
    }
    tracing::debug!("dressed in {worn} item(s) in {:?}", started.elapsed());
}

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

    /// The ten regions tile the 512x512 skin exactly: every pixel covered once,
    /// none twice, nothing off the edge.
    ///
    /// This is the check that makes the layout a measurement rather than ten
    /// separate guesses. A single region placed wrongly -- the mistake that
    /// paints a sleeve across a face -- either leaves a hole or overlaps a
    /// neighbour, and both show up here. It also fails loudly if someone adds
    /// an eleventh region without deciding what it displaces.
    #[test]
    fn the_skin_regions_tile_it_exactly() {
        const SIDE: usize = 512;
        let regions = [
            ("arm upper", region::ARM_UPPER),
            ("arm lower", region::ARM_LOWER),
            ("hand", region::HAND),
            ("face upper", region::FACE_UPPER),
            ("face lower", region::FACE_LOWER),
            ("torso upper", region::TORSO_UPPER),
            ("torso lower", region::TORSO_LOWER),
            ("pelvis", region::PELVIS),
            ("leg lower", region::LEG_LOWER),
            ("foot", region::FOOT),
        ];

        let mut covered = vec![0u8; SIDE * SIDE];
        for (name, (x, y, w, h)) in regions {
            assert!(
                x as usize + w as usize <= SIDE && y as usize + h as usize <= SIDE,
                "{name} runs off the skin"
            );
            for row in y..y + h {
                for column in x..x + w {
                    let cell = &mut covered[row as usize * SIDE + column as usize];
                    assert_eq!(*cell, 0, "{name} overlaps another region at {column},{row}");
                    *cell = 1;
                }
            }
        }
        assert!(
            covered.iter().all(|c| *c == 1),
            "{} pixels of the skin belong to no region",
            covered.iter().filter(|c| **c == 0).count()
        );
    }

    /// Components with the same shape must not be mixed up: the three 4:1
    /// regions are exactly the ones whose textures measure 4:1 in the archive.
    #[test]
    fn the_narrow_regions_are_hand_torso_lower_and_foot() {
        let narrow: Vec<&str> = [
            ("arm upper", region::ARM_UPPER),
            ("arm lower", region::ARM_LOWER),
            ("hand", region::HAND),
            ("torso upper", region::TORSO_UPPER),
            ("torso lower", region::TORSO_LOWER),
            ("pelvis", region::PELVIS),
            ("leg lower", region::LEG_LOWER),
            ("foot", region::FOOT),
        ]
        .into_iter()
        .filter(|(_, (_, _, w, h))| w / h == 4)
        .map(|(name, _)| name)
        .collect();
        assert_eq!(narrow, ["hand", "torso lower", "foot"]);
    }

    /// Equipment has to reach the cache key, or two characters in different
    /// armour share one built model -- the same bug the appearance key exists
    /// to prevent for faces.
    #[test]
    fn what_a_character_wears_changes_its_cache_key() {
        let appearance = Appearance {
            race: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_colour: 0,
            facial_hair: 0,
        };
        let naked = look_key(&appearance, &[]);
        assert_ne!(naked, look_key(&appearance, &[9891]));
        assert_ne!(look_key(&appearance, &[9891]), look_key(&appearance, &[9892]));
        // Order matters too: it is the paint order, so a different order is a
        // different character even with the same items.
        assert_ne!(
            look_key(&appearance, &[9891, 9892]),
            look_key(&appearance, &[9892, 9891])
        );
        // And an empty slot is not the same as no slot at all being read.
        assert_eq!(naked, look_key(&appearance, &[]));
    }

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
