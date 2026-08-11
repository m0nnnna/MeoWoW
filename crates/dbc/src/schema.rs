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
    /// Spell definitions. 234 columns, of which this names the few a client
    /// needs before it implements combat.
    Spell, SpellRow, path = r"DBFilesClient\Spell.dbc", fields = 234, {
        0   id: u32,
        1   category: u32,
        2   dispel_type: u32,
        3   mechanic: u32,
        4   attributes: u32,
        133 spell_icon_id: u32,
        134 active_icon_id: u32,
        136 name: loc,
        153 rank: loc,
        170 description: loc,
        187 tooltip: loc,
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

impl_table_info!(Map, AreaTable, CreatureDisplayInfo, CreatureModelData, Spell);

/// Marker so the unused-import lint does not fire on the re-exports the macro
/// relies on.
const _: () = {
    fn _uses<'a>(_: &Dbc, _: Row<'a>, _: Locale) {}
};
