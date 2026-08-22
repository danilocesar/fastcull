//! Copy-picks engine (specs/modules/fileops.md): two-phase — a pure,
//! inspectable PLAN, then an EXECUTE on a worker thread with streaming
//! BLAKE3 verification. Originals are never touched (copy, not move).
//!
//! The FLUSH BARRIER is the caller's duty: the app must call
//! `SidecarWriter::flush()` (after committing any in-progress panel edit)
//! BEFORE `execute` — a pick or caption made a moment ago must be in the
//! copied sidecar.
//!
//! COLLISION HANDLING v2, "the clash question" (fileops.md): the disk
//! decides, never the session's memory. A plan is built with a
//! [`ClashPolicy`]; the default [`ClashPolicy::Ask`] only MARKS the names
//! that are already occupied at the destination (`PlanAction::Clash`) and
//! refuses to run, so the app can ask its one question. The answer —
//! overwrite everything, create copies, or cancel — is a policy for the
//! whole run, and the plan is rebuilt with it.

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::iptc::{expand, ExpandContext, IptcError};
use crate::xmp::sidecar_path;

/// What to do with destination names that are already occupied — the
/// user's answer to the clash question (fileops.md), as a policy for the
/// whole run rather than a per-file list.
///
/// There is no "skip the clashing files" answer: v1's four-way
/// `ExistsMode` (rename / skip / overwrite / abort) and its forced
/// session-skip are gone. Cancel is not a policy either — it is the app
/// not executing anything at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClashPolicy {
    /// Build the plan, MARK every clashing image (`PlanAction::Clash`) and
    /// count it, but resolve nothing: this is the plan the dialog previews
    /// and the one whose clash count raises the question. [`execute`]
    /// refuses to run it (fileops.md: a plan frozen before the question is
    /// never executed).
    Ask,
    /// "Overwrite everything": clashing images are written in place. A
    /// destination RAW that is already byte-identical to the source is NOT
    /// re-transferred — only its sidecar is rewritten, and only if it
    /// differs.
    Overwrite,
    /// "Create copies": clashing images land under the first free numeric
    /// suffix, from `_1`, RAW and sidecar moving as a pair.
    CreateCopies,
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
    /// The RAW's size — what this job writes, except for a `Replace` whose
    /// destination turns out to be byte-identical (decided at copy time,
    /// by hash) and for an unanswered `Clash`.
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanAction {
    /// Copy RAW + sidecar to a destination pair that is FREE.
    Copy,
    /// As Copy, but the pair got a collision suffix (`_1`, `_2`, …) under
    /// [`ClashPolicy::CreateCopies`].
    CopyRenamed,
    /// The destination pair is occupied and the user answered "overwrite
    /// everything": this job MAY replace what is there — the only action
    /// allowed to (fileops.md rule 4).
    Replace,
    /// The destination pair is occupied and nothing has been decided yet
    /// ([`ClashPolicy::Ask`]). Never executed: it is the question.
    Clash,
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
    /// Bytes execute will write under THIS policy (RAWs of every job that
    /// is not an unanswered `Clash`). Under `Overwrite` this is the worst
    /// case: a byte-identical destination costs a read, not a write, but
    /// only the copy itself can know that.
    pub total_bytes: u64,
    /// [`ClashPolicy::Ask`] only: the RAW bytes of the CLASHING images,
    /// kept apart from `total_bytes` so the dialog can state the worst
    /// case ("everything") and the cost of each answer.
    pub clash_bytes: u64,
    /// None = statvfs failed ("free space unknown"), check skipped.
    pub free_bytes: Option<u64>,
    /// Images whose destination pair is occupied (fileops.md: the RAW name
    /// or its sidecar name, on disk or claimed by this plan).
    pub clashes: usize,
    /// Of those, the ones where ONLY the sidecar name is taken — a stray
    /// `.xmp` with no RAW beside it. Deliberately NOT shown in the
    /// question (persona 2026-08-21: it does not change the answer and
    /// costs a line at the moment of deciding); it is here because the
    /// plan should be able to describe itself honestly, and the tests
    /// assert on it.
    pub sidecar_only_clashes: usize,
    /// Jobs that took a `_k` suffix (`CreateCopies`).
    pub renamed: usize,
    /// Copied earlier this session but GONE from the destination when the
    /// plan looked (the user deleted the copy by hand). The dialog's amber
    /// note; it decides nothing.
    pub recopied: usize,
}

/// What this session copied WHERE: image id → the RAW path(s) it landed
/// at, one per destination folder.
///
/// READS ONLY (fileops.md, "Session memory reads, never decides"): the
/// grid's ✓ copied badge and the dialog's "N copied earlier but gone from
/// the destination — copying again" note. It has no say in what is
/// copied — that memory is exactly what caused the 2026-08-21 bug (a
/// forced skip over a folder the user had emptied by hand), and the clash
/// question replaced it with a question about what is actually on disk.
///
/// A copy counts only while it is still on disk: `refresh` re-stats every
/// remembered path, and `record` supersedes the entry of the same
/// (canonical) destination folder when a copy lands there again.
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
        // matched, or the stale path later reads as "gone" and the note
        // claims a re-copy that already happened).
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
/// filesystem (existence, free space) but changes nothing.
///
/// `policy` is the user's answer to the clash question, or
/// [`ClashPolicy::Ask`] before it has been asked. `session` supplies the
/// "copied earlier but gone" note ONLY: it never decides what is copied.
pub fn plan(
    sources: &[PlanSource],
    dest: &Path,
    template: Option<&str>,
    policy: ClashPolicy,
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
    // apply) and detect in-plan collisions before touching clashes.
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

    // Phase 2: the clash check, on the FINAL (template-expanded) names.
    // `{seq}` was assigned above and is never re-flowed by a suffix — the
    // `_k` rides on top of the whole templated name (fileops.md).
    let mut jobs = Vec::with_capacity(n);
    // Every name this plan claims — RAW *and* sidecar. The pair is the
    // unit: a name pair clashes when EITHER member is taken, and a suffix
    // is free only when BOTH are, so a copy is never split across two
    // numbers and a RAW never lands beside a sidecar it does not own.
    let mut taken: HashSet<String> = HashSet::new();
    let (mut clashes, mut sidecar_only, mut renamed, mut recopied) =
        (0usize, 0usize, 0usize, 0usize);
    let (mut total_bytes, mut clash_bytes, mut clash_free_bytes) = (0u64, 0u64, 0u64);
    // Folder → "is it `dest`?", compared canonically (the same destination
    // reached via another spelling keeps its note) and memoized per
    // folder: a 2,000-pick plan canonicalizes once, not 2,000 times.
    let mut is_dest: HashMap<PathBuf, bool> = HashMap::new();
    for (s, name) in sources.iter().zip(&names) {
        let src_xmp_path = sidecar_path(&s.path);
        let src_xmp = src_xmp_path.exists().then_some(src_xmp_path);
        let natural = dest.join(name);
        let xmp_name = xmp_name_of(name);
        let raw_taken = occupied(&natural) || taken.contains(name);
        let xmp_taken = occupied(&dest.join(&xmp_name)) || taken.contains(&xmp_name);
        let clash = raw_taken || xmp_taken;
        if clash {
            clashes += 1;
            if !raw_taken {
                sidecar_only += 1;
            }
        }
        let (dst_raw, action) = if !clash {
            (natural, PlanAction::Copy)
        } else {
            match policy {
                ClashPolicy::Ask => (natural, PlanAction::Clash),
                ClashPolicy::Overwrite => (natural, PlanAction::Replace),
                ClashPolicy::CreateCopies => {
                    // First free numeric suffix from `_1`, checking BOTH
                    // members of the pair, on disk and in-plan.
                    let mut k = 1usize;
                    let renamed_to = loop {
                        let cand = suffixed(name, k);
                        let cand_xmp = xmp_name_of(&cand);
                        if !occupied(&dest.join(&cand))
                            && !taken.contains(&cand)
                            && !occupied(&dest.join(&cand_xmp))
                            && !taken.contains(&cand_xmp)
                        {
                            break cand;
                        }
                        k += 1;
                    };
                    renamed += 1;
                    (dest.join(renamed_to), PlanAction::CopyRenamed)
                }
            }
        };
        // The amber note: a copy this session landed in THIS folder and
        // the user then deleted by hand. Information only — under every
        // answer the image goes out again (there is no skip any more).
        let landed_here = session.landed_paths(s.id).find(|p| {
            p.parent().is_some_and(|dir| {
                *is_dest
                    .entry(dir.to_path_buf())
                    .or_insert_with(|| canonicalize_lenient(dir) == dest_canon)
            })
        });
        if landed_here.is_some_and(|p| !p.exists()) {
            recopied += 1;
        }
        let final_name = dst_raw
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        taken.insert(xmp_name_of(&final_name));
        taken.insert(final_name);
        if action == PlanAction::Clash {
            clash_bytes += s.size;
        } else {
            total_bytes += s.size;
        }
        if !clash {
            clash_free_bytes += s.size;
        }
        jobs.push(CopyJob {
            id: s.id,
            src_raw: s.path.clone(),
            dst_xmp: sidecar_path(&dst_raw),
            dst_raw,
            src_xmp,
            action,
            bytes: s.size,
        });
    }

    // Free space is advisory-honest: an unreadable statvfs yields None
    // ("free space unknown" in the dialog), never a fake huge number
    // (gate finding). The check is repeated by the app right before
    // execute (plan-to-start staleness), and per-file ENOSPC failures
    // remain isolated regardless.
    //
    // What must fit depends on the answer (fileops.md rule 3). Under
    // "create copies" every clashing image is a NEW file, so the whole
    // total must fit. Before the answer, and under "overwrite everything",
    // only the CLASH-FREE total is required: the clashing files mostly
    // replace bytes that are already there, one temp file at a time, and a
    // destination that really is full then fails those files one by one
    // with an honest reason — without ever destroying the file already
    // there, because a verified temp is what gets committed.
    let free_bytes = fs2::available_space(existing_ancestor(dest)).ok();
    let needed = match policy {
        ClashPolicy::CreateCopies => total_bytes,
        ClashPolicy::Ask | ClashPolicy::Overwrite => clash_free_bytes,
    };
    if let Some(free) = free_bytes {
        if needed > free {
            return Err(PlanError::InsufficientSpace { needed, free });
        }
    }

    Ok(CopyPlan {
        jobs,
        total_bytes,
        clash_bytes,
        free_bytes,
        clashes,
        sidecar_only_clashes: sidecar_only,
        renamed,
        recopied,
    })
}

/// Is anything at all sitting under this name? `symlink_metadata`, not
/// `exists`: a BROKEN symlink is invisible to `exists()` and still makes
/// the name unusable, and a directory or a live symlink occupies it just
/// as a regular file does (fileops.md, the clash check). On a
/// case-insensitive volume the filesystem answers for the case-variant
/// too, which is the point.
fn occupied(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// The sidecar's file NAME for a RAW file name — `sidecar_path`'s rule
/// (append `.xmp`) applied to a bare name, so the clash check can compare
/// names without building a path first.
fn xmp_name_of(raw_name: &str) -> String {
    format!("{raw_name}.xmp")
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

/// `DSC01234.ARW` + 1 → `DSC01234_1.ARW` (suffix BEFORE the extension —
/// never `DSC01234.ARW_1`; the sidecar follows in lockstep via
/// `sidecar_path` on the result). Numbering starts at `_1` (v1 started at
/// `_2`).
///
/// Public because the clash question shows the user the name a "keep
/// both" answer would produce, and that naming rule is core's, not the
/// dialog's, to spell.
pub fn suffixed(name: &str, k: usize) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}_{k}.{ext}"),
        _ => format!("{name}_{k}"),
    }
}

// ------------------------------------------------------------------ execute

#[derive(Debug)]
pub enum CopyEvent {
    /// Emitted before each file (1-based index over plan jobs). `action`
    /// is what this job is about to do, so the progress line can say
    /// "checking" for an overwrite (which starts by hashing what is
    /// already there) rather than claiming a transfer that may never
    /// happen — persona finding 2026-08-21: a re-run that counts to 148
    /// while mostly hashing reads as "it is sending everything again".
    File {
        index: usize,
        total: usize,
        name: String,
        action: PlanAction,
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
    /// RAWs actually transferred and verified.
    pub copied: usize,
    /// Of those, the ones that landed under a `_k` name ("create copies").
    pub renamed: usize,
    /// Of those, the ones that replaced a file that was already there
    /// ("overwrite everything").
    pub replaced: usize,
    /// Clashing RAWs whose destination copy was already byte-identical to
    /// the source: BLAKE3-verified in place, not re-transferred.
    pub identical: usize,
    /// Sidecars rewritten ALONE, beside such an identical RAW — the
    /// caption-after-copy recovery (fileops.md).
    pub refreshed: usize,
    /// The first name that actually landed under a `_k` suffix — the
    /// report shows one real example, because the names are how the user
    /// finds those frames in the destination folder afterwards.
    pub renamed_example: Option<String>,
    pub failed: Vec<(String, String)>, // (name, reason)
    /// True iff every byte this run wrote or checked was BLAKE3-verified
    /// against the source stream — the "green light to format the card"
    /// sentence.
    pub all_verified: bool,
    pub cancelled: bool,
    /// Every id whose RAW is now at the destination because of this run —
    /// transferred or verified identical — with the path it landed at.
    /// What the session records (`SessionCopies::record`) for the ✓ badge.
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
        // flight, and the temp-name+commit contract guarantees no partial
        // is left behind.
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

/// Run the plan on a worker thread. The caller flushed sidecars already
/// (barrier — see module docs) and answered the clash question: a plan
/// that still carries `PlanAction::Clash` jobs copies NOTHING.
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
        all_verified: true,
        ..Default::default()
    };
    // A plan built before the question is a QUESTION, not an instruction
    // (fileops.md rule 3). The app replans with the answer's policy and
    // executes THAT; if a wiring mistake ever sends the frozen one here,
    // it must copy nothing rather than guess an answer.
    if plan.jobs.iter().any(|j| j.action == PlanAction::Clash) {
        report.all_verified = false;
        report.failed.push((
            "(the whole run)".into(),
            "unanswered clash question: this plan was built before the answer and must not run"
                .into(),
        ));
        tx.send(CopyEvent::Finished(report)).ok();
        return;
    }
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
            action: job.action,
        })
        .ok();
        let result = match job.action {
            // Unreachable: refused above, before a single byte moved.
            PlanAction::Clash => continue,
            PlanAction::Copy | PlanAction::CopyRenamed => {
                copy_pair(job, Commit::NoClobber).map(|()| {
                    report.copied += 1;
                    if job.action == PlanAction::CopyRenamed {
                        report.renamed += 1;
                        report.renamed_example.get_or_insert_with(|| name.clone());
                    }
                    report.landed.push((job.id, job.dst_raw.clone()));
                })
            }
            PlanAction::Replace => replace_pair(job).map(|outcome| {
                match outcome {
                    Replaced::Transferred { existed } => {
                        report.copied += 1;
                        if existed {
                            report.replaced += 1;
                        }
                    }
                    Replaced::AlreadyIdentical { refreshed } => {
                        report.identical += 1;
                        if refreshed {
                            report.refreshed += 1;
                        }
                    }
                }
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

/// What an overwrite actually had to do.
enum Replaced {
    /// The RAW crossed the wire (and replaced a file, if one was there).
    Transferred { existed: bool },
    /// The destination RAW was already byte-identical to the source —
    /// verified by hash, not re-transferred; `refreshed` says whether its
    /// sidecar had to be rewritten.
    AlreadyIdentical { refreshed: bool },
}

fn copy_pair(job: &CopyJob, commit: Commit) -> std::io::Result<()> {
    copy_verified(&job.src_raw, &job.dst_raw, commit)?;
    if let Some(src) = &job.src_xmp {
        copy_verified(src, &job.dst_xmp, commit)?;
    }
    Ok(())
}

/// "Overwrite everything" for one image (fileops.md): replace what is at
/// the destination — unless it is already the same file, in which case
/// only the sidecar can have changed, and a 100 MB RAW must not cross the
/// wire twice for a caption.
fn replace_pair(job: &CopyJob) -> std::io::Result<Replaced> {
    // Hash the DESTINATION first: when nothing is there (a sidecar-only
    // clash), the source is never read twice.
    let dst_hash = hash_file(&job.dst_raw).ok();
    let existed = dst_hash.is_some();
    let identical = match dst_hash {
        // An unreadable SOURCE is this file's own failure, not a reason to
        // guess — it propagates.
        Some(dst) => hash_file(&job.src_raw)? == dst,
        None => false,
    };
    if !identical {
        copy_pair(job, Commit::Replace)?;
        return Ok(Replaced::Transferred { existed });
    }
    // The RAW at the destination IS the source, byte for byte (BLAKE3, not
    // size-or-mtime guessing). Only the sidecar is rewritten, and only if
    // it differs.
    let refreshed = match &job.src_xmp {
        Some(src) => {
            let same = hash_file(&job.dst_xmp)
                .ok()
                .is_some_and(|dst| hash_file(src).is_ok_and(|s| s == dst));
            if !same {
                copy_verified(src, &job.dst_xmp, Commit::Replace)?;
            }
            !same
        }
        None => false,
    };
    Ok(Replaced::AlreadyIdentical { refreshed })
}

/// Streaming BLAKE3 of a whole file (the identity check inside an
/// overwrite; the copy computes its own while writing).
fn hash_file(p: &Path) -> std::io::Result<blake3::Hash> {
    let mut f = std::fs::File::open(p)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// How the verified temp file becomes the final name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Commit {
    /// NOTHING is replaced unless the user answered "overwrite everything"
    /// (fileops.md rule 4): a name that got occupied since the plan fails
    /// THIS file honestly instead of eating what is there.
    NoClobber,
    /// The user answered overwrite: replace in place, atomically.
    Replace,
}

/// Copy with streaming BLAKE3: hash the source WHILE writing a temp file,
/// fsync, RE-READ the destination and compare hashes, then commit into
/// place — a failure never leaves a partial file under the final name.
fn copy_verified(src: &Path, dst: &Path, commit: Commit) -> std::io::Result<()> {
    copy_verified_with(src, dst, commit, |_| {})
}

/// `tamper` runs on the TEMP file between fsync and the verify re-read —
/// a test seam so the mismatch branch is actually driven (gate finding:
/// the old corruption test asserted hash inequality of two buffers,
/// which is vacuously true and exercised nothing).
fn copy_verified_with(
    src: &Path,
    dst: &Path,
    commit: Commit,
    tamper: impl FnOnce(&Path),
) -> std::io::Result<()> {
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
        commit_temp(&tmp, dst, commit)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// Put the verified temp file under its final name.
///
/// The no-clobber half is the promise of the whole feature: `rename`
/// silently replaces an existing file, so it is used ONLY for an answered
/// overwrite. Everything else goes through `hard_link`, the portable
/// "create this name only if it is free" — it fails with `AlreadyExists`
/// instead of destroying what appeared there since the plan was built.
fn commit_temp(tmp: &Path, dst: &Path, commit: Commit) -> std::io::Result<()> {
    if commit == Commit::Replace {
        return std::fs::rename(tmp, dst);
    }
    match std::fs::hard_link(tmp, dst) {
        Ok(()) => {
            // The link IS the file now; the temp name is just a second
            // name for it. A failure to unlink it leaves a stray temp
            // file, which is debris — never a reason to call a landed,
            // verified copy a failure.
            std::fs::remove_file(tmp).ok();
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(appeared_during_copy()),
        Err(_) => {
            // Filesystems without hard links (FAT/exFAT cards, some
            // network mounts) answer EPERM/ENOSYS rather than
            // AlreadyExists: fall back to check-then-rename. The window
            // between the check and the rename is unavoidable there and is
            // recorded in fileops.md.
            if occupied(dst) {
                return Err(appeared_during_copy());
            }
            std::fs::rename(tmp, dst)
        }
    }
}

fn appeared_during_copy() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "a file appeared at the destination during the copy",
    )
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

    /// Plan, execute, and hand back the report — the shape every "then the
    /// user answers X" assertion needs.
    fn run(
        sources: &[PlanSource],
        dest: &Path,
        template: Option<&str>,
        policy: ClashPolicy,
        session: &SessionCopies,
    ) -> CopyReport {
        let p = super::plan(sources, dest, template, policy, session).unwrap();
        let (_h, rx) = execute(p);
        drain(rx)
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
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
            ClashPolicy::Ask,
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
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TemplateCollision { .. }));
        // Unknown variable propagates the template error.
        let err = super::plan(
            &sources,
            &dest,
            Some("{bogus}"),
            ClashPolicy::Ask,
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
                ClashPolicy::Ask,
                &SessionCopies::default()
            ),
            Err(PlanError::DestEqualsSource)
        ));
        assert!(matches!(
            super::plan(
                &sources,
                &src_dir.join("selects"),
                None,
                ClashPolicy::Ask,
                &SessionCopies::default()
            ),
            Err(PlanError::DestInsideSource)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The question's input: what clashes, how many bytes each answer
    /// costs, and the fact that the clash-free files are NOT resolved into
    /// anything yet.
    #[test]
    fn ask_marks_the_clashes_and_counts_their_bytes_apart() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"another body's frame").unwrap();

        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Clash);
        assert_eq!(p.jobs[1].action, PlanAction::Copy);
        assert_eq!((p.clashes, p.sidecar_only_clashes), (1, 0));
        assert_eq!(
            (p.total_bytes, p.clash_bytes),
            (2, 4),
            "the clash-free total and the cost of the clashing half are separate"
        );
        assert_eq!(p.renamed, 0, "Ask resolves nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// "Create copies": from `_1` (v1 started at `_2`), before the
    /// extension, and the PAIR moves together — a number occupied by a
    /// sidecar alone, or claimed by another job in the same plan, is not
    /// free.
    #[test]
    fn create_copies_suffixes_from_1_and_moves_the_whole_pair() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"foreign").unwrap();

        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::CopyRenamed);
        assert_eq!(
            p.jobs[0].dst_raw.file_name().unwrap(),
            "a_1.ARW",
            "the first free number is 1, and the suffix goes before the extension"
        );
        assert_eq!(p.jobs[0].dst_xmp.file_name().unwrap(), "a_1.ARW.xmp");
        assert_eq!((p.renamed, p.clashes), (1, 1));

        // `_1` is NOT free while only its SIDECAR name is taken: the pair
        // moves to `_2` rather than splitting across two numbers.
        std::fs::write(dest.join("a_1.ARW.xmp"), b"<orphan/>").unwrap();
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_2.ARW");
        assert_eq!(p.jobs[0].dst_xmp.file_name().unwrap(), "a_2.ARW.xmp");

        // A number claimed by an EARLIER job in the same plan is taken
        // too, sidecar name included: `a.ARW` takes `a_3`, so the picked
        // file actually called `a_3.ARW` has to move on to `a_3_1.ARW`.
        std::fs::write(dest.join("a_2.ARW"), b"foreign2").unwrap();
        let mut pair = src_with(&dir, &[("a.ARW", b"aaaa"), ("a_3.ARW", b"cc")]);
        pair[1].id = 1;
        let p = super::plan(
            &pair,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_3.ARW");
        assert_eq!(p.jobs[1].dst_raw.file_name().unwrap(), "a_3_1.ARW");
        assert_eq!(p.jobs[1].action, PlanAction::CopyRenamed);

        // …and the copies really land under those names, both members.
        let report = run(
            &pair,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        );
        assert_eq!((report.copied, report.renamed), (2, 2));
        assert_eq!(
            std::fs::read(dest.join("a.ARW")).unwrap(),
            b"foreign",
            "the file that was already there is untouched"
        );
        assert_eq!(std::fs::read(dest.join("a_3.ARW")).unwrap(), b"aaaa");
        assert_eq!(std::fs::read(dest.join("a_3_1.ARW")).unwrap(), b"cc");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// "Overwrite everything": a DIFFERING file is replaced; a destination
    /// RAW that is already byte-identical is verified in place and NOT
    /// re-transferred — only its sidecar is rewritten, and only if it
    /// differs. That is the caption-after-copy recovery (fileops.md).
    #[test]
    fn overwrite_replaces_a_differing_file_and_only_refreshes_an_identical_one() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"old").unwrap();

        // Differing: replaced in place, counted as a replacement.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Replace);
        assert_eq!(p.jobs[0].dst_raw, dest.join("a.ARW"), "replaced IN PLACE");
        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(
            (report.copied, report.replaced, report.identical),
            (1, 1, 0)
        );
        assert!(report.all_verified);
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"aaaa");
        assert!(dest.join("a.ARW.xmp").exists(), "the sidecar came with it");

        // Identical, sidecar unchanged: nothing moves at all, and the
        // image still counts as being at the destination (the ✓ badge).
        let before = std::fs::metadata(dest.join("a.ARW"))
            .unwrap()
            .modified()
            .unwrap();
        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        );
        assert_eq!(
            (report.copied, report.identical, report.refreshed),
            (0, 1, 0),
            "a byte-identical RAW must not cross the wire again"
        );
        assert_eq!(report.landed, vec![(0, dest.join("a.ARW"))]);
        assert!(report.all_verified);

        // Identical RAW, CHANGED sidecar (a caption added after the copy):
        // the sidecar alone is rewritten; the RAW is not touched at all.
        std::fs::write(sidecar_path(&sources[0].path), b"<xmp>fresh caption</xmp>").unwrap();
        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        );
        assert_eq!(
            (report.copied, report.identical, report.refreshed),
            (0, 1, 1)
        );
        assert_eq!(
            std::fs::read(dest.join("a.ARW.xmp")).unwrap(),
            b"<xmp>fresh caption</xmp>"
        );
        assert_eq!(
            std::fs::metadata(dest.join("a.ARW"))
                .unwrap()
                .modified()
                .unwrap(),
            before,
            "the RAW was never rewritten"
        );
        assert!(names_in(&dest).iter().all(|n| !n.contains("partial")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The clash check runs on the FINAL, template-expanded names, and a
    /// suffix rides on top of the whole templated name — `{seq}` is
    /// assigned before clash resolution and is never re-flowed (it is
    /// baked into permanent filenames; fileops.md).
    #[test]
    fn the_clash_check_sees_templated_names_and_never_reflows_seq() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        // Only the TEMPLATED name of the first image is taken; its
        // original name is not, and vice versa.
        std::fs::write(dest.join("2026-07-26_1.ARW"), b"foreign").unwrap();
        std::fs::write(dest.join("b.ARW"), b"irrelevant").unwrap();
        let template = Some("{date}_{seq}.{ext}");

        let p = super::plan(
            &sources,
            &dest,
            template,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(
            (p.jobs[0].action, p.jobs[1].action),
            (PlanAction::Clash, PlanAction::Copy),
            "the untemplated `b.ARW` sitting there is not a clash; the templated name is"
        );

        let p = super::plan(
            &sources,
            &dest,
            template,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(
            p.jobs[0].dst_raw.file_name().unwrap(),
            "2026-07-26_1_1.ARW",
            "the suffix rides on top of the templated name"
        );
        assert_eq!(
            p.jobs[1].dst_raw.file_name().unwrap(),
            "2026-07-26_2.ARW",
            "the second image keeps seq 2 — numbering is never re-flowed by a suffix"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Occupancy is what the FILESYSTEM says (`symlink_metadata`), not
    /// `exists()`: a directory, a live symlink and a BROKEN symlink all
    /// occupy the name. The broken one is the trap — `exists()` reports it
    /// as absent, and a copy that trusted `exists()` would rename straight
    /// over it.
    #[test]
    fn a_directory_or_a_broken_symlink_under_a_planned_name_is_a_clash() {
        let dir = tmp();
        let sources = src_with(
            &dir,
            &[("a.ARW", b"aaaa"), ("b.ARW", b"bb"), ("c.ARW", b"cc")],
        );
        let dest = dir.join("out");
        std::fs::create_dir_all(dest.join("a.ARW")).unwrap(); // a DIRECTORY
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("nowhere"), dest.join("b.ARW")).unwrap();
            // Sidecar name only: the RAW name `c.ARW` is free.
            std::os::unix::fs::symlink(dir.join("nowhere"), dest.join("c.ARW.xmp")).unwrap();
            assert!(!dest.join("b.ARW").exists(), "fixture: broken symlink");
        }
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(
            p.jobs[0].action,
            PlanAction::Clash,
            "a directory occupies the name"
        );
        #[cfg(unix)]
        {
            assert_eq!(
                (p.jobs[1].action, p.jobs[2].action),
                (PlanAction::Clash, PlanAction::Clash)
            );
            assert_eq!((p.clashes, p.sidecar_only_clashes), (3, 1));
        }
        // And "create copies" walks past all three onto free pairs.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_1.ARW");
        #[cfg(unix)]
        {
            assert_eq!(p.jobs[1].dst_raw.file_name().unwrap(), "b_1.ARW");
            assert_eq!(p.jobs[2].dst_raw.file_name().unwrap(), "c_1.ARW");
            let report = run(
                &sources,
                &dest,
                None,
                ClashPolicy::CreateCopies,
                &SessionCopies::default(),
            );
            assert_eq!(report.copied, 3);
            assert!(dest.join("a.ARW").is_dir(), "the directory is untouched");
            assert!(
                dest.join("b.ARW")
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the broken symlink is untouched"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Overwrite replaces FILES. A directory under a planned name is
    /// never removed — that file fails alone and the run continues — and
    /// a symlink is replaced as a link, never written through to whatever
    /// it points at (persona finding 2026-08-21).
    #[test]
    #[cfg(unix)]
    fn overwrite_never_removes_a_directory_and_replaces_a_symlink_not_its_target() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(dest.join("a.ARW")).unwrap();
        std::fs::write(dest.join("a.ARW").join("inside.txt"), b"someone's folder").unwrap();
        let target = dir.join("elsewhere.bin");
        std::fs::write(&target, b"NOT the copy's business").unwrap();
        std::os::unix::fs::symlink(&target, dest.join("b.ARW")).unwrap();

        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        );
        assert_eq!(report.copied, 1, "only the symlink was replaceable");
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(
            dest.join("a.ARW").is_dir() && dest.join("a.ARW").join("inside.txt").exists(),
            "the directory (and what is in it) survived"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"NOT the copy's business",
            "the symlink's TARGET was not written through"
        );
        assert!(
            !dest
                .join("b.ARW")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself was replaced by the copy"
        );
        assert_eq!(std::fs::read(dest.join("b.ARW")).unwrap(), b"bb");
        assert!(names_in(&dest).iter().all(|n| !n.contains("partial")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A plan built BEFORE the answer is a question, not an instruction:
    /// the executor refuses the whole run rather than guessing — which is
    /// also what Cancel means (nothing is copied at all, not even the
    /// clash-free files).
    #[test]
    fn execute_refuses_a_plan_built_before_the_answer() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"foreign").unwrap();
        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        );
        assert_eq!((report.copied, report.identical), (0, 0));
        assert!(!report.all_verified);
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(
            report.failed[0].1.contains("unanswered clash question"),
            "{:?}",
            report.failed
        );
        assert_eq!(
            names_in(&dest),
            vec!["a.ARW".to_string()],
            "not even the clash-free file went out"
        );
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"foreign");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Nothing is replaced unless the user answered overwrite — including
    /// a name that got occupied AFTER the plan was built: that file fails
    /// alone, with an honest reason, and the run continues.
    #[test]
    fn a_name_taken_after_the_plan_fails_that_file_alone() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Copy);
        // Between the plan (or the question) and the copy, something else
        // takes the name.
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"appeared").unwrap();
        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(report.copied, 1, "b still went out");
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(
            report.failed[0].1.contains("appeared at the destination"),
            "{:?}",
            report.failed
        );
        assert!(!report.all_verified, "a failure means no green light");
        assert_eq!(
            std::fs::read(dest.join("a.ARW")).unwrap(),
            b"appeared",
            "the file that appeared was NOT eaten"
        );
        assert!(
            names_in(&dest).iter().all(|n| !n.contains("partial")),
            "{:?}",
            names_in(&dest)
        );
        assert_eq!(report.landed, vec![(1, dest.join("b.ARW"))]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Free space follows the answer (fileops.md rule 3): before the
    /// answer, and under overwrite, only the CLASH-FREE bytes have to fit
    /// — the clashing files mostly replace bytes that are already there.
    /// "Create copies" writes all of them as new files, so its whole total
    /// must fit.
    #[test]
    fn the_free_space_check_follows_the_answer() {
        let dir = tmp();
        let mut sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"foreign").unwrap();
        // `a` clashes and is (claimed to be) bigger than any disk.
        sources[0].size = u64::MAX / 2;

        for policy in [ClashPolicy::Ask, ClashPolicy::Overwrite] {
            let p = super::plan(&sources, &dest, None, policy, &SessionCopies::default())
                .unwrap_or_else(|e| panic!("{policy:?} must not error on space: {e}"));
            assert!(p.free_bytes.is_some(), "the fixture volume answers statvfs");
        }
        let err = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, PlanError::InsufficientSpace { .. }),
            "create-copies must not promise a write it cannot make: {err}"
        );
        // A CLASH-FREE file that does not fit blocks every answer.
        sources[0].size = 4;
        sources[1].size = u64::MAX / 2;
        for policy in [
            ClashPolicy::Ask,
            ClashPolicy::Overwrite,
            ClashPolicy::CreateCopies,
        ] {
            assert!(
                matches!(
                    super::plan(&sources, &dest, None, policy, &SessionCopies::default()),
                    Err(PlanError::InsufficientSpace { .. })
                ),
                "{policy:?} must refuse when even the clash-free total does not fit"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The 2026-08-21 bug, under v2 rules: copy, delete the copies by hand
    /// (RAW + XMP), Ctrl+E again into the same folder — nothing clashes
    /// any more, so there is no question and the pairs simply go out
    /// again, with the note saying so. RED on the old code, whose
    /// session-skip turned the empty folder into "0 B to copy".
    #[test]
    fn a_hand_deleted_copy_goes_out_again_with_no_question() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa"), ("b.ARW", b"bb")]);
        // `a` has a source sidecar (every real pick does), `b` has none:
        // the two symptom variants ("the xmp comes back" / "nothing").
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        let mut session = SessionCopies::default();
        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        );
        for (id, path) in report.landed {
            session.record(id, path);
        }
        assert!(session.is_copied(0) && session.is_copied(1));
        for name in ["a.ARW", "a.ARW.xmp", "b.ARW"] {
            std::fs::remove_file(dest.join(name)).unwrap();
        }

        let p = super::plan(&sources, &dest, None, ClashPolicy::Ask, &session).unwrap();
        assert_eq!(
            (p.jobs[0].action, p.jobs[1].action),
            (PlanAction::Copy, PlanAction::Copy),
            "a hand-deleted copy is copied again — RAW and sidecar, no question"
        );
        assert_eq!(
            (p.clashes, p.total_bytes, p.recopied),
            (0, 6, 2),
            "the plan counts the bytes and names the gone copies"
        );
        // The badge follows the disk once asked.
        session.refresh();
        assert!(!session.is_copied(0) && !session.is_copied(1));

        let (_h, rx) = execute(p);
        let report = drain(rx);
        assert_eq!(report.copied, 2);
        assert!(report.all_verified);
        for (id, path) in report.landed {
            session.record(id, path);
        }
        assert!(dest.join("a.ARW").exists() && dest.join("a.ARW.xmp").exists());
        assert!(dest.join("b.ARW").exists());
        assert!(session.is_copied(0) && session.is_copied(1));

        // Run it again with everything still there: NOW it clashes (the
        // session's own copies are clashes like any others), the note is
        // silent, and overwrite re-verifies instead of re-transferring.
        let p = super::plan(&sources, &dest, None, ClashPolicy::Ask, &session).unwrap();
        assert_eq!((p.clashes, p.recopied), (2, 0));
        let report = run(&sources, &dest, None, ClashPolicy::Overwrite, &session);
        assert_eq!((report.copied, report.identical), (0, 2));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only the RAW deleted, its sidecar left behind: the sidecar NAME is
    /// occupied, so the pair clashes — "create copies" moves both members
    /// to `_1` beside the orphan, and "overwrite everything" heals the
    /// folder by putting the pair back under its own name.
    #[test]
    fn a_sidecar_left_behind_is_a_clash_the_answers_resolve_both_ways() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        let mut session = SessionCopies::default();
        for (id, path) in run(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .landed
        {
            session.record(id, path);
        }
        std::fs::remove_file(dest.join("a.ARW")).unwrap();
        assert!(dest.join("a.ARW.xmp").exists(), "fixture: the XMP stays");

        let p = super::plan(&sources, &dest, None, ClashPolicy::Ask, &session).unwrap();
        assert_eq!(p.jobs[0].action, PlanAction::Clash);
        assert_eq!(
            (p.clashes, p.sidecar_only_clashes, p.recopied),
            (1, 1, 1),
            "the question names the sidecar-only clash, and the note the gone copy"
        );

        // Create copies: the pair moves together, the orphan stays put.
        let report = run(&sources, &dest, None, ClashPolicy::CreateCopies, &session);
        assert_eq!((report.copied, report.renamed), (1, 1));
        assert_eq!(names_in(&dest), vec!["a.ARW.xmp", "a_1.ARW", "a_1.ARW.xmp"]);
        std::fs::remove_file(dest.join("a_1.ARW")).unwrap();
        std::fs::remove_file(dest.join("a_1.ARW.xmp")).unwrap();

        // Overwrite: the pair lands under its own name, and the orphan
        // sidecar is replaced by the one that actually describes the RAW.
        let report = run(&sources, &dest, None, ClashPolicy::Overwrite, &session);
        assert_eq!(
            (report.copied, report.replaced, report.identical),
            (1, 0, 0)
        );
        assert_eq!(names_in(&dest), vec!["a.ARW", "a.ARW.xmp"]);
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"aaaa");
        assert_eq!(
            std::fs::read(dest.join("a.ARW.xmp")).unwrap(),
            std::fs::read(sidecar_path(&sources[0].path)).unwrap(),
            "the sidecar beside our RAW is OUR sidecar"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The session's memory is the ✓ badge and the "copied earlier but
    /// gone" note, per destination — and nothing else. Copying to A, then
    /// B, then back to A must still know about both.
    #[test]
    fn session_copies_are_remembered_per_destination_for_the_badge() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let (dest_a, dest_b) = (dir.join("out-a"), dir.join("out-b"));
        let mut session = SessionCopies::default();
        for dest in [&dest_a, &dest_b] {
            let p = super::plan(&sources, dest, None, ClashPolicy::Ask, &session).unwrap();
            assert_eq!(p.jobs[0].action, PlanAction::Copy, "{dest:?} is empty");
            assert_eq!(p.recopied, 0);
            let (_h, rx) = execute(p);
            for (id, path) in drain(rx).landed {
                session.record(id, path);
            }
        }
        // The badge means "a copy is there, somewhere".
        assert!(session.is_copied(0));
        std::fs::remove_file(dest_a.join("a.ARW")).unwrap();
        session.refresh();
        assert!(session.is_copied(0), "B's copy still stands");
        // …and the note is per destination: A lost its copy, B did not.
        let p = super::plan(&sources, &dest_a, None, ClashPolicy::Ask, &session).unwrap();
        assert_eq!(
            (p.recopied, p.clashes),
            (1, 0),
            "A's copy is gone and nothing occupies the name — it just goes out again"
        );
        let p = super::plan(&sources, &dest_b, None, ClashPolicy::Ask, &session).unwrap();
        assert_eq!(p.recopied, 0, "B's copy is still there");
        std::fs::remove_file(dest_b.join("a.ARW")).unwrap();
        session.refresh();
        assert!(!session.is_copied(0));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Recording through another spelling of the same folder supersedes
    /// the entry it matched — otherwise the stale path reads as "gone" for
    /// ever and the note cries wolf on every re-run.
    #[test]
    fn record_supersedes_the_entry_of_a_re_spelled_folder() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        let mut session = SessionCopies::default();
        for (id, path) in run(&sources, &dest, None, ClashPolicy::Ask, &session).landed {
            session.record(id, path);
        }
        std::fs::remove_file(dest.join("a.ARW")).unwrap();
        // Copied again through `out/../out`, renamed by a template.
        let respelled = dir.join("out").join("..").join("out");
        for (id, path) in run(
            &sources,
            &respelled,
            Some("{filename}_x.{ext}"),
            ClashPolicy::Ask,
            &session,
        )
        .landed
        {
            session.record(id, path);
        }
        assert!(dest.join("a_x.ARW").exists());
        // Back under the plain spelling: the live `a_x` copy IS the
        // record, so nothing is "gone".
        let p = super::plan(&sources, &dest, None, ClashPolicy::Ask, &session).unwrap();
        assert_eq!(
            p.recopied, 0,
            "the re-spelled record superseded the dead one"
        );
        assert!(session.is_copied(0));
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
            ClashPolicy::Ask,
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
        copy_verified(&src, &dir.join("d.bin"), Commit::NoClobber).unwrap();
        assert_eq!(std::fs::read(dir.join("d.bin")).unwrap(), [3u8; 4096]);
        // FAULT INJECTION (gate finding: the old assertion was vacuous):
        // corrupt the temp file between fsync and the verify re-read —
        // the mismatch branch must fire, clean up the temp, and leave
        // NOTHING under the final name.
        let dst = dir.join("d2.bin");
        let err = copy_verified_with(&src, &dst, Commit::NoClobber, |tmp| {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().append(true).open(tmp).unwrap();
            f.write_all(b"CORRUPT").unwrap();
            f.sync_all().unwrap();
        })
        .unwrap_err();
        assert!(err.to_string().contains("BLAKE3 mismatch"), "{err}");
        assert!(!dst.exists(), "no file under the final name");
        // …and a corrupt REPLACE never destroys the file it would have
        // replaced (the verified temp is what gets committed).
        std::fs::write(&dst, b"the good copy").unwrap();
        let err = copy_verified_with(&src, &dst, Commit::Replace, |tmp| {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().append(true).open(tmp).unwrap();
            f.write_all(b"CORRUPT").unwrap();
            f.sync_all().unwrap();
        })
        .unwrap_err();
        assert!(err.to_string().contains("BLAKE3 mismatch"), "{err}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"the good copy");
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
            ClashPolicy::Ask,
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
            ClashPolicy::Ask,
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
