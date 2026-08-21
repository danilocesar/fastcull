//! Copy-picks engine (specs/modules/fileops.md): two-phase — a pure,
//! inspectable PLAN, then an EXECUTE on a worker thread with streaming
//! BLAKE3 verification. Originals are never touched (copy, not move).
//!
//! The FLUSH BARRIER is the caller's duty: the app must call
//! `SidecarWriter::flush()` (after committing any in-progress panel edit)
//! BEFORE `execute` — a pick or caption made a moment ago must be in the
//! copied sidecar.

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::iptc::{expand, ExpandContext, IptcError};
use crate::xmp::sidecar_path;

/// What to do when the destination file already exists. The UI exposes
/// Rename (default) and Skip only; Overwrite/Abort exist for the core
/// contract (fileops.md: overwrite can destroy a verified prior copy —
/// never surfaced in v1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistsMode {
    Rename,
    Skip,
    Overwrite,
    Abort,
}

/// One image's planned transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyJob {
    /// Session image id (badges + report wiring).
    pub id: usize,
    pub src_raw: PathBuf,
    pub dst_raw: PathBuf,
    /// Present only when the source sidecar file exists.
    pub src_xmp: Option<PathBuf>,
    pub dst_xmp: PathBuf,
    pub action: PlanAction,
    /// RAW bytes this job will copy (0 for Skip/SidecarRefresh).
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanAction {
    /// Copy RAW + sidecar to the (possibly template-renamed) destination.
    Copy,
    /// As Copy, but the name got a collision suffix (`_2`, `_3`, …) —
    /// listed in the plan preview (fileops.md: multi-camera days).
    CopyRenamed,
    /// Destination exists (or copied earlier this session and still
    /// there): nothing moves.
    Skip,
    /// RAW skipped but the SOURCE sidecar changed since the copy — the
    /// sidecar alone is refreshed (belt-and-braces; the user's workflow is
    /// metadata-before-copy).
    SidecarRefresh,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("destination is the source folder itself")]
    DestEqualsSource,
    #[error("destination is inside the source folder")]
    DestInsideSource,
    #[error("rename template: {0}")]
    Template(#[from] IptcError),
    #[error("template collision: '{name}' is produced by both {first} and {second}")]
    TemplateCollision {
        name: String,
        first: String,
        second: String,
    },
    #[error("destination already has {0} of these files (abort mode)")]
    DestExists(usize),
    #[error("not enough free space: need {needed} bytes, {free} available")]
    InsufficientSpace { needed: u64, free: u64 },
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// The inspectable plan the dialog previews. `jobs` keep the input order
/// (= session sort order, the `{seq}` contract).
#[derive(Debug, Default)]
pub struct CopyPlan {
    pub jobs: Vec<CopyJob>,
    /// Bytes execute will actually write (RAWs of Copy/CopyRenamed).
    pub total_bytes: u64,
    /// None = statvfs failed ("free space unknown"), check skipped.
    pub free_bytes: Option<u64>,
    pub renamed: usize,
    pub skipped: usize,
    pub refreshed: usize,
    /// Copied earlier this session but GONE from the destination when the
    /// plan looked (the user deleted the copy by hand) — and going out
    /// again as a Copy/CopyRenamed job. The dialog's amber note.
    pub recopied: usize,
}

/// What this session copied WHERE: image id → the RAW path(s) it landed
/// at, one per destination folder. The plan reads it for the re-run
/// default (fileops.md re-run trap: a file copied this session is
/// skipped, never `_2`-suffixed) and the grid's copied badge reads it.
///
/// A copy counts only while it is still on disk. `plan` re-checks the
/// landed path on every run — a copy the user deleted by hand is copied
/// again (the 2026-08-21 bug: the old id-only set forced a Skip over an
/// empty folder, so the sidecar came back and the RAW never did) — and
/// `refresh` re-checks for the badge. Remembering the LANDED path, not
/// the folder, is what makes a `_2` copy judged as `_2` (issue #14), and
/// keeping one entry per destination is what survives copying to A,
/// then B, then back to A (persona review 2026-08-21).
#[derive(Debug, Default, Clone)]
pub struct SessionCopies {
    landed: HashMap<usize, Vec<LandedCopy>>,
    /// Folder → canonical folder, memoized: `record` runs on the UI thread
    /// once per landed file and must not pay a `realpath` per file on a
    /// slow (network) destination (gate risk note).
    canon: HashMap<PathBuf, PathBuf>,
}

#[derive(Debug, Clone)]
struct LandedCopy {
    path: PathBuf,
    /// Re-checked by [`SessionCopies::refresh`]: false once the file is
    /// found missing, so the badge drops the moment the Copy dialog
    /// discovers it; `record` supersedes the entry when the copy lands
    /// again.
    present: bool,
}

impl SessionCopies {
    /// A RAW finished a verified copy and landed at `path`. Replaces an
    /// earlier entry in the same folder (a re-copy after a hand deletion
    /// supersedes the gone one); other folders' entries are kept.
    pub fn record(&mut self, id: usize, path: PathBuf) {
        // "Same folder" is the CANONICAL comparison `plan` makes (gate
        // finding: a re-spelled destination must supersede the entry it
        // matched, or the stale path later reads as "gone" and the image
        // goes out a second time).
        let canon = &mut self.canon;
        let mut canon_dir = |p: &Path| -> Option<PathBuf> {
            p.parent().map(|d| {
                canon
                    .entry(d.to_path_buf())
                    .or_insert_with(|| canonicalize_lenient(d))
                    .clone()
            })
        };
        let dir = canon_dir(&path);
        let entries = self.landed.entry(id).or_default();
        entries.retain(|e| canon_dir(&e.path) != dir);
        entries.push(LandedCopy {
            path,
            present: true,
        });
    }

    /// Re-stat every landed copy so [`SessionCopies::is_copied`] follows
    /// what is on disk (one `stat` per entry — cheap enough per replan).
    pub fn refresh(&mut self) {
        for e in self.landed.values_mut().flatten() {
            e.present = e.path.exists();
        }
        // A destination re-pointed mid-session (symlink) must not keep its
        // old canonical target past the next dialog open (gate risk note).
        self.canon.clear();
    }

    /// Is a copy of `id` still there, in any destination? The grid badge.
    pub fn is_copied(&self, id: usize) -> bool {
        self.landed
            .get(&id)
            .is_some_and(|v| v.iter().any(|e| e.present))
    }

    /// Every path `id` landed at this session, any destination.
    fn landed_paths(&self, id: usize) -> impl Iterator<Item = &Path> {
        self.landed
            .get(&id)
            .into_iter()
            .flatten()
            .map(|e| e.path.as_path())
    }
}

/// One picked image, in SESSION SORT ORDER (capture time by default —
/// fileops.md: `{seq}` follows it; the filter bar does not affect scope).
pub struct PlanSource {
    pub id: usize,
    pub path: PathBuf,
    pub size: u64,
    pub ctx: ExpandContext,
}

/// Build the plan. Pure with respect to MUTATION — it reads the
/// filesystem (existence, mtimes, free space) but changes nothing.
/// `session` = what this session already copied where (persona re-run
/// trap: a copy of the image that is still in THIS destination defaults
/// to Skip, never suffixing; a copy that is gone is copied again).
pub fn plan(
    sources: &[PlanSource],
    dest: &Path,
    template: Option<&str>,
    exists_mode: ExistsMode,
    session: &SessionCopies,
) -> Result<CopyPlan, PlanError> {
    // Dest-inside-source / equality (canonicalized where possible; a not-
    // yet-created destination canonicalizes its existing ancestors).
    let dest_canon = canonicalize_lenient(dest);
    if let Some(src_dir) = sources.first().and_then(|s| s.path.parent()) {
        let src_canon = src_dir.canonicalize().unwrap_or_else(|_| src_dir.into());
        if dest_canon == src_canon {
            return Err(PlanError::DestEqualsSource);
        }
        if dest_canon.starts_with(&src_canon) {
            return Err(PlanError::DestInsideSource);
        }
    }

    // Phase 1: expand every destination name (all-or-nothing, like IPTC
    // apply) and detect in-plan collisions before touching modes.
    let n = sources.len();
    let mut names: Vec<String> = Vec::with_capacity(n);
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, s) in sources.iter().enumerate() {
        let original = s
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = match template {
            None => original.clone(),
            Some(t) if t.trim().is_empty() => original.clone(),
            Some(t) => {
                let expanded = expand("rename", t, &s.ctx, i + 1, n)?;
                crate::iptc::sanitize_text(&expanded)
            }
        };
        if let Some(prev) = seen.insert(name.clone(), i) {
            return Err(PlanError::TemplateCollision {
                name,
                first: sources[prev]
                    .path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                second: original,
            });
        }
        names.push(name);
    }

    // Phase 2: exists handling per file.
    let mut jobs = Vec::with_capacity(n);
    let mut taken: HashSet<String> = HashSet::new(); // names claimed in-plan
    let mut existing_hits = 0usize;
    let (mut renamed, mut skipped, mut refreshed, mut recopied) = (0usize, 0usize, 0usize, 0usize);
    // Folder → "is it `dest`?", compared canonically (the same destination
    // reached via another spelling keeps its skip default) and memoized
    // per folder: a 2,000-pick plan canonicalizes once, not 2,000 times.
    let mut is_dest: HashMap<PathBuf, bool> = HashMap::new();
    for (s, name) in sources.iter().zip(&names) {
        let src_xmp_path = sidecar_path(&s.path);
        let src_xmp = src_xmp_path.exists().then_some(src_xmp_path.clone());
        let natural = dest.join(name);
        let exists = natural.exists();
        // The copy this session landed in THIS folder, if any — and only
        // while it is still there: a hand-deleted copy falls through to
        // the normal handling below and goes out again.
        let landed = session.landed_paths(s.id).find(|p| {
            p.parent().is_some_and(|dir| {
                *is_dest
                    .entry(dir.to_path_buf())
                    .or_insert_with(|| canonicalize_lenient(dir) == dest_canon)
            })
        });
        let (landed, gone) = match landed {
            Some(p) if p.exists() => (Some(p), None),
            Some(p) => (None, Some(p)),
            None => (None, None),
        };
        // A gone copy that had landed under a COLLISION SUFFIX of the
        // natural name says that name belongs to a foreign file (that is
        // why the copy took the suffix): Skip-existing must not take that
        // file for our copy, and its sidecar is never ours to refresh.
        // Only the suffix is evidence — a landed name that differs because
        // the template changed says nothing, and Skip-existing then means
        // skip (gate findings, rounds 1 and 2).
        let natural_is_foreign = gone.is_some_and(|p| match (p.file_name(), natural.file_name()) {
            (Some(l), Some(n)) => {
                is_collision_suffix_of(&l.to_string_lossy(), &n.to_string_lossy())
            }
            _ => false,
        });
        let gone = gone.is_some();
        let (dst_raw, action) = if let Some(landed) = landed {
            // Copied this session and still there: skip under the name it
            // actually landed as — a `_2` copy is judged as `_2`, never as
            // the natural name beside a foreign file (issue #14) — with
            // the sidecar-alone refresh if the source one changed.
            existing_hits += 1;
            let action = skip_or_refresh(landed, &src_xmp_path, src_xmp.is_some());
            match action {
                PlanAction::SidecarRefresh => refreshed += 1,
                _ => skipped += 1,
            }
            (landed.to_path_buf(), action)
        } else if exists && exists_mode == ExistsMode::Skip && !natural_is_foreign {
            existing_hits += 1;
            let action = skip_or_refresh(&natural, &src_xmp_path, src_xmp.is_some());
            match action {
                PlanAction::SidecarRefresh => refreshed += 1,
                _ => skipped += 1,
            }
            (natural, action)
        } else if exists {
            existing_hits += 1;
            match exists_mode {
                ExistsMode::Overwrite => (natural, PlanAction::Copy),
                ExistsMode::Abort => (natural, PlanAction::Copy), // counted; errors below
                _ => {
                    // Rename: first free numeric suffix, checking BOTH the
                    // disk and names already claimed by this plan.
                    let mut k = 2usize;
                    let renamed_to = loop {
                        let candidate = suffixed(name, k);
                        if !dest.join(&candidate).exists() && !taken.contains(&candidate) {
                            break candidate;
                        }
                        k += 1;
                    };
                    renamed += 1;
                    (dest.join(renamed_to), PlanAction::CopyRenamed)
                }
            }
        } else {
            (natural, PlanAction::Copy)
        };
        // The amber note counts only what actually goes out again: a gone
        // copy whose natural name is now taken by a file the session has
        // no evidence about, with Skip-existing on, ends up skipped — and
        // "copying again" would be a lie for it.
        if gone && matches!(action, PlanAction::Copy | PlanAction::CopyRenamed) {
            recopied += 1;
        }
        let final_name = dst_raw
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        taken.insert(final_name);
        let bytes = match action {
            PlanAction::Copy | PlanAction::CopyRenamed => s.size,
            _ => 0,
        };
        jobs.push(CopyJob {
            id: s.id,
            src_raw: s.path.clone(),
            dst_xmp: sidecar_path(&dst_raw),
            dst_raw,
            src_xmp,
            action,
            bytes,
        });
    }

    if exists_mode == ExistsMode::Abort && existing_hits > 0 {
        return Err(PlanError::DestExists(existing_hits));
    }

    let total_bytes: u64 = jobs.iter().map(|j| j.bytes).sum();
    // Free space is advisory-honest: an unreadable statvfs yields None
    // ("free space unknown" in the dialog), never a fake huge number
    // (gate finding). The check is repeated by the app right before
    // execute (plan-to-start staleness), and per-file ENOSPC failures
    // remain isolated regardless.
    let free_bytes = fs2::available_space(existing_ancestor(dest)).ok();
    if let Some(free) = free_bytes {
        if total_bytes > free {
            return Err(PlanError::InsufficientSpace {
                needed: total_bytes,
                free,
            });
        }
    }

    Ok(CopyPlan {
        jobs,
        total_bytes,
        free_bytes,
        renamed,
        skipped,
        refreshed,
        recopied,
    })
}

/// The skip-branch verdict for a destination RAW that stays: Skip — unless
/// the source sidecar is newer than the one next to that RAW (or there is
/// none there): then the sidecar alone is refreshed.
fn skip_or_refresh(dst_raw: &Path, src_xmp_path: &Path, src_xmp_exists: bool) -> PlanAction {
    let dst_xmp = sidecar_path(dst_raw);
    let refresh = src_xmp_exists && (!dst_xmp.exists() || mtime(src_xmp_path) > mtime(&dst_xmp));
    if refresh {
        PlanAction::SidecarRefresh
    } else {
        PlanAction::Skip
    }
}

fn mtime(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Canonicalize a path that may not exist yet (the destination is often
/// created BY the copy): canonicalize the deepest existing ancestor and
/// re-append the not-yet-existing remainder, so `src/selects` still
/// reads as INSIDE `src` before it is created.
fn canonicalize_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let anchor = existing_ancestor(p);
    let canon_anchor = anchor.canonicalize().unwrap_or_else(|_| anchor.into());
    match p.strip_prefix(anchor) {
        Ok(rest) => canon_anchor.join(rest),
        Err(_) => canon_anchor,
    }
}

/// Deepest ancestor that exists (free-space checks on a to-be-created dir).
fn existing_ancestor(p: &Path) -> &Path {
    let mut cur = p;
    while !cur.exists() {
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent,
            _ => break,
        }
    }
    cur
}

/// `DSC01234.ARW` + 2 → `DSC01234_2.ARW` (suffix BEFORE the extension;
/// the sidecar follows in lockstep via `sidecar_path` on the result).
fn suffixed(name: &str, k: usize) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}_{k}.{ext}"),
        _ => format!("{name}_{k}"),
    }
}

/// Is `landed` what [`suffixed`] makes of `natural` for some `k` — the
/// shape that records a collision at copy time (`a_2.ARW` of `a.ARW`)?
/// Exact parity with `suffixed`: `k >= 2`, no leading zero. The shape is
/// evidence, not provenance: a `{filename}_{seq}` rename template can
/// produce the same name for the session's own copy, in which case the
/// worst outcome is one extra verified `_2` copy with an honest note —
/// never a touched foreign file (accepted, gate round 3; carry the
/// CopyRenamed fact with the landed path if cross-session memory is ever
/// added).
fn is_collision_suffix_of(landed: &str, natural: &str) -> bool {
    fn split(name: &str) -> (&str, Option<&str>) {
        match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
            _ => (name, None),
        }
    }
    let (land_stem, land_ext) = split(landed);
    let (nat_stem, nat_ext) = split(natural);
    land_ext == nat_ext
        && land_stem
            .strip_prefix(nat_stem)
            .and_then(|rest| rest.strip_prefix('_'))
            .is_some_and(|k| {
                k.bytes().all(|b| b.is_ascii_digit())
                    && !k.starts_with('0')
                    && k.parse::<usize>().is_ok_and(|k| k >= 2)
            })
}

// ------------------------------------------------------------------ execute

#[derive(Debug)]
pub enum CopyEvent {
    /// Emitted before each file (1-based index over plan jobs).
    File {
        index: usize,
        total: usize,
        name: String,
    },
    Failed {
        id: usize,
        name: String,
        reason: String,
    },
    Finished(CopyReport),
}

#[derive(Debug, Default, Clone)]
pub struct CopyReport {
    pub copied: usize,
    pub skipped: usize,
    pub refreshed: usize,
    pub failed: Vec<(String, String)>, // (name, reason)
    /// True iff every copied byte was BLAKE3-verified against the source
    /// stream — the "green light to format the card" sentence.
    pub all_verified: bool,
    pub cancelled: bool,
    /// Every id that finished a full RAW copy, with the path it landed at
    /// — what the session records (`SessionCopies::record`) for the
    /// copied badge and the re-run skip default.
    pub landed: Vec<(usize, PathBuf)>,
}

pub struct CopyHandle {
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CopyHandle {
    /// Between-files cancellation (fileops.md): already-copied files stay.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for CopyHandle {
    fn drop(&mut self) {
        // Cancel-then-join (gate finding: quit / Open Folder mid-copy
        // previously joined WITHOUT cancelling — an unbounded, invisible
        // block on a big card). Cancel bounds the wait to the file in
        // flight, and the temp-name+rename contract guarantees no partial
        // is left behind.
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

/// Run the plan on a worker thread. The caller flushed sidecars already
/// (barrier — see module docs).
pub fn execute(plan: CopyPlan) -> (CopyHandle, Receiver<CopyEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let handle = std::thread::Builder::new()
        .name("copy-picks".into())
        .spawn(move || run_plan(plan, &tx, &flag))
        .expect("spawn copy worker");
    (
        CopyHandle {
            cancel,
            handle: Some(handle),
        },
        rx,
    )
}

fn run_plan(plan: CopyPlan, tx: &Sender<CopyEvent>, cancel: &AtomicBool) {
    let total = plan.jobs.len();
    let mut report = CopyReport {
        skipped: plan.skipped,
        all_verified: true,
        ..Default::default()
    };
    for (i, job) in plan.jobs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let name = job
            .dst_raw
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        tx.send(CopyEvent::File {
            index: i + 1,
            total,
            name: name.clone(),
        })
        .ok();
        let result = match job.action {
            PlanAction::Skip => Ok(()),
            PlanAction::SidecarRefresh => refresh_sidecar(job).map(|()| {
                report.refreshed += 1;
            }),
            PlanAction::Copy | PlanAction::CopyRenamed => copy_pair(job).map(|()| {
                report.copied += 1;
                report.landed.push((job.id, job.dst_raw.clone()));
            }),
        };
        if let Err(e) = result {
            report.all_verified = false;
            report.failed.push((name.clone(), e.to_string()));
            tx.send(CopyEvent::Failed {
                id: job.id,
                name,
                reason: e.to_string(),
            })
            .ok();
        }
    }
    tx.send(CopyEvent::Finished(report)).ok();
}

fn refresh_sidecar(job: &CopyJob) -> std::io::Result<()> {
    if let Some(src) = &job.src_xmp {
        copy_verified(src, &job.dst_xmp)?;
    }
    Ok(())
}

fn copy_pair(job: &CopyJob) -> std::io::Result<()> {
    copy_verified(&job.src_raw, &job.dst_raw)?;
    if let Some(src) = &job.src_xmp {
        copy_verified(src, &job.dst_xmp)?;
    }
    Ok(())
}

/// Copy with streaming BLAKE3: hash the source WHILE writing a temp file,
/// fsync, RE-READ the destination and compare hashes, then rename into
/// place — a failure never leaves a partial file under the final name.
fn copy_verified(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_verified_with(src, dst, |_| {})
}

/// `tamper` runs on the TEMP file between fsync and the verify re-read —
/// a test seam so the mismatch branch is actually driven (gate finding:
/// the old corruption test asserted hash inequality of two buffers,
/// which is vacuously true and exercised nothing).
fn copy_verified_with(src: &Path, dst: &Path, tamper: impl FnOnce(&Path)) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp_os = dst.as_os_str().to_owned();
    tmp_os.push(format!(".fastcull-partial-{}", std::process::id()));
    let tmp = PathBuf::from(tmp_os);
    let result = (|| -> std::io::Result<()> {
        let mut reader = std::fs::File::open(src)?;
        let mut writer = std::fs::File::create(&tmp)?;
        let mut src_hash = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            src_hash.update(&buf[..n]);
            writer.write_all(&buf[..n])?;
        }
        writer.sync_all()?;
        drop(writer);
        tamper(&tmp);
        // Verification pass: what the disk gives BACK must match what the
        // source stream said (fileops.md: a perfect-looking thumbnail
        // proves nothing — the embedded JPEG sits at the file's front).
        let mut verify = std::fs::File::open(&tmp)?;
        let mut dst_hash = blake3::Hasher::new();
        loop {
            let n = verify.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst_hash.update(&buf[..n]);
        }
        if src_hash.finalize() != dst_hash.finalize() {
            return Err(std::io::Error::other("BLAKE3 mismatch after copy"));
        }
        std::fs::rename(&tmp, dst)?;
        Ok(())
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::PickState;

    fn tmp() -> PathBuf {
        crate::testutil::scratch_dir("fops")
    }

    fn src_with(dir: &Path, names: &[(&str, &[u8])]) -> Vec<PlanSource> {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        names
            .iter()
            .enumerate()
            .map(|(i, (n, bytes))| {
                let p = src.join(n);
                std::fs::write(&p, bytes).unwrap();
                PlanSource {
                    id: i,
                    path: p,
                    size: bytes.len() as u64,
                    ctx: ExpandContext {
                        date: "2026-07-26".into(),
                        time: "210000".into(),
                        filename_stem: n.rsplit_once('.').map(|(s, _)| s).unwrap_or(n).into(),
                        camera: "ILCE-1".into(),
                        ext_upper: "ARW".into(),
                    },
                }
            })
            .collect()
    }

    fn drain(rx: Receiver<CopyEvent>) -> CopyReport {
        loop {
            match rx.recv().expect("event") {
                CopyEvent::Finished(r) => return r,
                _ => continue,
            }
        }
    }

    #[test]
    fn plan_templates_seq_and_rejects_collisions() {
        let dir = tmp();
        let sources = src_with(&dir, &[("b.ARW", b"bb"), ("a.ARW", b"aa")]);
        let dest = dir.join("out");
        // {seq} follows INPUT order (session sort), zero-padded to width.
        let plan = plan(
            &sources,
            &dest,
            Some("{date}_{seq}.{ext}"),
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(
            plan.jobs[0].dst_raw.file_name().unwrap(),
            "2026-07-26_1.ARW"
        );
        assert_eq!(
            plan.jobs[1].dst_raw.file_name().unwrap(),
            "2026-07-26_2.ARW"
        );
        assert_eq!(
            plan.jobs[1].dst_xmp.file_name().unwrap(),
            "2026-07-26_2.ARW.xmp",
            "sidecar lockstep on the RENAMED name"
        );
        // A constant template collides across images: hard error.
        let err = super::plan(
            &sources,
            &dest,
            Some("same.{ext}"),
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TemplateCollision { .. }));
        // Unknown variable propagates the template error.
        let err = super::plan(
            &sources,
            &dest,
            Some("{bogus}"),
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::Template(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plan_rejects_dest_inside_or_equal_to_source() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aa")]);
        let src_dir = sources[0].path.parent().unwrap().to_path_buf();
        assert!(matches!(
            super::plan(
                &sources,
                &src_dir,
                None,
                ExistsMode::Rename,
                &SessionCopies::default()
            ),
            Err(PlanError::DestEqualsSource)
        ));
        assert!(matches!(
            super::plan(
                &sources,
                &src_dir.join("selects"),
                None,
                ExistsMode::Rename,
                &SessionCopies::default()
            ),
            Err(PlanError::DestInsideSource)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exists_modes_rename_skip_abort_and_session_skip() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"old").unwrap();

        // Rename: suffix before the extension, sidecar in lockstep.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_2.ARW");
        assert_eq!(p.jobs[0].dst_xmp.file_name().unwrap(), "a_2.ARW.xmp");
        assert_eq!((p.renamed, p.skipped), (1, 0));
        assert_eq!(p.jobs[1].action, PlanAction::Copy);

        // Skip: no bytes for the existing one.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Skip,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
        assert_eq!(p.total_bytes, 2, "only b.ARW's bytes");

        // Overwrite: the existing destination is replaced in place.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Overwrite,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Copy);
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a.ARW");
        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(report.copied, 2);
        assert_eq!(
            std::fs::read(dest.join("a.ARW")).unwrap(),
            b"aaaa",
            "overwrite replaced the old bytes"
        );
        // Restore the collision fixture for the modes below.
        std::fs::write(dest.join("a.ARW"), b"old").unwrap();
        std::fs::remove_file(dest.join("b.ARW")).ok();

        // Abort: hard error naming the count.
        assert!(matches!(
            super::plan(
                &sources,
                &dest,
                None,
                ExistsMode::Abort,
                &SessionCopies::default()
            ),
            Err(PlanError::DestExists(1))
        ));

        // Session re-run trap (persona): a copy that landed this session
        // and is still there forces Skip even in Rename mode — never a
        // duplicate suffix.
        let mut copied = SessionCopies::default();
        copied.record(0, dest.join("a.ARW"));
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &copied).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a.ARW"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The 2026-08-21 bug: copy, delete the copies by hand (RAW + XMP),
    /// Ctrl+E again into the same folder — the old id-only "already
    /// copied" set forced a Skip over an empty folder, so the sidecar came
    /// back as a SidecarRefresh and the RAW never did. RED on the old
    /// code at the first assert (SidecarRefresh ≠ Copy).
    #[test]
    fn rerun_recopies_a_destination_the_user_deleted_by_hand() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        // `a` has a source sidecar (every real pick does), `b` has none:
        // the two symptom variants ("the xmp comes back" / "nothing").
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        let (_h, rx) = execute(p);
        let mut session = SessionCopies::default();
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        assert!(session.is_copied(0) && session.is_copied(1));
        for name in ["a.ARW", "a.ARW.xmp", "b.ARW"] {
            std::fs::remove_file(dest.join(name)).unwrap();
        }

        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(
            p.jobs[0].action,
            PlanAction::Copy,
            "a deleted RAW with a source sidecar must be copied again, not SidecarRefresh"
        );
        assert_eq!(
            p.jobs[1].action,
            PlanAction::Copy,
            "a deleted RAW without a sidecar must be copied again, not Skip"
        );
        assert_eq!(
            (p.total_bytes, p.skipped, p.refreshed, p.recopied),
            (6, 0, 0, 2),
            "the plan counts the bytes and reports the gone copies"
        );
        // The Skip-existing toggle is about collisions; it must not resurrect
        // the forced skip (the user flipped it in the real app: no effect).
        let p_skip = super::plan(&sources, &dest, None, ExistsMode::Skip, &session).unwrap();
        assert_eq!(p_skip.jobs[0].action, PlanAction::Copy);
        assert_eq!(p_skip.jobs[1].action, PlanAction::Copy);
        // The badge follows the disk once asked.
        session.refresh();
        assert!(!session.is_copied(0) && !session.is_copied(1));

        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(report.copied, 2);
        for (id, path) in report.landed {
            session.record(id, path);
        }
        assert!(dest.join("a.ARW").exists() && dest.join("a.ARW.xmp").exists());
        assert!(dest.join("b.ARW").exists());
        assert!(session.is_copied(0) && session.is_copied(1));
        // The re-run trap itself survives: copies present → skipped.
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
        assert_eq!(p.jobs[1].action, PlanAction::Skip);
        assert_eq!((p.skipped, p.recopied), (2, 0));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only the RAW deleted, the XMP left behind: the pair ships again
    /// together (an XMP next to no RAW is junk). The old code saw a dest
    /// sidecar newer than the source and answered plain Skip.
    #[test]
    fn rerun_ships_the_pair_when_only_the_raw_was_deleted() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        let (_h, rx) = execute(p);
        let mut session = SessionCopies::default();
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        std::fs::remove_file(dest.join("a.ARW")).unwrap();
        assert!(dest.join("a.ARW.xmp").exists(), "fixture: the XMP stays");
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Copy);
        assert_eq!((p.total_bytes, p.recopied), (4, 1));
        let (_h, rx) = execute(p);
        assert_eq!(drain(rx).copied, 1);
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"aaaa");
        assert_eq!(
            std::fs::read(dest.join("a.ARW.xmp")).unwrap(),
            std::fs::read(sidecar_path(&sources[0].path)).unwrap(),
            "the sidecar shipped with its RAW"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Issue #14: a copy that landed under a collision suffix is judged
    /// under THAT name on re-run — skipped as `_2`, its sidecar refreshed
    /// as `_2.xmp`, never the foreign `a.ARW.xmp` beside it — and when the
    /// `_2` pair is deleted by hand it lands as `_2` again.
    #[test]
    fn session_skip_follows_the_landed_name_not_the_natural_one() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"mine")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        // The other body's file of the same name, with its own sidecar.
        std::fs::write(dest.join("a.ARW"), b"foreign").unwrap();
        std::fs::write(dest.join("a.ARW.xmp"), b"<foreign/>").unwrap();

        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        let (_h, rx) = execute(p);
        let mut session = SessionCopies::default();
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        assert!(dest.join("a_2.ARW").exists() && dest.join("a_2.ARW.xmp").exists());

        // Re-run, nothing changed: skipped under the landed name.
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a_2.ARW"));
        assert_eq!(p.jobs[0].dst_xmp, dest.join("a_2.ARW.xmp"));

        // Source sidecar touched after the copy: the refresh targets
        // `a_2.ARW.xmp`, and the foreign sidecar is left alone.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(sidecar_path(&sources[0].path))
            .unwrap()
            .set_modified(later)
            .unwrap();
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::SidecarRefresh);
        assert_eq!(p.jobs[0].dst_xmp, dest.join("a_2.ARW.xmp"));
        let (_h, rx) = execute(p);
        assert_eq!(drain(rx).refreshed, 1);
        assert_eq!(
            std::fs::read(dest.join("a.ARW.xmp")).unwrap(),
            b"<foreign/>",
            "the foreign file's sidecar must never receive our refresh"
        );

        // The `_2` pair deleted by hand: out again as `_2` (natural taken).
        std::fs::remove_file(dest.join("a_2.ARW")).unwrap();
        std::fs::remove_file(dest.join("a_2.ARW.xmp")).unwrap();
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a_2.ARW"));
        assert_eq!(p.recopied, 1);
        // …and with Skip-existing on, too: the session knows the natural
        // name is a foreign file (that is why the copy took a suffix), so
        // the pick goes out as `_2` again and the foreign sidecar is never
        // touched (gate finding: the old branch refreshed it).
        let p = super::plan(&sources, &dest, None, ExistsMode::Skip, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a_2.ARW"));
        assert_eq!(p.recopied, 1);
        let (_h, rx) = execute(p);
        assert_eq!(drain(rx).copied, 1);
        assert!(dest.join("a_2.ARW").exists() && dest.join("a_2.ARW.xmp").exists());
        assert_eq!(
            std::fs::read(dest.join("a.ARW.xmp")).unwrap(),
            b"<foreign/>",
            "Skip-existing must not refresh the foreign file's sidecar either"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collision_suffix_shape_is_recognized() {
        for (landed, natural, expect) in [
            ("a_2.ARW", "a.ARW", true),
            ("a_10.ARW", "a.ARW", true),
            ("a.b_2.ARW", "a.b.ARW", true),
            ("a_2", "a", true),
            ("a_x.ARW", "a.ARW", false),
            ("a.ARW", "a.ARW", false),
            ("a_2.NEF", "a.ARW", false),
            ("b_2.ARW", "a.ARW", false),
            ("a_2.ARW", "a", false),
            ("2026_001.ARW", "a.ARW", false),
            ("a_1.ARW", "a.ARW", false),
            ("a_0.ARW", "a.ARW", false),
            ("a_02.ARW", "a.ARW", false),
            ("DSC_001.ARW", "DSC.ARW", false),
            ("DSC_001_2.ARW", "DSC_001.ARW", true),
        ] {
            assert_eq!(
                super::is_collision_suffix_of(landed, natural),
                expect,
                "{landed} vs {natural}"
            );
        }
    }

    /// Gate finding (round 2): a landed name that differs from the natural
    /// one only because the TEMPLATE changed is no evidence that the file
    /// now under the natural name is foreign — Skip-existing keeps meaning
    /// skip there; Rename keeps renaming.
    #[test]
    fn skip_existing_is_honored_when_the_only_evidence_is_a_template_change() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        let mut session = SessionCopies::default();
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        let (_h, rx) = execute(p);
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        std::fs::remove_file(dest.join("a.ARW")).unwrap();
        // A file of unknown origin sits under the templated name.
        std::fs::write(dest.join("a_x.ARW"), b"whose?").unwrap();
        let template = Some("{filename}_x.{ext}");
        let p = super::plan(&sources, &dest, template, ExistsMode::Skip, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip, "{:?}", p.jobs[0]);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a_x.ARW"));
        assert_eq!((p.skipped, p.recopied), (1, 0));
        let p = super::plan(&sources, &dest, template, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a_x_2.ARW"));
        assert_eq!(p.recopied, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Gate finding: recording through another spelling of the same folder
    /// supersedes the entry it matched — otherwise the stale path later
    /// reads as "gone" and the same image goes out a second time.
    #[test]
    fn record_supersedes_the_entry_of_a_re_spelled_folder() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        let mut session = SessionCopies::default();
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        let (_h, rx) = execute(p);
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        std::fs::remove_file(dest.join("a.ARW")).unwrap();
        // Copied again through `out/../out`, renamed by a template.
        let respelled = dir.join("out").join("..").join("out");
        let p = super::plan(
            &sources,
            &respelled,
            Some("{filename}_x.{ext}"),
            ExistsMode::Rename,
            &session,
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Copy);
        let (_h, rx) = execute(p);
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        // Back under the plain spelling: the live `_x` copy IS the record —
        // skipped, not a second copy under the old name.
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip, "{:?}", p.jobs[0]);
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_x.ARW");
        assert_eq!(p.recopied, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Persona gap B: copying to A, then to B, then back to A must still
    /// skip A's copies — the record is per destination, not per image.
    /// The same folder spelled differently still matches (canonical).
    #[test]
    fn session_copies_are_remembered_per_destination() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let (dest_a, dest_b) = (dir.join("out-a"), dir.join("out-b"));
        let mut session = SessionCopies::default();
        for dest in [&dest_a, &dest_b] {
            let p = super::plan(&sources, dest, None, ExistsMode::Rename, &session).unwrap();
            assert_eq!(
                p.jobs[0].action,
                PlanAction::Copy,
                "{dest:?} is a new destination"
            );
            let (_h, rx) = execute(p);
            for (id, path) in drain(rx).landed {
                session.record(id, path);
            }
        }
        // Back to A via a different spelling of the same folder.
        let dest_a_again = dir.join("out-b").join("..").join("out-a");
        let p = super::plan(&sources, &dest_a_again, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(
            p.jobs[0].action,
            PlanAction::Skip,
            "A's copy is still remembered"
        );
        let p = super::plan(&sources, &dest_b, None, ExistsMode::Rename, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip, "and so is B's");
        // The badge means "a copy is there, somewhere".
        std::fs::remove_file(dest_a.join("a.ARW")).unwrap();
        session.refresh();
        assert!(session.is_copied(0), "B's copy still stands");
        std::fs::remove_file(dest_b.join("a.ARW")).unwrap();
        session.refresh();
        assert!(!session.is_copied(0));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A template set AFTER a plain copy: the landed copy is skipped under
    /// its old name, and no orphan sidecar appears under the templated
    /// name (the old code refreshed `<templated>.ARW.xmp` next to nothing).
    #[test]
    fn rerun_with_a_new_template_keeps_the_landed_copy_without_orphans() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        let (_h, rx) = execute(p);
        let mut session = SessionCopies::default();
        for (id, path) in drain(rx).landed {
            session.record(id, path);
        }
        let p = super::plan(
            &sources,
            &dest,
            Some("{filename}_x.{ext}"),
            ExistsMode::Rename,
            &session,
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a.ARW"));
        let (_h, rx) = execute(p);
        assert_eq!(drain(rx).copied, 0);
        let names: Vec<String> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.contains("_x")),
            "no file under the templated name: {names:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skip_refreshes_changed_sidecar_only() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"prior").unwrap();
        // Source sidecar exists, destination has none -> refresh.
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Skip,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::SidecarRefresh);
        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(report.refreshed, 1);
        assert!(dest.join("a.ARW.xmp").exists());
        assert_eq!(
            std::fs::read(dest.join("a.ARW")).unwrap(),
            b"prior",
            "the RAW itself is untouched by a refresh"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn execute_copies_verifies_and_isolates_failures() {
        let dir = tmp();
        let sources = src_with(
            &dir,
            &[("good.ARW", &[7u8; 100_000]), ("bad.ARW", b"unreadable")],
        );
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sources[1].path, std::fs::Permissions::from_mode(0o000))
                .unwrap();
        }
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        let (_h, rx) = execute(p);
        let report = drain(rx);
        // The unreadable-source injection is chmod-based and only exists
        // on unix; on Windows both files legitimately copy (CI caught the
        // unconditional assertion counting 2).
        #[cfg(unix)]
        {
            assert_eq!(report.copied, 1);
            assert_eq!(report.landed, vec![(0, dest.join("good.ARW"))]);
            assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
            assert!(!report.all_verified);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(report.copied, 2);
            assert!(report.failed.is_empty());
        }
        assert_eq!(
            std::fs::read(dest.join("good.ARW")).unwrap(),
            vec![7u8; 100_000]
        );
        assert!(dest.join("good.ARW.xmp").exists(), "sidecar landed");
        // No partial files left under any name.
        let leftovers: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("partial"))
            .collect();
        assert!(leftovers.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sources[1].path, std::fs::Permissions::from_mode(0o644)).ok();
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copy_verified_detects_corruption_and_cleans_up() {
        let dir = tmp();
        let src = dir.join("s.bin");
        std::fs::write(&src, [3u8; 4096]).unwrap();
        // Happy path round-trips.
        copy_verified(&src, &dir.join("d.bin")).unwrap();
        assert_eq!(std::fs::read(dir.join("d.bin")).unwrap(), [3u8; 4096]);
        // FAULT INJECTION (gate finding: the old assertion was vacuous):
        // corrupt the temp file between fsync and the verify re-read —
        // the mismatch branch must fire, clean up the temp, and leave
        // NOTHING under the final name.
        let dst = dir.join("d2.bin");
        let err = copy_verified_with(&src, &dst, |tmp| {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().append(true).open(tmp).unwrap();
            f.write_all(b"CORRUPT").unwrap();
            f.sync_all().unwrap();
        })
        .unwrap_err();
        assert!(err.to_string().contains("BLAKE3 mismatch"), "{err}");
        assert!(!dst.exists(), "no file under the final name");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("partial"))
            .collect();
        assert!(leftovers.is_empty(), "temp cleaned after mismatch");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// fileops.md sidecar barrier: a pick made moments before "copy" must
    /// be in the COPIED sidecar — flush() then plan/execute, and the
    /// destination sidecar carries the fresh mark.
    #[test]
    fn sidecar_barrier_fresh_pick_lands_in_the_copy() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"payload")]);
        let (writer, _errs) = crate::sidecar_writer::SidecarWriter::start();
        writer.mark(sources[0].path.clone(), PickState::Picked);
        writer.flush(); // THE barrier the app runs before execute
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        assert!(
            p.jobs[0].src_xmp.is_some(),
            "flushed sidecar visible to plan"
        );
        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(report.copied, 1);
        let copied = crate::xmp::read_sidecar(&dest.join("a.ARW.xmp")).unwrap();
        assert_eq!(copied.pick, PickState::Picked, "fresh pick in the copy");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancel_between_files_keeps_finished_copies() {
        let dir = tmp();
        let big = vec![9u8; 3_000_000];
        let sources = src_with(
            &dir,
            &[
                ("a.ARW", big.as_slice()),
                ("b.ARW", big.as_slice()),
                ("c.ARW", big.as_slice()),
            ],
        );
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Rename,
            &SessionCopies::default(),
        )
        .unwrap();
        let (h, rx) = execute(p);
        h.cancel(); // set before/while the worker runs: between-files check
        let report = drain(rx);
        assert!(report.cancelled || report.copied == 3, "cancel raced fine");
        // Whatever finished is complete and verified — nothing partial.
        for j in ["a.ARW", "b.ARW", "c.ARW"] {
            let p = dest.join(j);
            if p.exists() {
                assert_eq!(std::fs::metadata(&p).unwrap().len(), 3_000_000);
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
