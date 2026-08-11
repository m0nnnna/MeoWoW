//! Inspection tool for a WoW 3.3.5a installation's data files.
//!
//! Points at a `Data` directory and reads through the patch chain exactly as
//! the client would, so what it prints is what the engine would see.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mpq::{Archive, Chain};

#[derive(Parser)]
#[command(name = "wow-cli", about = "Inspect WoW 3.3.5a client data")]
struct Cli {
    /// Path to the installation's `Data` directory.
    #[arg(long, short, global = true, env = "WOW_DATA")]
    data: Option<PathBuf>,

    /// Locale subdirectory holding the localized archives.
    #[arg(long, global = true, default_value = "enUS")]
    locale: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarize each archive in the load order.
    Info,
    /// List files, optionally filtered by a substring.
    Ls {
        /// Case-insensitive substring to match.
        filter: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Extract one file to disk.
    Extract {
        /// Archive path, e.g. `World\Maps\Azeroth\Azeroth.wdt`.
        name: String,
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Report which archive in the chain wins for a path.
    Which { name: String },
    /// Read every listed file and report failures. Slow but thorough.
    Verify {
        /// Stop after this many files.
        #[arg(long)]
        limit: Option<usize>,
        /// Only check files matching this substring.
        filter: Option<String>,
    },
    /// Inspect client database tables.
    #[command(subcommand)]
    Dbc(DbcCommand),
    /// Inspect and export textures.
    #[command(subcommand)]
    Blp(BlpCommand),
    /// Inspect models.
    #[command(subcommand)]
    M2(M2Command),
    /// Inspect world objects: buildings, dungeons, bridges.
    #[command(subcommand)]
    Wmo(WmoCommand),
    /// Inspect terrain.
    #[command(subcommand)]
    Adt(AdtCommand),
}

#[derive(Subcommand)]
enum AdtCommand {
    /// Summarize a map: which tiles exist and how alpha is stored.
    Map { map: String },
    /// Show one terrain tile.
    Tile {
        map: String,
        x: usize,
        y: usize,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Parse every tile of a map, checking that chunks meet at their edges.
    Survey {
        /// Map directory name, e.g. `Azeroth`. Omit to sweep every map.
        map: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum WmoCommand {
    /// Show a root file and its groups.
    Info {
        /// Archive path of the root `.wmo`, not a `_000` group file.
        path: String,
        #[arg(long, default_value_t = 12)]
        limit: usize,
    },
    /// Parse every root and group in the archives, validating the arrays.
    Survey {
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum M2Command {
    /// Show a model's header, textures, materials, and skin geometry.
    Info {
        /// Archive path; `.mdx` is rewritten to `.m2` automatically.
        path: String,
        /// Level of detail to describe.
        #[arg(long, default_value_t = 0)]
        lod: u32,
    },
    /// Parse every model and its skins, validating the index tables.
    Survey {
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Resolve a creature display id to its model, the way the renderer will.
    Creature { display_id: u32 },
    /// List a model's animations and how much of the skeleton each moves.
    Anims {
        path: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum BlpCommand {
    /// Show a texture's encoding and mip chain.
    Info { path: String },
    /// Export a mip level to PNG.
    Export {
        path: String,
        #[arg(long, default_value_t = 0)]
        level: usize,
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Parse every texture in the archives and tally what the format space
    /// actually looks like.
    Survey {
        /// Only survey paths matching this substring.
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum DbcCommand {
    /// List every DBC in the archives with its shape.
    List {
        /// Case-insensitive substring to match.
        filter: Option<String>,
    },
    /// Show a table's header and inferred column types.
    ///
    /// Column types are not stored in the file, so this guesses them from the
    /// data. Use it to transcribe a table that has no schema yet.
    Info { table: String },
    /// Dump rows using inferred column types.
    Dump {
        table: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Dump rows through a transcribed schema.
    Rows {
        /// One of: Map, AreaTable, CreatureDisplayInfo, CreatureModelData,
        /// AnimationData, Spell.
        table: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Check every transcribed schema against the files in this install.
    Check,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let data = cli
        .data
        .context("no data directory: pass --data or set WOW_DATA")?;

    let mut chain = Chain::open_wow_data(&data, &cli.locale)
        .with_context(|| format!("opening archives under {}", data.display()))?;

    match cli.command {
        Command::Info => info(&mut chain),
        Command::Ls { filter, limit } => ls(&mut chain, filter.as_deref(), limit),
        Command::Extract { name, out } => extract(&mut chain, &name, out),
        Command::Which { name } => which(&chain, &name),
        Command::Verify { limit, filter } => verify(&mut chain, limit, filter.as_deref()),
        Command::Dbc(cmd) => dbc_cmd(&mut chain, cmd),
        Command::Blp(cmd) => blp_cmd(&mut chain, cmd),
        Command::M2(cmd) => m2_cmd(&mut chain, cmd),
        Command::Wmo(cmd) => wmo_cmd(&mut chain, cmd),
        Command::Adt(cmd) => adt_cmd(&mut chain, cmd),
    }
}

fn adt_cmd(chain: &mut Chain, cmd: AdtCommand) -> Result<()> {
    match cmd {
        AdtCommand::Map { map } => adt_map(chain, &map),
        AdtCommand::Tile { map, x, y, limit } => adt_tile(chain, &map, x, y, limit),
        AdtCommand::Survey { map, limit } => adt_survey(chain, map.as_deref(), limit),
    }
}

/// Loads a map's WDT, which is where the alpha-map storage flag lives.
fn load_wdt(chain: &mut Chain, map: &str) -> Result<adt::Wdt> {
    let path = adt::wdt_path(map);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    adt::Wdt::parse(&bytes).with_context(|| format!("parsing {path}"))
}

fn adt_map(chain: &mut Chain, map: &str) -> Result<()> {
    let wdt = load_wdt(chain, map)?;
    println!("{}", adt::wdt_path(map));
    println!(
        "  flags {:#x}, {} of {} tiles present, alpha maps are {}-bit",
        wdt.flags,
        wdt.tile_count(),
        adt::TILES_PER_MAP * adt::TILES_PER_MAP,
        if wdt.big_alpha() { 8 } else { 4 }
    );

    // A coarse picture of which part of the grid the map occupies.
    let tiles = wdt.tiles();
    if let (Some(min_x), Some(max_x)) = (
        tiles.iter().map(|t| t.0).min(),
        tiles.iter().map(|t| t.0).max(),
    ) {
        let min_y = tiles.iter().map(|t| t.1).min().unwrap_or(0);
        let max_y = tiles.iter().map(|t| t.1).max().unwrap_or(0);
        println!("  occupied region: x {min_x}..={max_x}, y {min_y}..={max_y}");
        println!("  first tiles: {:?}", &tiles[..tiles.len().min(6)]);
    }
    Ok(())
}

fn adt_tile(chain: &mut Chain, map: &str, x: usize, y: usize, limit: usize) -> Result<()> {
    let wdt = load_wdt(chain, map)?;
    let path = adt::tile_path(map, x, y);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    let tile = adt::Adt::parse(&bytes, wdt.big_alpha())?;

    println!("{path}");
    println!(
        "  {} textures, {} doodad models, {} object models",
        tile.textures.len(),
        tile.doodad_models.len(),
        tile.object_models.len()
    );
    println!(
        "  {} doodad placements, {} world object placements",
        tile.doodads.len(),
        tile.objects.len()
    );

    let heights: Vec<f32> = tile
        .chunks
        .iter()
        .flat_map(|c| c.heights.iter().map(move |h| h + c.position[2]))
        .collect();
    let low = heights.iter().copied().fold(f32::MAX, f32::min);
    let high = heights.iter().copied().fold(f32::MIN, f32::max);
    println!("  elevation {low:.1} to {high:.1}");
    match tile.validate() {
        Ok(()) => println!("  chunk edges meet"),
        Err(e) => println!("  SEAM: {e}"),
    }

    println!("\n  textures:");
    for texture in tile.textures.iter().take(limit) {
        println!("    {texture}");
    }

    println!("\n  chunks (first {limit}):");
    for c in tile.chunks.iter().take(limit) {
        println!(
            "    {:>2},{:<2} area {:>5} {} layers, {} alpha maps, {} doodads, {} objects{}",
            c.index.0,
            c.index.1,
            c.area_id,
            c.layers.len(),
            c.alpha_maps.len(),
            c.doodad_refs.len(),
            c.object_refs.len(),
            if c.holes != 0 { format!(" holes {:#06x}", c.holes) } else { String::new() },
        );
    }

    if !tile.objects.is_empty() {
        println!("\n  world objects placed here:");
        for o in tile.objects.iter().take(limit) {
            println!(
                "    {} at [{:.0} {:.0} {:.0}] set {}",
                o.path, o.position[0], o.position[1], o.position[2], o.doodad_set
            );
        }
    }
    Ok(())
}

fn adt_survey(chain: &mut Chain, map: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    // Maps are named by their directory, which is what Map.dbc records.
    let maps: Vec<String> = match map {
        Some(m) => vec![m.to_string()],
        None => {
            let table = dbc::schema::Map::parse(&chain.read(dbc::schema::Map::PATH)?)?;
            let mut names: Vec<String> = table
                .iter()
                .map(|m| m.directory().to_string())
                .filter(|d| !d.is_empty())
                .collect();
            names.sort_unstable();
            names.dedup();
            names
        }
    };

    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut tiles_ok, mut tiles_missing, mut maps_ok) = (0usize, 0usize, 0usize);
    let (mut doodads, mut objects, mut budget) = (0u64, 0u64, limit.unwrap_or(usize::MAX));

    for name in &maps {
        let wdt = match load_wdt(chain, name) {
            Ok(w) => w,
            Err(_) => continue,
        };
        maps_ok += 1;
        for (x, y) in wdt.tiles() {
            if budget == 0 {
                break;
            }
            let path = adt::tile_path(name, x, y);
            let Ok(bytes) = chain.read(&path) else {
                tiles_missing += 1;
                continue;
            };
            budget -= 1;
            match adt::Adt::parse(&bytes, wdt.big_alpha()) {
                Ok(tile) => {
                    tiles_ok += 1;
                    doodads += tile.doodads.len() as u64;
                    objects += tile.objects.len() as u64;
                    if let Err(e) = tile.validate() {
                        let key = format!("edges: {}", first_clause(&e));
                        failures.entry(key).or_insert((0, path.clone())).0 += 1;
                    }
                }
                Err(e) => {
                    let key = e.to_string();
                    let key = key.split(" (").next().unwrap_or(&key).to_string();
                    failures.entry(key).or_insert((0, path.clone())).0 += 1;
                }
            }
        }
        if budget == 0 {
            break;
        }
        tracing::info!("{name}: done");
    }

    println!("\n{maps_ok} maps, {tiles_ok} tiles parsed, {tiles_missing} declared but absent");
    println!("  {doodads} doodad placements, {objects} world object placements");
    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

fn wmo_cmd(chain: &mut Chain, cmd: WmoCommand) -> Result<()> {
    match cmd {
        WmoCommand::Info { path, limit } => wmo_info(chain, &path, limit),
        WmoCommand::Survey { filter, limit } => wmo_survey(chain, filter.as_deref(), limit),
    }
}

fn wmo_info(chain: &mut Chain, path: &str, limit: usize) -> Result<()> {
    if wmo::is_group_path(path) {
        anyhow::bail!("{path} is a group file; pass the root .wmo instead");
    }
    let bytes = chain.read(path)?;
    let root = wmo::Root::parse(&bytes)?;
    // Group names live in the root, so groups need it passed in.
    let names = wmo::Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();
    let h = root.header;

    println!("{path}");
    println!(
        "  {} groups, {} materials, {} textures, {} portals, {} lights",
        h.group_count,
        root.materials.len(),
        root.textures().len(),
        h.portal_count,
        h.light_count
    );
    println!(
        "  bounds [{:.1} {:.1} {:.1}] .. [{:.1} {:.1} {:.1}]",
        h.bounding_box.0[0],
        h.bounding_box.0[1],
        h.bounding_box.0[2],
        h.bounding_box.1[0],
        h.bounding_box.1[1],
        h.bounding_box.1[2]
    );
    println!(
        "  ambient {:?}, flags {:#x}, wmo id {}",
        h.ambient_color, h.flags, h.wmo_id
    );

    if !root.doodad_sets.is_empty() {
        println!("\n  doodad sets:");
        for set in &root.doodad_sets {
            let name = if set.name.is_empty() { "<unnamed>" } else { &set.name };
            println!("    {name:<24} {} doodads", set.count);
        }
    }

    println!("\n  textures:");
    for texture in root.textures().iter().take(limit) {
        println!("    {texture}");
    }

    println!("\n  groups:");
    let (mut verts, mut tris, mut collision, mut failures) = (0usize, 0usize, 0usize, 0usize);
    for i in 0..h.group_count as usize {
        let gpath = wmo::group_path(path, i);
        let Ok(gbytes) = chain.read(&gpath) else {
            println!("    {i:>3}: {gpath} MISSING");
            failures += 1;
            continue;
        };
        let group = match wmo::Group::parse(&gbytes, &names) {
            Ok(g) => g,
            Err(e) => {
                println!("    {i:>3}: {e}");
                failures += 1;
                continue;
            }
        };
        verts += group.vertices.len();
        tris += group.triangle_count();
        let hidden = group
            .triangle_materials
            .iter()
            .filter(|t| t.is_collision_only())
            .count();
        collision += hidden;

        if i < limit {
            let name = if group.name.is_empty() { "<unnamed>" } else { &group.name };
            let hidden_note = if hidden > 0 {
                format!("{hidden} collision-only")
            } else {
                String::new()
            };
            println!(
                "    {i:>3}: {name:<26} {:>6} verts {:>6} tris {:>3} batches  {}{}{hidden_note}",
                group.vertices.len(),
                group.triangle_count(),
                group.batches.len(),
                if group.is_interior() { "interior " } else { "exterior " },
                if group.has_vertex_colors() { "vcolors " } else { "" },
            );
        }
        if let Err(e) = group.validate() {
            println!("         INVALID: {e}");
            failures += 1;
        }
    }
    if h.group_count as usize > limit {
        println!("    ... {} more", h.group_count as usize - limit);
    }
    println!(
        "\n  total: {verts} vertices, {tris} triangles, {collision} collision-only, \
         {failures} problems"
    );
    Ok(())
}

fn wmo_survey(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.map(str::to_lowercase);
    // Group files sit beside their roots and parse as WMOs; treating each as a
    // building would count every wall as its own object.
    let roots: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".wmo")
                && !wmo::is_group_path(&l)
                && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut ok, mut groups, mut verts, mut tris) = (0usize, 0usize, 0u64, 0u64);
    let (mut doodads, mut unresolved, mut biggest) = (0u64, 0usize, 0usize);

    for (i, name) in roots.iter().enumerate() {
        let Ok(bytes) = chain.read(name) else {
            unresolved += 1;
            continue;
        };
        let root = match wmo::Root::parse(&bytes) {
            Ok(r) => r,
            Err(e) => {
                let key = e.to_string();
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                failures.entry(key).or_insert((0, name.clone())).0 += 1;
                continue;
            }
        };
        ok += 1;
        doodads += root.doodads.len() as u64;
        let names = wmo::Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();

        for gi in 0..root.header.group_count as usize {
            let gpath = wmo::group_path(name, gi);
            let Ok(gbytes) = chain.read(&gpath) else {
                failures
                    .entry("group file missing".into())
                    .or_insert((0, gpath.clone()))
                    .0 += 1;
                continue;
            };
            match wmo::Group::parse(&gbytes, &names) {
                Ok(group) => {
                    groups += 1;
                    verts += group.vertices.len() as u64;
                    tris += group.triangle_count() as u64;
                    biggest = biggest.max(group.vertices.len());
                    if let Err(e) = group.validate() {
                        let key = format!("group invalid: {}", first_clause(&e));
                        failures.entry(key).or_insert((0, gpath.clone())).0 += 1;
                    }
                }
                Err(e) => {
                    let key = format!("group parse: {}", first_clause(&e.to_string()));
                    failures.entry(key).or_insert((0, gpath.clone())).0 += 1;
                }
            }
        }
        if i % 500 == 499 {
            tracing::info!("{}/{} objects", i + 1, roots.len());
        }
    }

    println!("\n{ok}/{} root objects parsed, {groups} groups", roots.len());
    println!("  {verts} vertices, {tris} triangles, {doodads} doodad placements");
    println!("  largest group: {biggest} vertices");
    println!("  {unresolved} listed roots did not resolve (tombstoned or stale)");
    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

fn m2_cmd(chain: &mut Chain, cmd: M2Command) -> Result<()> {
    match cmd {
        M2Command::Info { path, lod } => m2_info(chain, &path, lod),
        M2Command::Survey { filter, limit } => m2_survey(chain, filter.as_deref(), limit),
        M2Command::Creature { display_id } => m2_creature(chain, display_id),
        M2Command::Anims { path, limit } => m2_anims(chain, &path, limit),
    }
}

fn m2_anims(chain: &mut Chain, path: &str, limit: usize) -> Result<()> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)?;
    let sequences = model.sequences();

    // Most sequences keep their keyframes in sibling .anim files.
    let mut external = std::collections::BTreeMap::new();
    let mut missing = 0usize;
    for (i, seq) in sequences.iter().enumerate() {
        if seq.is_inline() {
            continue;
        }
        match chain.read(&m2::anim::external_anim_path(&path, seq)) {
            Ok(bytes) => {
                external.insert(i, bytes);
            }
            Err(_) => missing += 1,
        }
    }
    let bones = model.animated_bones_with(&external);

    // Animation ids are numbers in the model; the names live in a DBC.
    let names = dbc::schema::AnimationData::parse(
        &chain.read(dbc::schema::AnimationData::PATH)?,
    )
    .ok();
    let name_of = |id: u16| -> String {
        names
            .as_ref()
            .and_then(|t| t.iter().find(|r| r.id() == id as u32))
            .map(|r| r.name().to_string())
            .unwrap_or_else(|| format!("#{id}"))
    };

    let animated = bones.iter().filter(|b| b.is_animated()).count();
    println!("{path}");
    println!(
        "  {} bones ({animated} animated), {} sequences",
        bones.len(),
        sequences.len()
    );
    println!(
        "  {} external .anim files loaded, {missing} without one (aliases or absent)",
        external.len()
    );

    println!(
        "\n  {:>3} {:<22} {:>8} {:>5} {:>8} {:>7}  flags",
        "idx", "name", "duration", "var", "keyed", "speed"
    );
    for (i, seq) in sequences.iter().enumerate().take(limit) {
        // How many bones actually have keys for this sequence, which is what
        // separates a real animation from an alias or an empty slot.
        let keyed = bones
            .iter()
            .filter(|b| {
                b.rotation.sample(i, 0).is_some()
                    || b.translation.sample(i, 0).is_some()
                    || b.scale.sample(i, 0).is_some()
            })
            .count();
        let mut flags = Vec::new();
        if seq.is_inline() {
            flags.push("inline");
        } else {
            flags.push("external");
        }
        if seq.is_alias() {
            flags.push("alias");
        }
        println!(
            "  {i:>3} {:<22} {:>7}ms {:>5} {:>8} {:>7.2}  {}",
            name_of(seq.id),
            seq.duration_ms,
            seq.variation,
            keyed,
            seq.move_speed,
            flags.join(" ")
        );
    }
    if sequences.len() > limit {
        println!("  ... {} more", sequences.len() - limit);
    }

    // Bone indices in a vertex must address the model's bone list directly; if
    // they were submesh-relative, this maximum would be far below the count.
    let max_bone = model
        .vertices()
        .iter()
        .flat_map(|v| {
            v.bone_indices
                .iter()
                .zip(v.bone_weights)
                .filter(|(_, w)| *w > 0)
                .map(|(&i, _)| i as usize)
        })
        .max()
        .unwrap_or(0);
    println!(
        "\n  highest vertex bone index: {max_bone} of {} bones",
        bones.len()
    );
    Ok(())
}

fn m2_info(chain: &mut Chain, path: &str, lod: u32) -> Result<()> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)?;

    let (min, max) = model.bounding_box();
    println!("{path}");
    println!("  internal name: {:?}", model.name());
    println!(
        "  version {}, flags {:#x}, {} skin profile(s)",
        model.version(),
        model.global_flags(),
        model.skin_count()
    );
    println!(
        "  {} vertices, {} bones, {} textures, {} materials, {} sequences",
        model.vertex_count(),
        model.bones().len(),
        model.textures().len(),
        model.materials().len(),
        model.sequence_count()
    );
    println!(
        "  bounds [{:.2} {:.2} {:.2}] .. [{:.2} {:.2} {:.2}], radius {:.2}",
        min[0],
        min[1],
        min[2],
        max[0],
        max[1],
        max[2],
        model.bounding_sphere_radius()
    );

    println!("\n  textures:");
    for (i, t) in model.textures().iter().enumerate() {
        let what = if t.is_hardcoded() {
            t.filename.clone()
        } else {
            format!("<supplied at runtime, type {}>", t.kind)
        };
        println!("    {i:>2}: flags {:#06x}  {what}", t.flags);
    }

    println!("\n  materials:");
    for (i, m) in model.materials().iter().enumerate() {
        let mut notes = Vec::new();
        if m.unlit() {
            notes.push("unlit");
        }
        if m.two_sided() {
            notes.push("two-sided");
        }
        if m.depth_write_disabled() {
            notes.push("no depth write");
        }
        println!(
            "    {i:>2}: blend {}, flags {:#06x} {}",
            m.blend,
            m.flags,
            notes.join(" ")
        );
    }

    let roots = model.bones().iter().filter(|b| b.parent < 0).count();
    println!("\n  skeleton: {} bones, {roots} root(s)", model.bones().len());

    let skin_path = m2::skin_path(&path, lod);
    match chain.read(&skin_path) {
        Ok(bytes) => {
            let skin = m2::Skin::parse(&bytes)?;
            println!("\n  {skin_path}");
            println!(
                "    {} local vertices, {} indices ({} triangles), {} submeshes, {} batches",
                skin.vertex_map().len(),
                skin.triangles().len(),
                skin.triangles().len() / 3,
                skin.submeshes().len(),
                skin.batches().len()
            );
            match skin.validate(model.vertex_count()) {
                Ok(()) => println!("    index tables valid"),
                Err(e) => println!("    INVALID: {e}"),
            }

            let combos = model.texture_combos();
            let textures = model.textures();
            println!("\n    batches:");
            for (i, b) in skin.batches().iter().enumerate().take(24) {
                let sub = skin.submeshes().get(b.submesh_index as usize);
                // A batch names its texture indirectly, through the combo
                // table; this is the lookup the renderer performs per draw.
                let tex = combos
                    .get(b.texture_combo_index as usize)
                    .and_then(|&t| textures.get(t as usize))
                    .map(|t| {
                        if t.is_hardcoded() {
                            t.filename.clone()
                        } else {
                            format!("<runtime type {}>", t.kind)
                        }
                    })
                    .unwrap_or_else(|| "<none>".into());
                println!(
                    "      {i:>2}: submesh {:>3} (id {:>5}, {:>5} tris)  material {:>2}  {tex}",
                    b.submesh_index,
                    sub.map_or(0, |s| s.id),
                    sub.map_or(0, |s| s.triangle_count()),
                    b.material_index,
                );
            }
            if skin.batches().len() > 24 {
                println!("      ... {} more", skin.batches().len() - 24);
            }
        }
        Err(e) => println!("\n  {skin_path}: {e}"),
    }
    Ok(())
}

fn m2_survey(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.map(str::to_lowercase);
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".m2") && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut models, mut skins, mut verts, mut tris) = (0usize, 0usize, 0u64, 0u64);
    let (mut no_skin, mut max_verts, mut max_bones) = (0usize, 0usize, 0usize);
    let mut unresolved = 0usize;

    for (i, name) in names.iter().enumerate() {
        // Listed but absent: tombstoned by a patch, or a stale listfile entry.
        let Ok(bytes) = chain.read(name) else {
            unresolved += 1;
            continue;
        };
        let model = match m2::Model::parse(&bytes) {
            Ok(m) => m,
            Err(e) => {
                let key = e.to_string();
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                failures.entry(key).or_insert((0, name.clone())).0 += 1;
                continue;
            }
        };
        models += 1;
        for issue in model.validate() {
            let key = format!("model: {}", first_clause(&issue));
            failures.entry(key).or_insert((0, name.clone())).0 += 1;
        }
        verts += model.vertex_count() as u64;
        max_verts = max_verts.max(model.vertex_count());
        max_bones = max_bones.max(model.bones().len());

        let mut found_any = false;
        for lod in 0..model.skin_count().min(4) {
            let path = m2::skin_path(name, lod);
            let Ok(sb) = chain.read(&path) else { continue };
            match m2::Skin::parse(&sb) {
                Ok(skin) => {
                    found_any = true;
                    skins += 1;
                    tris += (skin.triangles().len() / 3) as u64;
                    if let Err(e) = skin.validate(model.vertex_count()) {
                        let key = format!("skin index table invalid: {}", first_clause(&e));
                        failures.entry(key).or_insert((0, path.clone())).0 += 1;
                    }
                }
                Err(e) => {
                    let key = format!("skin parse: {}", first_clause(&e.to_string()));
                    failures.entry(key).or_insert((0, path.clone())).0 += 1;
                }
            }
        }
        if !found_any {
            no_skin += 1;
        }
        if i % 2000 == 1999 {
            tracing::info!("{}/{} models", i + 1, names.len());
        }
    }

    println!("\n{models}/{} models parsed, {skins} skins", names.len());
    println!("  {verts} vertices, {tris} triangles across all levels of detail");
    println!("  largest model: {max_verts} vertices, {max_bones} bones");
    println!("  {no_skin} models had no readable skin");
    println!("  {unresolved} listed paths did not resolve (tombstoned or stale)");
    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

/// Trims a message to its first clause so similar errors group together.
fn first_clause(msg: &str) -> String {
    msg.split(&[',', ':'][..]).next().unwrap_or(msg).to_string()
}

fn m2_creature(chain: &mut Chain, display_id: u32) -> Result<()> {
    use dbc::schema::{CreatureDisplayInfo, CreatureModelData};

    let display = CreatureDisplayInfo::parse(&chain.read(CreatureDisplayInfo::PATH)?)?;
    let models = CreatureModelData::parse(&chain.read(CreatureModelData::PATH)?)?;

    let row = display
        .iter()
        .find(|d| d.id() == display_id)
        .with_context(|| format!("no CreatureDisplayInfo row {display_id}"))?;
    let model_row = models
        .iter()
        .find(|m| m.id() == row.model_id())
        .with_context(|| format!("no CreatureModelData row {}", row.model_id()))?;

    let dbc_path = model_row.model_name().to_string();
    let path = m2::model_path(&dbc_path);
    println!("display {display_id} -> model {} -> {dbc_path}", row.model_id());
    println!("  resolved: {path}");
    println!("  scale {:.2}, collision {:.2} wide x {:.2} high",
        model_row.model_scale(),
        model_row.collision_width(),
        model_row.collision_height());

    // Skins named by the DBC replace the model's runtime texture slots.
    let variations: Vec<&str> = [
        row.texture_variation_0(),
        row.texture_variation_1(),
        row.texture_variation_2(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    if !variations.is_empty() {
        println!("  texture variations: {}", variations.join(", "));
    }

    match chain.read(&path) {
        Ok(bytes) => {
            let model = m2::Model::parse(&bytes)?;
            println!(
                "  loaded: {} vertices, {} bones, {} textures",
                model.vertex_count(),
                model.bones().len(),
                model.textures().len()
            );
            // Runtime slots are where the DBC variations get substituted; the
            // directory comes from the model, the name from the DBC.
            for (i, t) in model.textures().iter().enumerate() {
                if !t.is_hardcoded() {
                    println!("    slot {i}: runtime type {} <- DBC variation", t.kind);
                }
            }
        }
        Err(e) => println!("  NOT FOUND: {e}"),
    }
    Ok(())
}

fn blp_cmd(chain: &mut Chain, cmd: BlpCommand) -> Result<()> {
    match cmd {
        BlpCommand::Info { path } => blp_info(chain, &path),
        BlpCommand::Export { path, level, out } => blp_export(chain, &path, level, out),
        BlpCommand::Survey { filter, limit } => blp_survey(chain, filter.as_deref(), limit),
    }
}

fn blp_info(chain: &mut Chain, path: &str) -> Result<()> {
    let bytes = chain.read(path)?;
    let tex = blp::Blp::parse(&bytes)?;
    println!("{path}");
    let usable = tex.usable_mip_count();
    println!(
        "  {}x{}  {}  alpha depth {}  {} mip levels ({usable} usable)  ({} bytes on disk)",
        tex.width(),
        tex.height(),
        tex.encoding().name(),
        tex.alpha_depth(),
        tex.mip_count(),
        bytes.len()
    );
    for level in 0..tex.mip_count() {
        let (w, h) = tex.level_size(level);
        let stored = match tex.level(level) {
            Some(blp::Level::Dxt { blocks, .. }) => blocks.len(),
            Some(blp::Level::Bgra(b)) => b.len(),
            Some(blp::Level::Palettized { indices, alpha, .. }) => indices.len() + alpha.len(),
            None => 0,
        };
        // Levels past the usable prefix are filler, not image data.
        let note = if level >= usable {
            format!("  padding (expected {})", tex.expected_level_bytes(level))
        } else {
            String::new()
        };
        println!("    {level:>2}: {w:>5}x{h:<5} {stored:>9} bytes{note}");
    }
    Ok(())
}

fn blp_export(chain: &mut Chain, path: &str, level: usize, out: Option<PathBuf>) -> Result<()> {
    let bytes = chain.read(path)?;
    let tex = blp::Blp::parse(&bytes)?;
    let rgba = tex
        .decode_rgba(level)
        .with_context(|| format!("no mip level {level} (texture has {})", tex.mip_count()))?;
    let (w, h) = tex.level_size(level);

    let out = out.unwrap_or_else(|| {
        let stem = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("texture")
            .trim_end_matches(".blp")
            .trim_end_matches(".BLP");
        PathBuf::from(format!("{stem}.png"))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::File::create(&out)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;

    println!(
        "wrote {}x{} ({}) to {}",
        w,
        h,
        tex.encoding().name(),
        out.display()
    );
    Ok(())
}

fn blp_survey(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.map(str::to_lowercase);
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".blp") && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    // An example per encoding makes the survey self-documenting: every row can
    // be reproduced with `blp export`.
    let mut kinds: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut ok, mut no_mips, mut widest) = (0usize, 0usize, 0u32);

    for (i, name) in names.iter().enumerate() {
        let Ok(bytes) = chain.read(name) else {
            continue; // tombstoned or stale listfile entry
        };
        match blp::Blp::parse(&bytes) {
            Ok(tex) => {
                ok += 1;
                widest = widest.max(tex.width());
                if tex.mip_count() <= 1 {
                    no_mips += 1;
                }
                kinds
                    .entry(format!(
                        "{:<11} alpha_depth={}",
                        tex.encoding().name(),
                        tex.alpha_depth()
                    ))
                    .or_insert_with(|| (0, name.clone()))
                    .0 += 1;
            }
            Err(e) => {
                let key = e.to_string();
                // Collapse the variable parts so one systematic gap is one row.
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                let entry = failures.entry(key).or_insert((0, name.clone()));
                entry.0 += 1;
            }
        }
        if i % 20000 == 19999 {
            tracing::info!("{}/{} surveyed", i + 1, names.len());
        }
    }

    println!("\n{ok}/{} textures parsed\n", names.len());
    println!("encodings in use:");
    for (kind, (count, example)) in &kinds {
        println!("  {count:>7}  {kind}\n           e.g. {example}");
    }
    println!("\nwidest texture: {widest}px; {no_mips} have a single mip level");

    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

fn info(chain: &mut Chain) -> Result<()> {
    let paths: Vec<_> = chain.archives().map(|a| a.path().to_path_buf()).collect();
    println!("{} archives, in load order (last wins):\n", paths.len());
    for (i, path) in paths.iter().enumerate() {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut archive = Archive::open(path)?;
        let listed = archive.list()?.len();
        println!(
            "  {:>2}. {:<34} {:>9.1} MiB  {:>7} listed",
            i + 1,
            path.file_name().unwrap_or_default().to_string_lossy(),
            size as f64 / (1024.0 * 1024.0),
            listed,
        );
    }
    println!("\ntotal unique paths: {}", chain.list()?.len());
    Ok(())
}

fn ls(chain: &mut Chain, filter: Option<&str>, limit: usize) -> Result<()> {
    let names = chain.list()?;
    let needle = filter.map(str::to_lowercase);
    let matched: Vec<&String> = names
        .iter()
        .filter(|n| {
            needle
                .as_ref()
                .is_none_or(|f| n.to_lowercase().contains(f.as_str()))
        })
        .collect();

    for name in matched.iter().take(limit) {
        match chain.stat(name) {
            Some(e) => println!("{:>12}  {}", e.size, name),
            None => println!("{:>12}  {}", "?", name),
        }
    }
    if matched.len() > limit {
        println!("... {} more (raise --limit)", matched.len() - limit);
    }
    println!("\n{} matched of {} total", matched.len(), names.len());
    Ok(())
}

fn extract(chain: &mut Chain, name: &str, out: Option<PathBuf>) -> Result<()> {
    let data = chain.read(name)?;
    let out = out.unwrap_or_else(|| {
        PathBuf::from(name.rsplit(['\\', '/']).next().unwrap_or("out.bin"))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, &data)?;
    println!("wrote {} bytes to {}", data.len(), out.display());
    Ok(())
}

fn which(chain: &Chain, name: &str) -> Result<()> {
    println!("{name}");
    match chain.source_of(name) {
        Some(path) => {
            println!("  -> {}", path.display());
            if let Some(e) = chain.stat(name) {
                println!(
                    "     {} bytes ({} packed), flags {:#010x}{}{}",
                    e.size,
                    e.packed_size,
                    e.flags,
                    if e.compressed { ", compressed" } else { "" },
                    if e.encrypted { ", encrypted" } else { "" },
                );
            }
        }
        None => println!("  -> does not resolve"),
    }

    // The full chain matters when a patch deletes something the base still
    // holds: the winning answer alone does not explain why.
    let trace = chain.trace(name);
    if !trace.is_empty() {
        println!("\n  chain (highest priority first):");
        for (path, state) in trace {
            let file = path.file_name().unwrap_or_default().to_string_lossy();
            match state {
                mpq::State::Present { size, flags } => {
                    println!("    {file:<30} present  {size:>9} bytes  flags {flags:#010x}")
                }
                mpq::State::Deleted { size, flags } => {
                    println!("    {file:<30} DELETED  {size:>9} bytes  flags {flags:#010x}")
                }
                mpq::State::Absent => {}
            }
        }
    }
    Ok(())
}

/// Accepts `Map`, `Map.dbc`, or a full archive path.
fn dbc_path(table: &str) -> String {
    if table.contains('\\') || table.contains('/') {
        table.to_string()
    } else if table.to_lowercase().ends_with(".dbc") {
        format!(r"DBFilesClient\{table}")
    } else {
        format!(r"DBFilesClient\{table}.dbc")
    }
}

fn dbc_cmd(chain: &mut Chain, cmd: DbcCommand) -> Result<()> {
    match cmd {
        DbcCommand::List { filter } => dbc_list(chain, filter.as_deref()),
        DbcCommand::Info { table } => dbc_info(chain, &table),
        DbcCommand::Dump { table, limit } => dbc_dump(chain, &table, limit),
        DbcCommand::Rows { table, limit } => dbc_rows(chain, &table, limit),
        DbcCommand::Check => dbc_check(chain),
    }
}

fn dbc_list(chain: &mut Chain, filter: Option<&str>) -> Result<()> {
    let needle = filter.map(str::to_lowercase);
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            lower.starts_with("dbfilesclient\\")
                && lower.ends_with(".dbc")
                && needle.as_ref().is_none_or(|f| lower.contains(f.as_str()))
        })
        .collect();

    println!("{:<40} {:>8} {:>7} {:>7}", "table", "records", "fields", "strings");
    let (mut ok, mut bad) = (0, 0);
    for name in &names {
        let short = name.rsplit('\\').next().unwrap_or(name);
        match chain.read(name).map_err(anyhow::Error::from).and_then(|b| {
            dbc::Dbc::parse(&b).map_err(anyhow::Error::from)
        }) {
            Ok(t) => {
                ok += 1;
                // Byte-packed tables cannot be read with word accessors, so
                // flag them rather than letting a schema quietly misread one.
                let note = if t.is_uniform() {
                    String::new()
                } else {
                    format!("  byte-packed ({} bytes/record)", t.record_size())
                };
                println!(
                    "{short:<40} {:>8} {:>7} {:>7}{note}",
                    t.len(),
                    t.fields(),
                    t.string_block().len()
                );
            }
            Err(e) => {
                bad += 1;
                println!("{short:<40} {:>8} {e}", "-");
            }
        }
    }
    println!("\n{ok} tables parsed, {bad} failed");
    Ok(())
}

fn load_dbc(chain: &mut Chain, table: &str) -> Result<(String, dbc::Dbc)> {
    let path = dbc_path(table);
    let bytes = chain
        .read(&path)
        .with_context(|| format!("reading {path}"))?;
    let parsed = dbc::Dbc::parse(&bytes).with_context(|| format!("parsing {path}"))?;
    Ok((path, parsed))
}

fn dbc_info(chain: &mut Chain, table: &str) -> Result<()> {
    let (path, t) = load_dbc(chain, table)?;
    println!("{path}");
    println!(
        "  {} records x {} fields ({} bytes/record), {} bytes of strings\n",
        t.len(),
        t.fields(),
        t.record_size(),
        t.string_block().len()
    );

    println!("inferred columns (types are guessed -- verify before trusting):");
    println!("  {:>5}  {:<8} {:>12} {:>12} {:>7}", "field", "type", "min", "max", "zeros");
    for c in dbc::infer::infer(&t) {
        use dbc::infer::ColumnKind as K;
        // Locale padding is noise; collapse it into the localized column.
        if c.kind == K::LocalePad {
            continue;
        }
        let (min, max) = match c.kind {
            K::Float => (
                format!("{:.3}", f32::from_bits(c.min)),
                format!("{:.3}", f32::from_bits(c.max)),
            ),
            _ => (c.min.to_string(), c.max.to_string()),
        };
        let note = if c.kind == K::Localized { "  (spans 17 fields)" } else { "" };
        println!(
            "  {:>5}  {:<8} {min:>12} {max:>12} {:>7}{note}",
            c.index,
            c.kind.as_str(),
            c.zeros
        );
    }
    Ok(())
}

fn dbc_dump(chain: &mut Chain, table: &str, limit: usize) -> Result<()> {
    let (path, t) = load_dbc(chain, table)?;
    let columns = dbc::infer::infer(&t);
    println!("{path} -- {} records\n", t.len());

    for (i, row) in t.rows().take(limit).enumerate() {
        let mut parts: Vec<String> = Vec::new();
        for c in &columns {
            use dbc::infer::ColumnKind as K;
            let v = row.raw(c.index);
            match c.kind {
                K::LocalePad | K::LocaleMask | K::Empty => continue,
                K::Float => parts.push(format!("{}={:.3}", c.index, f32::from_bits(v))),
                K::String => parts.push(format!("{}={:?}", c.index, t.string_at(v))),
                K::Localized => parts.push(format!("{}={:?}", c.index, t.string_at(v))),
                K::Bool => parts.push(format!("{}={}", c.index, v != 0)),
                K::Int => parts.push(format!("{}={v}", c.index)),
            }
        }
        println!("[{i}] {}", parts.join("  "));
    }
    if t.len() > limit {
        println!("\n... {} more (raise --limit)", t.len() - limit);
    }
    Ok(())
}

fn dbc_rows(chain: &mut Chain, table: &str, limit: usize) -> Result<()> {
    use dbc::schema::*;

    macro_rules! dispatch {
        ($($name:ident),* $(,)?) => {
            match table.to_lowercase().as_str() {
                $(
                    t if t == stringify!($name).to_lowercase() => {
                        let bytes = chain.read($name::PATH)?;
                        let parsed = $name::parse(&bytes)?;
                        println!("{} -- {} rows\n", $name::PATH, parsed.len());
                        for (i, row) in parsed.iter().take(limit).enumerate() {
                            println!("[{i}] {row:?}");
                        }
                        if parsed.len() > limit {
                            println!("\n... {} more (raise --limit)", parsed.len() - limit);
                        }
                        return Ok(());
                    }
                )*
                other => anyhow::bail!(
                    "no schema for {other:?}; known: {}. Use `dbc dump` for an \
                     untranscribed table.",
                    [$(stringify!($name)),*].join(", ")
                ),
            }
        };
    }

    dispatch!(Map, AreaTable, CreatureDisplayInfo, CreatureModelData, AnimationData, Spell)
}

fn dbc_check(chain: &mut Chain) -> Result<()> {
    use dbc::schema::*;

    macro_rules! check {
        ($($name:ident),* $(,)?) => {{
            let mut failures = 0;
            $(
                let label = $name::NAME;
                match chain.read($name::PATH) {
                    Ok(bytes) => match $name::parse(&bytes) {
                        Ok(t) => println!(
                            "  ok    {label:<22} {:>7} rows x {} fields",
                            t.len(),
                            $name::FIELDS
                        ),
                        Err(e) => {
                            failures += 1;
                            println!("  FAIL  {label:<22} {e}");
                        }
                    },
                    Err(e) => {
                        failures += 1;
                        println!("  FAIL  {label:<22} {e}");
                    }
                }
            )*
            failures
        }};
    }

    println!("checking transcribed schemas against this install:");
    let failures = check!(
        Map,
        AreaTable,
        CreatureDisplayInfo,
        CreatureModelData,
        AnimationData,
        Spell
    );
    println!();
    if failures == 0 {
        println!("all schemas match");
        Ok(())
    } else {
        anyhow::bail!("{failures} schema(s) do not match this build")
    }
}

fn verify(chain: &mut Chain, limit: Option<usize>, filter: Option<&str>) -> Result<()> {
    let names = chain.list()?;
    let needle = filter.map(str::to_lowercase);
    let targets: Vec<String> = names
        .into_iter()
        .filter(|n| {
            needle
                .as_ref()
                .is_none_or(|f| n.to_lowercase().contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let (mut ok, mut bytes) = (0usize, 0u64);
    let mut failures: Vec<(String, String)> = Vec::new();

    for (i, name) in targets.iter().enumerate() {
        match chain.read(name) {
            Ok(data) => {
                ok += 1;
                bytes += data.len() as u64;
            }
            Err(e) => failures.push((name.clone(), e.to_string())),
        }
        if i % 5000 == 4999 {
            tracing::info!("{}/{} checked", i + 1, targets.len());
        }
    }

    println!(
        "\nread {ok}/{} files, {:.2} GiB",
        targets.len(),
        bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    if failures.is_empty() {
        println!("no failures");
    } else {
        // Group by error text; a systematic format gap shows up as one huge
        // bucket, whereas real corruption is scattered.
        let mut kinds: std::collections::BTreeMap<String, (usize, String)> = Default::default();
        for (name, err) in &failures {
            let key = err.split(':').next().unwrap_or(err).to_string();
            let e = kinds.entry(key).or_insert((0, name.clone()));
            e.0 += 1;
        }
        println!("\n{} failures:", failures.len());
        for (kind, (count, example)) in kinds {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}
