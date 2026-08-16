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

use dbc::schema::{CharHairGeosets, CharSections, CharacterFacialHairStyles, ItemDisplayInfo};
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
    /// Equipment groups this character's gear has an opinion about.
    ///
    /// Separate from [`Look::geosets`] because "wearing nothing in this group"
    /// and "not knowing about this group" need different answers, and the
    /// difference is visible: an *empty* group must fall back to the bare body
    /// part, while a group the gear decided must show only what it named.
    /// Without this, taking a glove off would leave the hand missing rather
    /// than bare.
    pub decided_groups: Vec<u32>,
    /// Separate models this character carries, hung off the skeleton.
    ///
    /// Lives on the look rather than beside it because a look is exactly what
    /// the renderer caches equipment-aware geometry by -- [`look_key`] already
    /// folds the equipment in, so two characters in different weapons already
    /// cannot share a cache entry. Everything else here paints the character's
    /// own mesh; this is the one part that is a mesh of its own.
    pub held: Vec<HeldItem>,
    /// How this character holds itself with its weapon drawn, from whatever it
    /// is carrying. [`Stance::Unarmed`] for anyone holding nothing.
    pub stance: Stance,
}

/// One model a character carries, and where on the skeleton it hangs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldItem {
    /// Full archive path of the geometry, with `.mdx` already rewritten.
    pub model: String,
    /// The item's own skin, as the bare name `ItemDisplayInfo` stores. Resolved
    /// against the model's directory by the loader, the same way a creature's
    /// texture variation is.
    pub texture: String,
    /// Which [`m2::Attachment`] id on the *wielder* this hangs from when it is
    /// drawn and in the hands.
    pub attachment: u32,
    /// Where it rests when stowed, or `None` for an item with nowhere to go --
    /// which is not a gap: bows, guns and thrown weapons all record a
    /// `sheathe_type` of zero, and an item with no resting place stays in the
    /// hand rather than disappearing.
    pub stowed: Option<u32>,
    /// The stance this item calls for while it is drawn.
    pub stance: Stance,
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
    /// and the character rendered wearing a large white sheet. Every group
    /// above three is equipment of some kind, and drawing it is drawing gear
    /// nobody is wearing.
    ///
    /// The failure mode is now the safe direction. Too little geometry looks
    /// like a character missing a hat; too much looks like a bug, and did.
    pub fn shows(&self, id: u32) -> bool {
        // Geoset zero is the body itself, which is not a choice.
        if id == 0 || self.geosets.contains(&id) {
            return true;
        }
        if id == NECK_PATCH {
            return true;
        }
        let (group, variant) = (id / 100, id % 100);
        if MANAGED_GROUPS.contains(&group) || self.decided_groups.contains(&group) {
            // A group this character's own numbers or its gear decide: only
            // what they name, and they have already been checked above.
            return false;
        }
        // Everything else is an equipment group, and **variant one is the bare
        // body**, not a piece of gear. Hiding those outright left a character
        // with no forearms, hands, pelvis or legs -- a floating torso with
        // hands and feet nearby, which is what the screenshot showed. The
        // higher variants are the actual gear and stay off until this client
        // reads equipment.
        variant == 1
    }
}

/// The scrap of *body* that closes the back of the neck.
///
/// **It is not a cloak, and calling it one put a hole in every character in the
/// game.** The white sheet that made the first version of [`Look::shows`] hide
/// the whole of group 15 was real, but it was 1502 and up; 1501 was standing
/// next to the culprit and got hidden with it. Twenty triangles then stopped being
/// drawn on every character, leaving a rectangle of daylight between the
/// shoulder blades -- reported from play as "the back of the neck is missing
/// for everyone", and invisible until somebody looked at a character from
/// behind.
///
/// What separates them is a property of the *file* rather than a judgement
/// about the picture, which is why it is worth writing down: on both the human
/// male and the human female, 1501 is drawn with **material 0 and texture slot
/// 1, the body skin**, exactly like every other piece of bare body -- while
/// 1502 to 1506 use a different material and texture slot 2, the object skin
/// that a cape is painted with. They are not variants of one thing. Their
/// positions agree: 1501 is twenty triangles at the base of the neck (z 1.72 on
/// the male), and the cloaks are four times the size and hang from z 1.33.
///
/// So it is exempt even from the `decided_groups` rule above: a character
/// wearing a cloak still has a neck. If a cloak ever turns out to carry its own
/// collar, this is where the two would z-fight.
const NECK_PATCH: u32 = 1501;

/// Which geoset group a slot switches on, and which of an item's three
/// `geoset_group` columns selects the variant within it.
///
/// **Grounded in the data, not transcribed, and deliberately incomplete.**
/// Every entry here was established the same way: take every item of that
/// inventory type, look at which `geoset_group` values it uses, and check them
/// against the variants a character model actually contains. A slot whose items
/// use values 1 to 3 where the model holds `401`-`404` is switching group 4.
///
/// | slot | type | values in the data | model has | group |
/// |---|---|---|---|---|
/// | hands | 10 | 1, 2, 3 | 401-404 | 4 |
/// | feet | 8 | 1, 2, 3 | 501-505 | 5 |
/// | back | 16 | 1, 2, 5 | 1501-1506 | 15 |
/// | shirt, chest | 4, 5 | 1, 2 | 802, 803 | 8 |
/// | robe | 20 | 1, 2 | 1301, 1302 | 13 |
///
/// **Belt, legs and tabard are left out on purpose.** Their values do not line
/// up with the variants the model has -- a tabard uses value 1 where the only
/// tabard geoset is `1202`, and legs drive two columns at once -- so a mapping
/// for them would be a guess dressed as a table. This project has already paid
/// four attempts for geoset rules read rather than rendered. Leaving a slot out
/// costs a missing belt buckle; getting it wrong puts a robe's skirt on
/// somebody's arm.
fn geoset_rule(inventory_type: u8) -> Option<(u32, usize)> {
    Some(match inventory_type {
        // Shirt and chest both drive sleeves. Value 1 selects `801`, which no
        // character model contains -- a short sleeve needs no geometry -- so it
        // draws nothing, which is right rather than a gap.
        4 | 5 => (8, 0),
        8 => (5, 0),
        10 => (4, 0),
        16 => (15, 0),
        20 => (13, 0),
        _ => return None,
    })
}

/// Which folder an item's geometry lives in, and which hand it goes in.
///
/// **Measured, not transcribed** -- `wow-cli item held` joins all 46,096 rows
/// of `Item.dbc` to `ItemDisplayInfo` and asks which slots name geometry at
/// all, then resolves those names against every `Item\ObjectComponents`
/// folder. The held slots separate from the painted ones completely: each of
/// the types below fills its model column for 99.2% or more of its items and
/// resolves in exactly one folder, while a chest or a belt manages under 2%.
///
/// | type | items | names a model | folder |
/// |---|---|---|---|
/// | one-hand | 13 | 99.8% | Weapon |
/// | two-hand | 17 | 100% | Weapon |
/// | main hand | 21 | 100% | Weapon |
/// | off hand | 22 | 100% | Weapon |
/// | holdable | 23 | 99.2% | Weapon |
/// | shield | 14 | 100% | Shield |
///
/// **The column is `model_left` for every one of them, and that does not mean
/// the left hand.** The pair is really "first model, second model", and only a
/// genuinely paired item fills both: shoulders (type 3) put `LShoulder_...` in
/// one and `RShoulder_...` in the other, which is what proves the column names
/// themselves are right. A sword is a single model, so it sits in the first
/// column and goes in whichever hand its *slot* names. Reading the column as
/// the hand would put every weapon in the game in the wrong one.
///
/// **Bows, guns, thrown weapons and shoulders are deliberately absent.** Their
/// data resolves perfectly well -- ranged weapons are 100% in `Weapon`, and
/// shoulders 98.6% in `Shoulder` -- but this table's second column is a claim
/// about *which attachment point*, and that has been confirmed for the two
/// hands and nothing else. A shoulder needs the two shoulder attachments and a
/// bow is held differently again; guessing would put a rifle through a
/// character's palm, which renders plausibly and is never an error. Same
/// reasoning, and the same precedent, as [`geoset_rule`] leaving out belts.
fn held_rule(inventory_type: u8) -> Option<(&'static str, u32)> {
    Some(match inventory_type {
        13 | 17 | 21 => ("Weapon", m2::Attachment::HAND_RIGHT),
        22 | 23 => ("Weapon", m2::Attachment::HAND_LEFT),
        14 => ("Shield", m2::Attachment::HAND_LEFT),
        _ => return None,
    })
}

/// How a character holds itself when its weapon is out.
///
/// Not cosmetic. A character standing in the plain idle cycle with a drawn
/// weapon has its hands open and the grip floating somewhere near one of them,
/// which was the first thing noticed the moment weapons were drawn at all:
/// *"the hand is open holding the sword like he was precombat"*. Every
/// character model carries `ReadyUnarmed`, `Ready1H` and `Ready2H` precisely so
/// the hands close around what they are holding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stance {
    /// Nothing in hand, or nothing this client can identify.
    #[default]
    Unarmed,
    OneHand,
    TwoHand,
}

/// Which stance an equipped item calls for.
///
/// Reuses the inventory types [`held_rule`] already measured, so there is one
/// list of what counts as a weapon rather than two that can drift. Ranged
/// weapons are absent for the same reason they are absent there -- `ReadyBow`
/// and `ReadyRifle` exist, but this client does not draw a bow yet, and a
/// stance without geometry to match it would be a character miming.
fn stance_for(inventory_type: u8) -> Option<Stance> {
    match inventory_type {
        // Two-handed. `Ready2HL` exists for the long ones (staves, polearms)
        // and is not distinguished here: it is a different *hold*, and which
        // items want it has not been measured.
        17 => Some(Stance::TwoHand),
        13 | 21 | 22 | 23 => Some(Stance::OneHand),
        // A shield changes nothing about how the other arm is held.
        _ => None,
    }
}

/// Where a stowed weapon rests, from `Item.dbc`'s `sheathe_type`.
///
/// **The vocabulary was measured, and so was the fact that it is a real one.**
/// `wow-cli item sheath` cross-tabulates the column against inventory type
/// over all 46,096 items: every slot that only *paints* the body is 100% type
/// zero with a single distinct value, while the slots that hang geometry
/// spread across five. So the column is set because an item can be sheathed,
/// not incidentally. Confirmed item by item too -- a claymore reads 1, a stave
/// 2, a short sword 3, a shield 4.
///
/// | type | what carries it | rests |
/// |---|---|---|
/// | 1 | two-handed swords, axes, maces | back |
/// | 2 | staves and polearms | back, angled differently |
/// | 3 | one-handed weapons | hip |
/// | 4 | shields | back, centred |
/// | 7 | fist weapons and holdables | hip |
/// | 0 | bows, guns, thrown | nowhere -- they stay in hand |
///
/// The attachment ids come from the same geometric argument that identified
/// the hands, run over every playable race at once (`m2 attachments --anim 0`
/// poses the skeleton and reports where a weapon hung at each point would
/// *aim*, since an item model runs along its own +X):
///
/// - **32 and 33** sit at hip height, offset left and right, with the blade
///   pointing backward and slightly down -- a sword hanging hilt-forward at
///   the belt. Consistent on human, orc, night elf and tauren.
/// - **26 and 27** sit high on the shoulder blades with the blade pointing
///   *down*, which is a greatsword carried hilt-above-the-shoulder.
/// - **30 and 31** sit on the upper back with the blade pointing *up*, the
///   other way a long shaft is carried.
/// - **28** is the only centred point behind the torso.
///
/// Every one of these is a mirrored pair except 28, **and the side is the same
/// side as the hand**, which was measured rather than assumed. `m2
/// attach-trace` plays the model's own `Sheath` animation and reports which
/// static attachment the moving hand approaches: over the whole cycle the right
/// hand gets two to three times closer to 26 than to 27, on human, orc and
/// dwarf alike. So a right-hand weapon rests on the right of the back, and the
/// mirror follows.
///
/// That measurement matters because the alternative was going to be settled by
/// looking, and looking cannot settle it. A greatsword slung across a back has
/// two mirror images and *both* look like a greatsword slung across a back --
/// the same trap as the placement rotation that shipped 90 degrees wrong
/// because a building looks plausible from any side. What is asymmetric here is
/// not the picture but the animation: the character reaches over one specific
/// shoulder.
///
/// The equivalent trace for `HipSheath` does **not** separate 32 from 33 --
/// three races give three different answers -- so the hip follows the rule the
/// back established rather than a measurement of its own. Stated because it is
/// the weaker half, and the one to re-examine if a sheathed dagger looks wrong.
fn sheath_rule(sheathe_type: u32, hand: u32) -> Option<u32> {
    let right_handed = hand == m2::Attachment::HAND_RIGHT;
    let side = |right: u32, left: u32| if right_handed { right } else { left };
    Some(match sheathe_type {
        // A greatsword rides high on the shoulder blade, hilt up, blade down
        // across the back.
        1 => side(26, 27),
        // A shaft is carried the other way up, on the upper back.
        2 => side(31, 30),
        // A one-hander hangs at the belt, hilt forward and blade trailing.
        3 | 7 => side(33, 32),
        // A shield goes flat and centred on the back whichever arm carries it,
        // and 28 is the only point behind the torso that is not one of a pair.
        4 => 28,
        _ => return None,
    })
}

/// The eleven item slots `CreatureDisplayInfoExtra` carries, as inventory
/// types, in the order its columns run.
///
/// The order was measured rather than remembered: each column was identified by
/// what the items in it *paint*. The column whose 23,102 items set a foot
/// texture is the feet slot; the one whose items set only a lower-arm texture
/// is wrists; the one with almost no textures at all but a thousand cape
/// geosets is the back.
const NPC_ITEM_SLOTS: [u8; 11] = [
    1,  // head
    3,  // shoulders
    4,  // shirt
    5,  // chest
    6,  // belt
    7,  // legs
    8,  // feet
    9,  // wrists
    10, // hands
    19, // tabard
    16, // back
];

/// The geoset groups a character's own choices decide. Everything above these
/// is equipment -- see [`Look::shows`].
const MANAGED_GROUPS: [u32; 4] = [0, 1, 2, 3];

// There was a `HIDDEN_WITHOUT_EQUIPMENT` list here, holding group 15 alone, on
// the reading that the group had no bare-body variant. It had one -- see
// [`NECK_PATCH`] -- and the list existed to describe a single misidentified
// geoset. Deleted rather than emptied: a list with no members invites the next
// reader to add one by reasoning instead of by looking, which is how it got
// its first member.

/// A cache key for an appearance *and* what it is wearing.
///
/// Separate from [`Appearance::key`] because equipment changes how a character
/// looks without changing any of the five numbers: two humans in different
/// armour must not share a built model, which is the same bug `look_key`
/// already exists to prevent for faces.
pub fn look_key(appearance: &Appearance, equipment: &[(u32, u8)]) -> u64 {
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
    for (display, slot) in equipment {
        eat(u64::from(*display));
        eat(u64::from(*slot));
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

/// Resolves an appearance, dressed in the items whose display ids are given.
///
/// Every lookup degrades to `None` rather than failing: without a game
/// installation there are no tables to read, and the character still has to
/// draw -- untextured, as it did before dressing existed, rather than not at
/// all. An empty `equipment` resolves the same bare body this used to be a
/// separate function for.
///
/// `equipment` is taken in the order `SMSG_CHAR_ENUM` sends it, and that order
/// is used directly for the paint order rather than being mapped through a slot
/// enum -- which is worth stating, because it looks like luck and is not
/// *quite*. Where two items paint the same component, the array already runs
/// inner to outer: shirt before chest, bracer before glove, trouser before
/// boot. So no table of slot meanings has to be transcribed to get the layering
/// right, and the one thing that would be wrong -- an item painted over the
/// thing that should cover it -- is exactly what a render shows.
pub fn resolve_wearing(chain: &mut Chain, look: Appearance, equipment: &[(u32, u8)]) -> Look {
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

    // Read once and shared. `dress` needs it to paint and `held_items` needs it
    // to find geometry, and it is 58,000 rows -- the same table that taught this
    // project to measure a suspected cost rather than reason about it.
    let items = equipment
        .iter()
        .any(|(display_id, _)| *display_id != 0)
        .then(|| {
            chain
                .read(ItemDisplayInfo::PATH)
                .ok()
                .and_then(|bytes| ItemDisplayInfo::parse(&bytes).ok())
        })
        .flatten();
    if items.is_none() && equipment.iter().any(|(id, _)| *id != 0) {
        tracing::warn!("no ItemDisplayInfo: equipment will not be drawn");
    }
    // `Item.dbc` is read only when something is actually held, and only for
    // the sheath position: it is 46,000 rows, and a character carrying nothing
    // has no use for it. Everything else about equipment comes from
    // `ItemDisplayInfo` above.
    let held = items
        .as_ref()
        .map(|table| {
            let carries_geometry = equipment
                .iter()
                .any(|(id, kind)| *id != 0 && held_rule(*kind).is_some());
            let entries = carries_geometry
                .then(|| {
                    let started = std::time::Instant::now();
                    let parsed = chain
                        .read(dbc::schema::Item::PATH)
                        .ok()
                        .and_then(|bytes| dbc::schema::Item::parse(&bytes).ok());
                    tracing::debug!("read Item.dbc for sheath positions in {:?}", started.elapsed());
                    parsed
                })
                .flatten();
            held_items(table, entries.as_ref(), equipment)
        })
        .unwrap_or_default();

    let (mut body, mut skin) = (None, None);
    let (mut equipped, mut decided) = (Vec::new(), Vec::new());
    if let Some(sections) = &sections {
        skin = compose(chain, sections, look);
        if let (Some(skin), Some(items)) = (skin.as_mut(), items.as_ref()) {
            let (worn, groups) = dress(chain, skin, look, equipment, items);
            equipped = worn;
            decided = groups;
        }
        // Section type 0 is the base skin, indexed by skin colour -- the same
        // lookup `compose` starts from above. Kept here too, separately, only
        // as the path `Look::body` documents: what to fall back to if `skin`
        // is `None` because composition itself failed to run.
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
        geosets: geosets.into_iter().chain(equipped).collect(),
        decided_groups: decided,
        // The two-handed grip wins if anything calls for it: a character
        // cannot hold a greatsword in both hands and a dagger in one.
        stance: held
            .iter()
            .map(|item| item.stance)
            .max_by_key(|stance| match stance {
                Stance::TwoHand => 2,
                Stance::OneHand => 1,
                Stance::Unarmed => 0,
            })
            .unwrap_or_default(),
        held,
    }
}

/// The sheath type of whatever item wears a display id.
///
/// **The lookup runs the wrong way round and has to.** `sheathe_type` is a
/// column on `Item.dbc`, keyed by item *entry*; the character list gives this
/// client display ids and nothing else. So the question is not "what is this
/// item's sheath type" but "does a display id determine one", and that is
/// measurable: over every item in a held slot, 98.9% of displays are used at a
/// single sheath type.
///
/// The remaining 1.1% is settled towards the non-zero value on purpose. Almost
/// every disagreement is between zero and a real position, and zero means *no
/// resting place* -- so preferring it would leave a sword in the hand of a
/// character who should have stowed it, which is the failure this whole feature
/// exists to remove. Preferring the real position is wrong for at most a
/// handful of items and wrong visibly, which is the better direction.
fn sheathe_type_of(items: &dbc::schema::Item, display_id: u32) -> Option<u32> {
    items
        .iter()
        .filter(|item| item.display_info_id() == display_id)
        .map(|item| item.sheathe_type())
        .max()
}

/// Resolves the equipment that hangs off the skeleton rather than painting it.
///
/// Separate from [`dress`] and not gated on the composed skin, deliberately: a
/// character whose body texture failed to compose still holds its sword, and
/// tying the two together would make one failure hide the other.
fn held_items(
    table: &dbc::schema::ItemDisplayInfo,
    items: Option<&dbc::schema::Item>,
    equipment: &[(u32, u8)],
) -> Vec<HeldItem> {
    let mut held = Vec::new();
    for (display_id, inventory_type) in equipment.iter().filter(|(id, _)| *id != 0) {
        let Some((folder, attachment)) = held_rule(*inventory_type) else {
            continue;
        };
        let Some(row) = table.iter().find(|row| row.id() == *display_id) else {
            // Worth a line for the same reason `dress` logs it: the item exists
            // and this client cannot say what it looks like, which is not the
            // same as holding nothing.
            tracing::debug!("no ItemDisplayInfo row for held display {display_id}");
            continue;
        };
        // The first column, whichever hand the slot named -- see `held_rule`.
        let model = row.model_left();
        if model.is_empty() {
            continue;
        }
        let stowed = items
            .and_then(|items| sheathe_type_of(items, *display_id))
            .and_then(|sheathe| sheath_rule(sheathe, attachment));
        held.push(HeldItem {
            model: format!(
                r"Item\ObjectComponents\{folder}\{}",
                m2::model_path(model)
            ),
            texture: row.model_texture_left().to_string(),
            attachment,
            stowed,
            stance: stance_for(*inventory_type).unwrap_or_default(),
        });
    }
    // The paths, not the count. A held item that resolves to the wrong file and
    // one that resolves to no file both end as a character with empty hands,
    // and only the name says which -- the same reason this project logs the
    // body of a packet it refuses rather than its length.
    if !held.is_empty() {
        tracing::info!(
            "holding {}",
            held.iter()
                .map(|h| format!("{} on attachment {}", h.model, h.attachment))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    held
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
    /// For the geometry an NPC's gear switches on. Its *textures* are already
    /// in the baked skin, so this is read for geosets alone.
    items: Option<dbc::schema::ItemDisplayInfo>,
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
            items: chain
                .read(dbc::schema::ItemDisplayInfo::PATH)
                .ok()
                .and_then(|b| dbc::schema::ItemDisplayInfo::parse(&b).ok()),
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

        // The eleven item columns, by the slot each was measured to be.
        let equipment: Vec<(u32, u8)> = [
            row.item_display_0(), row.item_display_1(), row.item_display_2(),
            row.item_display_3(), row.item_display_4(), row.item_display_5(),
            row.item_display_6(), row.item_display_7(), row.item_display_8(),
            row.item_display_9(), row.item_display_10(),
        ]
        .into_iter()
        .zip(NPC_ITEM_SLOTS)
        .filter(|(display, _)| *display != 0)
        .collect();

        let mut worn = Vec::new();
        let mut decided = Vec::new();
        if !equipment.is_empty() {
            if let Some(items) = &self.items {
                for (display, inventory_type) in &equipment {
                    let Some((group, column)) = geoset_rule(*inventory_type) else {
                        continue;
                    };
                    let Some(item) = items.iter().find(|r| r.id() == *display) else {
                        continue;
                    };
                    let variant = [
                        item.geoset_group_0(),
                        item.geoset_group_1(),
                        item.geoset_group_2(),
                    ][column];
                    // Zero adds nothing -- see the same guard in `dress`.
                    if variant != 0 {
                        decided.push(group);
                        worn.push(group * 100 + variant);
                    }
                }
            }
        }

        Some(Look {
            skin: None,
            body,
            hair,
            geosets: geosets.into_iter().chain(worn).collect(),
            decided_groups: decided,
            // Empty, and not for want of data: `NPC_ITEM_SLOTS` carries no
            // weapon slot at all. `CreatureDisplayInfoExtra`'s eleven columns
            // are the eleven that paint the *body*, which was established by
            // measuring what each column's items paint -- so a guard's sword is
            // simply not in this table, and inventing a twelfth column to hold
            // it would be worse than a guard with empty hands.
            held: Vec::new(),
            stance: Stance::Unarmed,
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
fn dress(
    chain: &mut Chain,
    skin: &mut Skin,
    look: Appearance,
    equipment: &[(u32, u8)],
    table: &dbc::schema::ItemDisplayInfo,
) -> (Vec<u32>, Vec<u32>) {
    // `display` alone shadows a `tracing` helper of that name inside its
    // macros, so the binding is spelled out.
    let (mut geosets, mut decided) = (Vec::new(), Vec::new());
    if equipment.iter().all(|(display_id, _)| *display_id == 0) {
        return (geosets, decided);
    }
    let started = std::time::Instant::now();

    let mut worn = 0;
    for (display_id, inventory_type) in equipment.iter().filter(|(id, _)| *id != 0) {
        match table.iter().find(|row| row.id() == *display_id) {
            Some(row) => {
                wear(chain, skin, &row, look.gender);
                worn += 1;
                // Geometry as well as paint. A slot with no rule here switches
                // nothing on, which leaves the bare body showing -- the safe
                // direction, and the one `Look::shows` already fails towards.
                if let Some((group, column)) = geoset_rule(*inventory_type) {
                    let variant = [row.geoset_group_0(), row.geoset_group_1(), row.geoset_group_2()]
                        [column];
                    // **Variant zero means the item adds no geometry**, not
                    // that it decides the group is empty. Treating it as a
                    // decision hides the bare body part underneath, which is
                    // how a character in ordinary boots ends up with no feet --
                    // Testwolf's boots are exactly this case.
                    if variant != 0 {
                        decided.push(group);
                        geosets.push(group * 100 + variant);
                    }
                }
            }
            // A display id with no row is worth naming: it means the item
            // exists and this client cannot say what it looks like, which is
            // different from wearing nothing there.
            None => tracing::debug!("no ItemDisplayInfo row for display {display_id}"),
        }
    }
    tracing::debug!(
        "dressed in {worn} item(s) in {:?}; geosets {geosets:?}",
        started.elapsed()
    );
    (geosets, decided)
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

    /// The hands, and the slots that must *not* be confused with them.
    ///
    /// Asserting only that a sword lands in the right hand would pass just as
    /// well under the wrong rule, because nearly every held slot goes there.
    /// What separates a correct rule from a plausible one is the other half:
    /// the off-hand slots go to the *other* hand, and the slots that merely
    /// paint the skin -- chest, legs, boots -- hang nothing at all, even though
    /// a handful of their items do name a model. Same shape as the auto-attack
    /// filter, which had to prove the junk beside it was still refused.
    #[test]
    fn held_slots_choose_a_hand_and_the_painted_ones_choose_neither() {
        for slot in [13, 17, 21] {
            assert_eq!(
                held_rule(slot),
                Some(("Weapon", m2::Attachment::HAND_RIGHT)),
                "inventory type {slot} should be a right-hand weapon"
            );
        }
        assert_eq!(held_rule(22), Some(("Weapon", m2::Attachment::HAND_LEFT)));
        assert_eq!(held_rule(23), Some(("Weapon", m2::Attachment::HAND_LEFT)));
        assert_eq!(held_rule(14), Some(("Shield", m2::Attachment::HAND_LEFT)));

        // Painted, not held. Types 4/5 (shirt, chest), 7 (legs), 8 (feet),
        // 10 (hands) and 16 (back) all appear in `geoset_rule` or paint a
        // texture component, and a few of their items do fill a model column --
        // 16% of gloves do. None of them may put geometry in a hand.
        for slot in [1, 3, 4, 5, 6, 7, 8, 9, 10, 16, 19, 20] {
            assert_eq!(held_rule(slot), None, "inventory type {slot} is not held");
        }
        // Ranged and thrown are held in the real game and deliberately are not
        // here: which attachment they use has not been confirmed. See
        // `held_rule` -- this asserts the omission is a decision, not a gap
        // somebody closes by guessing.
        for slot in [15, 25, 26] {
            assert_eq!(
                held_rule(slot),
                None,
                "inventory type {slot} is deliberately unhandled"
            );
        }
    }

    /// A real equipped weapon resolves to a file that actually reads.
    ///
    /// The folder and the `.mdx` rename are both rules this client applies to a
    /// name it did not choose, and both fail the same silent way: a path that
    /// does not resolve is a character with empty hands, which looks exactly
    /// like the feature not being finished. Display 2380 is what `Testwolf` is
    /// carrying on the test realm -- a two-handed claymore, inventory type 17.
    ///
    /// Skipped without `WOW_DATA`, like every other test here that needs the
    /// archives.
    #[test]
    fn an_equipped_weapon_resolves_to_a_readable_model() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        let table = ItemDisplayInfo::parse(&chain.read(ItemDisplayInfo::PATH).unwrap()).unwrap();

        // With `Item.dbc` supplied, so the resting place resolves too -- the
        // claymore is a sheathe type 1 and must land on the back.
        let entries = dbc::schema::Item::parse(&chain.read(dbc::schema::Item::PATH).unwrap())
            .unwrap();
        let held = held_items(&table, Some(&entries), &[(2380, 17)]);
        assert_eq!(held.len(), 1, "the claymore was not resolved");
        assert_eq!(held[0].attachment, m2::Attachment::HAND_RIGHT);
        assert_eq!(
            held[0].stowed,
            Some(26),
            "a two-handed sword should rest on the back"
        );
        assert!(
            held[0].model.ends_with(".m2"),
            "{} was not rewritten from .mdx",
            held[0].model
        );
        let bytes = chain
            .read(&held[0].model)
            .unwrap_or_else(|e| panic!("{}: {e}", held[0].model));
        let model = m2::Model::parse(&bytes).expect("parsing the weapon");
        assert!(model.vertex_count() > 0);

        // The texture is a bare name resolved against the model's directory by
        // the loader; check the file it will look for is really there, because
        // a miss here is a grey sword rather than an error.
        let directory = held[0].model.rsplit_once('\\').unwrap().0;
        let texture = format!("{directory}\\{}.blp", held[0].texture);
        assert!(chain.read(&texture).is_ok(), "{texture} does not resolve");

        // And the hand it is going into exists on the model that will hold it.
        let human = m2::Model::parse(
            &chain
                .read(r"Character\Human\Male\HumanMale.m2")
                .expect("human male"),
        )
        .expect("parsing the wielder");
        assert!(
            human.attachment(held[0].attachment).is_some(),
            "the wielder has no right hand to hold it with"
        );
    }

    /// A stowed weapon rests on the same side as the hand that draws it, and
    /// never in the same place as the other hand's.
    ///
    /// The side is the half that was measured -- the `Sheath` animation carries
    /// the right hand two to three times closer to 26 than to 27 on every race
    /// tried. Asserting only "a two-hander goes to 26" would pass under a rule
    /// that sent *everything* to 26, so the mirror is asserted with it.
    #[test]
    fn a_stowed_weapon_rests_on_its_own_side() {
        const HAND_RIGHT: u32 = m2::Attachment::HAND_RIGHT;
        const HAND_LEFT: u32 = m2::Attachment::HAND_LEFT;

        for sheathe in [1, 2, 3, 7] {
            let right = sheath_rule(sheathe, HAND_RIGHT).expect("a right-hand resting place");
            let left = sheath_rule(sheathe, HAND_LEFT).expect("a left-hand resting place");
            assert_ne!(
                right, left,
                "sheathe type {sheathe} sends both hands to one point"
            );
        }
        assert_eq!(sheath_rule(1, HAND_RIGHT), Some(26), "greatsword, right hand");
        assert_eq!(sheath_rule(3, HAND_RIGHT), Some(33), "one-hander, right hip");
        // A shield is the exception: one centred point, whichever arm holds it.
        assert_eq!(sheath_rule(4, HAND_RIGHT), sheath_rule(4, HAND_LEFT));
    }

    /// A weapon with nowhere to rest keeps to the hand rather than vanishing.
    ///
    /// Sheathe type 0 is 97% or more of bows, guns and thrown weapons, and it
    /// means *no resting place* rather than "not known". Returning a position
    /// anyway would put a rifle somewhere nobody measured; returning `None`
    /// leaves it drawn, which is visibly odd rather than silently wrong.
    #[test]
    fn an_unsheathable_weapon_has_no_resting_place() {
        assert_eq!(sheath_rule(0, m2::Attachment::HAND_RIGHT), None);
        // And an unknown value is treated the same, not guessed at.
        assert_eq!(sheath_rule(99, m2::Attachment::HAND_RIGHT), None);
    }

    /// Every resting place is a real attachment on a real character model.
    ///
    /// The rule names six ids that were read off a posed human male. A number
    /// that is not on the model draws nothing at all and logs one debug line --
    /// exactly the silent failure this project keeps paying for -- so the ids
    /// are checked against the archives rather than trusted.
    #[test]
    fn every_resting_place_exists_on_the_model() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        for path in [
            r"Character\Human\Male\HumanMale.m2",
            r"Character\Orc\Male\OrcMale.m2",
            r"Character\Tauren\Female\TaurenFemale.m2",
        ] {
            let model = m2::Model::parse(&chain.read(path).expect(path)).expect("parsing");
            for sheathe in [1, 2, 3, 4, 7] {
                for hand in [m2::Attachment::HAND_RIGHT, m2::Attachment::HAND_LEFT] {
                    let Some(id) = sheath_rule(sheathe, hand) else {
                        continue;
                    };
                    assert!(
                        model.attachment(id).is_some(),
                        "{path} has no attachment {id} (sheathe type {sheathe})"
                    );
                }
            }
        }
    }

    /// The two hands must be different attachment points.
    ///
    /// Trivial to state and exactly the mistake that a copy-paste in
    /// `held_rule` would make -- and one that draws a shield and a sword in the
    /// same fist, which reads as "the shield is not being drawn".
    #[test]
    fn the_two_hands_are_not_the_same_point() {
        assert_ne!(m2::Attachment::HAND_LEFT, m2::Attachment::HAND_RIGHT);
    }

    /// A group the gear decided shows only what the gear named -- but a group
    /// it said nothing about still falls back to the bare body part.
    ///
    /// The difference is the whole reason `decided_groups` exists. Without it,
    /// taking a glove off leaves a hand missing rather than bare.
    #[test]
    fn gear_decides_its_own_group_and_leaves_the_rest_alone() {
        let gloved = Look {
            geosets: vec![403],
            decided_groups: vec![4],
            ..Default::default()
        };
        assert!(gloved.shows(403), "the glove it is wearing");
        assert!(!gloved.shows(401), "the bare hand it is covering");
        assert!(!gloved.shows(402), "a glove it is not wearing");
        // Boots were never decided, so the bare foot still shows.
        assert!(gloved.shows(501));
        assert!(!gloved.shows(502));
    }

    /// An item whose variant is zero adds no geometry and must not count as a
    /// decision.
    ///
    /// This is the bug the first version had, and it would have shown up as a
    /// character in ordinary boots having no feet: Testwolf's boots carry a
    /// geoset group of zero, which is the common case for starting gear.
    #[test]
    fn an_item_that_adds_no_geometry_leaves_the_bare_body() {
        let plain = Look {
            geosets: vec![],
            decided_groups: vec![],
            ..Default::default()
        };
        assert!(plain.shows(501), "the bare foot must survive plain boots");
        assert!(plain.shows(401), "and the bare hand");
        assert!(!plain.shows(502), "without turning gear on");
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
        assert_ne!(naked, look_key(&appearance, &[(9891, 5)]));
        assert_ne!(
            look_key(&appearance, &[(9891, 5)]),
            look_key(&appearance, &[(9892, 7)])
        );
        // Order matters too: it is the paint order, so a different order is a
        // different character even with the same items.
        assert_ne!(
            look_key(&appearance, &[(9891, 5), (9892, 7)]),
            look_key(&appearance, &[(9892, 7), (9891, 5)])
        );
        // And an empty slot is not the same as no slot at all being read.
        assert_eq!(naked, look_key(&appearance, &[]));
        // The slot is part of the key too: an item switches on a different
        // geoset group depending on which slot it is in, so the same display id
        // in two slots is two different characters.
        assert_ne!(
            look_key(&appearance, &[(9891, 5)]),
            look_key(&appearance, &[(9891, 10)])
        );
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
        // And group 15's variant one is **body**, not the white sheet. See
        // `NECK_PATCH`: on both human models 1501 is twenty triangles drawn
        // with the body skin at the base of the neck, while 1502 upward use
        // the cape texture and are four times the size. Hiding the group
        // wholesale to be rid of the sheet took the neck with it, and left a
        // rectangle of daylight between the shoulder blades of every
        // character in the world.
        //
        // Both halves asserted together on purpose: showing the patch is
        // worthless if it readmits the cloaks, and that is precisely the fix
        // that would look like it worked.
        assert!(look.shows(1501), "the neck patch is body geometry");
        for id in [1502, 1503, 1504, 1505, 1506] {
            assert!(!look.shows(id), "geoset {id} is a cloak nobody owns");
        }
    }

    /// A character wearing a cloak still has a neck.
    ///
    /// The exemption that the `decided_groups` rule would otherwise remove: a
    /// back item decides group 15, and "only what it names" would hide the
    /// body patch again for exactly the characters that have gear.
    #[test]
    fn a_cloak_does_not_take_the_neck_with_it() {
        let look = Look {
            geosets: vec![0, 1503],
            decided_groups: vec![15],
            ..Default::default()
        };
        assert!(look.shows(1503), "the cloak it is actually wearing");
        assert!(look.shows(1501), "and still a neck under it");
        assert!(!look.shows(1502), "but not a cloak it does not own");
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
