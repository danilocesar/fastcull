//! Headless driver for the FastCull engine.
//!
//! One subcommand per engine capability as milestones land (M1: `scan`,
//! `thumbs`); integration tests and QE drive the engine through this binary.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, Subcommand};
use fastcull_core::catalog::{LoadState, Session};
use fastcull_core::pipeline::{JobSpec, Pipeline, SessionEvent};

#[derive(Parser)]
#[command(name = "fastcull-cli", version = fastcull_core::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the RAW files of a folder (instant, no file contents read).
    Scan { folder: PathBuf },
    /// Mark picks/rejects headlessly (writes darktable-compatible sidecars).
    Cull {
        folder: PathBuf,
        /// File names (not paths) to mark as picked.
        #[arg(long, num_args = 1..)]
        pick: Vec<String>,
        /// File names to mark as rejected.
        #[arg(long, num_args = 1..)]
        reject: Vec<String>,
        /// File names to clear back to unmarked.
        #[arg(long, num_args = 1..)]
        clear: Vec<String>,
    },
    /// Run the thumbnail pipeline over a folder and report throughput.
    /// Exits 2 when any file failed (recorded decision: scripts must be able
    /// to detect partial failure without parsing output).
    Thumbs {
        folder: PathBuf,
        /// Write the thumbnails as JPEGs into this directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Preview-cache DB path (default: the per-user cache dir, capped by
        /// settings.toml's `performance.cache_cap`).
        #[arg(long, conflicts_with = "no_cache")]
        cache: Option<PathBuf>,
        /// Disable the preview cache entirely.
        #[arg(long)]
        no_cache: bool,
        /// Worker threads (default: all cores).
        #[arg(long)]
        threads: Option<usize>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Scan { folder } => scan(&folder),
        Command::Cull {
            folder,
            pick,
            reject,
            clear,
        } => cull(&folder, &pick, &reject, &clear),
        Command::Thumbs {
            folder,
            out,
            cache,
            no_cache,
            threads,
        } => thumbs(&folder, out, cache, no_cache, threads),
    }
}

fn scan(folder: &std::path::Path) -> anyhow::Result<()> {
    let t = Instant::now();
    let session = Session::open(folder)?;
    let elapsed = t.elapsed();
    for image in &session.images {
        let marker = match &image.state {
            LoadState::Failed(reason) => format!("  [FAILED: {reason}]"),
            _ => String::new(),
        };
        println!("{:>12}  {}{marker}", image.size, image.file_name());
    }
    println!(
        "{} RAW files in {} ({elapsed:.2?}{})",
        session.images.len(),
        session.folder.display(),
        if session.scan_errors > 0 {
            format!(", {} unreadable directory entries", session.scan_errors)
        } else {
            String::new()
        }
    );
    Ok(())
}

fn cull(
    folder: &std::path::Path,
    pick: &[String],
    reject: &[String],
    clear: &[String],
) -> anyhow::Result<()> {
    use fastcull_core::catalog::PickState;
    let mut failures = 0usize;
    let mut apply = |names: &[String], state: PickState| {
        for name in names {
            // Names only — never paths (a ../ would write outside the
            // folder; QE demonstrated exactly that).
            if name.contains('/') || name.contains('\\') || name.contains(':') || name == ".." {
                eprintln!("SKIPPED {name}: must be a file name, not a path");
                failures += 1;
                continue;
            }
            let raw = folder.join(name);
            if !raw.is_file() {
                eprintln!("SKIPPED {name}: no such file in folder");
                failures += 1;
                continue;
            }
            match fastcull_core::xmp::write_pick(&raw, state) {
                Ok(()) => println!("{state:?}	{name}"),
                Err(e) => {
                    eprintln!("FAILED {name}: {e}");
                    failures += 1;
                }
            }
        }
    };
    apply(pick, PickState::Picked);
    apply(reject, PickState::Rejected);
    apply(clear, PickState::Unmarked);
    if failures > 0 {
        std::process::exit(2);
    }
    Ok(())
}

/// Where the cache cap on the `cache:` line came from — the file only when
/// one was read; it used to say "from settings.toml" with no file at all
/// (QE 2026-10-01, D11). A missing file, an unreadable one and no config
/// dir all leave the default in force, and each says why. The `readers:`
/// line words an adaptive pool the same way ([`readers_source`]).
fn cap_source(loaded: &fastcull_core::settings::Loaded) -> &'static str {
    match (&loaded.path, &loaded.error) {
        (None, _) if std::env::var_os(fastcull_core::settings::NO_CONFIG_VAR).is_some() => {
            ", the default — FASTCULL_NO_CONFIG is set"
        }
        (None, _) => ", the default — this system has no config directory",
        (Some(_), Some(_)) => ", the default — settings.toml could not be read",
        (Some(path), None) if path.is_file() => " from settings.toml",
        (Some(_), None) => ", the default — no settings.toml",
    }
}

/// Where the read pool's configuration on the `readers:` line came from
/// (settings.md, "Performance › Read workers"): the environment, which wins;
/// the file's limit; or adaptive, worded as the `cache:` line words the
/// file it did or did not read.
fn readers_source(
    readers: fastcull_core::settings::Readers,
    loaded: &fastcull_core::settings::Loaded,
) -> String {
    use fastcull_core::settings::{Readers, MAX_READERS_VAR};
    match readers {
        Readers::Environment(n) => format!("{MAX_READERS_VAR}={n}"),
        Readers::Limit(n) => format!("max_readers = {n} from settings.toml"),
        Readers::Adaptive => format!("adaptive{}", cap_source(loaded)),
    }
}

fn thumbs(
    folder: &std::path::Path,
    out: Option<PathBuf>,
    cache: Option<PathBuf>,
    no_cache: bool,
    threads: Option<usize>,
) -> anyhow::Result<()> {
    let session = Session::open(folder)?;
    if session.images.is_empty() {
        println!("no RAW files in {}", folder.display());
        return Ok(());
    }
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir).context("creating --out directory")?;
    }
    // The same settings file the app reads (settings.md, ADR 0005): the
    // cache cap and the read workers are knobs the two binaries share, so
    // the CLI honours them with no flag surface of its own.
    let loaded = fastcull_core::settings::load_default();
    let settings = &loaded.settings;
    let cache_path = if no_cache {
        None
    } else if let Some(explicit) = cache {
        // A caller-provided cache is uncapped in v1 (catalog-cache.md).
        println!("cache: {}", explicit.display());
        Some(explicit)
    } else {
        let default = fastcull_core::cache::default_cache_path(settings.cache_cap_bytes());
        if let Some(p) = &default {
            println!(
                "cache: {} (cap {}{})",
                p.display(),
                settings.cache_cap_text(),
                cap_source(&loaded)
            );
        }
        default
    };

    let jobs: Vec<JobSpec> = session
        .images
        .iter()
        .map(|i| JobSpec {
            path: i.path.clone(),
            size: i.size,
            mtime: i.mtime,
        })
        .collect();
    let total = jobs.len();
    let threads = threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .max(1); // core clamps identically; keep the report honest

    let t = Instant::now();
    // FASTCULL_MAX_READERS wins over the file's `max_readers` (settings.md,
    // "Environment precedence") — resolved in core, the one place the two
    // are reconciled.
    let readers = fastcull_core::settings::resolve_max_readers_from_env(settings.max_readers);
    let (pipeline, events) = Pipeline::start(
        jobs,
        cache_path.clone(),
        threads,
        readers.override_for_pool(),
    );
    // The bounds the read pool ADOPTED, read back from the pool — never
    // from `readers` above: a line that echoed the CLI's own resolution
    // would stay true with the pool started on anything else (the app's
    // `read pool started` mark has the same rule, brief 008 D23/D27).
    // Until this line the call site above had no observable at all, and
    // passing `None` there left the suite green (QE 2026-10-01, D34).
    let (floor, cap) = pipeline.read_pool_bounds();
    println!(
        "readers: floor {floor} cap {cap} ({})",
        readers_source(readers, &loaded)
    );

    let mut thumbs_done = 0usize;
    let mut cache_hits = 0usize;
    let mut failures: Vec<(usize, String)> = Vec::new();
    let mut written_stems = std::collections::HashSet::new();
    // One terminal event per image: ThumbReady or Failed.
    while thumbs_done + failures.len() < total {
        match events.recv() {
            Ok(SessionEvent::ThumbReady {
                index,
                thumb_jpeg,
                from_cache,
                ..
            }) => {
                thumbs_done += 1;
                cache_hits += usize::from(from_cache);
                if let Some(dir) = &out {
                    let stem = session.images[index]
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| format!("img{index}"));
                    // Same stem from different files (a.ARW + a.arw): keep
                    // both instead of silently overwriting.
                    let name = if written_stems.insert(stem.clone()) {
                        format!("{stem}.jpg")
                    } else {
                        format!("{stem}_{index}.jpg")
                    };
                    std::fs::write(dir.join(&name), &thumb_jpeg)
                        .with_context(|| format!("writing thumb {name}"))?;
                }
                if thumbs_done.is_multiple_of(250) {
                    println!("  {thumbs_done}/{total}…");
                }
            }
            Ok(SessionEvent::Failed { index, reason }) => failures.push((index, reason)),
            Ok(SessionEvent::MetadataReady { .. }) | Ok(SessionEvent::Sidecar { .. }) => {}
            Err(_) => anyhow::bail!("pipeline hung up before finishing"),
        }
    }
    let elapsed = t.elapsed();
    drop(pipeline);

    for (index, reason) in &failures {
        eprintln!("FAILED {}: {reason}", session.images[*index].file_name());
    }
    println!(
        "{thumbs_done}/{total} thumbnails ({cache_hits} from cache, {} failed) in {elapsed:.2?} — {:.0} files/sec on {threads} threads",
        failures.len(),
        total as f64 / elapsed.as_secs_f64()
    );
    if !failures.is_empty() {
        std::process::exit(2);
    }
    Ok(())
}
