//! Typed views over the tables we have transcribed.
//!
//! Column layouts are build-specific and are **not** in the file, so each
//! table declares the field count it expects and refuses to load anything
//! else. That check is the whole safety net: a table from a different build
//! parses fine and silently returns nonsense, and the field count is the
//! cheapest way to catch it.
//!
//! Schemas may be sparse. Declaring only the columns we use is normal --
//! `Spell` has 234 of them and most are irrelevant to a client that does not
//! implement combat yet. Unlisted columns stay reachable through
//! [`Dbc::row`](crate::Dbc::row).

use crate::{Dbc, Error, Locale, Row};

/// Expands a column declaration into an accessor.
#[doc(hidden)]
#[macro_export]
macro_rules! dbc_read {
    ($row:expr, $loc:expr, $idx:expr, u32) => {
        $row.u32($idx)
    };
    ($row:expr, $loc:expr, $idx:expr, i32) => {
        $row.i32($idx)
    };
    ($row:expr, $loc:expr, $idx:expr, f32) => {
        $row.f32($idx)
    };
    ($row:expr, $loc:expr, $idx:expr, bool) => {
        $row.bool($idx)
    };
    ($row:expr, $loc:expr, $idx:expr, str) => {
        $row.string($idx)
    };
    ($row:expr, $loc:expr, $idx:expr, loc) => {
        $row.localized_or_english($idx, $loc)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! dbc_type {
    (u32) => { u32 };
    (i32) => { i32 };
    (f32) => { f32 };
    (bool) => { bool };
    (str) => { &str };
    (loc) => { &str };
}

/// Declares a typed table: a loader that validates the field count, plus a row
/// type with one accessor per declared column.
#[macro_export]
macro_rules! dbc_table {
    (
        $(#[$meta:meta])*
        $name:ident, $row_name:ident, path = $path:literal, fields = $fields:literal, {
            $( $(#[$fmeta:meta])* $idx:literal $field:ident : $kind:ident ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        pub struct $name {
            dbc: $crate::Dbc,
            locale: $crate::Locale,
        }

        impl $name {
            /// Archive path this table is loaded from.
            pub const PATH: &'static str = $path;
            /// Field count for build 12340.
            pub const FIELDS: u32 = $fields;
            pub const NAME: &'static str = stringify!($name);

            /// Parses the table, rejecting a file whose shape does not match
            /// the transcribed schema.
            pub fn parse(bytes: &[u8]) -> ::core::result::Result<Self, $crate::Error> {
                let dbc = $crate::Dbc::parse(bytes)?;
                if dbc.fields() != Self::FIELDS {
                    return ::core::result::Result::Err($crate::Error::UnexpectedSchema {
                        table: Self::NAME,
                        expected: Self::FIELDS,
                        got: dbc.fields(),
                    });
                }
                // Word accessors assume uniform 4-byte columns, which a few
                // tables in this build violate.
                if !dbc.is_uniform() {
                    return ::core::result::Result::Err($crate::Error::NonUniform {
                        table: Self::NAME,
                        record_size: dbc.record_size(),
                        fields: dbc.fields(),
                    });
                }
                ::core::result::Result::Ok(Self { dbc, locale: $crate::Locale::EnUs })
            }

            /// Selects which locale localized columns resolve to. Falls back
            /// to English when the requested locale is blank, which it is for
            /// every locale the install was not downloaded for.
            pub fn with_locale(mut self, locale: $crate::Locale) -> Self {
                self.locale = locale;
                self
            }

            pub fn len(&self) -> usize { self.dbc.len() }
            pub fn is_empty(&self) -> bool { self.dbc.is_empty() }
            pub fn dbc(&self) -> &$crate::Dbc { &self.dbc }

            pub fn get(&self, index: usize) -> ::core::option::Option<$row_name<'_>> {
                self.dbc.row(index).map(|row| $row_name { row, locale: self.locale })
            }

            pub fn iter(&self) -> impl ::core::iter::ExactSizeIterator<Item = $row_name<'_>> {
                let locale = self.locale;
                self.dbc.rows().map(move |row| $row_name { row, locale })
            }
        }

        impl<'a> ::core::iter::IntoIterator for &'a $name {
            type Item = $row_name<'a>;
            type IntoIter = ::std::vec::IntoIter<$row_name<'a>>;
            fn into_iter(self) -> Self::IntoIter {
                self.iter().collect::<::std::vec::Vec<_>>().into_iter()
            }
        }

        #[doc = concat!("A row of [`", stringify!($name), "`].")]
        #[derive(Clone, Copy)]
        pub struct $row_name<'a> {
            row: $crate::Row<'a>,
            // Unread on tables that declare no localized columns.
            #[allow(dead_code)]
            locale: $crate::Locale,
        }

        impl<'a> $row_name<'a> {
            $(
                $(#[$fmeta])*
                pub fn $field(&self) -> $crate::dbc_type!($kind) {
                    $crate::dbc_read!(self.row, self.locale, $idx, $kind)
                }
            )*

            /// The underlying record, for columns this schema does not name.
            pub fn raw(&self) -> $crate::Row<'a> { self.row }
        }

        impl ::core::fmt::Debug for $row_name<'_> {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!($row_name))
                    $( .field(stringify!($field), &self.$field()) )*
                    .finish()
            }
        }
    };
}

dbc_table! {
    /// Every map instance: continents, dungeons, battlegrounds, and test maps.
    ///
    /// `directory` is the key that matters for rendering -- it names the folder
    /// under `World\Maps` holding the terrain tiles.
    Map, MapRow, path = r"DBFilesClient\Map.dbc", fields = 66, {
        0  id: u32,
        /// Folder under `World\Maps`, e.g. `Azeroth`.
        1  directory: str,
        /// 0 world, 1 dungeon, 2 raid, 3 battleground, 4 arena.
        2  instance_type: u32,
        3  flags: u32,
        4  pvp: u32,
        /// Player-facing name, e.g. `Eastern Kingdoms`.
        5  name: loc,
        22 area_table_id: u32,
        57 loading_screen_id: u32,
        58 minimap_icon_scale: f32,
        /// Map a corpse is moved to on death, or -1.
        59 corpse_map_id: i32,
        60 corpse_x: f32,
        61 corpse_y: f32,
        63 expansion_id: u32,
        65 max_players: u32,
    }
}

dbc_table! {
    /// Named regions within a map, forming a tree via `parent_area_id`.
    AreaTable, AreaTableRow, path = r"DBFilesClient\AreaTable.dbc", fields = 36, {
        0  id: u32,
        /// The `Map.dbc` row this area belongs to.
        1  map_id: u32,
        /// Enclosing area, or 0 for a top-level zone.
        2  parent_area_id: u32,
        /// Bit index into the client's explored-areas bitfield.
        3  area_bit: u32,
        4  flags: u32,
        7  ambience_id: u32,
        8  zone_music: u32,
        9  intro_sound: u32,
        10 exploration_level: i32,
        11 name: loc,
        28 faction_group_mask: u32,
        33 min_elevation: f32,
        34 ambient_multiplier: f32,
    }
}

dbc_table! {
    /// Visual appearance of a creature: which model, skin, and scale to use.
    ///
    /// Indirection is deliberate -- many creatures share one model with
    /// different textures, so this points at [`CreatureModelData`] rather than
    /// naming a file.
    CreatureDisplayInfo, CreatureDisplayInfoRow,
    path = r"DBFilesClient\CreatureDisplayInfo.dbc", fields = 16, {
        0  id: u32,
        /// Row in `CreatureModelData.dbc`.
        1  model_id: u32,
        2  sound_id: u32,
        3  extended_display_info_id: u32,
        4  model_scale: f32,
        5  model_alpha: u32,
        /// Skin names substituted into the model's texture slots.
        6  texture_variation_0: str,
        7  texture_variation_1: str,
        8  texture_variation_2: str,
        9  portrait_texture_name: str,
        10 blood_id: u32,
        11 npc_sound_id: u32,
        12 particle_color_id: u32,
        13 creature_geoset_data: u32,
        14 object_effect_package_id: u32,
    }
}

dbc_table! {
    /// The actual model file behind a creature, plus its collision and
    /// footprint properties.
    CreatureModelData, CreatureModelDataRow,
    path = r"DBFilesClient\CreatureModelData.dbc", fields = 28, {
        0  id: u32,
        1  flags: u32,
        /// Path to the `.mdx`/`.m2` model. Stored with an `.mdx` extension
        /// even in builds that ship `.m2` files.
        2  model_name: str,
        3  size_class: u32,
        4  model_scale: f32,
        5  blood_id: u32,
        13 sound_id: u32,
        14 collision_width: f32,
        15 collision_height: f32,
        16 mount_height: f32,
        17 geo_box_min_x: f32,
        18 geo_box_min_y: f32,
        19 geo_box_min_z: f32,
        20 geo_box_max_x: f32,
        21 geo_box_max_y: f32,
        22 geo_box_max_z: f32,
    }
}

dbc_table! {
    /// Names for animation ids. An M2 sequence stores only the numeric id, so
    /// this is the only way to know that sequence 0 is `Stand`.
    AnimationData, AnimationDataRow,
    path = r"DBFilesClient\AnimationData.dbc", fields = 8, {
        0 id: u32,
        /// Not localized: these are internal names like `Stand` or `Attack1H`.
        1 name: str,
        2 weapon_flags: u32,
        3 body_flags: u32,
        4 flags: u32,
        /// Animation to play instead when this one is unavailable.
        5 fallback: u32,
        6 behaviour_id: u32,
    }
}

dbc_table! {
    /// Which spells belong to which skill line, and to whom.
    ///
    /// This is how a real client decides what goes in a spellbook. Filtering by
    /// `Spell.dbc`'s attribute bits instead is guesswork that does not survive
    /// contact with the data: `Opening`, `Closing` and `Honorless Target` are
    /// all learnable-looking by attributes and belong in no spellbook, while
    /// `Heroic Strike` and `Auto Attack` share no single distinguishing bit.
    /// Membership here is the actual mechanism rather than a correlate of it.
    SkillLineAbility, SkillLineAbilityRow,
    path = r"DBFilesClient\SkillLineAbility.dbc", fields = 14, {
        0 id: u32,
        1 skill_line: u32,
        2 spell_id: u32,
        /// Bitmask; zero means every race.
        3 race_mask: u32,
        /// Bitmask over class ids, counting from bit 0 for warrior. Zero means
        /// every class.
        4 class_mask: u32,
    }
}

dbc_table! {
    /// Icon paths, which is the only way to get from a spell to its artwork.
    ///
    /// `Spell.dbc` stores an icon *id*, not a path, so an action bar needs
    /// both tables and a BLP loader before it can draw anything recognisable.
    /// The path has no extension on the wire -- `Interface\Icons\Spell_Fire_
    /// Fireball02` -- and `.blp` has to be appended.
    SpellIcon, SpellIconRow, path = r"DBFilesClient\SpellIcon.dbc", fields = 2, {
        0 id: u32,
        1 texture: str,
    }
}

dbc_table! {
    /// Spell definitions. 234 columns, of which this names the few a client
    /// needs before it implements combat.
    ///
    /// The effect columns below carry the numbers a description's `$s1`-style
    /// tokens stand in for. **Every one of them was located by a property of
    /// the data rather than transcribed**, because a wrong column index here
    /// parses perfectly and quotes a confident wrong number at the player --
    /// the exact failure this project's rules single out. What each test was
    /// is recorded on the column.
    Spell, SpellRow, path = r"DBFilesClient\Spell.dbc", fields = 234, {
        0   id: u32,
        1   category: u32,
        2   dispel_type: u32,
        3   mechanic: u32,
        4   attributes: u32,
        /// Row in `SpellDuration`, behind the `$d` token.
        ///
        /// Found by asking which column is non-zero *because* a spell has a
        /// duration: 98.5% of the 8,159 descriptions saying `$d` have this
        /// set against 39.0% of those that do not, and the durations it
        /// resolves to are 98.7% whole seconds (10s, 15s, 30s, 8s, 6s...).
        /// Three other columns point at valid `SpellDuration` ids just as
        /// often and are pure coincidence -- any column of small integers
        /// hits somewhere in a 130-row table.
        40  duration_index: u32,
        /// Die sides per effect, behind `$M1`: the top of a damage range,
        /// where [`Self::effect_base_points`] plus one is the bottom.
        ///
        /// Found by the property a range must have. An earlier test counted
        /// descriptions merely *mentioning* `$m1` and got a flat answer,
        /// because a single quoted value needs no die; restricted to the 88
        /// that quote both `$m1` and `$M1`, this column exceeds one in 96.6%
        /// of them against 24.9% of spells quoting a bare `$s1`.
        74  effect_die_sides: i32,
        75  effect_die_sides_2: i32,
        76  effect_die_sides_3: i32,
        /// Base value per effect, behind `$s1`. **Stored one below what is
        /// displayed.**
        ///
        /// Both facts come from the same test, and neither is transcribed. Of
        /// the 3,775 descriptions reading `$s1%`, this column plus one is a
        /// multiple of five for 69.3% -- and without the plus one, for 5.2%.
        /// Percentages in a game are round numbers; a thirteen-fold split
        /// settles both which column and which offset. `$s2` and `$s3` then
        /// pick out 81 and 82 by the same test, at 78% and 80%, which is what
        /// makes this a three-wide array rather than one lucky column.
        80  effect_base_points: i32,
        81  effect_base_points_2: i32,
        82  effect_base_points_3: i32,
        /// Row in `SpellRadius` per effect, behind `$a1`.
        ///
        /// Non-zero in 96.1% of the 721 descriptions saying `$a1` against
        /// 16.9% of the rest; `$a2` and `$a3` separate their own columns at
        /// 100% against 6.4% and 1.6%.
        92  effect_radius_index: u32,
        93  effect_radius_index_2: u32,
        94  effect_radius_index_3: u32,
        /// Milliseconds between ticks of a periodic effect, behind `$t1`.
        ///
        /// Non-zero in 95.6% of the 1,072 descriptions saying `$t1` against
        /// 5.6% of the rest, and it reads as tick periods and nothing else:
        /// 3000, 2000, 1000, 5000, 10000.
        98  effect_aura_period: i32,
        99  effect_aura_period_2: i32,
        100 effect_aura_period_3: i32,
        133 spell_icon_id: u32,
        134 active_icon_id: u32,
        136 name: loc,
        153 rank: loc,
        170 description: loc,
        187 tooltip: loc,
    }
}

dbc_table! {
    /// How long an effect lasts, indexed by [`SpellRow::duration_index`].
    ///
    /// A handful of rows carry a nonsense [`SpellDurationRow::duration`]
    /// alongside a sane [`SpellDurationRow::max_duration`] -- id 2 reads
    /// 300000010ms with a 30s maximum. Real data, not a parse error: the
    /// field count and record size both check out and the other 127 rows are
    /// ordinary. Callers wanting a number to show a player should prefer the
    /// smaller of the two rather than trusting either alone.
    SpellDuration, SpellDurationRow, path = r"DBFilesClient\SpellDuration.dbc", fields = 4, {
        0 id: u32,
        1 duration: i32,
        2 duration_per_level: i32,
        3 max_duration: i32,
    }
}

dbc_table! {
    /// Effect radii in yards, indexed by [`SpellRow::effect_radius_index`].
    SpellRadius, SpellRadiusRow, path = r"DBFilesClient\SpellRadius.dbc", fields = 4, {
        0 id: u32,
        1 radius: f32,
        2 radius_per_level: f32,
        3 max_radius: f32,
    }
}

/// Loads a table from anything that can hand back file bytes.
///
/// Kept generic so the caller decides where data comes from -- an MPQ chain in
/// the client, a loose file in a test.
pub fn parse_with<T, F>(load: F) -> Result<T, LoadError>
where
    F: FnOnce(&str) -> Result<Vec<u8>, LoadError>,
    T: TableInfo,
{
    let bytes = load(T::path())?;
    T::from_bytes(&bytes).map_err(LoadError::Parse)
}

/// Object-safe hooks the loader needs; implemented by every [`dbc_table!`].
pub trait TableInfo: Sized {
    fn path() -> &'static str;
    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("could not read table: {0}")]
    Source(String),
    #[error(transparent)]
    Parse(#[from] Error),
}

macro_rules! impl_table_info {
    ($($t:ty),* $(,)?) => {
        $(
            impl TableInfo for $t {
                fn path() -> &'static str { Self::PATH }
                fn from_bytes(bytes: &[u8]) -> Result<Self, Error> { Self::parse(bytes) }
            }
        )*
    };
}

impl_table_info!(
    Map,
    AreaTable,
    CreatureDisplayInfo,
    CreatureModelData,
    AnimationData,
    Spell,
    SpellIcon,
    SkillLineAbility
);

/// Marker so the unused-import lint does not fire on the re-exports the macro
/// relies on.
const _: () = {
    fn _uses<'a>(_: &Dbc, _: Row<'a>, _: Locale) {}
};
