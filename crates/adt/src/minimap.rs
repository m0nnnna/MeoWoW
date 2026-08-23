//! The minimap's art, and the index that finds it.
//!
//! Every tile of terrain has a 256x256 picture of itself baked at build time,
//! and those pictures are **not stored under their own names**. They sit in
//! `Textures\Minimap\` named by the MD5 of their contents, and
//! `Textures\Minimap\md5translate.trs` is the only thing that says which hash
//! belongs to `Azeroth\map32_48.blp`. Without it the art is 18,536 files with
//! nothing to distinguish them.
//!
//! # The file, and what was measured about it
//!
//! It is CRLF text, 19,090 lines, in two kinds:
//!
//! ```text
//! dir: Azeroth
//! Azeroth\map32_48.blp	b53fb722839e0c7a81bae678ea694f5c.blp
//! ```
//!
//! **The `dir:` lines are redundant and this parser ignores them.** That is a
//! measurement rather than a convenience: every one of the 18,644 entries
//! carries its own full directory, and the directory it carries agrees with
//! the `dir:` line above it on **18,644 of 18,644**. A parser that tracks the
//! header as state and one that reads only the entry lines therefore produce
//! the same index, and the one with no state cannot desynchronise on a file
//! whose first line is an entry.
//!
//! **Content hashing means one picture can serve many tiles.** 18,644 entries
//! name only 14,420 distinct files -- the flat black tile under the world's
//! unreachable corners is referenced 1,127 times. So this is a map *into* the
//! art and never out of it: a file name does not identify a tile.
//!
//! **Every reference resolves.** All 14,420 hashes named here exist in the
//! archive chain of a 12340 install, and 4,116 further hash-named files exist
//! that nothing references -- leftovers of earlier patches. An index built by
//! listing the directory instead would be 22% noise with no way to place any
//! of it.
//!
//! # Only some of it is terrain
//!
//! Of 445 directories, **63 hold `map<x>_<y>.blp` tiles** and the rest hold
//! per-WMO art (`WMO\Azeroth\Buildings\Castle\castle01_000_00_00.blp`) for
//! building interiors, which this client does not draw. [`Translate::tile`]
//! asks only the terrain question and the WMO entries are parsed, counted and
//! left alone rather than filtered out at parse time -- a survey that cannot
//! see them cannot report that they are there.
//!
//! # The coordinates are the ADT's own
//!
//! `map<x>_<y>.blp` uses the same pair as [`crate::tile_path`], which is
//! settled by the tile *sets* rather than assumed: for a map, the pairs named
//! here and the tiles its `WDT` says exist are the same set, and a continent's
//! set is not symmetric under exchanging the two numbers. See
//! `wow-cli minimap tiles`.

use std::collections::HashMap;

/// Archive path of the index.
pub const TRANSLATE_PATH: &str = r"textures\Minimap\md5translate.trs";

/// Edge of one minimap tile, in texels. One tile is one ADT, so a texel is
/// [`crate::TILE_SIZE`] / 256 world units -- just over two.
pub const TILE_TEXELS: usize = 256;

/// Texels across one map chunk: sixteen chunks to a tile, 256 texels to a
/// tile. The number matters because a chunk is the finest thing the terrain
/// files can state a fact about, and so is the unit any check of this art
/// against the terrain is scored in.
pub const TEXELS_PER_CHUNK: usize = TILE_TEXELS / crate::CHUNKS_PER_TILE;

/// `md5translate.trs`: logical tile name to the file that holds its picture.
#[derive(Default, Clone)]
pub struct Translate {
    /// Lowercased logical path (`azeroth\map32_48.blp`) to the hash-named file
    /// (`b53f...blp`, no directory). Lowercased because the file's own casing
    /// is inconsistent -- `Zul'gurub` and `ZulAman` sit beside `Azjol_LowerCity`
    /// -- and a caller naming a map from `Map.dbc` has no reason to match it.
    entries: HashMap<String, String>,
}

impl Translate {
    /// Reads the index. Never fails: an unreadable or truncated file yields
    /// the entries that did parse, because a missing minimap tile draws as a
    /// hole and a missing *index* would otherwise take the whole feature down.
    pub fn parse(text: &str) -> Self {
        let mut entries = HashMap::new();
        for line in text.lines() {
            // `dir:` headers restate what every entry already carries -- see
            // the module docs for the count that establishes it.
            let Some((logical, file)) = line.split_once('\t') else {
                continue;
            };
            let (logical, file) = (logical.trim(), file.trim());
            if logical.is_empty() || file.is_empty() {
                continue;
            }
            entries.insert(logical.to_ascii_lowercase(), file.to_string());
        }
        Self { entries }
    }

    /// How many tiles the index names, terrain and WMO alike.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The picture for one terrain tile, as a full archive path.
    ///
    /// `map` is a directory name as `Map.dbc` states it -- `Azeroth`,
    /// `Kalimdor` -- and `x`/`y` are the pair [`crate::tile_path`] uses.
    pub fn tile_path(&self, map: &str, x: usize, y: usize) -> Option<String> {
        let key = format!(r"{}\map{x}_{y}.blp", map.to_ascii_lowercase());
        self.entries.get(&key).map(|file| art_path(file))
    }

    pub fn wmo_tiles(&self, root: &str, group: usize) -> Vec<(usize, usize, String)> {
        let root = root.replace('/', "\\").to_ascii_lowercase();
        let Some(root) = root.strip_prefix(r"world\wmo\") else {
            return Vec::new();
        };
        let Some((directory, file)) = root.rsplit_once('\\') else {
            return Vec::new();
        };
        let Some(stem) = file.strip_suffix(".wmo") else {
            return Vec::new();
        };
        let prefix = format!(r"wmo\{directory}\{stem}_{group:03}_");
        let mut out = self
            .entries
            .iter()
            .filter_map(|(key, file)| {
                let suffix = key.strip_prefix(&prefix)?.strip_suffix(".blp")?;
                let (x, y) = suffix.split_once('_')?;
                Some((x.parse().ok()?, y.parse().ok()?, art_path(file)))
            })
            .collect::<Vec<_>>();
        out.sort_unstable_by_key(|(x, y, _)| (*x, *y));
        out
    }

    /// Every terrain tile this index names for one map, as `(x, y)`.
    ///
    /// Sorted, so a comparison against [`crate::Wdt::tiles`] is a comparison
    /// of two sequences rather than of two orders.
    pub fn tiles(&self, map: &str) -> Vec<(usize, usize)> {
        let prefix = format!(r"{}\map", map.to_ascii_lowercase());
        let mut out: Vec<(usize, usize)> = self
            .entries
            .keys()
            .filter_map(|key| key.strip_prefix(&prefix))
            .filter_map(parse_tile_suffix)
            .collect();
        out.sort_unstable();
        out
    }

    /// Every entry, as `(logical name, file)`. For surveys: the file names
    /// repeat and the logical names do not, which is the property that makes
    /// this index one-way and is only visible from outside if both halves are
    /// reachable.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(logical, file)| (logical.as_str(), file.as_str()))
    }

    /// Every directory that holds terrain tiles, lowercased and sorted.
    ///
    /// The WMO directories are excluded here and only here: they are real
    /// entries and [`Self::len`] counts them, but they are not maps and
    /// nothing that walks maps should have to know their shape.
    pub fn maps(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .entries
            .keys()
            .filter_map(|key| {
                let (dir, file) = key.rsplit_once('\\')?;
                parse_tile_suffix(file.strip_prefix("map")?)?;
                // A terrain directory is one level deep; `wmo\azeroth\...`
                // never is, and a nested directory that happened to hold a
                // `map1_2.blp` would not be a map either.
                (!dir.contains('\\')).then(|| dir.to_string())
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Archive path of one hash-named picture.
pub fn art_path(file: &str) -> String {
    format!(r"Textures\Minimap\{file}")
}

/// `32_48.blp` to `(32, 48)`, and anything else to nothing.
fn parse_tile_suffix(suffix: &str) -> Option<(usize, usize)> {
    let stem = suffix.strip_suffix(".blp")?;
    let (x, y) = stem.split_once('_')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

/// A square of world centred on a point, and where things in it fall.
///
/// This is the minimap's projection, and it lives beside the tile index for
/// the same reason the world map's lives beside `WorldMapArea.dbc`: **the
/// tool that checks a projection must not compute it a second way.** `wow-cli minimap stitch` draws a
/// picture from this and the viewer draws the frame from it, so the picture
/// is evidence about the frame rather than about a second implementation that
/// happens to agree today.
///
/// The one difference from a map page is what is nailed down. A page is fixed
/// to the ground and the player moves across it; this is fixed to the player
/// and the ground slides under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// World position at the middle.
    pub x: f32,
    pub y: f32,
    /// World units across the square.
    pub range: f32,
}

impl Viewport {
    pub fn new(x: f32, y: f32, range: f32) -> Self {
        Self {
            x,
            y,
            range: range.max(1.0),
        }
    }

    /// Where a world position falls, as fractions with `(0.5, 0.5)` at the
    /// middle and `(0, 0)` at the top left.
    ///
    /// **`+x` is up the screen and `+y` is to the left**, the same convention
    /// [`dbc::worldmap::Page::project`] uses -- deliberately, so the two
    /// frames' arrows cannot end up pointing different ways.
    ///
    /// Runs past `0..1` outside the square rather than clamping: a caller
    /// drawing a blip needs to know it is off the edge, which is what decides
    /// whether to draw it at all.
    pub fn project(&self, x: f32, y: f32) -> (f32, f32) {
        (
            0.5 + (self.y - y) / self.range,
            0.5 + (self.x - x) / self.range,
        )
    }

    /// Every tile the square touches, as `(x, y)` in the pair
    /// [`crate::tile_path`] and [`Translate::tile_path`] both
    /// use. Tiles outside the 64x64 grid are dropped.
    pub fn tiles_touching(&self) -> Vec<(usize, usize)> {
        let half = self.range / 2.0;
        let low = crate::tile_at(self.x - half, self.y - half);
        let high = crate::tile_at(self.x + half, self.y + half);
        let mut out = Vec::new();
        for tx in low.0.min(high.0)..=low.0.max(high.0) {
            for ty in low.1.min(high.1)..=low.1.max(high.1) {
                if (0..crate::TILES_PER_MAP as i32).contains(&tx) && (0..crate::TILES_PER_MAP as i32).contains(&ty)
                {
                    out.push((tx as usize, ty as usize));
                }
            }
        }
        out
    }

    /// Where one tile's picture goes, as `[u0, v0, u1, v1]`.
    ///
    /// **The tile's own pixel layout is the measured one**: `(0, 0)` of the
    /// art is the corner with the largest world `x` and `y`, across runs with
    /// falling `y` and down with falling `x`. That reading was picked out of
    /// eight by `wow-cli minimap orient`, which scores the art against the
    /// water `MH2O` says is there, and confirmed by `wow-cli minimap seams`,
    /// which reads no terrain at all and asks only whether neighbouring tiles
    /// join up. See [the module docs](self).
    pub fn tile_rect(&self, x: usize, y: usize) -> [f32; 4] {
        let origin_x = (32.0 - y as f32) * crate::TILE_SIZE;
        let origin_y = (32.0 - x as f32) * crate::TILE_SIZE;
        let (u0, v0) = self.project(origin_x, origin_y);
        let (u1, v1) = self.project(origin_x - crate::TILE_SIZE, origin_y - crate::TILE_SIZE);
        [u0, v0, u1, v1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five real lines, CRLF, with the `dir:` header the parser is documented
    /// to ignore -- including one entry whose two references share a file,
    /// which is the property that makes this index one-way.
    const SAMPLE: &str = concat!(
        "dir: Azeroth\r\n",
        "Azeroth\\map32_48.blp\tb53fb722839e0c7a81bae678ea694f5c.blp\r\n",
        "Azeroth\\map32_49.blp\te7f0dea73ee6baca78231aaf4b7e772a.blp\r\n",
        "dir: AhnQiraj\r\n",
        "AhnQiraj\\map27_52.blp\ta96bb9ceebc36ae410930d272792957a.blp\r\n",
        "AhnQiraj\\map27_53.blp\ta96bb9ceebc36ae410930d272792957a.blp\r\n",
        "dir: WMO\\Azeroth\\Buildings\\Castle\r\n",
        "WMO\\Azeroth\\Buildings\\Castle\\castle01_000_00_00.blp\t\
         0000000000000000000000000000dead.blp\r\n",
    );

    #[test]
    fn resolves_a_tile_to_its_hashed_art() {
        let index = Translate::parse(SAMPLE);
        assert_eq!(index.len(), 5);
        assert_eq!(
            index.tile_path("Azeroth", 32, 48).as_deref(),
            Some(r"Textures\Minimap\b53fb722839e0c7a81bae678ea694f5c.blp")
        );
        // The caller names the map however `Map.dbc` does; the file's own
        // casing is not something a caller should have to reproduce.
        assert_eq!(
            index.tile_path("AZEROTH", 32, 48),
            index.tile_path("azeroth", 32, 48)
        );
        assert_eq!(index.tile_path("Azeroth", 32, 50), None);
    }

    /// One picture serving two tiles is the reason this index is not
    /// invertible, and the reason a directory listing cannot replace it.
    #[test]
    fn one_picture_can_serve_two_tiles() {
        let index = Translate::parse(SAMPLE);
        assert_eq!(
            index.tile_path("AhnQiraj", 27, 52),
            index.tile_path("AhnQiraj", 27, 53)
        );
        assert!(index.tile_path("AhnQiraj", 27, 52).is_some());
    }

    /// WMO art is counted but is not a map, and a tile is not found by a
    /// prefix that happens to match part of a longer path.
    #[test]
    fn wmo_directories_are_not_maps() {
        let index = Translate::parse(SAMPLE);
        assert_eq!(index.maps(), vec!["ahnqiraj".to_string(), "azeroth".into()]);
        assert_eq!(index.tiles("Azeroth"), vec![(32, 48), (32, 49)]);
        assert!(index.tiles("WMO").is_empty());
    }

    #[test]
    fn resolves_wmo_group_art_by_root_and_tile() {
        let index = Translate::parse(
            "WMO\\KhazModan\\Cities\\Ironforge\\ironforge_001_00_00.blp\tfirst.blp\n\
             WMO\\KhazModan\\Cities\\Ironforge\\ironforge_001_01_00.blp\tsecond.blp\n\
             WMO\\KhazModan\\Cities\\Ironforge\\ironforge_002_00_00.blp\tother.blp",
        );
        assert_eq!(
            index.wmo_tiles(
                r"World\wmo\KhazModan\Cities\Ironforge\ironforge.wmo",
                1
            ),
            vec![
                (0, 0, r"Textures\Minimap\first.blp".to_string()),
                (1, 0, r"Textures\Minimap\second.blp".to_string()),
            ]
        );
    }

    /// The header lines carry no information this parser needs, so a file
    /// that starts mid-directory reads identically.
    #[test]
    fn dir_headers_are_ignorable() {
        let with = Translate::parse(SAMPLE);
        let without: String = SAMPLE
            .lines()
            .filter(|line| !line.starts_with("dir:"))
            .map(|line| format!("{line}\r\n"))
            .collect();
        let without = Translate::parse(&without);
        assert_eq!(with.len(), without.len());
        assert_eq!(
            with.tile_path("Azeroth", 32, 48),
            without.tile_path("Azeroth", 32, 48)
        );
    }
}
