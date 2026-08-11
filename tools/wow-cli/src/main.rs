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
    }
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
    match chain.source_of(name) {
        Some(path) => {
            println!("{name}\n  -> {}", path.display());
            if let Some(e) = chain.stat(name) {
                println!(
                    "     {} bytes ({} packed){}{}",
                    e.size,
                    e.packed_size,
                    if e.compressed { ", compressed" } else { "" },
                    if e.encrypted { ", encrypted" } else { "" },
                );
            }
        }
        None => println!("{name}\n  -> not present in any archive"),
    }
    Ok(())
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
