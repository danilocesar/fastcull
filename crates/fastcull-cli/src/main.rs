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
    /// Run the thumbnail pipeline over a folder and report throughput.
    /// Exits 2 when any file failed (recorded decision: scripts must be able
    /// to detect partial failure without parsing output).
    Thumbs {
        folder: PathBuf,
        /// Write the thumbnails as JPEGs into this directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Preview-cache DB path (default: per-user config dir).
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
    let cache_path = if no_cache {
        None
    } else {
        cache.or_else(fastcull_core::cache::default_cache_path)
    };
    if let Some(p) = &cache_path {
        println!("cache: {}", p.display());
    }

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
    let (pipeline, events) = Pipeline::start(jobs, cache_path.clone(), threads);

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
            Ok(SessionEvent::MetadataReady { .. }) => {}
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
