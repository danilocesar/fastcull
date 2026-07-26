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
    /// Destination exists (or copied earlier this session): nothing moves.
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
/// `already_copied` = image ids copied to THIS destination earlier in the
/// session (persona re-run trap: they default to Skip, never suffixing).
pub fn plan(
    sources: &[PlanSource],
    dest: &Path,
    template: Option<&str>,
    exists_mode: ExistsMode,
    already_copied: &HashSet<usize>,
) -> Result<CopyPlan, PlanError> {
    // Dest-inside-source / equality (canonicalized where possible; a not-
    // yet-created destination canonicalizes its existing ancestors).
    if let Some(src_dir) = sources.first().and_then(|s| s.path.parent()) {
        let src_canon = src_dir.canonicalize().unwrap_or_else(|_| src_dir.into());
        let dest_canon = canonicalize_lenient(dest);
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
    let (mut renamed, mut skipped, mut refreshed) = (0usize, 0usize, 0usize);
    for (s, name) in sources.iter().zip(&names) {
        let src_xmp_path = sidecar_path(&s.path);
        let src_xmp = src_xmp_path.exists().then_some(src_xmp_path.clone());
        let natural = dest.join(name);
        let exists = natural.exists();
        let forced_skip = already_copied.contains(&s.id);
        let (dst_raw, action) = if forced_skip || (exists && exists_mode == ExistsMode::Skip) {
            if exists {
                existing_hits += 1;
            }
            // Skip — unless the source sidecar is newer than the copied
            // one (or the copy has none): refresh the sidecar alone.
            let dst_xmp = sidecar_path(&natural);
            let refresh =
                src_xmp.is_some() && (!dst_xmp.exists() || mtime(&src_xmp_path) > mtime(&dst_xmp));
            if refresh {
                refreshed += 1;
                (natural, PlanAction::SidecarRefresh)
            } else {
                skipped += 1;
                (natural, PlanAction::Skip)
            }
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
    })
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
    /// Ids that finished a full RAW copy (session "copied" badges +
    /// the re-run skip default).
    pub copied_ids: Vec<usize>,
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
                report.copied_ids.push(job.id);
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
        let dir = std::env::temp_dir().join(format!(
            "fastcull-fops-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
            &HashSet::new(),
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
            &HashSet::new(),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TemplateCollision { .. }));
        // Unknown variable propagates the template error.
        let err = super::plan(
            &sources,
            &dest,
            Some("{bogus}"),
            ExistsMode::Rename,
            &HashSet::new(),
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
                &HashSet::new()
            ),
            Err(PlanError::DestEqualsSource)
        ));
        assert!(matches!(
            super::plan(
                &sources,
                &src_dir.join("selects"),
                None,
                ExistsMode::Rename,
                &HashSet::new()
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
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &HashSet::new()).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_2.ARW");
        assert_eq!(p.jobs[0].dst_xmp.file_name().unwrap(), "a_2.ARW.xmp");
        assert_eq!((p.renamed, p.skipped), (1, 0));
        assert_eq!(p.jobs[1].action, PlanAction::Copy);

        // Skip: no bytes for the existing one.
        let p = super::plan(&sources, &dest, None, ExistsMode::Skip, &HashSet::new()).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
        assert_eq!(p.total_bytes, 2, "only b.ARW's bytes");

        // Overwrite: the existing destination is replaced in place.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ExistsMode::Overwrite,
            &HashSet::new(),
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
            super::plan(&sources, &dest, None, ExistsMode::Abort, &HashSet::new()),
            Err(PlanError::DestExists(1))
        ));

        // Session re-run trap (persona): already-copied forces Skip even
        // in Rename mode — never a duplicate suffix.
        let copied: HashSet<usize> = [0].into();
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &copied).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Skip);
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
        let p = super::plan(&sources, &dest, None, ExistsMode::Skip, &HashSet::new()).unwrap();
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
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &HashSet::new()).unwrap();
        let (_h, rx) = execute(p);
        let report = drain(rx);
        // The unreadable-source injection is chmod-based and only exists
        // on unix; on Windows both files legitimately copy (CI caught the
        // unconditional assertion counting 2).
        #[cfg(unix)]
        {
            assert_eq!(report.copied, 1);
            assert_eq!(report.copied_ids, vec![0]);
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
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &HashSet::new()).unwrap();
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
        let p = super::plan(&sources, &dest, None, ExistsMode::Rename, &HashSet::new()).unwrap();
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
