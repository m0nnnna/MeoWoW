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
