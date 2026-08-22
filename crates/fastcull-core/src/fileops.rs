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
    #[error("the destination is a file, not a folder")]
    DestNotADirectory,
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
    /// [`ClashPolicy::Ask`] only: the name the FIRST clashing image would
    /// actually land under if the answer is "keep both". The question
    /// shows it, and showing `_1` when `_1` is already taken is a promise
    /// the copy then breaks (gate finding 2026-08-21).
    pub keep_both_example: Option<String>,
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
    // A destination that EXISTS but is not a folder is a plan error, not a
    // pile of per-file "File exists (os error 17)" failures the user has to
    // decode (QE finding 2026-08-21). "Exists" is asked of the link itself,
    // so a DANGLING symlink is caught too — it satisfies neither
    // `metadata()` nor `create_dir_all`, and used to slip through into the
    // same pile (gate finding). Not existing at all is fine: the copy
    // creates the folder.
    if dest.symlink_metadata().is_ok() && !dest.metadata().is_ok_and(|m| m.is_dir()) {
        return Err(PlanError::DestNotADirectory);
    }
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
    let mut keep_both_example: Option<String> = None;
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
                ClashPolicy::Ask => {
                    // What "keep both" would really do with the first
                    // clashing image — the same walk, from the same state,
                    // so the question names the file the copy will make.
                    if keep_both_example.is_none() {
                        keep_both_example = Some(first_free_suffix(dest, name, &taken));
                    }
                    (natural, PlanAction::Clash)
                }
                ClashPolicy::Overwrite => (natural, PlanAction::Replace),
                ClashPolicy::CreateCopies => {
                    renamed += 1;
                    (
                        dest.join(first_free_suffix(dest, name, &taken)),
                        PlanAction::CopyRenamed,
                    )
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
        keep_both_example,
        recopied,
    })
}

/// The first number from 1 upward whose WHOLE PAIR is free — both
/// `<stem>_k.<ext>` and its sidecar name, on disk and unclaimed by this
/// plan. The pair moves together, so a copy is never split across two
/// numbers (fileops.md); growth is unbounded by design, and each layer
/// costs the user a deliberate answer.
fn first_free_suffix(dest: &Path, name: &str, taken: &HashSet<String>) -> String {
    let mut k = 1usize;
    loop {
        let cand = suffixed(name, k);
        let cand_xmp = xmp_name_of(&cand);
        if !occupied(&dest.join(&cand))
            && !taken.contains(&cand)
            && !occupied(&dest.join(&cand_xmp))
            && !taken.contains(&cand_xmp)
        {
            return cand;
        }
        k += 1;
    }
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
    /// Overwrites that left a sidecar at the destination exactly where it
    /// was, because THIS pick has none of its own to write (its sidecar
    /// write failed, or the card is read-only). The RAW is ours and the
    /// `.xmp` beside it is not — the report says so rather than leaving
    /// the user to find out from darktable (QE finding 2026-08-21).
    pub foreign_sidecars_left: usize,
    /// The first name that actually landed under a `_k` suffix — the
    /// report shows one real example, because the names are how the user
    /// finds those frames in the destination folder afterwards.
    pub renamed_example: Option<String>,
    pub failed: Vec<(String, String)>, // (name, reason)
    /// True iff every byte this run wrote or checked was BLAKE3-verified
    /// against the source stream. NOT the green light on its own — a run
    /// that moved nothing leaves this true; see
    /// [`CopyReport::earned_the_green_light`].
    pub all_verified: bool,
    pub cancelled: bool,
    /// Every id whose RAW is now at the destination because of this run —
    /// transferred or verified identical — with the path it landed at.
    /// What the session records (`SessionCopies::record`) for the ✓ badge.
    pub landed: Vec<(usize, PathBuf)>,
}

impl CopyReport {
    /// May this run print "all checksums verified" — the sentence that
    /// tells the user the card is safe to format (fileops.md)?
    ///
    /// The rule lives HERE, not in the dialog that renders it (CLAUDE.md
    /// rule 5): a run must actually have verified something. Files copied
    /// count, and so do files an overwrite found byte-identical at the
    /// destination — that check IS a BLAKE3 comparison of both ends. A
    /// cancelled run, a run with a failure, and a run that moved nothing
    /// have verified nothing worth a green light.
    pub fn earned_the_green_light(&self) -> bool {
        self.copied + self.identical > 0
            && self.all_verified
            && self.failed.is_empty()
            && !self.cancelled
    }
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
    // Every name this run has already put on disk, lowercased. An
    // overwrite commits with `rename`, which replaces silently — and on a
    // destination that cannot tell `C.ARW` from `c.ARW` (exFAT card, SMB
    // share, APFS/NTFS) the second of two same-run names would replace the
    // FIRST ONE'S verified copy while the report said both were copied
    // (gate finding 2026-08-22; the plan's in-plan `taken` set is
    // exact-case and cannot see it). Checked at the last instant, so a
    // case-SENSITIVE destination — where the two names are genuinely
    // different files — never sees a false alarm.
    let mut landed_names: HashSet<String> = HashSet::new();
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
            PlanAction::Replace if would_eat_our_own(job, &landed_names) => {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "another file in this run already landed under this name — the destination \
                     cannot tell the two names apart",
                ))
            }
            PlanAction::Replace => replace_pair(job, cancel).map(|outcome| {
                match outcome {
                    Replaced::Transferred { existed } => {
                        report.copied += 1;
                        if existed {
                            report.replaced += 1;
                        }
                    }
                    Replaced::AlreadyIdentical {
                        refreshed,
                        sidecar_error,
                    } => {
                        report.identical += 1;
                        if refreshed {
                            report.refreshed += 1;
                        }
                        if let Some(reason) = sidecar_error {
                            report.all_verified = false;
                            report.failed.push((name.clone(), reason));
                        }
                    }
                }
                if job.src_xmp.is_none() && occupied(&job.dst_xmp) {
                    report.foreign_sidecars_left += 1;
                }
                report.landed.push((job.id, job.dst_raw.clone()));
            }),
        };
        if result.is_ok() {
            landed_names.insert(name.to_lowercase());
        }
        if let Err(e) = result {
            if e.kind() == std::io::ErrorKind::Interrupted && cancel.load(Ordering::Relaxed) {
                // Cancelled INSIDE an overwrite's identity pass, which is
                // not a failure. Without this the wait that `Drop` joins on
                // would span a whole re-verify of a big RAW on top of the
                // file in flight (gate finding).
                report.cancelled = true;
                break;
            }
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

/// Would this overwrite replace a file THIS RUN just landed? Only true
/// when the destination really does collapse the two names: the name is
/// occupied AND (case-insensitively) one we already wrote. On a
/// case-sensitive destination the name is simply free and this is false.
fn would_eat_our_own(job: &CopyJob, landed_names: &HashSet<String>) -> bool {
    job.dst_raw
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .is_some_and(|n| landed_names.contains(&n) && occupied(&job.dst_raw))
}

/// What an overwrite actually had to do.
enum Replaced {
    /// The RAW crossed the wire (and replaced a file, if one was there).
    Transferred { existed: bool },
    /// The destination RAW was already byte-identical to the source —
    /// verified by hash, not re-transferred; `refreshed` says whether its
    /// sidecar had to be rewritten, and `sidecar_error` carries a sidecar
    /// rewrite that FAILED. That is not a failed file: the RAW at the
    /// destination is this pick's, verified — so the run counts it as
    /// identical AND reports the sidecar honestly (gate finding
    /// 2026-08-22: it used to be reported as a wholly failed file with
    /// `identical = 0`, over a destination that was in fact correct).
    AlreadyIdentical {
        refreshed: bool,
        sidecar_error: Option<String>,
    },
}

fn copy_pair(job: &CopyJob, commit: Commit) -> std::io::Result<()> {
    copy_verified(&job.src_raw, &job.dst_raw, commit)?;
    if let Some(src) = &job.src_xmp {
        // The RAW is already in place when this runs, so the failure the
        // user needs to read is not "copy failed" but "half of the pair
        // landed" — under an overwrite that means our RAW is now sitting
        // next to the sidecar that was there before (QE finding).
        copy_verified(src, &job.dst_xmp, commit).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("the RAW landed but its sidecar did not: {e}"),
            )
        })?;
    }
    Ok(())
}

/// "Overwrite everything" for one image (fileops.md): replace what is at
/// the destination — unless it is already the same file, in which case
/// only the sidecar can have changed, and a 100 MB RAW must not cross the
/// wire twice for a caption.
fn replace_pair(job: &CopyJob, cancel: &AtomicBool) -> std::io::Result<Replaced> {
    // "Replaced" is decided BEFORE anything is written, from what is under
    // either name — an unreadable destination RAW and a sidecar-only clash
    // both really do replace something, and deriving the count from "the
    // destination hashed" reported neither (gate finding 2026-08-21).
    let existed = occupied(&job.dst_raw) || occupied(&job.dst_xmp);
    // Hash the DESTINATION first: when nothing hashable is there (a
    // sidecar-only clash), the source is never read twice.
    let dst_hash = comparable_hash(&job.dst_raw, cancel)?;
    let identical = match dst_hash {
        // An unreadable SOURCE is this file's own failure, not a reason to
        // guess — it propagates.
        Some(dst) => hash_file(&job.src_raw, cancel)? == dst,
        None => false,
    };
    if !identical {
        copy_pair(job, Commit::Replace)?;
        return Ok(Replaced::Transferred { existed });
    }
    // The RAW at the destination IS the source, byte for byte (BLAKE3, not
    // size-or-mtime guessing). Only the sidecar is rewritten, and only if
    // it differs.
    let (refreshed, sidecar_error) = match &job.src_xmp {
        Some(src) => {
            let same = match comparable_hash(&job.dst_xmp, cancel)? {
                Some(dst) => hash_file(src, cancel)? == dst,
                None => false,
            };
            if same {
                (false, None)
            } else {
                // The RAW is already right — this is the caption-after-copy
                // refresh. If it fails, say which half is good rather than
                // calling the whole file failed (fileops.md §4).
                match copy_verified(src, &job.dst_xmp, Commit::Replace) {
                    Ok(()) => (true, None),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err(e),
                    Err(e) => (
                        false,
                        Some(format!(
                            "the RAW at the destination is this pick's, verified — but its \
                             sidecar could not be refreshed: {e}"
                        )),
                    ),
                }
            }
        }
        // No sidecar of our own to write: a foreign one under this name
        // stays where it is. Nothing at the destination is ever deleted
        // (fileops.md) — "keep both" is the answer that avoids the pairing.
        None => (false, None),
    };
    Ok(Replaced::AlreadyIdentical {
        refreshed,
        sidecar_error,
    })
}

/// The hash to compare an overwrite's destination against — `None` when
/// there is nothing there that CAN be compared.
///
/// Only a REGULAR FILE is ever read (`symlink_metadata`, so a symlink is
/// not one): reading a FIFO or a device standing under a planned name
/// would block this worker for ever, and `CopyHandle`'s drop JOINS it, so
/// quitting would hang too (gate finding 2026-08-21). An unreadable file
/// is not comparable either. Every such name simply is not "identical"
/// and falls through to the replace, where the rename takes the name
/// (symlink, FIFO, unreadable file) or fails honestly (directory).
fn comparable_hash(p: &Path, cancel: &AtomicBool) -> std::io::Result<Option<blake3::Hash>> {
    if !p.symlink_metadata().is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    match hash_file(p, cancel) {
        Ok(h) => Ok(Some(h)),
        // A cancel must reach the caller; anything else means "cannot
        // compare", not "fail this file".
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(e),
        Err(_) => Ok(None),
    }
}

/// Streaming BLAKE3 of a whole file (the identity check inside an
/// overwrite; the copy computes its own while writing). Polls the run's
/// cancel flag between blocks: on a slow destination this read is as long
/// as a copy, and cancellation is only as prompt as its longest step.
fn hash_file(p: &Path, cancel: &AtomicBool) -> std::io::Result<blake3::Hash> {
    let mut f = std::fs::File::open(p)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
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
    // A SHORT, PER-FILE-UNIQUE temp name in the destination folder — never
    // the final name plus a suffix, and never one shared name.
    //
    // Two failures shaped this (both found by the gate, 2026-08-21). The
    // suffix version added ~25 bytes to the final name, so a long templated
    // name could land its RAW and then fail its sidecar on the filesystem's
    // name limit alone. And ONE name for every file is worse than it looks:
    // the commit hard-links the temp to its final name and then unlinks the
    // temp, and if that unlink fails (a Windows sharing violation from an
    // AV scanner or the indexer is the ordinary cause) the temp name is
    // still a second name for the file just committed — so the NEXT file's
    // create would truncate a copy this app had already reported verified.
    // `create_new` closes that door for good: a name we did not just create
    // is never written through.
    let dir = dst.parent().unwrap_or_else(|| Path::new(""));
    let (tmp, writer) = create_temp(dir)?;
    let result = (|| -> std::io::Result<()> {
        let mut reader = std::fs::File::open(src)?;
        let mut writer = writer;
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
            //
            // Swallowing that failure is only safe because temp names are
            // never REUSED (see `create_temp`): the alias left behind is
            // another name for a committed, verified file, and nothing
            // will ever open it again. With one shared temp name this
            // `.ok()` was a data-loss path — the next file truncated it.
            std::fs::remove_file(tmp).ok();
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(appeared_during_copy()),
        // Filesystems without hard links (FAT/exFAT cards, some network
        // mounts) answer EPERM/ENOSYS rather than AlreadyExists.
        Err(_) => commit_without_hard_links(tmp, dst),
    }
}

/// The fallback for a destination whose filesystem has no hard links:
/// check, then rename. The window between the two is unavoidable there
/// (recorded in fileops.md) — which is exactly why it is NOT the normal
/// path, and why it is a named function with its own test rather than a
/// branch nothing ever runs (gate finding: CI's tmpfs always links).
fn commit_without_hard_links(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    if occupied(dst) {
        return Err(appeared_during_copy());
    }
    std::fs::rename(tmp, dst)
}

/// A fresh temp file in `dir`, created EXCLUSIVELY: the name is unique per
/// file (process id + a monotonic counter, so a second copy worker — the
/// listed v2 background copy — is safe on the same path), and `create_new`
/// means an existing name is never opened, let alone truncated. A number
/// already in use (a leftover from a crashed run, or an alias a failed
/// unlink left behind) is skipped rather than reused.
fn create_temp(dir: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut last = None;
    for _ in 0..1024 {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let cand = dir.join(format!(".fastcull-partial-{}-{n}", std::process::id()));
        match std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&cand)
        {
            Ok(f) => return Ok((cand, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temp name at the destination")))
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
        assert_eq!(
            p.keep_both_example.as_deref(),
            Some("a_1.ARW"),
            "the question names the file 'keep both' would make"
        );
        // …and when `_1` is taken it names the number that IS free: the
        // question must not promise a name the copy will not use.
        std::fs::write(dest.join("a_1.ARW"), b"also there").unwrap();
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Ask,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.keep_both_example.as_deref(), Some("a_2.ARW"));
        std::fs::remove_file(dest.join("a_1.ARW")).unwrap();
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

    /// Overwrite must never HANG. A FIFO standing under a planned name is
    /// not something to compare against — reading it blocks for ever, on
    /// the very worker `CopyHandle`'s drop joins, so the app would hang on
    /// quit as well (gate finding). It is replaced like any other
    /// non-directory name.
    #[test]
    #[cfg(unix)]
    fn overwrite_does_not_hang_on_a_fifo_under_a_planned_name() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dest.join("a.ARW"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !made {
            eprintln!("skipped: no mkfifo here");
            return;
        }
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(
            p.jobs[0].action,
            PlanAction::Replace,
            "the FIFO occupies the name"
        );
        let (handle, rx) = execute(p);
        // A deadline instead of `drain`: on the pre-fix code this run never
        // ends, and the handle must then be LEAKED rather than dropped —
        // dropping it joins the worker that is blocked on the read.
        let mut report = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(CopyEvent::Finished(r)) => {
                    report = Some(r);
                    break;
                }
                Ok(_) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
        let Some(report) = report else {
            std::mem::forget(handle);
            panic!("the copy worker never finished — it is blocked reading the FIFO");
        };
        assert_eq!(
            (report.copied, report.replaced),
            (1, 1),
            "{:?}",
            report.failed
        );
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"aaaa");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The mirror image of issue #14, recorded rather than prevented: a
    /// pick with NO sidecar of its own overwrites a RAW whose sidecar
    /// belongs to another photograph. Nothing at the destination is ever
    /// deleted, so that sidecar stays — the answer that avoids the pairing
    /// is "keep both". This test exists so the outcome is a decision with
    /// a name, not a surprise (gate finding).
    #[test]
    fn overwrite_without_a_sidecar_of_our_own_leaves_the_foreign_one() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"mine")]);
        assert!(
            sidecar_path(&sources[0].path).symlink_metadata().is_err(),
            "fixture: this pick has no sidecar (the write failed, or the card is read-only)"
        );
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"another body").unwrap();
        std::fs::write(dest.join("a.ARW.xmp"), b"<foreign/>").unwrap();

        // It clashes on BOTH names, and "keep both" walks the pair clear.
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::CreateCopies,
            &SessionCopies::default(),
        )
        .unwrap();
        assert_eq!(p.jobs[0].dst_raw.file_name().unwrap(), "a_1.ARW");

        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        );
        assert_eq!(
            (report.copied, report.replaced, report.refreshed),
            (1, 1, 0)
        );
        assert_eq!(
            report.foreign_sidecars_left, 1,
            "the report has to say that the .xmp beside our RAW is not ours"
        );
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"mine");
        assert_eq!(
            std::fs::read(dest.join("a.ARW.xmp")).unwrap(),
            b"<foreign/>",
            "we never write, and never delete, a sidecar we do not have"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The commit path used on filesystems with no hard links (FAT cards,
    /// some network mounts). CI always has links, so this branch would
    /// otherwise first run on a user's card (gate finding): it must still
    /// refuse an occupied name rather than rename over it.
    #[test]
    fn the_no_hard_link_fallback_still_refuses_an_occupied_name() {
        let dir = tmp();
        let tmp_file = dir.join("t.partial");
        let dst = dir.join("landing.ARW");
        std::fs::write(&tmp_file, b"verified").unwrap();
        commit_without_hard_links(&tmp_file, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"verified");
        assert!(!tmp_file.exists(), "the temp name is gone after the rename");

        std::fs::write(&tmp_file, b"second").unwrap();
        let err = commit_without_hard_links(&tmp_file, &dst).unwrap_err();
        assert!(
            err.to_string().contains("appeared at the destination"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"verified",
            "the file that was there must survive"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A destination that exists but is a FILE is a plan error, not a pile
    /// of per-file "File exists" failures to decode (QE finding).
    #[test]
    fn a_destination_that_is_a_file_is_rejected_by_the_plan() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("not-a-folder");
        std::fs::write(&dest, b"I am a file").unwrap();
        assert!(matches!(
            super::plan(
                &sources,
                &dest,
                None,
                ClashPolicy::Ask,
                &SessionCopies::default()
            ),
            Err(PlanError::DestNotADirectory)
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), b"I am a file");
        // A DANGLING symlink is not a folder either — `metadata()` cannot
        // see it, so it used to slip through into the same pile of
        // per-file failures (gate finding).
        #[cfg(unix)]
        {
            let link = dir.join("dangling-dest");
            std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();
            assert!(matches!(
                super::plan(
                    &sources,
                    &link,
                    None,
                    ClashPolicy::Ask,
                    &SessionCopies::default()
                ),
                Err(PlanError::DestNotADirectory)
            ));
            // …while a symlink TO a folder is a perfectly good destination.
            let real = dir.join("real-out");
            std::fs::create_dir_all(&real).unwrap();
            let good = dir.join("link-to-folder");
            std::os::unix::fs::symlink(&real, &good).unwrap();
            assert!(super::plan(
                &sources,
                &good,
                None,
                ClashPolicy::Ask,
                &SessionCopies::default()
            )
            .is_ok());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// When the RAW has landed and its sidecar then fails, the failure must
    /// SAY that half the pair is there — under an overwrite that means our
    /// RAW is now beside the sidecar that was there before (QE finding).
    #[test]
    fn a_sidecar_that_fails_after_its_raw_landed_says_so() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"old").unwrap();
        // A directory under the sidecar's name: the RAW commits, the
        // sidecar's commit cannot.
        std::fs::create_dir_all(dest.join("a.ARW.xmp")).unwrap();

        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        );
        assert_eq!(report.copied, 0, "the job as a whole did not succeed");
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(
            report.failed[0]
                .1
                .contains("the RAW landed but its sidecar did not"),
            "{:?}",
            report.failed
        );
        assert_eq!(
            std::fs::read(dest.join("a.ARW")).unwrap(),
            b"aaaa",
            "the RAW really did land — which is what the message says"
        );
        assert!(dest.join("a.ARW.xmp").is_dir(), "the directory survived");
        assert!(names_in(&dest).iter().all(|n| !n.contains("partial")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The temp file is a FIXED name in the destination folder, so a long
    /// destination name cannot push the temp past the filesystem's limit
    /// and split a pair (QE finding: a 228-byte name landed its RAW and
    /// failed its sidecar).
    #[test]
    fn a_very_long_destination_name_still_ships_the_whole_pair() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        // 228 bytes + ".ARW.xmp" stays inside NAME_MAX (255) — but not with
        // a 25-byte temp suffix on top of it.
        let long = "x".repeat(224);
        let report = run(
            &sources,
            &dest,
            Some(&format!("{long}.{{ext}}")),
            ClashPolicy::Ask,
            &SessionCopies::default(),
        );
        assert_eq!(
            (report.copied, report.failed.len()),
            (1, 0),
            "{:?}",
            report.failed
        );
        assert!(dest.join(format!("{long}.ARW")).exists());
        assert!(
            dest.join(format!("{long}.ARW.xmp")).exists(),
            "the sidecar shipped with its RAW"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The temp file must never be a name this app might open again. The
    /// commit hard-links the temp to its final name and then unlinks the
    /// temp; if that unlink fails — a Windows sharing violation from a
    /// scanner is the ordinary cause — the temp name is STILL a name for
    /// the file just committed, so reusing it would truncate a copy the
    /// report has already called verified (gate finding 2026-08-21).
    #[test]
    fn a_temp_name_is_never_reused_or_written_through() {
        let dir = tmp();
        let src = dir.join("s.bin");
        std::fs::write(&src, [1u8; 4096]).unwrap();

        let mut first = PathBuf::new();
        copy_verified_with(&src, &dir.join("a.bin"), Commit::NoClobber, |t| {
            first = t.to_path_buf()
        })
        .unwrap();
        // Stand in for the alias a failed unlink leaves behind: on the
        // filesystems where that happens this IS `a.bin`, under a second
        // name, verified and committed.
        std::fs::write(&first, b"a committed, verified RAW").unwrap();

        let mut second = PathBuf::new();
        copy_verified_with(&src, &dir.join("b.bin"), Commit::NoClobber, |t| {
            second = t.to_path_buf()
        })
        .unwrap();
        assert_ne!(first, second, "every file gets its own temp name");
        assert_eq!(
            std::fs::read(&first).unwrap(),
            b"a committed, verified RAW",
            "the next copy truncated a file it did not create"
        );
        assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), [1u8; 4096]);

        // And a name that is ALREADY TAKEN is skipped, never opened. The
        // numbers this process hands out are consecutive, so putting
        // squatters on the next few is how the collision path gets driven:
        // with an exclusive create they are stepped over, with a plain
        // create the first one is opened and truncated.
        let (taken, _f) = create_temp(&dir).unwrap();
        let next: u64 = taken
            .file_name()
            .and_then(|n| n.to_string_lossy().rsplit('-').next().map(str::to_owned))
            .and_then(|n| n.parse().ok())
            .expect("temp names end in their number");
        let squatters: Vec<PathBuf> = (next + 1..next + 9)
            .map(|n| dir.join(format!(".fastcull-partial-{}-{n}", std::process::id())))
            .collect();
        for sq in &squatters {
            std::fs::write(sq, b"not mine").unwrap();
        }
        let (fresh, _f) = create_temp(&dir).unwrap();
        assert!(
            !squatters.contains(&fresh),
            "a taken temp name was handed out again: {fresh:?}"
        );
        for sq in &squatters {
            assert_eq!(
                std::fs::read(sq).unwrap(),
                b"not mine",
                "a file this process did not create was written through: {sq:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The caption-after-copy refresh can fail on its own (ENOSPC, EACCES,
    /// something else under the sidecar's name) — and when it does, the RAW
    /// at the destination is still this pick's, verified. The run says
    /// exactly that instead of calling the whole file failed (gate finding
    /// 2026-08-22: it reported `identical = 0` over a correct destination).
    #[test]
    fn a_failed_refresh_still_counts_the_raw_it_verified() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        crate::xmp::write_pick(&sources[0].path, PickState::Picked).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        // The destination RAW is already byte-identical…
        std::fs::write(dest.join("a.ARW"), b"aaaa").unwrap();
        // …and its sidecar name is a directory, so the refresh cannot land.
        std::fs::create_dir_all(dest.join("a.ARW.xmp")).unwrap();

        let report = run(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        );
        assert_eq!(
            (report.identical, report.refreshed, report.copied),
            (1, 0, 0),
            "the identical RAW is still identical"
        );
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(
            report.failed[0]
                .1
                .contains("sidecar could not be refreshed")
                && report.failed[0].1.contains("verified"),
            "{:?}",
            report.failed
        );
        assert!(
            !report.earned_the_green_light(),
            "a run with a failure is never a green light"
        );
        assert_eq!(std::fs::read(dest.join("a.ARW")).unwrap(), b"aaaa");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An overwrite commits with a rename, which replaces silently — so on
    /// a destination that cannot tell two names apart, the second of them
    /// must not eat the first one's verified copy. The guard fires only
    /// when the name really is occupied by something this run landed, so a
    /// case-SENSITIVE destination never sees a false alarm (gate finding
    /// 2026-08-22).
    #[test]
    fn an_overwrite_never_replaces_a_file_this_run_just_landed() {
        let dir = tmp();
        let sources = src_with(&dir, &[("a.ARW", b"aaaa")]);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.ARW"), b"foreign").unwrap();
        let p = super::plan(
            &sources,
            &dest,
            None,
            ClashPolicy::Overwrite,
            &SessionCopies::default(),
        )
        .unwrap();
        let job = &p.jobs[0];
        // Nothing landed yet: the overwrite is exactly what was asked for.
        assert!(!would_eat_our_own(job, &HashSet::new()));
        // A different name this run landed is no reason to refuse.
        let other: HashSet<String> = ["b.arw".to_string()].into_iter().collect();
        assert!(!would_eat_our_own(job, &other));
        // The destination collapsing this name onto one we already wrote is.
        let same: HashSet<String> = ["a.arw".to_string()].into_iter().collect();
        assert!(would_eat_our_own(job, &same));
        // …and when the name is NOT occupied, there is nothing to eat —
        // which is what a case-sensitive destination reports.
        std::fs::remove_file(dest.join("a.ARW")).unwrap();
        assert!(!would_eat_our_own(job, &same));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The green light is core's rule, and it follows what was verified.
    #[test]
    fn the_green_light_needs_verified_bytes() {
        let verified_copy = CopyReport {
            copied: 3,
            all_verified: true,
            ..Default::default()
        };
        assert!(verified_copy.earned_the_green_light());
        // An overwrite that only RE-VERIFIED earns it too: that check is a
        // BLAKE3 comparison of both ends.
        assert!(CopyReport {
            identical: 145,
            all_verified: true,
            ..Default::default()
        }
        .earned_the_green_light());
        // A run that moved nothing, a cancelled run and a run with a
        // failure never do.
        assert!(!CopyReport {
            all_verified: true,
            ..Default::default()
        }
        .earned_the_green_light());
        assert!(!CopyReport {
            cancelled: true,
            ..verified_copy.clone()
        }
        .earned_the_green_light());
        assert!(!CopyReport {
            failed: vec![("a.ARW".into(), "nope".into())],
            ..verified_copy.clone()
        }
        .earned_the_green_light());
        assert!(!CopyReport {
            all_verified: false,
            ..verified_copy
        }
        .earned_the_green_light());
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
            (1, 1, 0),
            "the orphan sidecar really was replaced, and the report says so"
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
        // Cancel when the worker ANNOUNCES the first file: the flag is set
        // while that file is still being read, written and verified, so
        // the between-files check at the top of the next iteration is what
        // stops the run. Cancelling before `execute` even returned let the
        // whole plan finish first, and the assertion passed without ever
        // taking that branch (gate finding).
        match rx.recv().expect("first file event") {
            CopyEvent::File { index, .. } => assert_eq!(index, 1),
            other => panic!("expected the first file event, got {other:?}"),
        }
        h.cancel();
        let report = drain(rx);
        assert!(report.cancelled, "the between-files check never fired");
        assert!(
            report.copied < 3,
            "cancel is between files, so the rest must not go out: {report:?}"
        );
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
