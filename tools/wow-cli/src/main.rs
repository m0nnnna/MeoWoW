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
        /// One of: Map, AreaTable, CreatureDisplayInfo, CreatureModelData, Spell.
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
    }
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

    dispatch!(Map, AreaTable, CreatureDisplayInfo, CreatureModelData, Spell)
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
    let failures = check!(Map, AreaTable, CreatureDisplayInfo, CreatureModelData, Spell);
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
