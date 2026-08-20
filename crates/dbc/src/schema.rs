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
    /// What a *humanoid* creature looks like: the character-creation choices an
    /// NPC was built from, and the pre-baked texture of the result.
    ///
    /// Reached through [`CreatureDisplayInfo::extended_display_info_id`], and it
    /// is not an optional extra. 15,446 of the 24,262 display ids in build
    /// 12340 -- **64% of every creature appearance in the game** -- have an
    /// extended row and *no* texture variations at all, so this table is the
    /// only thing that says what colour they are. Without it those models bind
    /// a placeholder and every guard, innkeeper and quest giver in the world
    /// renders as a white ghost.
    ///
    /// `bake_name` is the whole point: a texture of the finished NPC, armour
    /// and all, already composed by the artist. A client that has it needs none
    /// of the layer blending a *player's* skin requires -- see
    /// `apps/viewer/src/character.rs`. It is a bare filename, resolved under
    /// `Textures\BakedNpcTextures\`.
    ///
    /// **Those files are in the archives but not in the listfile**, which is a
    /// trap worth naming: `wow-cli ls BakedNpcTextures` shows 50 of them and a
    /// coverage check built on that listing concluded 0.1% of bakes ship. MPQ
    /// resolves by hash, not by listing, so 40 of 40 randomly sampled names
    /// read back fine. Listing a directory and reading a path are different
    /// questions.
    ///
    /// The columns were confirmed by consistency rather than transcription:
    /// grouping every row by `race` and `gender` and asking which *model* the
    /// displays pointing at it use gives 33 groups, each dominated by exactly
    /// the matching character model -- race 1 male by `HumanMale.mdx` 2,133
    /// times, race 20 male by `NorthrendSkeletonMale.mdx` 30 times, and so on
    /// through all 21 races. A tail of one to five rows per group names a
    /// different model, which is the data reusing an extra row across displays,
    /// not a column that means something else.
    CreatureDisplayInfoExtra, CreatureDisplayInfoExtraRow,
    path = r"DBFilesClient\CreatureDisplayInfoExtra.dbc", fields = 21, {
        0  id: u32,
        /// `ChrRaces` id, 1..=21. Races 12 and up are NPC-only -- fel orc,
        /// naga, broken, vrykul, tuskarr, taunka and three kinds of skeleton
        /// and troll -- which is why this runs past the ten playable ones.
        1  race: u32,
        /// 0 male, 1 female.
        2  gender: u32,
        3  skin: u32,
        4  face: u32,
        5  hair_style: u32,
        6  hair_colour: u32,
        7  facial_hair: u32,
        /// Eleven equipped item display ids, in slot order. Read but unused
        /// until this client draws equipment; named here because the next
        /// person to want armour on an NPC should not have to rediscover that
        /// the data was already in hand.
        8  item_display_0: u32,
        9  item_display_1: u32,
        10 item_display_2: u32,
        11 item_display_3: u32,
        12 item_display_4: u32,
        13 item_display_5: u32,
        14 item_display_6: u32,
        15 item_display_7: u32,
        16 item_display_8: u32,
        17 item_display_9: u32,
        18 item_display_10: u32,
        19 flags: u32,
        /// Filename under `Textures\BakedNpcTextures\`, e.g.
        /// `CreatureDisplayExtra-00036.blp`. Empty on 22 of 15,475 rows.
        20 bake_name: str,
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
    /// What a piece of equipment looks like: the textures painted onto the
    /// wearer, and the geometry switched on for it.
    ///
    /// **The texture columns name themselves, which is why they did not have to
    /// be transcribed.** Every stored name carries the component it belongs to
    /// as a suffix -- `Generic_HuWk_01_Sleeve_AU` is an arm-upper, `..._TL` a
    /// torso-lower -- so the column order is checkable against the data rather
    /// than against memory. Over all 57,986 rows each column is 98.9% to 100%
    /// dominated by exactly its own suffix, in the order below. The stragglers
    /// are Blizzard's own typos: a handful of names ending `_A`, and a few
    /// truncated to a bare trailing underscore.
    ///
    /// Their *regions* on the composed skin were confirmed the same way. The
    /// eight components come in two resolutions -- 128 wide and 256 wide -- so
    /// size alone says nothing, but the aspect ratio does: hand, torso-lower
    /// and foot measure 4:1 and the other five 2:1, which is exactly what the
    /// character layout in `apps/viewer/src/character.rs` predicts and what
    /// would break if any two regions were swapped.
    ItemDisplayInfo, ItemDisplayInfoRow,
    path = r"DBFilesClient\ItemDisplayInfo.dbc", fields = 25, {
        0  id: u32,
        /// Attached geometry, e.g. `LShoulder_Cloth_AhnQiraj_A_01.mdx`. 100% of
        /// the 19,702 non-empty values end in `.mdx`, which is what identifies
        /// the column. Not drawn yet -- shoulders, weapons and helms hang off
        /// M2 attachment points, which is its own piece of work.
        1  model_left: str,
        2  model_right: str,
        3  model_texture_left: str,
        4  model_texture_right: str,
        /// 97.5% of non-empty values begin `INV_`.
        5  inventory_icon: str,
        6  inventory_icon_2: str,
        /// Which variant of the geoset groups this item turns on. **Not
        /// verified**: nothing here reads them yet, and which *group* each
        /// applies to depends on the item's inventory type, which is client
        /// logic rather than a column. Named so the next person knows the data
        /// is in hand; do not trust the meaning without rendering it.
        7  geoset_group_0: u32,
        8  geoset_group_1: u32,
        9  geoset_group_2: u32,
        10 flags: u32,
        11 spell_visual_id: u32,
        12 group_sound_index: u32,
        13 helmet_geoset_vis_male: u32,
        14 helmet_geoset_vis_female: u32,
        /// The eight body components, in the order their own name suffixes
        /// confirm. Bare names: the path is
        /// `Item\TextureComponents\<Component>\<name>_<M|F|U>.blp`.
        15 arm_upper: str,
        16 arm_lower: str,
        17 hand: str,
        18 torso_upper: str,
        19 torso_lower: str,
        20 leg_upper: str,
        21 leg_lower: str,
        22 foot: str,
        23 item_visual: u32,
        24 particle_colour_id: u32,
    }
}

dbc_table! {
    /// The bridge from an item to its appearance.
    ///
    /// Needed because the wire carries item *entry* ids while everything about
    /// looks is keyed by display id. `SMSG_CHAR_ENUM` helpfully sends display
    /// ids directly, so our own character needs none of this; another player's
    /// visible-item fields do.
    ///
    /// `display_info_id` was picked out against a control rather than
    /// transcribed: all 46,096 of its values are real `ItemDisplayInfo` ids,
    /// where the item id in column 0 -- a number of the same magnitude, drawn
    /// from an overlapping range -- manages only 89.6%. That gap is the whole
    /// argument; this project has been burned before by a column that looked
    /// valid because *any* small integer points somewhere inside a big table.
    Item, ItemRow, path = r"DBFilesClient\Item.dbc", fields = 8, {
        0 id: u32,
        1 class_id: u32,
        2 subclass_id: u32,
        3 sound_override_subclass: i32,
        4 material: i32,
        /// Row in [`ItemDisplayInfo`].
        5 display_info_id: u32,
        /// Which slot the item occupies, and therefore which geosets and
        /// texture components it may touch.
        6 inventory_type: u32,
        7 sheathe_type: u32,
    }
}

dbc_table! {
    /// A light: where in the world it applies, and which parameter sets to use
    /// under each weather condition.
    ///
    /// Lighting in this game is positional, not per-zone-by-name. A light sits
    /// at a point on a map with an inner and outer radius, and the client
    /// blends between whichever lights contain the camera; the row with a zero
    /// position and an enormous radius is the map's default.
    ///
    /// The eight parameter columns are conditions -- clear, storm, and so on.
    /// Only the first few are populated on most rows: column 13 is zero on all
    /// 715 rows and column 14 on 714 of them, which is what a sparsely used
    /// tail of an array looks like rather than a mis-split field.
    Light, LightRow, path = r"DBFilesClient\Light.dbc", fields = 15, {
        0 id: u32,
        1 map_id: u32,
        /// Where the light applies, in world coordinates.
        2 x: f32,
        3 y: f32,
        4 z: f32,
        /// Inside `falloff_start` the light applies fully; past `falloff_end`
        /// not at all.
        5 falloff_start: f32,
        6 falloff_end: f32,
        /// `LightParams` id per weather condition. The first is the one to use
        /// until weather exists.
        7  params_clear: u32,
        8  params_clear_water: u32,
        9  params_storm: u32,
        10 params_storm_water: u32,
        11 params_death: u32,
        12 params_unknown_1: u32,
        13 params_unknown_2: u32,
        14 params_unknown_3: u32,
    }
}

dbc_table! {
    /// One set of lighting parameters, which is really a pointer to two blocks
    /// of curves.
    ///
    /// **The curves are not in this table and are not addressed by a column.**
    /// Each row owns eighteen `LightIntBand` rows and six `LightFloatBand`
    /// rows, found by arithmetic on the id:
    ///
    /// ```text
    /// LightIntBand.id   = (LightParams.id - 1) * 18 + n + 1   for n in 0..18
    /// LightFloatBand.id = (LightParams.id - 1) * 6  + n + 1   for n in 0..6
    /// ```
    ///
    /// That rule was measured, not assumed, and it closes exactly. There are
    /// 850 `LightParams` rows, 15,300 int bands (850 x 18) and 5,100 float
    /// bands (850 x 6) -- but the ids are *sparse* in all three, running to 917,
    /// 16,506 and 5,502. The arithmetic reproduces both maxima on the nose:
    /// 916 x 18 + 18 = 16,506 and 916 x 6 + 6 = 5,502. Two independent
    /// agreements at the ends of two tables, on top of exact row-count ratios.
    LightParams, LightParamsRow,
    path = r"DBFilesClient\LightParams.dbc", fields = 9, {
        0 id: u32,
        1 highlight_sky: u32,
        2 light_skybox_id: u32,
        3 glow: f32,
        4 water_shallow_alpha: f32,
        5 water_deep_alpha: f32,
        6 ocean_shallow_alpha: f32,
        7 ocean_deep_alpha: f32,
        8 flags: u32,
    }
}

dbc_table! {
    /// One colour curve over the day: up to sixteen keys, each a time and a
    /// packed colour.
    ///
    /// Time is in half-minutes since midnight, so a full day is 2,880 and a
    /// key at 1,440 is noon.
    ///
    /// **Which of the eighteen bands is which is deliberately not named here.**
    /// They are ambient, diffuse, sky gradient, fog and so on, and this client
    /// has confirmed none of that against the data yet. Naming them from memory
    /// is the mistake `describe_cast_failure` exists to avoid: a wrong *name*
    /// never fails, it just misexplains. `wow-cli light` prints all eighteen so
    /// they can be identified by what they do.
    LightIntBand, LightIntBandRow,
    path = r"DBFilesClient\LightIntBand.dbc", fields = 34, {
        0 id: u32,
        /// How many of the sixteen key slots are used. Zero is a real answer:
        /// a band with no keys contributes nothing.
        1 count: u32,
    }
}

dbc_table! {
    /// One scalar curve over the day -- fog distances and similar -- in the
    /// same shape as [`LightIntBand`].
    LightFloatBand, LightFloatBandRow,
    path = r"DBFilesClient\LightFloatBand.dbc", fields = 34, {
        0 id: u32,
        1 count: u32,
    }
}

dbc_table! {
    /// A sky model, named by [`LightParams::light_skybox_id`].
    ///
    /// **This is the skybox this client did not have, and the reason it did
    /// not is still true**: 124 rows for a world of thousands of zones, named
    /// `StratholmeSkybox`, `CavernsOfTimeSky`, `NetherstormSkyBox` -- special
    /// places. Azeroth's default light names skybox 0 and gets none, which is
    /// why the five-band gradient is the ordinary outdoor sky and not an
    /// approximation of one.
    ///
    /// **Row 4 is `Environments\Stars\Stars.mdx`, and that is the useful
    /// part.** The star dome is not a decoration this client invented a place
    /// for: it is an entry in the same table as every zone's painted backdrop,
    /// which is what makes drawing it a transcription rather than a choice.
    ///
    /// Paths carry the historical `.mdx` extension -- see [`m2::model_path`]
    /// in the `m2` crate, which is where that rewrite lives for every table
    /// with the same habit.
    LightSkybox, LightSkyboxRow,
    path = r"DBFilesClient\LightSkybox.dbc", fields = 3, {
        0 id: u32,
        /// The model, with an `.mdx` extension that must be rewritten.
        1 model: str,
        /// 0 on 60 rows, 1 on some and 2 on others. **Deliberately not
        /// named**: three values with no observed consequence is not a flag
        /// this client has identified, and a wrong name for it would never
        /// fail loudly.
        2 flags: u32,
    }
}

/// Key slots in a lighting band. Both band tables carry sixteen.
pub const LIGHT_BAND_KEYS: usize = 16;
/// Colour curves per [`LightParams`].
pub const INT_BANDS_PER_PARAMS: u32 = 18;
/// Scalar curves per [`LightParams`].
pub const FLOAT_BANDS_PER_PARAMS: u32 = 6;
/// Half-minutes in a day, which is the unit a band's key times are in.
pub const DAY_HALF_MINUTES: u32 = 2880;

impl LightIntBandRow<'_> {
    /// Key `index` as `(time in half-minutes, packed colour)`.
    ///
    /// Read positionally rather than through thirty-two named columns: these
    /// are an array in the file and pretending otherwise would invite reading
    /// `time_9` where `value_9` was meant.
    pub fn key(&self, index: usize) -> Option<(u32, u32)> {
        if index >= self.count() as usize || index >= LIGHT_BAND_KEYS {
            return None;
        }
        Some((
            self.row.u32(2 + index),
            self.row.u32(2 + LIGHT_BAND_KEYS + index),
        ))
    }
}

impl LightFloatBandRow<'_> {
    /// Key `index` as `(time in half-minutes, value)`.
    pub fn key(&self, index: usize) -> Option<(u32, f32)> {
        if index >= self.count() as usize || index >= LIGHT_BAND_KEYS {
            return None;
        }
        Some((
            self.row.u32(2 + index),
            f32::from_bits(self.row.raw(2 + LIGHT_BAND_KEYS + index)),
        ))
    }
}

/// The id of one of a params row's colour curves.
///
/// See [`LightParams`] for why this is arithmetic rather than a column, and
/// for the two independent checks that fix it.
pub fn int_band_id(params_id: u32, band: u32) -> u32 {
    (params_id - 1) * INT_BANDS_PER_PARAMS + band + 1
}

/// The id of one of a params row's scalar curves.
pub fn float_band_id(params_id: u32, band: u32) -> u32 {
    (params_id - 1) * FLOAT_BANDS_PER_PARAMS + band + 1
}

dbc_table! {
    /// What a game object looks like: doors, chests, mailboxes, signposts,
    /// campfires, and the ships and zeppelins the server sends every client.
    ///
    /// A different table from `CreatureDisplayInfo` and indexed by a different
    /// update field, which matters more than it sounds: display 603 is a wolf in
    /// one and an inn bench in the other, so a client that reads the wrong field
    /// gets a plausible id and the wrong model. See
    /// `update::fields::GAMEOBJECT_DISPLAY_ID` for how the right field was
    /// found, and for the false positive that nearly passed.
    ///
    /// `model` may name an `.mdx` *or* a `.wmo` -- a mailbox is a small model
    /// and a ship is a building -- so a caller must be prepared for both. The
    /// renderer's path-keyed loader already is.
    GameObjectDisplayInfo, GameObjectDisplayInfoRow,
    path = r"DBFilesClient\GameObjectDisplayInfo.dbc", fields = 19, {
        0 id: u32,
        /// `.mdx` or `.wmo`, e.g. `World\Generic\Human\Passive Doodads\...`.
        1 model: str,
        /// Ten sound slots. Named for completeness; nothing here plays them.
        2  sound_0: u32,
        3  sound_1: u32,
        4  sound_2: u32,
        5  sound_3: u32,
        6  sound_4: u32,
        7  sound_5: u32,
        8  sound_6: u32,
        9  sound_7: u32,
        10 sound_8: u32,
        11 sound_9: u32,
        /// The object's own extent, for hit-testing a click without loading it.
        12 geo_box_min_x: f32,
        13 geo_box_min_y: f32,
        14 geo_box_min_z: f32,
        15 geo_box_max_x: f32,
        16 geo_box_max_y: f32,
        17 geo_box_max_z: f32,
        18 object_effect_package_id: u32,
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
    /// A spell's set of visual *moments* -- precast, casting, impact and so
    /// on -- each naming a [`SpellVisualKit`] row.
    ///
    /// **The six kit columns identified themselves by name, the same way
    /// `INTERFACE_CLICK` did.** AzerothCore's `SpellVisualEntry` documents
    /// this table's layout in comments but reads none of it -- the fields are
    /// declared and then unused, so nothing there was ever property-tested
    /// against real behaviour. Two structural facts backed trusting the
    /// *positions* anyway: the format string (`"dxxxxxxiixxx...x"`, 32
    /// characters) puts six skipped columns between the id and a `HasMissile`
    /// bool, matching the six kit fields exactly; and `HasMissile` (column 7)
    /// is true for 88.4% of spells with a non-zero `Spell.dbc` `Speed` (a
    /// missile travel speed, independently offset-matched the same way
    /// `duration_index` was) against 1.8% of speed-zero spells -- two
    /// separately-derived columns agreeing on which spells have a projectile.
    ///
    /// That confirmed the *block*, not which label attaches to which of the
    /// three early columns -- so six spells with an obvious, well-known sound
    /// were checked by ear... by name, rather: `Spell.dbc` id 116 (Frostbolt)
    /// names column 1's kit `Frost Precast`, column 2's `Ice Cast`, column
    /// 3's `BlizzardImpactVariations`. Fireball (133) gives `Precast Fire
    /// Low`, `Fire Cast`, `Molten Blast Impact`. Shadow Bolt (686) gives
    /// `Precast Shadow Low`, `Shadow Cast`, `DeathCoil Impact`. Power Word:
    /// Shield's column 4 -- `StateKit`, not one of the three -- names `Divine
    /// Shield`, a persistent buff rather than a cast or an impact. Every
    /// spell checked agrees with the label its own sound's name asserts.
    /// [`Spell::spell_visual`] is the 100%-resolving link into this table,
    /// same test as [`Spell::duration_index`].
    SpellVisual, SpellVisualRow, path = r"DBFilesClient\SpellVisual.dbc", fields = 32, {
        0 id: u32,
        /// Plays as the cast begins, before [`Self::casting_kit`]. Named
        /// `Frost Precast`, `Precast Fire Low`, `PrecastMagicLow` -- the word
        /// is in the sound's own name.
        1 precast_kit: u32,
        /// The sustained cast visual/sound, e.g. `Ice Cast`, `Fire Cast`,
        /// `Shadow Cast`, `Magic Cast`.
        2 casting_kit: u32,
        /// Plays when the spell resolves against its target, e.g. `Molten
        /// Blast Impact`, `BlizzardImpactVariations`, `DeathCoil Impact`.
        3 impact_kit: u32,
        /// A persistent visual/sound for as long as the spell's effect
        /// lasts, e.g. Power Word: Shield's `Divine Shield`. No channel-tick
        /// or aura-duration event exists yet to hang this off, so it is
        /// transcribed and unused.
        4 state_kit: u32,
        5 state_done_kit: u32,
        /// A channelled spell's own visual, e.g. Arcane Missiles'
        /// `PrecastMagicLow`. Unused for the same reason as
        /// [`Self::state_kit`] -- no channel event exists yet.
        6 channel_kit: u32,
        /// True for 88.4% of spells with a non-zero `Spell.dbc` `Speed`
        /// against 1.8% of speed-zero spells -- see the table's doc comment.
        /// Not consulted by anything yet; recorded because it is what
        /// confirmed the columns before it.
        7 has_missile: bool,
    }
}

dbc_table! {
    /// One of a spell's visual moments -- precast, cast, impact and so on --
    /// with the sound that plays for it.
    ///
    /// **Only the sound is named here.** Which *moment* a row belongs to
    /// (precast, casting, impact...) is not this table's business -- that is
    /// [`SpellVisual`], which names several of these per spell and is where
    /// the moment is now confirmed.
    ///
    /// Column 15 identified itself by the same test as `SpellDuration`:
    /// validity is nearly free (`SpellVisualKit`'s own 8,663 ids are 56%
    /// dense over their range, so almost any small integer lands on one),
    /// but *type* is not. Of 4,680 non-zero, non-sentinel values in this
    /// column, 4,653 resolve to a real `SoundEntries` row at all (99.4%),
    /// and of those, 99.9% are `SoundEntries` type 1 -- `Sound\Spells`,
    /// `Sound\Creature`. No other column among the table's other eighteen
    /// non-float, non-empty candidates got anywhere close: the runner-up
    /// (column 13) had only seven non-zero values to test at all, and the
    /// rest scattered across four or five sound types with no single one
    /// past 70%.
    SpellVisualKit, SpellVisualKitRow, path = r"DBFilesClient\SpellVisualKit.dbc", fields = 38, {
        0  id: u32,
        /// The `AnimationData` row the caster plays for this moment, or `0`
        /// for a kit that moves nobody. `0xFFFF_FFFF` also appears and means
        /// the same thing -- see [`SpellVisualKitRow::anim`], which folds
        /// both to `None`.
        ///
        /// **Validity could not have found this column and did not.**
        /// `AnimationData` is 506 rows numbered 0..505, so nearly any small
        /// integer resolves -- three other columns here also resolve 100% of
        /// the time and none of them is an animation. What identifies it is
        /// that it *varies the way an animation varies*: grouped by which
        /// [`SpellVisual`] slot names the kit, this column's top names are
        ///
        /// | moment | most common animations |
        /// |---|---|
        /// | precast (609 set) | `ReadySpellOmni` 106, `ReadySpellDirected` 94 |
        /// | casting (1,453) | `SpellCastOmni` 275, `SpellCastDirected` 244 |
        /// | channel (519) | `ChannelCastDirected` 292, `ChannelCastOmni` 96 |
        /// | impact (320) | `CombatCritical` 73, `CombatWound` 72, `Knockdown` 37 |
        /// | state (373) | `Stun` 46, `ChannelCastOmni` 31, `Whirlwind` 23 |
        ///
        /// Every row of that table is the family a person would name for the
        /// moment, and the controls are not: column 16 gives `Stop`, `Walk`,
        /// `Dead` and column 17 gives `StandWound`, `ShuffleRight` with the
        /// same 100% validity and no relation to the moment at all. Same
        /// instrument as the `Light.dbc` storm column and
        /// `CreatureSoundData` -- ask whether the candidate varies the way
        /// the thing it names varies, not whether its values are legal.
        ///
        /// **The impact row is a finding rather than a curiosity**: an impact
        /// kit's animation is the *victim's* reaction, not the caster's, so a
        /// client that played every kit's animation on whoever cast the spell
        /// would make a mage flinch each time their own bolt landed. Only the
        /// precast, casting and channel slots are read for the caster.
        2  anim: u32,
        15 sound: u32,
    }
}

impl SpellVisualKitRow<'_> {
    /// [`Self::anim`] as an animation id, with both ways of saying "none"
    /// folded together.
    ///
    /// The column stores `0` on 7,110 of 8,663 rows and `-1` (as
    /// `0xFFFF_FFFF`) on a further handful, and the two mean the same thing
    /// here. Folding them at the accessor rather than at each call site is
    /// the point: `0` is also a perfectly good animation id -- it is `Stand`
    /// -- so a caller that tests the raw column for zero is right by accident
    /// and a caller that does not is wrong silently.
    pub fn animation(&self) -> Option<u16> {
        match self.anim() {
            0 | u32::MAX => None,
            id => u16::try_from(id).ok(),
        }
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
        /// Milliseconds before this spell itself can be cast again, `0` for
        /// no individual cooldown -- `foss-wow#74`'s action-bar sweep.
        ///
        /// **Not located by this project's usual property test.** This
        /// column and [`Self::category_recovery_time`] sit at the same
        /// offsets AzerothCore's own `SpellEntry` struct names them at
        /// (`DBCStructure.h`, which documents the 3.3.5a client's DBC
        /// layout -- public documentation of a file format, not server
        /// logic; rule 2 permits reading it for a field's meaning).
        /// What stands in for a property test here is that this project's
        /// *own*, separately-derived columns already agree with that same
        /// struct's numbering at two other offsets with no coordination
        /// possible between the two derivations: [`Self::duration_index`]
        /// (40, found from `$d` token correlation) and
        /// [`Self::effect_die_sides`] (74-76, found from `$M1`/`$m1`
        /// ranges). A layout that was wrong here would have to be wrong at
        /// 40 and 74 too by coincidence, in a way that still produced a
        /// 98.5% and 96.6% correlation. Confirmed live rather than left at
        /// that: casting a spell with a real, well-known cooldown and
        /// watching the sweep clear on time, not instantly and not never --
        /// see the seeding code in `apps/viewer/src/spells.rs`.
        29  recovery_time: u32,
        /// Milliseconds before every spell **sharing this spell's
        /// `category`** can be cast again -- a shared cooldown, the way all
        /// potions or all forms of one shapeshift compete for one timer.
        /// `0` when the spell has no category-wide cooldown. See
        /// [`Self::recovery_time`]'s doc comment for how this offset was
        /// identified; the sweep uses whichever of the two is non-zero,
        /// preferring the spell's own.
        30  category_recovery_time: u32,
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
        /// A missile's travel speed in yards/second, `0` for a spell with no
        /// projectile. Offset-matched against AzerothCore's `SpellEntry`
        /// like [`Self::recovery_time`], and cross-checked rather than left
        /// at that: [`SpellVisual::has_missile`] is true for 88.4% of the
        /// spells this is non-zero for, against 1.8% of the spells it is
        /// zero for -- two independently-offset columns agreeing on which
        /// spells have a projectile.
        47  speed: f32,
        /// Row in [`SpellVisual`], which names the sounds a cast makes.
        /// Resolves for 100% of the 32,770 non-zero values in this build --
        /// the same test as [`Self::duration_index`]. A second slot sits at
        /// field 132 (`AzerothCore`'s `SpellVisual[2]`); not transcribed
        /// because nothing here has needed it and its meaning (a second
        /// race/form variant, per public documentation) is unconfirmed.
        131 spell_visual: u32,
        133 spell_icon_id: u32,
        134 active_icon_id: u32,
        136 name: loc,
        153 rank: loc,
        170 description: loc,
        187 tooltip: loc,
    }
}

dbc_table! {
    /// The eleven playable classes, and what to call one.
    ///
    /// Read for exactly one thing: the character-selection screen, which has a
    /// class id off `SMSG_CHAR_ENUM` and nothing to turn it into a word. That
    /// makes the localised name the only column here that matters, and it is
    /// also the one that identifies the rest -- **a name is the one thing in a
    /// binary format that cannot be a coincidence**, and a wrong offset here
    /// would print `PET` or `WARRIOR` rather than `Warrior`.
    ///
    /// Those two neighbours are what make the offset worth stating. Field 3 is
    /// a *pet* name token (`PET`, `DEMON`) and field 55 is the uppercase
    /// filename token (`WARRIOR`) -- both strings, both plausible, and both
    /// wrong for a screen a person reads. The localised block starts at 4, and
    /// the arithmetic says so independently of the text: a `loc` block is
    /// sixteen locale slots and a flags word, so the mask at 20 places its
    /// first slot at 4, exactly as the masks at 37 and 54 place the female and
    /// male name blocks that follow.
    ChrClasses, ChrClassesRow, path = r"DBFilesClient\ChrClasses.dbc", fields = 60, {
        0 id: u32,
        /// Which power the class spends -- 0 mana, 1 rage, 2 focus, 3 energy.
        /// **Not read anywhere**: a live unit's power type arrives in its
        /// `BYTES_0` field, which is a fact about the unit rather than about
        /// its class, and a druid disagrees with this column in three forms.
        2 display_power: u32,
        /// `PET` on nine of the ten rows and `DEMON` on the warlock. Named
        /// only so the neighbouring offset is documented rather than
        /// mysterious -- see the note above.
        3 pet_name_token: str,
        /// What the class is called, e.g. `Warrior`.
        4 name: loc,
        /// The uppercase token, e.g. `WARRIOR`. The form the original client
        /// uses as a key; kept for the same reason as `pet_name_token`.
        55 filename: str,
    }
}

dbc_table! {
    /// Every race, playable and NPC-only, and what to call one.
    ///
    /// The companion to [`ChrClasses`] and read for the same one reason. Ids
    /// run 1..=21 in this build; 12 and up are NPC-only (fel orc, naga,
    /// broken, and so on) and never arrive from a character list, but they are
    /// in the table and nothing here filters them -- a row that cannot be
    /// asked for costs nothing, and inventing a "playable" predicate would be
    /// transcribing a rule this project has not measured.
    ///
    /// The name block starts at 14 by the same arithmetic that places
    /// [`ChrClasses`]'s: its locale mask sits at 30, sixteen slots later.
    ChrRaces, ChrRacesRow, path = r"DBFilesClient\ChrRaces.dbc", fields = 69, {
        0 id: u32,
        1 flags: u32,
        /// The race's own faction, which is what decides who it can speak to.
        /// Not read here; named because it is what a later "can these two
        /// group" question would want and it is otherwise an unlabelled small
        /// integer.
        2 faction_id: u32,
        /// `CreatureDisplayInfo` for a male and a female of this race. The
        /// character screen does not use them -- a character list carries its
        /// own display ids -- but they are what makes this table's identity
        /// checkable against a model that already renders.
        4 male_display_id: u32,
        5 female_display_id: u32,
        /// The prefix the client puts in front of a texture name, e.g. `Hu`
        /// for human. The same key `CharSections` rows are built around.
        6 client_prefix: str,
        /// What the race is called, e.g. `Night Elf`.
        14 name: loc,
    }
}

dbc_table! {
    /// A character's skin, face, hair and facial-hair textures.
    ///
    /// Players do not get their appearance from `CreatureDisplayInfo` the way
    /// creatures do -- display id 49 is every human male alive, and its texture
    /// columns are empty. The look comes from here instead, keyed by race,
    /// gender, what *kind* of section it is, and the two indices the player
    /// chose at character creation.
    ///
    /// Column meanings were read off the data rather than transcribed: for
    /// race 1 / sex 0, section type 0 yields `HumanMaleSkin00_00`, type 1
    /// `HumanMaleFaceLower00_00` alongside `...FaceUpper00_00`, type 2
    /// `FacialLowerHair00_00`, and type 4 `HumanMaleNakedPelvisSkin00_00`.
    /// Names that say what they are is about as unambiguous as a column gets.
    CharSections, CharSectionsRow, path = r"DBFilesClient\CharSections.dbc", fields = 10, {
        0 id: u32,
        1 race: u32,
        2 gender: u32,
        /// 0 base skin, 1 face, 2 facial hair, 3 hair, 4 underwear.
        3 section_type: u32,
        /// Up to three layers. Only the first is used for a base skin; a face
        /// splits into a lower and an upper half across the first two.
        4 texture_0: str,
        5 texture_1: str,
        6 texture_2: str,
        7 flags: u32,
        /// Which variation of this section -- the face or hairstyle number.
        8 variation: u32,
        /// Which colour of it -- the skin or hair colour number.
        9 colour: u32,
    }
}

dbc_table! {
    /// Which geoset a hairstyle turns on.
    ///
    /// A character model ships every hairstyle as a separate geoset in group
    /// zero and expects the client to show exactly one. Draw them all and the
    /// character wears every haircut at once, which is precisely what this
    /// client did before reading this table.
    CharHairGeosets, CharHairGeosetsRow, path = r"DBFilesClient\CharHairGeosets.dbc", fields = 6, {
        0 id: u32,
        1 race: u32,
        2 gender: u32,
        /// The hairstyle number the player chose.
        3 variation: u32,
        /// The geoset in group zero to show. Zero is the bald scalp.
        4 geoset: u32,
        /// Whether the scalp shows through, for styles that do not cover it.
        5 show_scalp: u32,
    }
}

dbc_table! {
    /// Which geosets a facial-hair choice turns on, across three groups.
    ///
    /// The three columns are variants within geoset groups 1, 3 and 2 -- in
    /// that order, which is not the order they are numbered. Confirmed against
    /// `HumanMale.m2`, which ships exactly `101 102 201 202 301 302`: variants
    /// one and two of each of the three groups, and nothing else that would
    /// fit a different reading.
    CharacterFacialHairStyles, CharacterFacialHairStylesRow,
        path = r"DBFilesClient\CharacterFacialHairStyles.dbc", fields = 8, {
        0 race: u32,
        1 gender: u32,
        2 variation: u32,
        3 geoset_100: u32,
        4 geoset_300: u32,
        5 geoset_200: u32,
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

dbc_table! {
    /// Where a released ghost reappears, keyed by the graveyard id a
    /// `SMSG_DEATH_RELEASE_LOC` names.
    ///
    /// The server picks the graveyard and sends its position directly, so
    /// nothing in the death flow needs to *resolve* one -- this table exists
    /// so a graveyard id in a packet or a log can be turned into a place a
    /// person recognises.
    ///
    /// Columns identified by their own shape, then checked against two
    /// derivations sharing no code with each other or with this table:
    /// `map_id` lands on a real `Map.dbc` row for **100%** of 685 rows,
    /// where a control column (the graveyard's own id, read as if it were a
    /// map id) hits only 5% -- `Map.dbc` is small and dense, so validity
    /// alone proves little and the gap between candidate and control is the
    /// argument. And the Stormwind and Redridge graveyards' `z` land within
    /// 2.2 and 0.001 units of the ground `wow-cli adt height` computes at
    /// the same `x`/`y` from the terrain files, independently of this table.
    ///
    /// One row (id 1036) has `x = y = z = 0.0` and is named `"Reuse"` in the
    /// data itself -- a genuine Blizzard placeholder, not a parse bug; the
    /// same shape as `SpellDuration`'s nonsense duration on id 2.
    WorldSafeLocs, WorldSafeLocsRow, path = r"DBFilesClient\WorldSafeLocs.dbc", fields = 22, {
        0 id: u32,
        /// `Map.dbc` row this graveyard sits on.
        1 map_id: u32,
        2 x: f32,
        3 y: f32,
        4 z: f32,
        /// Player-facing description, e.g. `"Redridge Mountains"` or
        /// `"Duskwood, Darkshire"` -- not always the graveyard's own name.
        5 name: loc,
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

dbc_table! {
    /// Every sound the client can play: what it is called, where its files
    /// live, and how loud and how far it carries.
    ///
    /// The central sound table. Nothing else here names a file -- `ZoneMusic`,
    /// `SoundAmbience` and the rest all point *at* this by id, so this is the
    /// one that has to be right.
    ///
    /// **A sound is a set of files, not one file.** Ten name columns and ten
    /// weights sit side by side: a footstep or a sword hit picks one at random
    /// each time it plays, which is what stops a fight sounding like a metronome.
    /// Most entries fill only the first one or two. The names are bare
    /// filenames and [`SoundEntriesRow::directory`] holds the folder they sit
    /// in, so a playable path is the two joined.
    ///
    /// **Transcribed against a check the data cannot fake.** Column indices
    /// here were read off the file's own shape -- ten consecutive string
    /// columns of decreasing density, then ten small-integer columns with the
    /// same decreasing density -- and that pattern alone would also fit
    /// several wrong alignments. What settles it is that the strings must name
    /// *files that exist*: joining column 23 to columns 3-12 has to produce
    /// paths the archive can resolve, and it does for essentially every entry.
    /// A one-column slip breaks that immediately, where a plausible-looking
    /// dump would not. See `wow-cli sound survey`.
    SoundEntries, SoundEntriesRow,
    path = r"DBFilesClient\SoundEntries.dbc", fields = 30, {
        0 id: u32,
        /// What kind of sound this is -- music, an ambience loop, a spell, a
        /// footstep. Values run 1-53 in this build.
        ///
        /// See [`SoundType`], which names the three values whose contents
        /// are unambiguous and passes everything else through as a number.
        1 sound_type: u32,
        /// A human-readable label, always set. Useful for finding a sound by
        /// eye and worthless for playing one.
        2 name: str,
        /// The folder the files sit in, relative to the archive root, e.g.
        /// `Sound\Ambience\ZoneSpecific`. Joined to a filename from
        /// [`SoundEntriesRow::files`] to make a path that resolves.
        23 directory: str,
        /// 0.01 to 1.0 across the table.
        24 volume: f32,
        25 flags: u32,
        /// Below this distance the sound plays at full volume.
        26 min_distance: f32,
        /// Past this distance it is not audible at all. 1 to 1000.
        27 distance_cutoff: f32,
        28 eax_definition: u32,
        29 advanced_id: u32,
    }
}

dbc_table! {
    /// Which music a zone plays, and how long it stays quiet between tracks.
    ///
    /// Reached from [`AreaTable`]'s `zone_music`. Names no file itself -- the
    /// two id columns point at [`SoundEntries`], which is where the paths
    /// live.
    ///
    /// **Day and night are separate tracks, and that is what identifies the
    /// pair.** Two adjacent columns of sound ids could just as easily be one
    /// value duplicated, or a track and a fallback. `Zone-Forest` has 2523 in
    /// both and says nothing; `Zone-EvilForest` has 2524 and 2534, and
    /// `Zone-Jungle` has 5494 and 2535. A column that differs from its
    /// neighbour on some rows and not others is two things, not one written
    /// twice.
    ///
    /// Verified beyond mere validity: every id here resolves to a
    /// [`SoundEntries`] row **of the music type**, which a wrong column would
    /// not manage -- see `wow-cli sound zones`.
    ZoneMusic, ZoneMusicRow, path = r"DBFilesClient\ZoneMusic.dbc", fields = 8, {
        0 id: u32,
        /// A label like `Zone-Forest`. Never played, useful for finding a row.
        1 name: str,
        /// Milliseconds of silence between tracks. Min before max: column 3
        /// reaches 1,800,000 where column 2 stops at 300,000, and a minimum
        /// cannot exceed its own maximum.
        ///
        /// **Not used yet.** This client plays a zone's track on a loop rather
        /// than pausing between plays, so these are transcribed and ignored.
        2 silence_min_day: u32,
        3 silence_max_day: u32,
        4 silence_min_night: u32,
        5 silence_max_night: u32,
        /// [`SoundEntries`] id played during the day.
        6 day_sound: u32,
        /// [`SoundEntries`] id played at night. Often the same as the day
        /// track; sometimes deliberately not.
        7 night_sound: u32,
    }
}

dbc_table! {
    /// The looping background of a zone -- birdsong, wind, water.
    ///
    /// Reached from [`AreaTable`]'s `ambience_id`, and the smallest table
    /// here: an id and two [`SoundEntries`] ids, for day and night.
    ///
    /// Every one of the 412 ids across this table resolves to a
    /// [`SoundEntries`] row of the ambience type, which is a far stronger
    /// statement than the ids merely being in range -- see
    /// `wow-cli sound zones`.
    SoundAmbience, SoundAmbienceRow,
    path = r"DBFilesClient\SoundAmbience.dbc", fields = 3, {
        0 id: u32,
        1 day_sound: u32,
        2 night_sound: u32,
    }
}

dbc_table! {
    /// A creature's voice: what it says when it attacks, is hurt, or dies.
    ///
    /// Reached from `CreatureDisplayInfo`'s `sound_id`. Thirty-eight columns
    /// of `SoundEntries` ids with nothing in the file saying which is which,
    /// and every one of them holds ids from the same range -- so validity
    /// separates none of them and a wrong column plays a footstep for a death.
    ///
    /// **The columns identified themselves through the names of the sounds
    /// they point at.** `SoundEntries` carries a human label per sound and
    /// those labels are systematic, so a column whose entries are called
    /// `WolfDeath`, `BearDeath` and `KoboldDeath` is the death column. That is
    /// a measurement, and `wow-cli sound creatures` is the instrument -- it
    /// tallies the trailing word of every name each column reaches.
    ///
    /// Only the five that came back overwhelming are named:
    ///
    /// | field | rows set | name tail | also |
    /// |---|---|---|---|
    /// | 1 | 818 | `Attack` x787 | 816 are creature-typed sounds |
    /// | 3 | 836 | `Wound` x826 | 836 |
    /// | 4 | 818 | `Crit`/`Critical` x749 | 818 |
    /// | 6 | 659 | `Death` x634 | 658 |
    /// | 10 | 575 | `Aggro` x459 | 575 |
    ///
    /// The rest stay unnamed. Several are obvious guesses -- field 13 is 351
    /// `Aggro` and field 33 is 15 `Birth` -- and a guess is what this refuses.
    ///
    /// Field 0 is the id, and it is worth saying why that needed proving too:
    /// it is set on all 1,306 rows and 935 of those "resolve" to a real sound,
    /// which looks like a populated sound column until you notice only 102 of
    /// them are creature sounds. Ids overlapping a table's id range is a
    /// coincidence of magnitude, not a reference.
    CreatureSoundData, CreatureSoundDataRow,
    path = r"DBFilesClient\CreatureSoundData.dbc", fields = 38, {
        0  id: u32,
        /// Played when the creature swings.
        1  attack: u32,
        /// Played when the creature is hit.
        3  wound: u32,
        /// Played when the creature is hit hard.
        4  wound_critical: u32,
        /// Played when it falls over.
        6  death: u32,
        /// Played when it notices you.
        10 aggro: u32,
        /// Which group of surface sounds this creature's feet use: a
        /// [`FootstepTerrainLookup::creature_footstep_id`], **not** a
        /// `SoundEntries` id like every other column in this table.
        ///
        /// That is what identified it, and it is the cleanest identification
        /// in the table. `FootstepTerrainLookup` names only **23** distinct
        /// groups out of the 0..=188 range they span, so landing inside that
        /// set is not free the way landing inside `SoundEntries` is. Of the 38
        /// columns here, field 9 is set on 738 rows and **738 of 738** name a
        /// real group; the best any other column manages is 21 of 1,306
        /// (1.6%), and those are coincidences of magnitude in the id column.
        /// The values also span exactly the group range -- 6 to 188 -- where
        /// every neighbouring column holds four-figure sound ids.
        9  footstep_group: u32,
    }
}

dbc_table! {
    /// The materials a foot can land on: dirt, stone, snow, wood, grass.
    ///
    /// Twelve rows, and the second column is a *name* -- which makes this one
    /// of the few tables here that identifies its own contents. Everything
    /// downstream is checked against those names rather than against a
    /// remembered ordering.
    ///
    /// **[`TerrainTypeRow::sound_id`] is not the row id**, and the difference
    /// matters because both are small integers in overlapping ranges. `Dirt`
    /// is row 0 and sound 1, `Metallic` row 1 and sound 2, and so on -- an
    /// off-by-one all the way down, with `DustyGrass` (row 9) sharing `Grass`'s
    /// sound 6 and `None` (row 10) having sound 0. What
    /// [`FootstepTerrainLookup`] is keyed by is the **sound**, not the row; see
    /// its doc comment for the measurement that separates the two.
    ///
    /// The two spray columns name the ground effect kicked up by a footfall.
    /// Only `Snow` sets them in this build, and nothing here draws either.
    TerrainType, TerrainTypeRow,
    path = r"DBFilesClient\TerrainType.dbc", fields = 6, {
        0 id: u32,
        /// `Dirt`, `Metallic`, `Stone`, `Snow`, `Wood`, `Grass`, `Leaves`,
        /// `Sand`, `Soggy`, `DustyGrass`, `None`, `Water`.
        1 name: str,
        2 footstep_spray_run: u32,
        3 footstep_spray_walk: u32,
        /// What [`FootstepTerrainLookup`] keys on. Ten distinct values over
        /// twelve rows.
        4 sound_id: u32,
        5 friction: bool,
    }
}

dbc_table! {
    /// Which sound a particular creature makes stepping on a particular
    /// surface: 217 rows of `(creature footstep group, terrain, sound)`.
    ///
    /// **The terrain column is a [`TerrainType::sound_id`], not a
    /// [`TerrainType`] row id**, and the two readings both parse. What
    /// separates them is that `SoundEntries` names its rows: taking the fifty
    /// footstep sounds whose name carries a material word and asking which
    /// reading agrees with it, the sound-id reading scores **25 of 50** and the
    /// row-id reading **9 of 50** -- and the misses are not scattered. Terrain
    /// 1 is `Dirt` five times out of five, 3 is `Stone` four of five, 4 is
    /// `Snow` five of five, 5 is `Wood` five of five and 6 is `Grass` four of
    /// five. The three values that never match a material of their own --
    /// `Leaves`, `Sand` and `Soggy` -- reach grass and dirt sounds instead,
    /// because this build ships no leaf or sand footstep to reach. Under the
    /// row-id reading every one of those columns is off by one and `Snow`
    /// plays on wood.
    ///
    /// Terrain **0** is not a row of [`TerrainType`] at all -- its ids start at
    /// sound 1 -- and the seventeen rows carrying it reach dirt sounds. It is
    /// the fallback, and this client uses it wherever the ground does not name
    /// a terrain, which is most of the world.
    ///
    /// Both sound columns resolve completely: **342 of 342** references land on
    /// a real `SoundEntries` row. They are also of different *types*, which is
    /// what named them: all 217 ordinary sounds are type 3, while 115 of the
    /// 125 splash sounds are type 20 and 115 of them have `Splash` in their
    /// name.
    FootstepTerrainLookup, FootstepTerrainLookupRow,
    path = r"DBFilesClient\FootstepTerrainLookup.dbc", fields = 5, {
        0 id: u32,
        /// The group of surfaces one kind of creature has sounds for, named by
        /// [`CreatureSoundData::footstep_group`].
        1 creature_footstep_id: u32,
        /// A [`TerrainType::sound_id`], or 0 for the fallback.
        2 terrain: u32,
        /// `SoundEntries` id for stepping on dry ground.
        3 sound: u32,
        /// `SoundEntries` id for stepping in water. Zero on 92 of 217 rows.
        4 sound_splash: u32,
    }
}

dbc_table! {
    /// What grows on a terrain texture, and -- the only column read here --
    /// what it sounds like underfoot.
    ///
    /// A map chunk's texture layer names one of these rows through its
    /// `effect_id`, and this is the only link in the game's data from a patch
    /// of ground to a [`TerrainType`]. The other columns are the grass and
    /// scrub doodads scattered over it, which this client does not draw.
    ///
    /// **22,708 of 24,981 rows carry terrain 0**, which is not `Dirt`: it is
    /// the row saying nothing about the surface, exactly as in
    /// [`FootstepTerrainLookup`]. Most of the table is doodad scatter with no
    /// sound attached, so a client treating 0 as `Dirt` would be asserting a
    /// material for the majority of the world on no evidence.
    GroundEffectTexture, GroundEffectTextureRow,
    path = r"DBFilesClient\GroundEffectTexture.dbc", fields = 11, {
        0 id: u32,
        /// A [`TerrainType`] **row id**, unlike everything downstream of it.
        /// Values run 0..=11, which is exactly that table's twelve rows.
        10 terrain_type: u32,
    }
}

dbc_table! {
    /// What a weapon sounds like when it lands.
    ///
    /// Thirty rows, one per weapon subclass, and each names ten sound ids for
    /// the ten things a weapon can hit -- then ten more for the critical
    /// versions of the same.
    ///
    /// **The columns named themselves, the same way `CreatureSoundData`'s
    /// did.** Row 1 is subclass 0 and its first three ids resolve to sounds
    /// called `Axe1H_ArmorFlesh`, `Axe1H_ArmorChain` and `Axe1H_ArmorPlate`;
    /// the second block's first is `Axe1H_ArmorFleshCritical`. That is the
    /// layout stated by the data rather than recalled: flesh, chain and plate
    /// in order, and the second block mirroring the first.
    ///
    /// Only flesh is transcribed. Chain and plate need the *target's* armour,
    /// which this client does not know -- and the remaining seven columns
    /// (shield impacts, parries, wood, stone) are events it does not model
    /// either. Naming columns nothing can use would be transcribing for its
    /// own sake; they are recorded in the doc above and left in the file.
    ///
    /// Field 1 is the subclass from [`Item`]: a two-handed sword is subclass 8
    /// and lands on the row whose field 1 is 8.
    WeaponImpactSounds, WeaponImpactSoundsRow,
    path = r"DBFilesClient\WeaponImpactSounds.dbc", fields = 23, {
        0  id: u32,
        /// [`Item`]'s `subclass_id` for weapons.
        1  weapon_subclass: u32,
        /// Hitting an unarmoured target -- which is every creature, as far as
        /// this client can tell.
        3  flesh: u32,
        /// The same, on a critical.
        13 flesh_critical: u32,
    }
}

dbc_table! {
    /// One drawable map page, and the rectangle of the world it shows.
    ///
    /// 108 rows in this build: a page per zone, per city and per instance,
    /// plus a whole-continent page each for `Azeroth`, `Kalimdor`,
    /// `Expansion01` and `Northrend` (the rows whose `area_id` is 0).
    ///
    /// **The four bounds are named for the axis they were measured to hold,
    /// not for an edge of the picture.** Which world axis each pair covers is
    /// a fact about the data; which *side* of the image a bound corresponds to
    /// is a fact about the projection, and that is decided once in
    /// [`crate::worldmap`] rather than smuggled in through a field name here.
    /// The measurement: quest 783's server-side point of interest is
    /// `(-8903, -163)` in `WorldMapAreaId` 30, and this table's row 30
    /// (`Elwynn`) has fields 6/7 spanning `-7939.583 .. -10254.166` and fields
    /// 4/5 spanning `1535.417 .. -1935.417`. Only one assignment puts the
    /// point inside its own zone, and `wow-cli map calibrate` re-runs that
    /// containment test across every zone using terrain area ids.
    ///
    /// `field 4 > field 5` and `field 6 > field 7` on all 108 rows, which is
    /// what makes `max`/`min` the honest names.
    WorldMapArea, WorldMapAreaRow, path = r"DBFilesClient\WorldMapArea.dbc", fields = 11, {
        0 id: u32,
        /// The [`Map`] row this page belongs to.
        1 map_id: u32,
        /// The [`AreaTable`] zone this page draws, or 0 for a continent page.
        2 area_id: u32,
        /// Folder under `Interface\WorldMap` holding the twelve tiles, e.g.
        /// `Elwynn` -- an internal name, not the player-facing one, which
        /// lives in [`AreaTable`].
        3 directory: str,
        /// Largest world `y` the page covers.
        4 y_max: f32,
        /// Smallest world `y` the page covers.
        5 y_min: f32,
        /// Largest world `x` the page covers.
        6 x_max: f32,
        /// Smallest world `x` the page covers.
        7 x_min: f32,
    }
}

dbc_table! {
    /// A patch of a zone page revealed once the player has explored it.
    ///
    /// The base twelve tiles of a zone page are the *unexplored* picture;
    /// every named sub-area is a separate texture blitted on top at a pixel
    /// offset. 988 rows in this build.
    ///
    /// This client does not draw the overlays yet -- what it uses them for is
    /// **calibration**. Each row states, in map-image pixels, where an
    /// [`AreaTable`] area sits on its page, and the terrain files state, in
    /// world coordinates, which area every chunk belongs to. Those two are
    /// derived from different files by different tools, so agreement between
    /// them is evidence about the projection rather than about either one.
    /// See `wow-cli map calibrate`.
    ///
    /// Fields 6 and 7 are zero on all 988 rows and are therefore left
    /// unnamed: a column that never varies cannot be identified, and guessing
    /// at it would be transcription.
    WorldMapOverlay, WorldMapOverlayRow,
    path = r"DBFilesClient\WorldMapOverlay.dbc", fields = 17, {
        0  id: u32,
        /// The [`WorldMapArea`] page this patch is drawn on.
        1  world_map_area_id: u32,
        /// The [`AreaTable`] area revealed. Three more follow it, non-zero on
        /// the rows where one texture covers several areas.
        2  area_id_0: u32,
        3  area_id_1: u32,
        4  area_id_2: u32,
        5  area_id_3: u32,
        /// Base name of the patch texture, e.g. `NORTHSHIREVALLEY`.
        8  texture: str,
        9  width: u32,
        10 height: u32,
        /// Pixels from the page's left edge to the patch's left edge.
        11 offset_x: u32,
        /// Pixels from the page's top edge to the patch's top edge.
        12 offset_y: u32,
        /// The clickable box inside the patch, in page pixels.
        ///
        /// **The order named itself, by a margin rather than unanimously.**
        /// Read as top/left/bottom/right, 862 of the 868 rows stating both a
        /// texture rectangle and a box put the box inside the rectangle; read
        /// as left/top/right/bottom, only 123 do, and the very first row's box
        /// starts 77 pixels left of the texture that contains it. The six that
        /// fit neither are named in
        /// `overlay_hit_rects_lie_inside_their_textures`, which counts both
        /// readings over the whole table.
        13 hit_top: u32,
        14 hit_left: u32,
        15 hit_bottom: u32,
        16 hit_right: u32,
    }
}

dbc_table! {
    /// Every kind of liquid an `MH2O` sheet can name: water, ocean, lava and
    /// slime, plus the per-instance variants of each.
    ///
    /// Small enough -- 26 rows in this build -- that the whole table was read
    /// rather than sampled, which is what makes the column identifications
    /// below statements about all of it.
    LiquidType, LiquidTypeRow, path = r"DBFilesClient\LiquidType.dbc", fields = 45, {
        0 id: u32,
        /// Internal name, e.g. `Slow Water`, `Green Lava`, `Naxxramas - Slime`.
        ///
        /// **Descriptive, not authoritative.** See [`LiquidTypeRow::category`]:
        /// one row is called `Orange Slime` and is categorised as water.
        1 name: str,
        2 flags: u32,
        /// What kind of liquid this is -- see [`LiquidCategory`], which is the
        /// column the whole feature turns on.
        3 category: u32,
        /// [`SoundEntries`] row played while in this liquid. 1111-3880 across
        /// the table, all of them real sound ids.
        4 sound_id: u32,
        /// Aura the *server* applies to anyone in this liquid, or 0.
        ///
        /// Named because it explains what is otherwise a puzzling zero: the
        /// ordinary magma rows carry 57634 and the ordinary water rows carry
        /// nothing, so the damage a lava pool does is a spell the server casts
        /// rather than anything a client computes. This client never sends it.
        5 spell_id: u32,
        /// 1 for the water-like materials, 2 for magma and slime, 3 for the
        /// procedural water introduced in Wrath.
        14 material_id: u32,
        /// Path of the surface art, with `%d` standing for the frame number --
        /// `XTextures\river\lake_a.%d.blp`, `XTextures\lava\lava.%d.blp`.
        ///
        /// The independent witness for [`LiquidTypeRow::category`]: every row
        /// the category calls magma reaches a file under `lava` or `LavaGreen`,
        /// and every row it calls slime reaches one under `slime`. Two
        /// different columns agreeing is evidence; a column agreeing with its
        /// own name is not.
        15 texture: str,
    }
}

/// What a [`LiquidType`] row actually *is*, which decides whether standing in
/// it is swimming or dying.
///
/// **Identified by the names its rows carry and confirmed by the art they
/// reach**, which is the same move that named `CreatureSoundData`'s death
/// column. Field 3 runs 0-3 over the whole 26-row table, and the two readings
/// agree completely:
///
/// | value | rows named | textures they reach |
/// |---|---|---|
/// | 0 | `Water`, `Slow Water`, `Fast Water`, `WMO Water` | `XTextures\river\` |
/// | 1 | `Ocean`, `Slow Ocean`, `Fast Ocean`, `WMO Ocean` | `XTextures\ocean\` |
/// | 2 | `Magma`, `Green Lava`, `Chamber Magma` | `XTextures\lava\`, `LavaGreen` |
/// | 3 | `Slime`, `WMO Slime`, `Naxxramas - Slime` | `XTextures\slime\` |
///
/// **And the one row where the two disagree is the reason this is a column
/// rather than a name match.** Row 181 is called `Orange Slime`, draws
/// `LavaOrange`, and its category is **0 -- water**. A client that decided
/// what burns you by looking for "slime" or "lava" in a name would set a
/// player on fire in a pond the server considers harmless, and would do it
/// silently, because nothing about the resulting damage disagrees with
/// anything the client can see. The server reads this column
/// (`LiquidTypeEntry::Type`), so this column is what the client reads too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidCategory {
    /// Rivers, lakes and pools. Swimmable, harmless.
    Water,
    /// Sea water. Swimmable, harmless, and darker.
    Ocean,
    /// Lava. Swimmable in the sense that a character floats in it, and the
    /// server burns them for it.
    Magma,
    /// Slime. Same again, with a different school of damage.
    Slime,
    /// A value this build does not use. Drawn as water and never treated as
    /// harmful: inventing damage from an unrecognised number is exactly the
    /// fabrication a raw passthrough exists to refuse.
    Unknown(u32),
}

impl LiquidCategory {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::Water,
            1 => Self::Ocean,
            2 => Self::Magma,
            3 => Self::Slime,
            other => Self::Unknown(other),
        }
    }

    /// Whether the *server* damages a character in contact with this.
    ///
    /// Advisory only, and deliberately so. Nothing in this client subtracts a
    /// hit point: health is a replicated field, the server computes
    /// environmental damage from its own copy of the same terrain, and a
    /// client that also applied it would be inventing a number that nothing
    /// can check. This exists so the interface can *warn* -- tinting the
    /// screen, showing the timer the server starts -- not so it can act.
    pub fn is_harmful(&self) -> bool {
        matches!(self, Self::Magma | Self::Slime)
    }

    /// Whether a character in this floats and swims rather than walking.
    ///
    /// True of lava as well as water, which surprises people: a character
    /// dropped into a lava lake swims in it, and burns while doing so.
    pub fn is_swimmable(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl LiquidTypeRow<'_> {
    /// This row's [`LiquidCategory`].
    pub fn kind(&self) -> LiquidCategory {
        LiquidCategory::from_raw(self.category())
    }

    /// The surface art for one animation frame.
    ///
    /// The stored path carries a literal `%d`; the frames are numbered from 1.
    /// Returns the path unchanged when it holds no placeholder, which is how
    /// the procedural rows store a single reflection map.
    pub fn texture_frame(&self, frame: u32) -> String {
        self.texture().replace("%d", &frame.to_string())
    }
}

/// The [`SoundEntriesRow::sound_type`] values this client acts on.
///
/// **Measured, not remembered.** The column runs 1-53 across 26 distinct
/// values in this build, and naming them from memory is the mistake
/// `describe_cast_failure` exists to refuse: a wrong label for a category does
/// not fail, it quietly misexplains what a sound is for.
///
/// So the question asked was not "which number is music" but "what do the
/// entries carrying each number actually contain" -- `wow-cli sound types`
/// tallies, for every value, which folders its entries' files sit in. Only the
/// values where that answer is overwhelming are named here:
///
/// | value | entries | where their files live |
/// |---|---|---|
/// | 28 | 632 | 629 under `Sound\Music` |
/// | 50 | 273 | 273 under `Sound\Ambience` |
/// | 10 | 6380 | 6153 under `Sound\Creature` |
///
/// Everything else is passed through as a number. Type 1, for instance, is
/// 69% `Sound\Spells` and 10% `Sound\Creature`, which is not clean enough to
/// put a name on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundType {
    /// Zone and city music.
    Music,
    /// Looping zone ambience.
    Ambience,
    /// Creature vocalisations.
    Creature,
    /// Anything this client has not confirmed, as the raw column value.
    Other(u32),
}

impl SoundType {
    pub fn from_raw(value: u32) -> Self {
        match value {
            10 => Self::Creature,
            28 => Self::Music,
            50 => Self::Ambience,
            other => Self::Other(other),
        }
    }
}

impl SoundEntriesRow<'_> {
    /// The first string column holding a filename.
    const FILE_BASE: usize = 3;
    /// The first column holding a weight, one per file.
    const WEIGHT_BASE: usize = 13;
    /// How many of each there are.
    pub const VARIATIONS: usize = 10;

    /// The filenames this sound may play, with the empty slots dropped.
    ///
    /// Bare names -- join with [`SoundEntriesRow::directory`] for something
    /// that resolves. Most sounds have one or two; a few have all ten.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        let row = self.raw();
        (0..Self::VARIATIONS)
            .map(move |i| row.string(Self::FILE_BASE + i))
            .filter(|name| !name.is_empty())
    }

    /// How likely each file is, in the same order as [`SoundEntriesRow::files`].
    ///
    /// **Not normalised, and deliberately not interpreted as a percentage.**
    /// The values run 0-40 with no obvious total, so what they are relative to
    /// has not been established -- a caller wanting one of several files
    /// should weight by these rather than assume they sum to anything.
    pub fn weights(&self) -> impl Iterator<Item = u32> + '_ {
        let row = self.raw();
        (0..Self::VARIATIONS)
            .filter(move |i| !row.string(Self::FILE_BASE + i).is_empty())
            .map(move |i| row.u32(Self::WEIGHT_BASE + i))
    }

    /// Full archive paths for every file this sound may play.
    ///
    /// The join is a backslash because that is what the archive uses; see
    /// `mpq`'s note on path normalisation. Returns owned strings because the
    /// join has to allocate anyway.
    pub fn paths(&self) -> Vec<String> {
        let directory = self.directory().trim_end_matches('\\');
        self.files()
            .map(|name| {
                if directory.is_empty() {
                    name.to_string()
                } else {
                    format!("{directory}\\{name}")
                }
            })
            .collect()
    }
}


impl_table_info!(
    Map,
    AreaTable,
    CreatureDisplayInfo,
    CreatureModelData,
    AnimationData,
    Spell,
    SpellIcon,
    SkillLineAbility,
    WorldSafeLocs,
    SpellVisualKit
);

dbc_table! {
    /// A place a flight master can send you, and where it is in the world.
    ///
    /// 364 rows in this build. The id is what
    /// [`PLAYER_FIELD_KNOWN_TAXI_MASK`](../../world/update/fields) indexes by
    /// bit, and what [`TaxiPath`]'s endpoints name.
    ///
    /// **The position is the load-bearing column here**, and not because
    /// anything is drawn at it. It is the *check*: a path's first waypoint in
    /// [`TaxiPathNode`] must land on the node it departs from and its last on
    /// the node it arrives at, and those two facts come out of different
    /// tables. That is what identifies [`TaxiPath`]'s `from`/`to` columns,
    /// which are otherwise two adjacent small integers that both resolve --
    /// the same trap as `MOMT`'s ground type and the same escape as the
    /// entry-to-display-id pairing that confirmed `SMSG_LOOT_RESPONSE`.
    TaxiNodes, TaxiNodesRow, path = r"DBFilesClient\TaxiNodes.dbc", fields = 24, {
        0 id: u32,
        /// The [`Map`] this node stands on.
        1 map_id: u32,
        2 x: f32,
        3 y: f32,
        4 z: f32,
        /// What the flight master's list calls it, e.g. `Stormwind, Elwynn`.
        ///
        /// A **name**, which makes it the strongest column in the table for
        /// checking any claim about the others -- the rule the M2 event
        /// stride, `GroundEffectTexture`'s terrain column and 4.24's trainer
        /// greeting all rest on.
        5 name: loc,
        /// The **creature template entry** of the beast this node's flights
        /// leave on, for a Horde character.
        ///
        /// **A creature entry, not a `CreatureDisplayInfo` id**, and the
        /// difference is not academic: reading it as a display id resolves
        /// 2,224 to `NightElfFemale.mdx` and 541 to nothing at all. It was
        /// caught only because the resolution was checked by **name** -- a
        /// character model is obvious nonsense for a flying mount, where a
        /// plausible-but-wrong small integer would have gone unnoticed. A
        /// creature entry is server data, so this client cannot resolve it
        /// from any table it has; the mount it actually draws arrives in the
        /// replicated `UNIT_FIELD_MOUNTDISPLAYID` instead.
        ///
        /// **The faction split was measured, and the obvious test was the
        /// wrong one.** Two columns of mount ids look like a faction pair,
        /// and the first check tried was whether the two id sets are
        /// disjoint. They are not -- thirty ids appear in both -- which reads
        /// as a refutation and is not one: the overlap is entirely *neutral*
        /// mounts that both sides ride at a shared hub, `Riding Drake, Red`
        /// nine times in each column. What settled it was resolving the ids
        /// against the server's `creature_template` and reading the **names**:
        /// this column holds `Wind Rider` 75 times and `Riding Bat` 20, and
        /// [`Self::mount_alliance`] holds `Riding Gryphon` 73 times and
        /// `Riding Hippogryph` 25.
        ///
        /// A hand-picked sample nearly wrote the opposite finding into this
        /// file: eight famous cities happened to contain none of the 95 nodes
        /// that fill both columns, so "no node sets both, therefore not a
        /// faction pair" survived until the whole table was counted.
        22 mount_horde: u32,
        /// The same for an Alliance character. See [`Self::mount_horde`] for
        /// how the two were told apart and why it took names to do it.
        23 mount_alliance: u32,
    }
}

dbc_table! {
    /// One flight route: where it starts, where it ends, what it costs.
    ///
    /// 915 rows. A route is *directional* -- a return trip is a separate row
    /// -- so the pair of endpoint columns is not symmetric and getting them
    /// the wrong way round is not cosmetic: it would fly a player from their
    /// destination to where they already are, and every id involved would
    /// still resolve.
    ///
    /// **Which column is `from` is settled geometrically**, by
    /// [`TaxiPathNode`]'s waypoints landing on [`TaxiNodes`]' coordinates in
    /// the right order. Validity cannot separate them; both are node ids.
    TaxiPath, TaxiPathRow, path = r"DBFilesClient\TaxiPath.dbc", fields = 4, {
        0 id: u32,
        /// The [`TaxiNodes`] row this route departs from.
        1 from_node: u32,
        /// The [`TaxiNodes`] row it arrives at.
        2 to_node: u32,
        /// Copper. Zero on 145 rows, which are the free intra-city hops.
        3 cost: u32,
    }
}

dbc_table! {
    /// One waypoint of a flight route, in order.
    ///
    /// 22,586 rows, and the table that actually describes a flight: the
    /// server sends a *path id*, and this is the only thing that says where
    /// the gryphon goes between the two ends.
    ///
    /// The index within a path is its own column rather than implied by row
    /// order -- the same rule as a loot slot and a gossip option index, and
    /// worth honouring here for the ordinary reason that a table is not
    /// guaranteed to be stored sorted.
    TaxiPathNode, TaxiPathNodeRow, path = r"DBFilesClient\TaxiPathNode.dbc", fields = 11, {
        0 id: u32,
        /// The [`TaxiPath`] this waypoint belongs to.
        1 path_id: u32,
        /// Position along that path, from zero.
        2 index: u32,
        /// The [`Map`] this waypoint is on. A path does not change maps
        /// mid-flight in this build, but the column is per-waypoint.
        3 map_id: u32,
        4 x: f32,
        5 y: f32,
        6 z: f32,
        /// Takes only 0, 1 and 2 over all 22,586 rows, and 22,491 of them are
        /// zero. **Deliberately left a number**: three values with no
        /// behaviour observed is not an enum this project may name, the same
        /// refusal `LiquidType`'s categories and the trainer `kind` get.
        7 flags: u32,
        /// Seconds the flight pauses here. Non-zero on 92 rows.
        8 delay: u32,
        9 arrival_event: u32,
        10 departure_event: u32,
    }
}

/// Marker so the unused-import lint does not fire on the re-exports the macro
/// relies on.
const _: () = {
    fn _uses<'a>(_: &Dbc, _: Row<'a>, _: Locale) {}
};
