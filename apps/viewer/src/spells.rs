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
            "spell data loaded in {:?}: {} of {} spells named, {} with icons",
            started.elapsed(),
            book.known.len(),
            wanted.len(),
            book.known.values().filter(|s| s.icon_path.is_some()).count()
        );
        book
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
}
