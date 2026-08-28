//! What a spell is called and what it looks like.
//!
//! The action bar needs a name and an icon per spell; the network only ever
//! sends an id. `Spell.dbc` turns an id into a name and an *icon id*, and
//! `SpellIcon.dbc` turns that into a texture path, which is then an ordinary
//! BLP out of the archives.
//!
//! **All of it is optional.** This client ships no game data and must run
//! without an installation, so every lookup here degrades: no archives means no
//! names and no icons, and the action bar falls back to drawing the spell's
//! number. The interface being unusable without `--data` would be the one place
//! the "never bundle assets" rule turned into "does not work without them".

use std::collections::{HashMap, HashSet};

use mpq::Chain;
use render::Gpu;

use dbc::spelltext::{self, Values};

/// Everything known about one spell.
#[derive(Debug, Clone)]
pub struct SpellInfo {
    pub name: String,
    /// Empty for most spells -- most have no rank at all, not even rank 1.
    pub rank: String,
    pub description: String,
    pub icon_path: Option<String>,
    /// Passive spells cannot be cast and have no business on a bar. Weapon
    /// skills, languages and racial bonuses are all passive, and a warrior
    /// knows far more of those than castable spells.
    pub passive: bool,
}

/// Reads one spell's numbers, resolving the two that are stored as an index.
///
/// A thin wrapper over [`dbc::spelltext::values_from_row`] -- see that
/// function's doc comment for why the mapping lives there rather than here:
/// `wow-cli spell tokens` needs the exact same one.
fn values_of(
    row: &dbc::schema::SpellRow<'_>,
    durations: &HashMap<u32, i32>,
    radii: &HashMap<u32, f32>,
) -> Values {
    dbc::spelltext::values_from_row(row, durations, radii)
}

/// `SPELL_ATTR0_PASSIVE`.
///
/// Verified rather than assumed: with this bit set aside, a warrior's bar stops
/// filling with `Axes`, `Language: Common` and `Sword Specialization`. It is
/// necessary but nowhere near sufficient -- see [`Spellbook::castable`].
const ATTR_PASSIVE: u32 = 0x0000_0040;

/// `Auto Attack`, the ability that starts and stops melee.
///
/// A hardcoded id, which this project usually refuses to do -- so the evidence
/// is worth writing down rather than the number alone. `Spell.dbc` row 6603 in
/// build 12340 is named `Auto Attack`, is not passive (attributes `0x10`), and
/// carries the description *"Automatically attacks a target in melee with an
/// equipped weapon until cancelled."* It is also the only spell in
/// `SkillLineAbility` that sits on the generic line 183 with a class mask of
/// zero *and* every character knows -- `Testwolf`'s 54 known spells include it.
///
/// The number is fixed for the client build this project targets, the same way
/// a `Map.dbc` id is, and unlike a *field offset* a wrong value here fails
/// loudly and immediately: the slot would name some other spell, and pressing
/// it would cast that spell rather than swing.
///
/// It is special-cased rather than left to the ordinary cast path because
/// auto-attack is not a cast. `CMSG_CAST_SPELL` with 6603 is not what a real
/// client sends; `CMSG_ATTACKSWING` is, and the server answers it with
/// `SMSG_ATTACKSTART`. See `App::activate_slot`.
pub const AUTO_ATTACK: u32 = 6603;

/// Which animation a spell poses its caster in, for every spell in the game.
///
/// **Not scoped to what the player knows**, unlike everything else in this
/// file. A cast bar belongs to whoever is casting, and most of the casting in
/// view is done by creatures and other players -- so a table that only
/// answered for the character's own spellbook would leave every NPC caster
/// standing perfectly still, which is the bug this exists to fix.
///
/// Two moments, from two of `SpellVisual`'s six kit columns:
///
/// * the **precast** kit's animation is the wind-up, held while the cast bar
///   runs -- `ReadySpellDirected` for Fireball, Frostbolt and Shadow Bolt;
/// * the **casting** kit's is the release, played once when the spell lands
///   -- `SpellCastDirected` for those three, `Attack1H` for Sinister Strike,
///   `Special1H` for Eviscerate and Heroic Strike.
///
/// That list is worth reading twice: those are the animations anyone who has
/// played the game would name for those spells, arrived at by following two
/// table columns. It is the same evidence that identified
/// [`dbc::schema::SpellVisualKitRow::anim`] in the first place, one level up.
///
/// The **impact** kit is deliberately not read. Its animations are
/// `CombatCritical`, `CombatWound` and `Knockdown` -- the *victim's* flinch,
/// not the caster's gesture -- so playing it on the caster would make a mage
/// recoil from their own fireball. The **channel** kit is transcribed and
/// unused for a different reason: nothing here parses a channel start, so
/// there is no event to hang it on.
#[derive(Default)]
pub struct CastAnimations {
    /// Spell id to `(wind-up head, release head)`, stored as animation ids
    /// rather than as resolved chains: 17,837 spells name a release and a
    /// chain is twelve times the size of the number it starts from. The
    /// chains are built from [`Self::chains`] on demand, which is a hash
    /// lookup and at most a dozen array writes.
    heads: HashMap<u32, (Option<u16>, Option<u16>)>,
    /// Every head that appears above, already walked down `AnimationData`'s
    /// fallback column. A few hundred entries against tens of thousands of
    /// spells, because the same handful of gestures serve the whole game.
    chains: HashMap<u16, crate::world::Cycle>,
}

impl CastAnimations {
    /// The pose to hold while a cast of this spell winds up.
    pub fn wind_up(&self, spell: u32) -> Option<crate::world::Cycle> {
        let (head, _) = self.heads.get(&spell)?;
        self.chains.get(&(*head)?).copied()
    }

    /// The gesture to play once when a cast of this spell lands.
    pub fn release(&self, spell: u32) -> Option<crate::world::Cycle> {
        let (_, head) = self.heads.get(&spell)?;
        self.chains.get(&(*head)?).copied()
    }

    /// How many spells resolved to at least one animation. For the log line
    /// that says whether this table loaded at all.
    pub fn len(&self) -> usize {
        self.heads.len()
    }

    /// Turns what a unit is doing about a spell into the pose to draw.
    ///
    /// One function rather than two lookups at each call site, because the
    /// *precedence* between the two is a decision and it should be made once:
    /// a cast in flight is a fact about now and a cast that landed is a fact
    /// about a moment ago, so the first wins whenever both are true -- which
    /// they routinely are, since the landing of the previous cast outlives
    /// the start of the next one.
    ///
    /// Falling back from an absent wind-up to the release is deliberate too.
    /// 15,920 spells name a precast animation and 17,837 name a casting one,
    /// and only 10,460 name both, so a spell with a cast bar and no wind-up
    /// is common -- and holding its release gesture for the length of the
    /// bar is a better picture than standing at ease through it.
    pub fn pose(
        &self,
        casting: Option<u32>,
        landed: Option<(u32, u32)>,
    ) -> Option<crate::world::SpellPose> {
        use crate::world::SpellPose;

        if let Some(spell) = casting {
            if let Some(cycle) = self.wind_up(spell).or_else(|| self.release(spell)) {
                return Some(SpellPose::WindUp(cycle));
            }
        }
        let (age, spell) = landed?;
        Some(SpellPose::Released(age, self.release(spell)?))
    }

    /// The same table for a caller that has no `Spell.dbc` parsed already --
    /// the headless render path.
    ///
    /// It costs a 60MB read that a screenshot has no other use for, and it is
    /// worth it: the alternative is a `--screenshot` that silently draws
    /// every caster standing at ease while the window draws them casting,
    /// which is the wrong kind of evidence about the window. Same reasoning
    /// as that path already loading `Item.dbc` so people are dressed the same
    /// way in both.
    pub fn read(chain: &mut Chain) -> Self {
        match chain
            .read(dbc::schema::Spell::PATH)
            .ok()
            .and_then(|bytes| dbc::schema::Spell::parse(&bytes).ok())
        {
            Some(spells) => Self::load(chain, &spells),
            None => Self::default(),
        }
    }

    /// Reads `Spell` → `SpellVisual` → `SpellVisualKit` → `AnimationData`.
    ///
    /// Takes the already-parsed `Spell` table rather than reading it again:
    /// it is 60MB and 185ms, and [`Spellbook::load`] is already holding one.
    ///
    /// Degrades to empty like every other lookup here -- a client with no
    /// installation draws no casts rather than failing to start.
    fn load(chain: &mut Chain, spells: &dbc::schema::Spell) -> Self {
        use dbc::schema::{AnimationData, SpellVisual, SpellVisualKit};

        let mut table = CastAnimations::default();
        let Some(visuals) = chain
            .read(SpellVisual::PATH)
            .ok()
            .and_then(|bytes| SpellVisual::parse(&bytes).ok())
        else {
            return table;
        };
        let Some(kits) = chain
            .read(SpellVisualKit::PATH)
            .ok()
            .and_then(|bytes| SpellVisualKit::parse(&bytes).ok())
        else {
            return table;
        };
        // The fallback column, which is what turns one animation id into a
        // chain a model without it can still follow.
        let fallback: HashMap<u16, u16> = chain
            .read(AnimationData::PATH)
            .ok()
            .and_then(|bytes| AnimationData::parse(&bytes).ok())
            .map(|animations| {
                animations
                    .iter()
                    .filter_map(|row| {
                        Some((u16::try_from(row.id()).ok()?, u16::try_from(row.fallback()).ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let kit_animation: HashMap<u32, u16> = kits
            .iter()
            .filter_map(|row| Some((row.id(), row.animation()?)))
            .collect();
        // Spell visual to the pair of animations its two useful kits name.
        let by_visual: HashMap<u32, (Option<u16>, Option<u16>)> = visuals
            .iter()
            .filter_map(|row| {
                let wind_up = kit_animation.get(&row.precast_kit()).copied();
                let release = kit_animation.get(&row.casting_kit()).copied();
                (wind_up.is_some() || release.is_some()).then(|| (row.id(), (wind_up, release)))
            })
            .collect();

        for row in spells.iter() {
            if let Some(pair) = by_visual.get(&row.spell_visual()) {
                table.heads.insert(row.id(), *pair);
            }
        }
        for (wind_up, release) in table.heads.values() {
            for head in [wind_up, release].into_iter().flatten() {
                table
                    .chains
                    .entry(*head)
                    .or_insert_with(|| crate::world::Cycle::chain_from(*head, &fallback));
            }
        }
        table
    }
}

/// Names, icons and the GPU textures they turned into.
#[derive(Default)]
pub struct Spellbook {
    known: HashMap<u32, SpellInfo>,
    /// Uploaded icons, by texture path. Several spells share one icon, so this
    /// is keyed by path rather than by spell.
    icons: HashMap<String, Option<egui::TextureId>>,
    /// Held so the texture views the egui ids refer to stay alive.
    #[allow(dead_code)]
    uploaded: Vec<render::UploadedTexture>,
    /// Which classes each spell belongs to, from `SkillLineAbility`. A mask of
    /// zero means the spell is in the generic skill line and belongs to nobody
    /// in particular -- which is where all the internal effects live.
    class_masks: HashMap<u32, u32>,
    /// The numbers each description's tokens stand in for. Keyed more widely
    /// than [`Self::known`], because a description may refer to a spell the
    /// character does not know -- see [`dbc::spelltext::referenced_spells`].
    values: HashMap<u32, Values>,
    /// How long a cast puts the spell itself on cooldown, in milliseconds --
    /// `Spell.dbc`'s `recovery_time` if the spell has one of its own, else
    /// `category_recovery_time` if it shares one, else absent. See
    /// [`dbc::schema::Spell::recovery_time`]'s doc comment for how the two
    /// columns were identified. Absent rather than zero for a spell with
    /// neither, the same reasoning [`Self::known_name`] returns `None`
    /// rather than an empty string: "no cooldown" and "never looked up" want
    /// different callers to be able to tell them apart, even though today's
    /// one caller treats them the same.
    cooldowns: HashMap<u32, u32>,
    /// Whether a game installation was available at all.
    pub have_data: bool,
    /// What a cast looks like, for every spell rather than only the known
    /// ones -- see [`CastAnimations`].
    pub cast_animations: CastAnimations,
    /// Ids [`Self::resolve_extra`] has already looked up, successfully or
    /// not. Separate from [`Self::known`] so a spell id `Spell.dbc` genuinely
    /// has no row for is not retried on every hover -- without this a
    /// consumable whose entry the server never sent a use-spell for would
    /// rescan the whole table once per frame for as long as it stayed
    /// hovered.
    resolved_extra: HashSet<u32>,
}

impl Spellbook {
    /// Reads names and icon paths for a set of spell ids.
    ///
    /// Scoped to the ids actually wanted rather than the whole table:
    /// `Spell.dbc` is around fifty thousand rows of 234 columns, and a
    /// character knows a few dozen spells.
    pub fn load(chain: &mut Chain, wanted: &HashSet<u32>) -> Self {
        use dbc::schema::{SkillLineAbility, Spell, SpellDuration, SpellIcon, SpellRadius};

        let mut book = Spellbook::default();
        if wanted.is_empty() {
            return book;
        }

        let started = std::time::Instant::now();
        let spells = match chain.read(Spell::PATH).ok().and_then(|bytes| Spell::parse(&bytes).ok()) {
            Some(table) => table,
            None => {
                tracing::info!("no Spell.dbc; the action bar will show spell ids");
                return book;
            }
        };
        book.have_data = true;
        tracing::debug!("Spell.dbc read in {:?}", started.elapsed());
        // Before the `wanted` filter below, and over every row rather than a
        // few dozen: a creature casting at the player is not in this
        // character's spellbook and still has to move.
        book.cast_animations = CastAnimations::load(chain, &spells);

        // The two index tables a description's `$d` and `$a1` resolve
        // through. Both are tiny -- 130 and 58 rows -- and both are optional:
        // without them those tokens stay visible, which is the same
        // degradation as everything else in this file.
        let durations: HashMap<u32, i32> = chain
            .read(SpellDuration::PATH)
            .ok()
            .and_then(|bytes| SpellDuration::parse(&bytes).ok())
            .map(|table| {
                table
                    .iter()
                    // A few rows carry a nonsense base duration next to a sane
                    // maximum (id 2 reads 300000010ms against a 30s maximum),
                    // so take the smaller. Showing a player "83 hours" for a
                    // 30-second buff is the kind of confident wrong number
                    // this whole feature is trying not to produce.
                    .map(|row| (row.id(), row.duration().min(row.max_duration())))
                    .collect()
            })
            .unwrap_or_default();
        let radii: HashMap<u32, f32> = chain
            .read(SpellRadius::PATH)
            .ok()
            .and_then(|bytes| SpellRadius::parse(&bytes).ok())
            .map(|table| table.iter().map(|row| (row.id(), row.radius())).collect())
            .unwrap_or_default();

        // Icon ids first, so the icon table is only read if something needs it.
        let mut icon_ids: HashMap<u32, u32> = HashMap::new();
        let mut referenced: HashSet<u32> = HashSet::new();
        for row in spells.iter() {
            let id = row.id();
            if !wanted.contains(&id) {
                continue;
            }
            icon_ids.insert(id, row.spell_icon_id());
            let description = row.description().to_string();
            spelltext::referenced_spells(&description, &mut referenced);
            book.values.insert(id, values_of(&row, &durations, &radii));
            // The spell's own cooldown wins over a shared category one --
            // see `Spell::recovery_time`'s doc comment.
            let cooldown_ms = if row.recovery_time() > 0 {
                row.recovery_time()
            } else {
                row.category_recovery_time()
            };
            if cooldown_ms > 0 {
                book.cooldowns.insert(id, cooldown_ms);
            }
            book.known.insert(
                id,
                SpellInfo {
                    name: row.name().to_string(),
                    rank: row.rank().to_string(),
                    description,
                    icon_path: None,
                    passive: row.attributes() & ATTR_PASSIVE != 0,
                },
            );
        }

        // A second pass for the spells the first pass's *descriptions* point
        // at -- `Power Word: Shield` quotes the duration of `Weakened Soul`,
        // which no character knows and `wanted` therefore never contains.
        // Cheap, because the table is already parsed: this is another walk
        // over memory, not another read.
        referenced.retain(|id| !book.values.contains_key(id));
        if !referenced.is_empty() {
            for row in spells.iter() {
                if referenced.contains(&row.id()) {
                    book.values.insert(row.id(), values_of(&row, &durations, &radii));
                }
            }
        }

        tracing::debug!(
            "spells scanned at {:?}; {} referenced by description",
            started.elapsed(),
            referenced.len()
        );
        let icons = chain
            .read(SpellIcon::PATH)
            .ok()
            .and_then(|bytes| SpellIcon::parse(&bytes).ok());
        if let Some(icons) = icons {
            let mut paths: HashMap<u32, String> = HashMap::new();
            for row in icons.iter() {
                paths.insert(row.id(), row.texture().to_string());
            }
            for (spell, icon) in icon_ids {
                if let Some(path) = paths.get(&icon) {
                    if let Some(info) = book.known.get_mut(&spell) {
                        // The table stores the path without an extension.
                        info.icon_path = Some(format!("{path}.blp"));
                    }
                }
            }
        }

        tracing::debug!("icons resolved at {:?}", started.elapsed());
        // Which of these are real, learnable abilities rather than internal
        // effects. See `SkillLineAbility` for why this is a table lookup and
        // not an attribute test.
        if let Some(abilities) = chain
            .read(SkillLineAbility::PATH)
            .ok()
            .and_then(|bytes| SkillLineAbility::parse(&bytes).ok())
        {
            for row in abilities.iter() {
                let spell = row.spell_id();
                if wanted.contains(&spell) {
                    // A spell can appear on several lines; take the union, so
                    // one class-owned row is enough to claim it.
                    *book.class_masks.entry(spell).or_insert(0) |= row.class_mask();
                }
            }
        }

        tracing::info!(
            "spell data loaded in {:?}: {} of {} spells named, {} with icons, \
             {} with a cast animation",
            started.elapsed(),
            book.known.len(),
            wanted.len(),
            book.known.values().filter(|s| s.icon_path.is_some()).count(),
            book.cast_animations.len()
        );
        book
    }

    /// Reads name, rank, description, icon and effect values for one spell
    /// [`Self::load`] was never asked for -- an item's on-use effect, most
    /// often, since which items exist is not known until well after login;
    /// or a trainer's whole offered list, which by definition is spells the
    /// character does not have yet and so was never in `wanted` either.
    ///
    /// A full `Spell.dbc` scan per call rather than another batch load: an
    /// item's on-use spell is discovered one at a time, as a bag or worn
    /// slot is first hovered, and there are only ever a handful of them in
    /// one session (food, drink, potions, bandages). Re-running `load`'s
    /// batch for every single one would rescan the whole fifty-thousand-row
    /// table on every new item instead of once at login. [`Self::resolved_extra`]
    /// makes repeat calls for the same id -- which a held-open tooltip makes
    /// every frame -- free after the first.
    pub fn resolve_extra(&mut self, chain: &mut Chain, spell: u32) {
        if spell == 0 || self.known.contains_key(&spell) || self.resolved_extra.contains(&spell) {
            return;
        }
        self.resolved_extra.insert(spell);

        use dbc::schema::{Spell, SpellDuration, SpellIcon, SpellRadius};
        let Some(table) = chain.read(Spell::PATH).ok().and_then(|bytes| Spell::parse(&bytes).ok())
        else {
            return;
        };
        let Some(row) = table.iter().find(|row| row.id() == spell) else {
            return;
        };

        // Same lookup `load` does for every spell in `wanted`, just for one
        // id: without it a trainer's or an on-use item's icon square would
        // stay blank forever, the same silent gap `name` had before this
        // function existed at all.
        let icon_path = chain
            .read(SpellIcon::PATH)
            .ok()
            .and_then(|bytes| SpellIcon::parse(&bytes).ok())
            .and_then(|icons| {
                let wanted_icon = row.spell_icon_id();
                // The table stores the path without an extension, same as
                // `load`. Mapped to an owned string inside this closure,
                // before `icons` itself goes out of scope.
                icons
                    .iter()
                    .find(|icon| icon.id() == wanted_icon)
                    .map(|icon| format!("{}.blp", icon.texture()))
            });

        // Same two small index tables `load` reads, for the same tokens --
        // `$d` and `$a1` need them to resolve at all.
        let durations: HashMap<u32, i32> = chain
            .read(SpellDuration::PATH)
            .ok()
            .and_then(|bytes| SpellDuration::parse(&bytes).ok())
            .map(|table| {
                table
                    .iter()
                    .map(|row| (row.id(), row.duration().min(row.max_duration())))
                    .collect()
            })
            .unwrap_or_default();
        let radii: HashMap<u32, f32> = chain
            .read(SpellRadius::PATH)
            .ok()
            .and_then(|bytes| SpellRadius::parse(&bytes).ok())
            .map(|table| table.iter().map(|row| (row.id(), row.radius())).collect())
            .unwrap_or_default();

        self.values.insert(spell, values_of(&row, &durations, &radii));
        self.known.insert(
            spell,
            SpellInfo {
                name: row.name().to_string(),
                rank: row.rank().to_string(),
                description: row.description().to_string(),
                icon_path,
                passive: row.attributes() & ATTR_PASSIVE != 0,
            },
        );
    }

    /// What to call a spell on a bar.
    pub fn name(&self, spell: u32) -> String {
        match self.known.get(&spell) {
            Some(info) if !info.name.is_empty() => info.name.clone(),
            // Without game data the id is genuinely all that is known, and
            // showing it beats showing nothing.
            _ => format!("#{spell}"),
        }
    }

    /// The spell's real name, or `None` when the book has never heard of it.
    ///
    /// Distinct from [`Spellbook::name`], which always produces something to
    /// put on a bar. A combat log needs the difference: `Wrath` is a name and
    /// `#5176` is an admission, and the log says `spell 5176` rather than
    /// dressing the admission up as one.
    pub fn known_name(&self, spell: u32) -> Option<String> {
        self.known
            .get(&spell)
            .map(|info| info.name.clone())
            .filter(|name| !name.is_empty())
    }

    /// The rank shown on a tooltip, e.g. `Rank 2`. Empty for the many spells
    /// -- most of them -- that have none.
    pub fn rank(&self, spell: u32) -> String {
        self.known.get(&spell).map(|info| info.rank.clone()).unwrap_or_default()
    }

    /// The description shown on a tooltip. Empty without game data, the same
    /// as every other lookup here.
    ///
    /// `Spell.dbc` stores descriptions as *templates* -- `78` is "A strong
    /// attack that increases melee damage by $s1" -- and this fills in the
    /// tokens whose meaning was confirmed against the data, leaving the rest
    /// visible. [`dbc::spelltext`] says which are which and why.
    ///
    /// Anything unresolved is left visible on purpose rather than stripped:
    /// this project's rule
    /// against transcribing an unverified table applies to a number's
    /// *substitution* as much as to a status code's name, and a tooltip that
    /// quietly reported a wrong damage figure would be believed. A token that
    /// is obviously a token says "not implemented yet" to the one person who
    /// can tell the difference. Blanking the tokens instead would produce
    /// sentences that read as finished and mean nothing.
    pub fn description(&self, spell: u32) -> String {
        match self.known.get(&spell) {
            Some(info) => spelltext::substitute(&info.description, spell, &self.values),
            None => String::new(),
        }
    }

    /// How long a successful cast puts this spell on cooldown, in
    /// milliseconds. `0` for a spell with no cooldown of its own -- most of
    /// them -- and for one this book has never heard of, which without game
    /// data is every spell.
    pub fn cooldown_ms(&self, spell: u32) -> u32 {
        self.cooldowns.get(&spell).copied().unwrap_or(0)
    }

    /// The icon for a spell, uploading it the first time it is asked for.
    ///
    /// Failures are cached as `None`: a BLP that will not load will not load
    /// on the next frame either, and retrying every frame would hitch forever
    /// over one bad path.
    pub fn icon(
        &mut self,
        gpu: &Gpu,
        renderer: &mut egui_wgpu::Renderer,
        chain: &mut Chain,
        spell: u32,
    ) -> Option<egui::TextureId> {
        let path = self.known.get(&spell)?.icon_path.clone()?;
        if let Some(cached) = self.icons.get(&path) {
            return *cached;
        }

        let id = (|| {
            let bytes = chain.read(&path).ok()?;
            let image = blp::Blp::parse(&bytes).ok()?;
            let uploaded = render::texture::upload_blp(gpu, &image, &path);
            let id = renderer.register_native_texture(
                &gpu.device,
                &uploaded.view,
                wgpu::FilterMode::Linear,
            );
            self.uploaded.push(uploaded);
            Some(id)
        })();
        if id.is_none() {
            tracing::debug!("no icon at {path}");
        }
        self.icons.insert(path, id);
        id
    }

    /// The spells worth putting on a bar automatically, in a stable order.
    ///
    /// It took three attempts and real data to get this right, so the reasoning
    /// is worth keeping.
    ///
    /// Dropping passives is necessary -- a warrior knows more weapon skills and
    /// languages than abilities -- and nowhere near sufficient: `Opening`,
    /// `Closing`, `Duel` and `Honorless Target` are not passive either and
    /// belong on no bar. **No attribute bit separates them from `Heroic
    /// Strike`**: it is `0x50014`, `Opening` is `0x190`, `Auto Attack` is
    /// `0x10`, and there is no bit the wanted ones share that the junk lacks.
    /// Mere membership in `SkillLineAbility` does not separate them either --
    /// all of them are in it.
    ///
    /// What does separate them is *which* line: the real abilities sit on the
    /// class's own skill line with a matching `class_mask`, while every
    /// internal effect sits on line 183, the generic catch-all, with a mask of
    /// zero. So a spell earns a slot by belonging to **this character's
    /// class**, which is a fact about the data rather than a correlate of one.
    ///
    /// Without game data nothing can be filtered, because nothing is known --
    /// and a bar of numbers the player can actually press beats an empty one.
    ///
    /// **Auto-attack is the one deliberate exception, and it has to be one.**
    /// `SkillLineAbility` puts spell 6603 on line 183 with a class mask of
    /// zero -- the same bucket as `Opening`, `Closing` and `Honorless Target`,
    /// which is precisely the bucket the rule above exists to reject. So the
    /// mechanism that correctly keeps a warrior's bar free of junk necessarily
    /// rejects the one ability every character in the game uses. Widening the
    /// rule to admit line 183 would readmit all of it; naming the single spell
    /// admits exactly what was checked.
    pub fn castable(&self, known: &[u32], class: u8) -> Vec<u32> {
        // Class ids count from 1, and the mask's bit 0 is the first class.
        let wanted_class = 1u32 << class.saturating_sub(1) as u32;
        let mut castable: Vec<u32> = known
            .iter()
            .copied()
            .filter(|id| {
                if *id == AUTO_ATTACK {
                    return true;
                }
                match self.known.get(id) {
                    Some(info) => {
                        !info.passive
                            && self
                                .class_masks
                                .get(id)
                                .is_some_and(|mask| mask & wanted_class != 0)
                    }
                    None => !self.have_data,
                }
            })
            .collect();
        // Stable across sessions, so a slot keeps its spell.
        castable.sort_unstable();
        // Auto-attack first, in both the book and the bar it seeds: it is the
        // ability a melee character uses most and the only one every class
        // has, and burying it among the sorted ids would make the list read as
        // though it were missing.
        if let Some(at) = castable.iter().position(|id| *id == AUTO_ATTACK) {
            castable[..=at].rotate_right(1);
        }
        castable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> Option<Chain> {
        let data = std::env::var_os("WOW_DATA")?;
        Some(Chain::open_wow_data(data, "enUS").expect("opening archives"))
    }

    /// The whole path, against the real archives: columns to values, values
    /// through two index tables, a second pass for a spell the character does
    /// not know, and the template filled in.
    ///
    /// `Power Word: Shield` was picked because it exercises all of that in one
    /// string *and* because every number in it is checkable away from this
    /// code: the shield lasts 30 seconds and `Weakened Soul` 15, which is what
    /// the two `SpellDuration` hops have to produce. `spelltext`'s own tests
    /// prove the grammar against values written by hand; this proves the
    /// values are the ones actually in the file.
    #[test]
    fn a_real_description_resolves_end_to_end() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        let book = Spellbook::load(&mut chain, &HashSet::from([17, 78, 6673]));
        assert_eq!(
            book.description(78),
            "A strong attack that increases melee damage by 11 and causes a high \
             amount of threat."
        );
        assert_eq!(
            book.description(6673),
            "The warrior shouts, increasing attack power of all raid and party \
             members within 30 yards by 15.  Lasts 2 min."
        );

        // The cross-reference: `$6788d` is the duration of `Weakened Soul`,
        // which no character knows and `wanted` above does not contain.
        let shield = book.description(17);
        assert!(
            shield.contains("absorbing 44 damage.  Lasts 30 sec."),
            "own values did not resolve: {shield}"
        );
        assert!(
            shield.contains("cannot be shielded again for 15 sec."),
            "a referenced spell's duration did not resolve: {shield}"
        );
        assert!(!shield.contains('$'), "a token survived: {shield}");
    }

    /// **A trainer's whole list, reproduced exactly.** `wanted` is the
    /// character's *own* spells; a trainer's rows are, by definition, spells
    /// it does not have, so `Spellbook::load` never sees them and `name`
    /// fell through to `#{id}` for every single row -- reported live as a
    /// trainer window showing nothing but numbers. Spell 1784 is `Stealth`,
    /// the entry-level rogue trainer spell this project's own fixtures teach
    /// with `.learn 1784` (see `CLAUDE.md`'s `Roguetest`).
    #[test]
    fn a_trainer_spell_never_in_wanted_still_gets_a_name_and_an_icon() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        let mut book = Spellbook::load(&mut chain, &HashSet::new());
        assert_eq!(book.name(1784), "#1784", "sanity: not resolved yet");

        book.resolve_extra(&mut chain, 1784);
        assert_eq!(book.name(1784), "Stealth");
        assert!(
            book.known.get(&1784).unwrap().icon_path.is_some(),
            "a trainer row's icon square must not stay blank forever"
        );
    }

    /// Auto-attack survives the filter that exists to reject everything it
    /// looks like.
    ///
    /// This is the check the whole exception is for. Spell 6603 sits on
    /// `SkillLineAbility`'s generic line 183 with a class mask of zero -- the
    /// same row shape as `Opening`, `Duel` and `Honorless Target`, which is
    /// exactly what [`Spellbook::castable`] is built to throw away. So the two
    /// halves have to be asserted together: the one spell is admitted, *and*
    /// the junk it is indistinguishable from is still refused. Testing only
    /// the first would pass just as well if the filter had been widened to
    /// admit line 183 wholesale, which is the wrong fix and the tempting one.
    ///
    /// Run against a real warrior's spell list rather than an invented one:
    /// the ids below are `Testwolf`'s, and the junk ones are the specific
    /// spells that turned up on a bar during 4.3 and prompted the filter.
    #[test]
    fn auto_attack_is_admitted_and_the_junk_beside_it_is_not() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        // Auto Attack, Opening, Closing, Duel, Honorless Target, Heroic
        // Strike, Battle Shout, and one passive weapon skill.
        let known = [AUTO_ATTACK, 6233, 6246, 7266, 2479, 78, 6673, 196];
        let book = Spellbook::load(&mut chain, &HashSet::from(known));
        // Class 1 is warrior.
        let castable = book.castable(&known, 1);

        assert_eq!(
            castable.first(),
            Some(&AUTO_ATTACK),
            "auto-attack has to lead the list, and is instead {castable:?}"
        );
        assert!(castable.contains(&78), "Heroic Strike was filtered out");
        assert!(castable.contains(&6673), "Battle Shout was filtered out");
        for junk in [6233u32, 6246, 7266, 2479, 196] {
            assert!(
                !castable.contains(&junk),
                "{} ({junk}) reached the bar; the filter was widened rather than \
                 given one exception",
                book.name(junk)
            );
        }
    }

    /// The survey: run every description in the build through substitution and
    /// check the shape of the result.
    ///
    /// Two spot-checked spells prove the columns are right. They cannot prove
    /// the *scanner* is, and a grammar bug is systematic -- it eats a `$` on
    /// one construct and mangles thousands of strings at once, which is
    /// exactly the failure mode this project builds surveys to catch. So:
    /// substitution may never invent a `$`, and it must actually resolve a
    /// large share of what it sees rather than passing everything through.
    #[test]
    fn substitution_over_every_description_in_the_build() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        // Every id in the table, so the survey covers constructs no warrior
        // would ever see.
        let all: HashSet<u32> = {
            use dbc::schema::Spell;
            let bytes = chain.read(Spell::PATH).expect("Spell.dbc");
            let table = Spell::parse(&bytes).expect("parsing Spell.dbc");
            table.iter().map(|row| row.id()).collect()
        };
        let book = Spellbook::load(&mut chain, &all);

        let (mut had, mut resolved, mut grew) = (0usize, 0usize, 0usize);
        for (id, info) in &book.known {
            if !info.description.contains('$') {
                continue;
            }
            had += 1;
            let out = book.description(*id);
            let before = info.description.matches('$').count();
            let after = out.matches('$').count();
            if after > before {
                grew += 1;
            }
            if after == 0 {
                resolved += 1;
            }
        }

        eprintln!(
            "{resolved} of {had} token-bearing descriptions resolved completely \
             ({:.0}%)",
            100.0 * resolved as f64 / had as f64
        );
        assert_eq!(grew, 0, "substitution invented a `$` in {grew} descriptions");
        assert!(had > 20_000, "the survey covered only {had} descriptions");
        // Measured at 82% -- 18,611 of 22,633 -- when written. The four
        // confirmed value tokens alone account for 62%; the rest is the
        // cross-spell references, which are the second most common construct
        // in the table and were nearly left out. The floor below is
        // deliberately slack: it is here to catch a scanner that stops
        // resolving anything, not to pin a number that moves whenever a
        // construct is added.
        assert!(
            resolved * 2 > had,
            "only {resolved} of {had} resolved; the scanner has regressed"
        );
    }

    /// Spells the client cannot fully explain must still read as themselves.
    /// `Rejuvenation` wraps its healing in `${$m1*5*$<mult>}`, which needs a
    /// table this client does not read -- so the expression has to survive
    /// intact next to the duration, which does resolve.
    ///
    /// The duration is also the one place in this feature where the file
    /// checks itself. `$d` resolves to 15 seconds and the tick period column
    /// reads 3000ms; the description's own hand-written `*5` is the number of
    /// ticks that implies. Two columns located by separate statistical tests
    /// agree with a literal that a Blizzard designer typed, which is a
    /// stronger statement than either test made alone.
    #[test]
    fn an_unsupported_expression_survives_beside_a_resolved_token() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        let book = Spellbook::load(&mut chain, &HashSet::from([774]));
        assert_eq!(
            book.description(774),
            "Heals the target for ${$m1*5*$<mult>} over 15 sec."
        );
    }

    /// `foss-wow#74`: `recovery_time`/`category_recovery_time` located by
    /// reading AzerothCore's public `SpellEntry` layout rather than this
    /// project's usual property test -- see `Spell::recovery_time`'s doc
    /// comment for why that stands in for one here. What settles it is a
    /// number nobody could get right by accident: `Charge` (spell 100) has
    /// no cooldown of its own, `category_recovery_time` `15000` and nothing
    /// else on the row resembling a duration -- and fifteen seconds is
    /// Charge's real, independently-known cooldown in this client's build.
    /// `Heroic Strike` and `Battle Shout` are the controls: both are
    /// castable at will in the real game and both columns read `0` on their
    /// rows, so the test would fail if either candidate column were merely
    /// "some small integer" rather than the one that means cooldown.
    #[test]
    fn charges_cooldown_reads_its_real_fifteen_seconds() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        let book = Spellbook::load(&mut chain, &HashSet::from([100, 78, 6673]));
        assert_eq!(book.cooldown_ms(100), 15_000, "Charge's real 15s cooldown");
        assert_eq!(book.cooldown_ms(78), 0, "Heroic Strike is GCD-only");
        assert_eq!(book.cooldown_ms(6673), 0, "Battle Shout has no cooldown");
    }

    /// **Spells everybody can picture, posed by following two columns.**
    ///
    /// This is the evidence that `SpellVisualKit`'s column 2 is the animation
    /// and that the two `SpellVisual` slots are the moments they are labelled
    /// as. Validity could not have shown it -- `AnimationData` is 506 rows
    /// numbered 0..505 and three other columns in the same table resolve just
    /// as often -- so what is asserted is the *identity* of the animation
    /// each named spell reaches, which nothing but the right column produces:
    ///
    /// * the three iconic ranged nukes wind up in `ReadySpellDirected` and
    ///   release in `SpellCastDirected`;
    /// * `Sinister Strike` releases in `Attack1H` -- it *is* a weapon swing;
    /// * `Eviscerate` and `Heroic Strike` release in `Special1H`.
    ///
    /// A column of coincidentally-valid small integers does not name the
    /// gesture a player would name for six spells in a row.
    #[test]
    fn a_spells_cast_animation_is_the_one_anybody_would_name() {
        let mut chain = match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        };

        // `AnimationData` ids, from the table.
        const READY_SPELL_DIRECTED: u16 = 51;
        const SPELL_CAST_DIRECTED: u16 = 53;
        const ATTACK_1H: u16 = 17;
        const SPECIAL_1H: u16 = 57;
        const STAND: u16 = 0;

        let casts = CastAnimations::read(&mut chain);
        for (spell, what) in [(133, "Fireball"), (116, "Frostbolt"), (686, "Shadow Bolt")] {
            assert_eq!(
                casts.wind_up(spell).as_deref().and_then(|c| c.first().copied()),
                Some(READY_SPELL_DIRECTED),
                "{what} should wind up in the directed ready pose"
            );
            assert_eq!(
                casts.release(spell).as_deref().and_then(|c| c.first().copied()),
                Some(SPELL_CAST_DIRECTED),
                "{what} should release in the directed cast"
            );
        }
        assert_eq!(
            casts.release(1752).as_deref().and_then(|c| c.first().copied()),
            Some(ATTACK_1H),
            "Sinister Strike is a weapon swing and animates as one"
        );
        for (spell, what) in [(2098, "Eviscerate"), (78, "Heroic Strike")] {
            assert_eq!(
                casts.release(spell).as_deref().and_then(|c| c.first().copied()),
                Some(SPECIAL_1H),
                "{what} should release in the one-handed special"
            );
        }

        // Every chain ends somewhere a model without the gesture can go --
        // the wolf case. Without it a casting creature draws its bind pose.
        for spell in [133, 116, 686, 1752, 2098, 78] {
            for chain in [casts.wind_up(spell), casts.release(spell)].into_iter().flatten() {
                assert_eq!(chain.last(), Some(&STAND), "spell {spell} can resolve to nothing");
            }
        }

        // And the pose that gets *drawn*, which is where the two moments are
        // told apart: mid-cast is the wind-up even though a release for the
        // same spell exists, and a landed cast an instant later is not.
        let pose = casts.pose(Some(133), None).expect("Fireball has a pose");
        assert!(
            matches!(pose, crate::world::SpellPose::WindUp(c) if c.first() == Some(&READY_SPELL_DIRECTED))
        );
        let landed = casts.pose(None, Some((100, 133))).expect("a landed Fireball");
        assert!(
            matches!(landed, crate::world::SpellPose::Released(100, c)
                if c.first() == Some(&SPELL_CAST_DIRECTED))
        );
        // A spell with no visual at all poses nobody, rather than posing them
        // at animation zero -- `Stand` is a real id and `0` in this column
        // means "none", which is exactly the confusion `animation()` folds.
        assert!(casts.pose(Some(5019), None).is_none(), "`Shoot` names no visual");
    }
}
