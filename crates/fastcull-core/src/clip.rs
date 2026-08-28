//! Export frames as video (specs/modules/video-export.md): the selected
//! frames' embedded full-res JPEGs, copied byte-for-byte into ONE Motion
//! JPEG QuickTime file.
//!
//! Same two-phase shape as the copy engine (fileops.rs): a PLAN that
//! reads but never writes, then an EXECUTE on a worker thread with
//! streaming BLAKE3 verification. It reuses the copy engine's file
//! primitives on purpose — the unique temp name, the no-clobber commit,
//! the clash policy — because ADR 0004 gives both operations the same
//! contract: nothing at the destination is replaced without the user's
//! Overwrite answer, and no RAW is ever opened for writing.
//!
//! This module decides three things the copy engine has no equivalent of:
//!
//! 1. **The cadence** — how long each frame is on screen — measured from
//!    the frames' own capture timestamps (`Cadence`).
//! 2. **Which frames may share one track** — a Motion JPEG track has ONE
//!    frame size and ONE display orientation, so anything that does not
//!    match the first frame is skipped and reported, never scaled.
//! 3. **Where the JPEG bytes are** — an offset and a length inside each
//!    RAW, so the write is a byte copy and the samples are never all in
//!    memory at once.

pub mod qt;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::fileops::ClashPolicy;

/// Sample duration assumed when the frames carry no usable timing
/// (video-export.md: "timing not in the files — assumed 15 fps"). 67 ms
/// is 1/15 s rounded to the movie timescale of 1000.
const ASSUMED_SAMPLE_MS: u32 = 67;

/// Shortest sample this module will write, in milliseconds — 9 ms is
/// 111 fps, the fastest whole-millisecond cadence that still sits inside
/// the spec's [10 fps, 120 fps] window at timescale 1000 (8 ms would be
/// 125 fps, i.e. outside it).
const MIN_SAMPLE_MS: u32 = 9;

/// Longest sample this module will write: 100 ms is 10 fps, the spec's
/// floor. A selection of singles shot minutes apart plays at 10 fps and
/// says so, rather than becoming a one-frame-per-minute "video".
const MAX_SAMPLE_MS: u32 = 100;

/// Longest destination file name this module will try to create. Every
/// mainstream filesystem stops at 255 BYTES per name (ext4, APFS, NTFS,
/// exFAT), and the name here is built from two of the user's own file
/// stems, so two long stems can exceed it. Refusing at plan time costs
/// the user a message; finding out at commit time would cost them the
/// whole write first (the bytes are already on disk by then).
const MAX_NAME_BYTES: usize = 255;

/// One frame offered to the export, in any order — [`plan`] sorts.
#[derive(Clone, Debug)]
pub struct ClipSource {
    /// Session image id (the app's own handle on the frame).
    pub id: usize,
    pub path: PathBuf,
    /// The file's name as it is on disk (`DSC05010.ARW`): the output name
    /// is built from these stems, and it is the tiebreak when two frames
    /// carry the same timestamp.
    pub name: String,
    /// Capture instant in milliseconds, `burst::FrameMeta::time_ms`.
    /// None = the file carries no usable timestamp.
    pub time_ms: Option<i64>,
    /// Did `SubSecTimeOriginal` contribute? Without it the timestamp has
    /// 1 s granularity and cannot measure a burst's cadence.
    pub has_subsec: bool,
}

/// Why a frame is not in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// No embedded JPEG this app can use (the loupe's "no usable embedded
    /// preview" case, a 0-byte file, an unreadable one).
    NoPreview,
    /// A different frame size from the first frame's. Scaling it would be
    /// a re-encode and padding it would be an edit, so it is left out.
    Size { width: u32, height: u32 },
    /// A different display orientation from the first frame's.
    Orientation(u16),
}

impl SkipReason {
    /// The reason as the dialog and the report say it — one phrase, so
    /// the two surfaces can never word the same fact differently.
    pub fn text(&self) -> String {
        match self {
            SkipReason::NoPreview => "no usable embedded JPEG".to_string(),
            SkipReason::Size { width, height } => format!("different size ({width}×{height})"),
            SkipReason::Orientation(o) => format!("different orientation (EXIF {o})"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Skipped {
    pub id: usize,
    pub name: String,
    pub reason: SkipReason,
}

/// One sample: where its bytes live inside the RAW. The bytes themselves
/// are never loaded here — [`execute`] streams them.
#[derive(Clone, Debug)]
pub struct ClipFrame {
    pub id: usize,
    pub path: PathBuf,
    pub name: String,
    /// Byte offset of the embedded JPEG inside the RAW.
    pub offset: u64,
    /// Its length in bytes — this sample's size in the finished file.
    pub len: u64,
}

/// Where the frame rate came from. Both the plan line and the report
/// print [`Cadence::text`], so the user reads the same sentence before
/// and after (video-export.md: it must be impossible to miss before
/// Enter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CadenceSource {
    /// Measured from the frames' own millisecond timestamps.
    Measured,
    /// No pair of frames carried millisecond timing: 15 fps assumed.
    NoTiming,
    /// Measured, but so far outside a playable range that it was pulled
    /// into it — two bodies interleaved, or a selection of singles.
    Clamped { median_ms: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cadence {
    /// How long ONE frame is on screen, in milliseconds (the movie
    /// timescale is 1000, so this is also the sample duration).
    pub sample_ms: u32,
    pub source: CadenceSource,
}

impl Cadence {
    pub fn fps(&self) -> f64 {
        1000.0 / f64::from(self.sample_ms.max(1))
    }

    /// The cadence as ONE phrase for the plan line and the report.
    ///
    /// When the cadence was measured this is just the rate. In the two
    /// fallback cases it says what happened INSTEAD of the bare rate —
    /// repeating "15 fps" twice in one line reads as a stutter, and the
    /// phrases the spec pins ("assumed 15 fps", "clamped") are here.
    pub fn text(&self) -> String {
        match self.source {
            CadenceSource::Measured => format!("{} fps", fps_text(self.fps())),
            CadenceSource::NoTiming => "timing not in the files — assumed 15 fps".to_string(),
            CadenceSource::Clamped { median_ms } => format!(
                "gaps of {} — clamped to {} fps",
                gap_text(median_ms),
                fps_text(self.fps())
            ),
        }
    }
}

/// A frame rate with one decimal, trimmed: `30.3`, `10`, `111.1`.
fn fps_text(fps: f64) -> String {
    let s = format!("{fps:.1}");
    s.strip_suffix(".0").unwrap_or(&s).to_string()
}

/// A gap in the units it is easiest to recognise: `4.0 s`, `250 ms`.
fn gap_text(ms: i64) -> String {
    if ms >= 1000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

/// What the write will do with the destination name — the copy engine's
/// `PlanAction`, minus the cases that only exist for a pair of files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipAction {
    /// The name is free.
    Write,
    /// The name was taken and the user answered "keep both": this lands
    /// under `_1`, `_2`, …
    WriteRenamed,
    /// The name was taken and the user answered "overwrite".
    Replace,
    /// The name is taken and nothing has been decided yet
    /// ([`ClashPolicy::Ask`]). Never executed: it is the question.
    Clash,
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    /// Fewer than two frames could share one track. `skipped` carries
    /// WHY the others could not — without it the dialog can only say
    /// "not enough frames", which is the one thing the user can already
    /// see (they selected them).
    #[error("a video needs at least 2 frames; {kept} would be left")]
    TooFewFrames { kept: usize, skipped: Vec<Skipped> },
    #[error("the destination is not a folder")]
    DestNotADirectory,
    #[error("not enough free space: need {needed} bytes, {free} available")]
    InsufficientSpace { needed: u64, free: u64 },
    #[error("the file name would be {len} bytes long, which no filesystem accepts: {name}")]
    NameTooLong { name: String, len: usize },
}

/// The inspectable plan the dialog previews.
#[derive(Clone, Debug)]
pub struct ClipPlan {
    /// The samples, in CAPTURE ORDER — the file's own order, whatever the
    /// grid was sorted by.
    pub frames: Vec<ClipFrame>,
    /// Frame size of every sample (the first frame's; the rest matched it
    /// or were skipped).
    pub width: u32,
    pub height: u32,
    /// Display orientation carried by the track matrix, already reduced
    /// to its unmirrored counterpart (1, 3, 6 or 8).
    pub orientation: u16,
    /// How many kept frames had a MIRRORED EXIF orientation (2/4/5/7) and
    /// were treated as their unmirrored counterpart.
    pub mirrored: usize,
    pub cadence: Cadence,
    /// Frames left out, with their reason.
    pub skipped: Vec<Skipped>,
    /// The file this plan writes.
    pub dst: PathBuf,
    pub action: ClipAction,
    /// The JPEG bytes alone.
    pub sample_bytes: u64,
    /// What the finished file will occupy: samples plus the header, which
    /// is known exactly because every sample size is known.
    pub total_bytes: u64,
    /// None = the free-space query failed ("free space unknown").
    pub free_bytes: Option<u64>,
    /// [`ClashPolicy::Ask`] only: the name a "keep both" answer would
    /// really land under, so the question can name it.
    pub keep_both_example: Option<String>,
}

impl ClipPlan {
    /// Playing time of the finished file, in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames.len() as u64 * u64::from(self.cadence.sample_ms)
    }

    /// The destination's file name.
    pub fn file_name(&self) -> String {
        self.dst
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Skips grouped by reason, most frames first: "2 frames skipped:
    /// different size (5616×3744)". Empty when nothing was skipped.
    pub fn skipped_text(&self) -> String {
        skipped_text(&self.skipped)
    }
}

/// The one place skip reasons are turned into a sentence — the plan line
/// and the report share it so they can never disagree.
pub fn skipped_text(skipped: &[Skipped]) -> String {
    if skipped.is_empty() {
        return String::new();
    }
    let mut groups: Vec<(String, usize)> = Vec::new();
    for s in skipped {
        let text = s.reason.text();
        match groups.iter_mut().find(|(t, _)| *t == text) {
            Some((_, n)) => *n += 1,
            None => groups.push((text, 1)),
        }
    }
    groups.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let parts: Vec<String> = groups
        .iter()
        .map(|(text, n)| format!("{n} frame{}: {text}", if *n == 1 { "" } else { "s" }))
        .collect();
    format!("skipped — {}", parts.join(" · "))
}

/// EXIF orientation reduced to the rotation the track matrix can carry.
///
/// A Motion JPEG track has one display transform and QuickTime's matrix
/// can rotate but not mirror in any way a phone editor honours, so the
/// four mirrored orientations degrade to their unmirrored counterpart —
/// the same picture, un-flipped — and the report says how many did
/// (video-export.md, "Orientation").
pub fn unmirrored(orientation: u16) -> u16 {
    match orientation {
        2 => 1,
        4 => 3,
        5 => 8,
        7 => 6,
        1 | 3 | 6 | 8 => orientation,
        // Anything else is not an EXIF orientation at all (a corrupt tag):
        // treat it as "as stored", which is what the pipeline does.
        _ => 1,
    }
}

/// Bytes the finished file occupies: the samples plus this module's exact
/// header. Public because the dialog states the file's size before a byte
/// is written, and "roughly" is not good enough on a 4 GB export.
pub fn file_bytes(sample_bytes: u64, frames: usize) -> u64 {
    sample_bytes + qt::header_len(frames, sample_bytes)
}

/// Which frames the export would take (video-export.md, "Scope").
///
/// The SELECTION when there is one — the same batch the IPTC panel acts
/// on. With nothing selected, the BURST under the cursor, whole,
/// including any of its frames the current filter hides: a burst is a
/// fact about capture times, not about what the grid is showing, and the
/// user asking for "this burst" means all of it. Neither → nothing, and
/// the menu item is disabled with [`unavailable_reason`].
///
/// The ids come back in `group_of` order (the session's capture order);
/// [`plan`] sorts them properly anyway, from the timestamps themselves.
pub fn scope(selected: &[usize], cursor: usize, group_of: &[Option<usize>]) -> Vec<usize> {
    if !selected.is_empty() {
        return selected.to_vec();
    }
    let Some(Some(group)) = group_of.get(cursor).copied() else {
        return Vec::new();
    };
    group_of
        .iter()
        .enumerate()
        .filter(|(_, g)| **g == Some(group))
        .map(|(i, _)| i)
        .collect()
}

/// How many frames the export would take, WITHOUT building the list.
///
/// The menu item's enabled state asks this on every refresh, and
/// [`scope`] answers it by scanning the whole session's grouping — 50,000
/// comparisons per repaint on a big folder, for a number the app already
/// has: the selection count the status bar shows, and the burst size the
/// burst badge shows. Both are O(1) to the caller.
///
/// Kept next to `scope` so the two can never drift, and
/// `the_scope_and_its_count_agree` pins that they do — the same
/// arrangement `Selection::batch` and `Selection::count_in_view` have,
/// and for the same reason.
pub fn scope_len(selected: usize, burst_size: usize) -> usize {
    if selected > 0 {
        selected
    } else {
        burst_size
    }
}

/// Why the export cannot run, in the words the status line says it —
/// `None` when it can. The menu item is disabled in both non-None cases,
/// and pressing the key anyway says this rather than doing nothing
/// (video-export.md: "never a silent grey item").
pub fn unavailable_reason(frames: usize) -> Option<&'static str> {
    match frames {
        0 => Some("select frames or stand in a burst"),
        1 => Some("one frame is not a video — select more, or stand in a burst"),
        _ => None,
    }
}

/// Measure the cadence from the frames' own capture times.
///
/// Only gaps between CONSECUTIVE frames that BOTH carry millisecond
/// precision are measured: a timestamp without `SubSecTimeOriginal` has
/// 1 s granularity, so its "gaps" are 0 or 1000 ms and would invent a
/// cadence out of rounding. With no such pair at all the export falls
/// back to 15 fps and says so.
///
/// The MEDIAN gap, not the mean (video-export.md): two bursts selected
/// together have one huge gap between them, and a mean would stretch
/// every frame of both to hide it.
pub fn cadence(frames: &[ClipSource]) -> Cadence {
    let mut gaps: Vec<i64> = Vec::new();
    for pair in frames.windows(2) {
        if let (Some(a), Some(b)) = (pair[0].time_ms, pair[1].time_ms) {
            if pair[0].has_subsec && pair[1].has_subsec {
                gaps.push((b - a).abs());
            }
        }
    }
    if gaps.is_empty() {
        return Cadence {
            sample_ms: ASSUMED_SAMPLE_MS,
            source: CadenceSource::NoTiming,
        };
    }
    gaps.sort_unstable();
    let median = gaps[gaps.len() / 2];
    // `median` is >= 0 and can be enormous (a selection spanning a day),
    // so the cast is done through the clamp, never before it.
    let wanted = median.clamp(i64::from(MIN_SAMPLE_MS), i64::from(MAX_SAMPLE_MS)) as u32;
    Cadence {
        sample_ms: wanted,
        source: if i64::from(wanted) == median {
            CadenceSource::Measured
        } else {
            CadenceSource::Clamped { median_ms: median }
        },
    }
}

/// Capture order: timestamp first, file name as the tiebreak, frames
/// without a timestamp last (the same rule `filter::view` sorts the grid
/// by, so "capture order" means one thing in this app).
fn capture_order(sources: &[ClipSource]) -> Vec<ClipSource> {
    let mut ordered = sources.to_vec();
    ordered.sort_by(|a, b| match (a.time_ms, b.time_ms) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.name.cmp(&b.name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });
    ordered
}

/// The stem of a file name (`DSC05010.ARW` -> `DSC05010`). A name that is
/// all extension keeps its whole self, because a name we did not invent
/// is the user's business (fileops.md's rule, same reasoning).
fn stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => name,
    }
}

/// `DSC05010-DSC05039.mov` — first and last stem in capture order, so the
/// name doubles as the frame-range record (video-export.md). Two frames
/// whose stems are equal (the same name with two extensions) collapse to
/// one stem rather than producing `a-a.mov`.
pub fn clip_name(first: &str, last: &str) -> String {
    let (a, b) = (stem(first), stem(last));
    if a == b {
        format!("{a}.mov")
    } else {
        format!("{a}-{b}.mov")
    }
}

/// The embedded full-res JPEG of one RAW: where it is, how big it is, and
/// the file's EXIF orientation. `None` when the file has nothing usable —
/// unreadable, 0 bytes, or previews too small to be the real image.
fn probe(path: &Path) -> Option<(crate::raw::EmbeddedJpeg, u16)> {
    let mut file = std::fs::File::open(path).ok()?;
    let previews = crate::raw::find_embedded_jpegs(&mut file).ok()?;
    let jpeg = previews.fullres()?.clone();
    Some((jpeg, previews.orientation))
}

/// Build the plan. Reads the filesystem (the RAWs' preview tables, the
/// destination, free space) and changes nothing.
///
/// `policy` is the user's answer to the clash question, or
/// [`ClashPolicy::Ask`] before it has been asked.
pub fn plan(
    sources: &[ClipSource],
    dest: &Path,
    policy: ClashPolicy,
) -> Result<ClipPlan, ClipError> {
    // Free space is advisory-honest: an unreadable query yields None
    // ("free space unknown" in the dialog), never a fake huge number.
    let free = fs2::available_space(crate::fileops::existing_ancestor(dest)).ok();
    plan_with_free(sources, dest, policy, free)
}

/// [`plan`] with the free-space answer supplied. A seam, not an API: a
/// full disk cannot be arranged in a unit test, and the refusal it must
/// produce is the one thing standing between the user and a truncated
/// 4 GB file.
fn plan_with_free(
    sources: &[ClipSource],
    dest: &Path,
    policy: ClashPolicy,
    free_bytes: Option<u64>,
) -> Result<ClipPlan, ClipError> {
    // One frame is not a video. Checked before anything is read so a
    // mis-wired caller cannot spend a preview scan on it.
    if sources.len() < 2 {
        return Err(ClipError::TooFewFrames {
            kept: sources.len(),
            skipped: Vec::new(),
        });
    }
    // A destination that EXISTS but is not a folder — a regular file, or
    // a DANGLING symlink, which `metadata()` cannot see (the fileops.md
    // finding: both used to reach the write and come back as an
    // undecodable OS error). Not existing at all is fine: the write
    // creates the folder.
    if dest.symlink_metadata().is_ok() && !dest.metadata().is_ok_and(|m| m.is_dir()) {
        return Err(ClipError::DestNotADirectory);
    }

    let ordered = capture_order(sources);

    // Uniformity: the first frame WITH a usable JPEG sets the size and the
    // orientation; everything else matches it or is skipped. Never scaled,
    // never padded, never rotated (video-export.md "skip, never scale").
    let mut frames: Vec<ClipFrame> = Vec::with_capacity(ordered.len());
    let mut kept_sources: Vec<ClipSource> = Vec::with_capacity(ordered.len());
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut track: Option<(u32, u32, u16)> = None;
    let mut mirrored = 0usize;
    let mut sample_bytes = 0u64;
    for s in &ordered {
        let Some((jpeg, orientation)) = probe(&s.path) else {
            skipped.push(Skipped {
                id: s.id,
                name: s.name.clone(),
                reason: SkipReason::NoPreview,
            });
            continue;
        };
        let display = unmirrored(orientation);
        let (w, h, o) = *track.get_or_insert((jpeg.width, jpeg.height, display));
        if (jpeg.width, jpeg.height) != (w, h) {
            skipped.push(Skipped {
                id: s.id,
                name: s.name.clone(),
                reason: SkipReason::Size {
                    width: jpeg.width,
                    height: jpeg.height,
                },
            });
            continue;
        }
        if display != o {
            skipped.push(Skipped {
                id: s.id,
                name: s.name.clone(),
                reason: SkipReason::Orientation(orientation),
            });
            continue;
        }
        if display != orientation {
            mirrored += 1;
        }
        sample_bytes += jpeg.len;
        frames.push(ClipFrame {
            id: s.id,
            path: s.path.clone(),
            name: s.name.clone(),
            offset: jpeg.offset,
            len: jpeg.len,
        });
        kept_sources.push(s.clone());
    }
    let (width, height, orientation) = track.unwrap_or((0, 0, 1));
    if frames.len() < 2 {
        return Err(ClipError::TooFewFrames {
            kept: frames.len(),
            skipped,
        });
    }

    let cadence = cadence(&kept_sources);
    let total_bytes = file_bytes(sample_bytes, frames.len());

    // The name, from the frames that are actually IN the file: a range
    // that names a frame the user will not find in it is worse than no
    // range at all.
    let first = frames.first().map(|f| f.name.as_str()).unwrap_or_default();
    let last = frames.last().map(|f| f.name.as_str()).unwrap_or_default();
    let name = clip_name(first, last);
    if name.len() > MAX_NAME_BYTES {
        return Err(ClipError::NameTooLong {
            len: name.len(),
            name,
        });
    }

    // The clash question, exactly as Copy Picks asks it (fileops.md): the
    // disk decides, one answer governs the run, and nothing is replaced
    // without the Overwrite answer.
    let natural = dest.join(&name);
    let mut keep_both_example = None;
    let (dst, action) = if !crate::fileops::occupied(&natural) {
        (natural, ClipAction::Write)
    } else {
        match policy {
            ClashPolicy::Ask => {
                keep_both_example = Some(crate::fileops::first_free_name(dest, &name));
                (natural, ClipAction::Clash)
            }
            ClashPolicy::Overwrite => (natural, ClipAction::Replace),
            ClashPolicy::CreateCopies => (
                dest.join(crate::fileops::first_free_name(dest, &name)),
                ClipAction::WriteRenamed,
            ),
        }
    };

    // A destination that cannot hold a file this big for a reason free
    // space cannot see — FAT32 above 4 GB — fails at write time with the
    // OS error and the temp removed; there is no portable way to ask
    // beforehand (video-export.md, "Free space").
    //
    // An overwrite still writes the whole file to a temp first, so the
    // space has to be there under every answer.
    if let Some(free) = free_bytes {
        if total_bytes > free {
            return Err(ClipError::InsufficientSpace {
                needed: total_bytes,
                free,
            });
        }
    }

    Ok(ClipPlan {
        frames,
        width,
        height,
        orientation,
        mirrored,
        cadence,
        skipped,
        dst,
        action,
        sample_bytes,
        total_bytes,
        free_bytes,
        keep_both_example,
    })
}

// ------------------------------------------------------------------ execute

#[derive(Debug)]
pub enum ClipEvent {
    /// Before each frame's bytes are copied (1-based).
    Frame {
        index: usize,
        total: usize,
        name: String,
    },
    /// The samples are written; the finished file is being re-read and
    /// re-hashed. On a 4 GB export this pass is long enough that a silent
    /// progress line reads as a hang.
    Verifying,
    Finished(ClipReport),
}

#[derive(Debug, Default, Clone)]
pub struct ClipReport {
    /// Frames in the finished file.
    pub frames: usize,
    /// The file's size on disk.
    pub bytes: u64,
    /// Where it landed (None when nothing was written).
    pub path: Option<PathBuf>,
    /// The name it landed under — `_1` included, when the user answered
    /// "keep both".
    pub name: String,
    pub duration_ms: u64,
    pub cadence: Option<Cadence>,
    /// It replaced a file that was already there (the Overwrite answer).
    pub replaced: bool,
    /// Frames left out, with their reasons — the same list the plan line
    /// showed.
    pub skipped: Vec<Skipped>,
    /// Kept frames whose mirrored EXIF orientation was degraded.
    pub mirrored: usize,
    /// Every sample was BLAKE3-hashed on the way in AND re-hashed from
    /// the finished file, and the `moov` re-parse described exactly the
    /// samples written.
    pub all_verified: bool,
    pub cancelled: bool,
    /// What went wrong, if anything did.
    pub failed: Option<String>,
}

impl ClipReport {
    /// May this run print "all checksums verified"?
    ///
    /// The rule lives HERE and not in the dialog that renders it
    /// (CLAUDE.md rule 5). A run has earned it only when a file actually
    /// landed, every sample matched on the way back out, and nothing
    /// failed or was cancelled.
    pub fn earned_the_green_light(&self) -> bool {
        self.frames > 0
            && self.all_verified
            && self.failed.is_none()
            && !self.cancelled
            && self.path.is_some()
    }
}

pub struct ClipHandle {
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ClipHandle {
    /// Cancellation between frames, and inside the long reads (the
    /// verify pass polls the same flag): a cancel is only as prompt as
    /// the operation's longest step.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for ClipHandle {
    fn drop(&mut self) {
        // Cancel-then-join, like the copy engine: quitting mid-export
        // must not block on 4 GB of writing, and the temp+commit contract
        // guarantees no partial file under the final name.
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

/// Write the plan on a worker thread.
pub fn execute(plan: ClipPlan) -> (ClipHandle, Receiver<ClipEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let handle = std::thread::Builder::new()
        .name("export-video".into())
        .spawn(move || run_plan(&plan, &tx, &flag))
        .expect("spawn video export worker");
    (
        ClipHandle {
            cancel,
            handle: Some(handle),
        },
        rx,
    )
}

fn run_plan(plan: &ClipPlan, tx: &Sender<ClipEvent>, cancel: &AtomicBool) {
    run_plan_with(plan, tx, cancel, |_| {})
}

/// [`run_plan`] with a test seam. `tamper` runs on the TEMP file after it
/// is flushed and before it is verified — the only way to drive the
/// "the disk gave back something else" branch for real, rather than
/// asserting that two buffers a test just built are equal (the finding
/// that shaped the copy engine's equivalent seam).
fn run_plan_with(
    plan: &ClipPlan,
    tx: &Sender<ClipEvent>,
    cancel: &AtomicBool,
    tamper: impl FnOnce(&Path),
) {
    let mut report = ClipReport {
        skipped: plan.skipped.clone(),
        mirrored: plan.mirrored,
        cadence: Some(plan.cadence),
        duration_ms: plan.duration_ms(),
        name: plan.file_name(),
        ..Default::default()
    };
    // A plan built before the question is a QUESTION, not an instruction
    // (fileops.md rule 3, inherited): the app replans with the answer's
    // policy and executes THAT.
    if plan.action == ClipAction::Clash {
        report.failed = Some(
            "unanswered clash question: this plan was built before the answer and must not run"
                .into(),
        );
        tx.send(ClipEvent::Finished(report)).ok();
        return;
    }
    match write_clip(plan, tx, cancel, tamper) {
        Ok(Written::Committed { bytes }) => {
            report.frames = plan.frames.len();
            report.bytes = bytes;
            report.path = Some(plan.dst.clone());
            report.replaced = plan.action == ClipAction::Replace;
            report.all_verified = true;
        }
        Ok(Written::Cancelled) => {
            report.cancelled = true;
        }
        Err(e) => {
            report.failed = Some(e.to_string());
        }
    }
    tx.send(ClipEvent::Finished(report)).ok();
}

enum Written {
    Committed { bytes: u64 },
    Cancelled,
}

/// Write the whole file to a unique temp name in the destination folder,
/// verify it, and only then put it under its final name.
///
/// The order is the point. Nothing ever appears under the destination
/// name until every byte of it has been read back and matched, so a
/// crash, a full disk or a cancel leaves at most one hidden
/// `.fastcull-partial-*` file — never a truncated `.mov` the user might
/// hand to an editor.
fn write_clip(
    plan: &ClipPlan,
    tx: &Sender<ClipEvent>,
    cancel: &AtomicBool,
    tamper: impl FnOnce(&Path),
) -> std::io::Result<Written> {
    use std::io::Write as _;

    let dir = plan.dst.parent().unwrap_or_else(|| Path::new(""));
    std::fs::create_dir_all(dir)?;
    let (tmp, file) = crate::fileops::create_temp(dir)?;
    let outcome = (|| -> std::io::Result<Written> {
        let sizes: Vec<u64> = plan.frames.iter().map(|f| f.len).collect();
        let spec = qt::TrackSpec {
            width: plan.width,
            height: plan.height,
            orientation: plan.orientation,
            sample_ms: plan.cadence.sample_ms,
            sample_sizes: sizes,
        };
        let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
        qt::write_header(&mut out, &spec)?;
        // The samples, streamed: one 1 MB buffer for the whole export, so
        // a 400-frame selection costs the same memory as a 2-frame one.
        let mut hashes: Vec<blake3::Hash> = Vec::with_capacity(plan.frames.len());
        let mut buf = vec![0u8; 1 << 20];
        for (i, frame) in plan.frames.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Written::Cancelled);
            }
            tx.send(ClipEvent::Frame {
                index: i + 1,
                total: plan.frames.len(),
                name: frame.name.clone(),
            })
            .ok();
            hashes.push(copy_sample(frame, &mut out, &mut buf)?);
        }
        let mut file = out.into_inner().map_err(std::io::Error::other)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        if cancel.load(Ordering::Relaxed) {
            return Ok(Written::Cancelled);
        }
        tamper(&tmp);
        tx.send(ClipEvent::Verifying).ok();
        verify(&tmp, plan, &spec, &hashes, cancel, &mut buf)?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(Written::Cancelled);
        }

        let bytes = std::fs::metadata(&tmp)?.len();
        let commit = if plan.action == ClipAction::Replace {
            crate::fileops::Commit::Replace
        } else {
            crate::fileops::Commit::NoClobber
        };
        crate::fileops::commit_temp(&tmp, &plan.dst, commit)?;
        Ok(Written::Committed { bytes })
    })();
    // A failure, a cancel, or a panic-free early return: the temp goes.
    // The one path that must NOT delete it is a successful commit, where
    // the temp name no longer exists.
    if !matches!(outcome, Ok(Written::Committed { .. })) {
        std::fs::remove_file(&tmp).ok();
    }
    outcome
}

/// Copy ONE embedded JPEG from its RAW into the output, hashing it as it
/// goes. The RAW is opened read-only and never anything else (ADR 0003).
fn copy_sample<W: std::io::Write>(
    frame: &ClipFrame,
    out: &mut W,
    buf: &mut [u8],
) -> std::io::Result<blake3::Hash> {
    use std::io::Seek as _;
    let mut raw = std::fs::File::open(&frame.path)?;
    raw.seek(std::io::SeekFrom::Start(frame.offset))?;
    let mut hasher = blake3::Hasher::new();
    let mut left = frame.len;
    while left > 0 {
        let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        // `read_exact` and not `read`: a short read here means the RAW
        // shrank or was truncated under us, and a sample that is shorter
        // than the size already written into `stsz` would make the whole
        // file misaligned. Better to fail this export honestly.
        raw.read_exact(&mut buf[..want])?;
        hasher.update(&buf[..want]);
        out.write_all(&buf[..want])?;
        left -= want as u64;
    }
    Ok(hasher.finalize())
}

/// Read the finished file back and prove it is what was planned.
///
/// Two independent checks, because they catch different lies. The
/// per-sample hashes prove the BYTES survived the trip to the disk. The
/// `moov` re-parse proves the INDEX describes them — a file whose header
/// says a sample is 12 MB at offset X is unplayable if the bytes are
/// somewhere else, and every byte of it can still hash correctly.
fn verify(
    tmp: &Path,
    plan: &ClipPlan,
    spec: &qt::TrackSpec,
    hashes: &[blake3::Hash],
    cancel: &AtomicBool,
    buf: &mut [u8],
) -> std::io::Result<()> {
    let mut file = std::fs::File::open(tmp)?;
    let movie = qt::read_movie(&mut file)
        .map_err(|e| std::io::Error::other(format!("the finished file did not parse back: {e}")))?;
    let expected = qt::sample_offsets(spec);
    if movie.samples.len() != plan.frames.len() {
        return Err(std::io::Error::other(format!(
            "the finished file describes {} samples, not {}",
            movie.samples.len(),
            plan.frames.len()
        )));
    }
    let describes_this_export = movie.timescale == qt::TIMESCALE
        && movie.sample_ms == plan.cadence.sample_ms
        && movie.stts_entries == 1
        && movie.width == plan.width
        && movie.height == plan.height
        && movie.sample_width == plan.width
        && movie.sample_height == plan.height
        && movie.format == *b"jpeg"
        && movie.major_brand == *b"qt  "
        && movie.co64
        && movie.moov_before_mdat
        && movie.matrix == qt::display_matrix(plan.orientation);
    if !describes_this_export {
        return Err(std::io::Error::other(
            "the finished file's moov does not describe this export",
        ));
    }
    for (i, (sample, frame)) in movie.samples.iter().zip(&plan.frames).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        if sample.size != frame.len || Some(&sample.offset) != expected.get(i) {
            return Err(std::io::Error::other(format!(
                "sample {} is not where the plan put it",
                i + 1
            )));
        }
        if hash_range(&mut file, sample.offset, sample.size, buf)? != hashes[i] {
            return Err(std::io::Error::other(format!(
                "sample {} does not match the bytes read from {}",
                i + 1,
                frame.name
            )));
        }
    }
    Ok(())
}

/// BLAKE3 of a byte range of an open file, read in the caller's buffer.
fn hash_range(
    file: &mut std::fs::File,
    offset: u64,
    len: u64,
    buf: &mut [u8],
) -> std::io::Result<blake3::Hash> {
    use std::io::Seek as _;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut hasher = blake3::Hasher::new();
    let mut left = len;
    while left > 0 {
        let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        file.read_exact(&mut buf[..want])?;
        hasher.update(&buf[..want]);
        left -= want as u64;
    }
    Ok(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::tiff_testutil::{tiny_jpeg, TiffBuilder};
    use crate::testutil::scratch_dir;

    const TAG_WIDTH: u16 = 0x0100;
    const TAG_HEIGHT: u16 = 0x0101;
    const TAG_ORIENTATION: u16 = 0x0112;
    const TAG_JPEG_OFFSET: u16 = 0x0201;
    const TAG_JPEG_LENGTH: u16 = 0x0202;

    /// A synthetic RAW: a TIFF container whose IFD0 points at one
    /// "full-res" JPEG of the given dimensions, padded to `len` bytes so
    /// tests can give each frame a distinct sample size. The payload is a
    /// real (tiny) JPEG, so the preview walker accepts it exactly as it
    /// accepts a camera's.
    fn raw_with(dir: &Path, name: &str, w: u16, h: u16, orientation: u16, len: usize) -> PathBuf {
        let mut b = TiffBuilder::new(true);
        let mut payload = tiny_jpeg(w, h);
        assert!(len >= payload.len(), "padding only");
        payload.resize(len, 0x5A);
        let at = b.add_blob(&payload);
        let ifd0 = b.add_ifd(
            &[
                (TAG_WIDTH, 3, 1, u32::from(w)),
                (TAG_HEIGHT, 3, 1, u32::from(h)),
                (TAG_ORIENTATION, 3, 1, u32::from(orientation)),
                (TAG_JPEG_OFFSET, 4, 1, at),
                (TAG_JPEG_LENGTH, 4, 1, len as u32),
            ],
            0,
        );
        b.set_ifd0(ifd0);
        let path = dir.join(name);
        std::fs::write(&path, b.cursor().into_inner()).unwrap();
        path
    }

    /// A file the scan admits and the preview walker finds nothing in:
    /// a valid TIFF with dimensions but no JPEG pointer — the loupe's
    /// "no usable embedded preview" case.
    fn raw_without_preview(dir: &Path, name: &str) -> PathBuf {
        let mut b = TiffBuilder::new(true);
        let ifd0 = b.add_ifd(&[(TAG_WIDTH, 3, 1, 4000), (TAG_HEIGHT, 3, 1, 3000)], 0);
        b.set_ifd0(ifd0);
        let path = dir.join(name);
        std::fs::write(&path, b.cursor().into_inner()).unwrap();
        path
    }

    fn source(id: usize, path: &Path, time_ms: Option<i64>, has_subsec: bool) -> ClipSource {
        ClipSource {
            id,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_path_buf(),
            time_ms,
            has_subsec,
        }
    }

    /// Sources with no files behind them — enough for the cadence, which
    /// never touches the disk.
    fn timed(times: &[Option<i64>], has_subsec: bool) -> Vec<ClipSource> {
        times
            .iter()
            .enumerate()
            .map(|(i, t)| ClipSource {
                id: i,
                path: PathBuf::from(format!("f{i:04}.ARW")),
                name: format!("f{i:04}.ARW"),
                time_ms: *t,
                has_subsec,
            })
            .collect()
    }

    // ---------------------------------------------------------- cadence

    /// A 30 fps A1 burst: ~33 ms between frames, so one frame is on
    /// screen for 33 ms and the file plays in real time.
    #[test]
    fn the_median_gap_is_the_frame_duration() {
        // 30 frames at 33/34 ms alternating, as a 30 fps camera really
        // writes them.
        let times: Vec<Option<i64>> = (0..30).map(|i| Some(1000 + i * 100 / 3)).collect();
        let c = cadence(&timed(&times, true));
        assert_eq!(c.sample_ms, 33);
        assert_eq!(c.source, CadenceSource::Measured);
        assert_eq!(c.text(), "30.3 fps");
    }

    /// Two bursts selected together: the pause between them is ONE frame
    /// step, not a stretch applied to every frame. The mean would be
    /// 33 ms x 28 + 4000 ms / 29 = ~170 ms — 5x too slow.
    #[test]
    fn a_pause_between_two_bursts_does_not_stretch_every_frame() {
        let mut t = 0i64;
        let mut times = Vec::new();
        for i in 0..30 {
            if i == 15 {
                t += 4000; // the photographer let go and squeezed again
            } else if i > 0 {
                t += 33;
            }
            times.push(Some(t));
        }
        let c = cadence(&timed(&times, true));
        assert_eq!(c.sample_ms, 33, "the median ignores the one long gap");
        assert_eq!(c.source, CadenceSource::Measured);
    }

    /// Without SubSecTimeOriginal a timestamp has 1 s granularity, so its
    /// gaps are 0 or 1000 ms — a cadence invented out of rounding. The
    /// export says so instead of pretending.
    #[test]
    fn one_second_granularity_falls_back_to_fifteen_fps() {
        let times: Vec<Option<i64>> = (0..10).map(|i| Some(i * 1000)).collect();
        let c = cadence(&timed(&times, false));
        assert_eq!((c.sample_ms, c.source), (67, CadenceSource::NoTiming));
        assert_eq!(c.text(), "timing not in the files — assumed 15 fps");
        // ...and the same for frames with no timestamp at all.
        let none = cadence(&timed(&[None, None, None], true));
        assert_eq!(none.source, CadenceSource::NoTiming);
    }

    /// Gaps outside a playable range are pulled into it AND reported —
    /// two bodies interleaved (gaps of ~0) or a selection of singles
    /// (gaps of minutes). Both would otherwise produce a file no editor
    /// can use.
    #[test]
    fn implausible_gaps_are_clamped_and_said_so() {
        let fast = cadence(&timed(&[Some(0), Some(3), Some(6), Some(9)], true));
        assert_eq!(fast.sample_ms, MIN_SAMPLE_MS);
        assert_eq!(fast.source, CadenceSource::Clamped { median_ms: 3 });
        assert_eq!(fast.text(), "gaps of 3 ms — clamped to 111.1 fps");

        let slow = cadence(&timed(&[Some(0), Some(4000), Some(8000)], true));
        assert_eq!(slow.sample_ms, MAX_SAMPLE_MS);
        assert_eq!(slow.source, CadenceSource::Clamped { median_ms: 4000 });
        assert_eq!(slow.text(), "gaps of 4.0 s — clamped to 10 fps");
    }

    /// Only frames that BOTH carry millisecond precision measure a gap:
    /// one whole-second frame dropped into a burst must not add a 1000 ms
    /// sample to the population.
    #[test]
    fn only_millisecond_pairs_measure_a_gap() {
        let mut frames = timed(&[Some(0), Some(33), Some(66), Some(1066)], true);
        frames[3].has_subsec = false;
        let c = cadence(&frames);
        assert_eq!(c.sample_ms, 33, "the 1000 ms pair does not vote");
    }

    /// A cadence that was measured explains nothing; the two fallbacks
    /// always do. This is the persona rule — the wording that warns must
    /// be identical in the plan line and the report, and there is exactly
    /// one function producing it.
    #[test]
    fn the_cadence_only_explains_itself_when_it_had_to() {
        let measured = Cadence {
            sample_ms: 33,
            source: CadenceSource::Measured,
        };
        assert_eq!(measured.text(), "30.3 fps");
        for c in [
            Cadence {
                sample_ms: 67,
                source: CadenceSource::NoTiming,
            },
            Cadence {
                sample_ms: 100,
                source: CadenceSource::Clamped { median_ms: 5000 },
            },
        ] {
            assert!(
                c.text().contains("assumed 15 fps") || c.text().contains("clamped"),
                "a fallback cadence must say so: {}",
                c.text()
            );
        }
    }

    // ------------------------------------------------------------ scope

    /// What the export takes: the selection when there is one, otherwise
    /// the WHOLE burst under the cursor — including frames the current
    /// filter hides, because "this burst" means the burst.
    #[test]
    fn the_scope_is_the_selection_or_the_burst_under_the_cursor() {
        // Frames 0..3 are one burst, 4 is a single, 5..7 another burst.
        let groups = vec![
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            None,
            Some(1),
            Some(1),
            Some(1),
        ];
        assert_eq!(scope(&[], 1, &groups), vec![0, 1, 2, 3]);
        assert_eq!(scope(&[], 6, &groups), vec![5, 6, 7]);
        // A single: nothing to export, and the menu item says why.
        assert!(scope(&[], 4, &groups).is_empty());
        assert_eq!(
            unavailable_reason(0),
            Some("select frames or stand in a burst")
        );
        // A selection wins over the burst, and can cross bursts.
        assert_eq!(scope(&[2, 3, 5], 1, &groups), vec![2, 3, 5]);
        // One selected frame is not a video, and the reason says so.
        assert_eq!(scope(&[2], 1, &groups), vec![2]);
        assert!(unavailable_reason(1).is_some_and(|r| r.contains("not a video")));
        assert_eq!(unavailable_reason(2), None);
        // A cursor past the end of a (stale) grouping is not a panic.
        assert!(scope(&[], 99, &groups).is_empty());
    }

    /// The cheap count and the real list must always agree: the menu
    /// item is enabled from one and the export runs on the other, and a
    /// disagreement is an item that opens a dialog with nothing in it
    /// (or refuses to open one that would have worked).
    #[test]
    fn the_scope_and_its_count_agree() {
        let groups = vec![Some(0), Some(0), Some(0), None, Some(1), Some(1)];
        // No selection: the count is the burst size under the cursor.
        for cursor in 0..groups.len() {
            let burst = groups[cursor]
                .map(|g| groups.iter().filter(|x| **x == Some(g)).count())
                .unwrap_or(0);
            assert_eq!(
                scope(&[], cursor, &groups).len(),
                scope_len(0, burst),
                "cursor {cursor}"
            );
        }
        // With a selection, the selection wins in both.
        for selected in [vec![1usize], vec![1, 2], vec![0, 1, 4, 5]] {
            assert_eq!(
                scope(&selected, 0, &groups).len(),
                scope_len(selected.len(), 3)
            );
        }
    }

    // ------------------------------------------------------------ order

    /// Capture order, whatever order the caller hands them over in, with
    /// the file name breaking ties and untimed frames last (the rule
    /// `filter::view` sorts the grid by).
    #[test]
    fn capture_order_sorts_by_time_then_name_then_the_untimed() {
        let sources = vec![
            ClipSource {
                id: 0,
                path: "z.ARW".into(),
                name: "z.ARW".into(),
                time_ms: Some(200),
                has_subsec: true,
            },
            ClipSource {
                id: 1,
                path: "n.ARW".into(),
                name: "n.ARW".into(),
                time_ms: None,
                has_subsec: false,
            },
            ClipSource {
                id: 2,
                path: "b.ARW".into(),
                name: "b.ARW".into(),
                time_ms: Some(100),
                has_subsec: true,
            },
            ClipSource {
                id: 3,
                path: "a.ARW".into(),
                name: "a.ARW".into(),
                time_ms: Some(100),
                has_subsec: true,
            },
        ];
        let names: Vec<String> = capture_order(&sources)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, ["a.ARW", "b.ARW", "z.ARW", "n.ARW"]);
    }

    // ------------------------------------------------------------ names

    #[test]
    fn the_name_is_the_frame_range() {
        assert_eq!(
            clip_name("DSC05010.ARW", "DSC05039.ARW"),
            "DSC05010-DSC05039.mov"
        );
        // One stem, two extensions: not `a-a.mov`.
        assert_eq!(clip_name("a.ARW", "a.NEF"), "a.mov");
        // A name that is all extension is the user's business, kept whole.
        assert_eq!(clip_name(".hidden", "b.ARW"), ".hidden-b.mov");
    }

    // ---------------------------------------------------------- reasons

    #[test]
    fn skips_are_grouped_by_reason_biggest_first() {
        let s = |id, reason| Skipped {
            id,
            name: format!("f{id}.ARW"),
            reason,
        };
        let text = skipped_text(&[
            s(0, SkipReason::NoPreview),
            s(
                1,
                SkipReason::Size {
                    width: 5616,
                    height: 3744,
                },
            ),
            s(
                2,
                SkipReason::Size {
                    width: 5616,
                    height: 3744,
                },
            ),
        ]);
        assert_eq!(
            text,
            "skipped — 2 frames: different size (5616×3744) · 1 frame: no usable embedded JPEG"
        );
        assert!(skipped_text(&[]).is_empty());
    }

    /// A mirrored EXIF orientation keeps its ROTATION and loses the flip:
    /// QuickTime's matrix cannot mirror in a way phone editors honour, so
    /// the alternative to degrading would be skipping the frame.
    #[test]
    fn mirrored_orientations_degrade_to_their_rotation() {
        assert_eq!(unmirrored(2), 1);
        assert_eq!(unmirrored(4), 3);
        assert_eq!(unmirrored(5), 8);
        assert_eq!(unmirrored(7), 6);
        for straight in [1, 3, 6, 8] {
            assert_eq!(unmirrored(straight), straight);
        }
        assert_eq!(unmirrored(99), 1, "a corrupt tag reads as 'as stored'");
    }

    // ------------------------------------------------------------- plan

    #[test]
    fn the_plan_is_in_capture_order_and_names_the_range() {
        let dir = scratch_dir("clip-plan");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        // Handed over in a deliberately wrong order (the grid was sorted
        // by name descending).
        let sources = vec![
            source(
                0,
                &raw_with(&src, "c.ARW", 400, 300, 1, 600),
                Some(2066),
                true,
            ),
            source(
                1,
                &raw_with(&src, "a.ARW", 400, 300, 1, 500),
                Some(2000),
                true,
            ),
            source(
                2,
                &raw_with(&src, "b.ARW", 400, 300, 1, 550),
                Some(2033),
                true,
            ),
        ];
        let p = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(
            p.frames.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["a.ARW", "b.ARW", "c.ARW"],
            "a video that plays backwards because the grid was sorted \
             descending is a bug"
        );
        assert_eq!(p.file_name(), "a-c.mov");
        assert_eq!(p.dst, dest.join("a-c.mov"));
        assert_eq!(p.action, ClipAction::Write);
        assert_eq!((p.width, p.height), (400, 300));
        assert_eq!(p.orientation, 1);
        assert_eq!(p.sample_bytes, 500 + 550 + 600);
        assert_eq!(
            p.total_bytes,
            p.sample_bytes + qt::header_len(3, p.sample_bytes),
            "the size the dialog quotes is the size the file will have"
        );
        assert_eq!(p.cadence.sample_ms, 33);
        assert!(p.skipped.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Uniformity: one track, one frame size, one orientation. Everything
    /// else is skipped and REPORTED — never scaled (a re-encode), never
    /// padded (an edit), never silently dropped.
    #[test]
    fn frames_that_cannot_share_the_track_are_skipped_and_reported() {
        let dir = scratch_dir("clip-skip");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            // A crop-mode shot: different size.
            source(
                1,
                &raw_with(&src, "b.ARW", 380, 285, 1, 500),
                Some(33),
                true,
            ),
            // A portrait frame among landscapes.
            source(
                2,
                &raw_with(&src, "c.ARW", 400, 300, 6, 500),
                Some(66),
                true,
            ),
            // Nothing this app can use.
            source(3, &raw_without_preview(&src, "d.ARW"), Some(99), true),
            // A 0-byte file (a card pulled mid-write).
            source(
                4,
                &{
                    let p = src.join("e.ARW");
                    std::fs::write(&p, []).unwrap();
                    p
                },
                Some(132),
                true,
            ),
            source(
                5,
                &raw_with(&src, "f.ARW", 400, 300, 1, 500),
                Some(165),
                true,
            ),
        ];
        let p = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(
            p.frames.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["a.ARW", "f.ARW"]
        );
        assert_eq!(p.skipped.len(), 4);
        let text = p.skipped_text();
        assert!(text.contains("different size (380×285)"), "{text}");
        assert!(text.contains("different orientation (EXIF 6)"), "{text}");
        assert!(text.contains("2 frames: no usable embedded JPEG"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The name must name frames that are IN the file. A skipped first
    /// or last frame would otherwise put a range on the file that the
    /// user cannot find inside it — and the name is the frame-range
    /// record (video-export.md, "Files").
    #[test]
    fn the_name_names_only_frames_that_are_in_the_file() {
        let dir = scratch_dir("clip-namekept");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_without_preview(&src, "a.ARW"), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
            source(
                2,
                &raw_with(&src, "c.ARW", 400, 300, 1, 500),
                Some(66),
                true,
            ),
            source(3, &raw_without_preview(&src, "d.ARW"), Some(99), true),
        ];
        let p = plan(&sources, &dir.join("out"), ClashPolicy::Ask).unwrap();
        assert_eq!(p.file_name(), "b-c.mov", "a.ARW and d.ARW are not in it");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The FIRST frame with a usable JPEG sets the track — not the first
    /// frame handed over. A leading unreadable file must not decide that
    /// nothing matches.
    #[test]
    fn the_first_usable_frame_sets_the_track() {
        let dir = scratch_dir("clip-first");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_without_preview(&src, "a.ARW"), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 6, 500),
                Some(33),
                true,
            ),
            source(
                2,
                &raw_with(&src, "c.ARW", 400, 300, 6, 500),
                Some(66),
                true,
            ),
        ];
        let p = plan(&sources, &dir.join("out"), ClashPolicy::Ask).unwrap();
        assert_eq!(p.frames.len(), 2);
        assert_eq!(p.orientation, 6, "the portrait pair set the matrix");
        assert_eq!(
            (p.width, p.height),
            (400, 300),
            "sensor orientation, unrotated"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A mirrored orientation is kept (degraded to its rotation) and
    /// counted, so the report can say it — the picture is right, the flip
    /// is not, and silence about that would be a lie.
    #[test]
    fn a_mirrored_frame_is_kept_degraded_and_counted() {
        let dir = scratch_dir("clip-mirror");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 6, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 7, 500),
                Some(33),
                true,
            ),
        ];
        let p = plan(&sources, &dir.join("out"), ClashPolicy::Ask).unwrap();
        assert_eq!(p.frames.len(), 2, "7 and 6 are the same rotation");
        assert_eq!(p.orientation, 6);
        assert_eq!(p.mirrored, 1);
        assert!(p.skipped.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// One frame is not a video, and neither is a selection whose frames
    /// all turn out to be unusable. Both refuse at PLAN time — before a
    /// destination is even touched.
    #[test]
    fn fewer_than_two_frames_refuses_at_plan_time() {
        let dir = scratch_dir("clip-few");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let one = vec![source(
            0,
            &raw_with(&src, "a.ARW", 400, 300, 1, 500),
            Some(0),
            true,
        )];
        assert!(matches!(
            plan(&one, &dir.join("out"), ClashPolicy::Ask),
            Err(ClipError::TooFewFrames { kept: 1, .. })
        ));
        let mismatched = vec![
            source(0, &raw_with(&src, "b.ARW", 400, 300, 1, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "c.ARW", 380, 285, 1, 500),
                Some(33),
                true,
            ),
        ];
        // ...and the refusal carries WHY the others were left out, so
        // the dialog can say something the user cannot already see.
        match plan(&mismatched, &dir.join("out"), ClashPolicy::Ask) {
            Err(ClipError::TooFewFrames { kept: 1, skipped }) => assert_eq!(
                skipped_text(&skipped),
                "skipped — 1 frame: different size (380×285)"
            ),
            other => panic!("expected a refusal naming the reason, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A destination that EXISTS and is not a folder is a plan error, not
    /// a pile of undecodable OS errors after the write — including a
    /// DANGLING symlink, which `metadata()` cannot see (the fileops.md
    /// finding, which this module inherits).
    #[test]
    fn a_destination_that_is_not_a_folder_refuses_at_plan_time() {
        let dir = scratch_dir("clip-dest");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        let file = dir.join("a-file");
        std::fs::write(&file, b"not a folder").unwrap();
        assert!(matches!(
            plan(&sources, &file, ClashPolicy::Ask),
            Err(ClipError::DestNotADirectory)
        ));
        #[cfg(unix)]
        {
            let dangling = dir.join("dangling");
            std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).unwrap();
            assert!(matches!(
                plan(&sources, &dangling, ClashPolicy::Ask),
                Err(ClipError::DestNotADirectory)
            ));
        }
        // A folder that does not exist yet is fine — the write creates it.
        assert!(plan(&sources, &dir.join("new"), ClashPolicy::Ask).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The clash question, the copy engine's: the name on disk decides,
    /// `Ask` refuses to resolve it and names what "keep both" would do,
    /// and nothing is ever replaced without the Overwrite answer.
    #[test]
    fn a_taken_name_raises_the_question_and_each_answer_lands_somewhere_else() {
        let dir = scratch_dir("clip-clash");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        // Nothing there yet.
        let free = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(free.action, ClipAction::Write);
        assert!(free.keep_both_example.is_none());

        std::fs::write(dest.join("a-b.mov"), b"yesterday's export").unwrap();
        let asked = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(asked.action, ClipAction::Clash);
        assert_eq!(asked.keep_both_example.as_deref(), Some("a-b_1.mov"));
        assert_eq!(asked.dst, dest.join("a-b.mov"));

        let kept = plan(&sources, &dest, ClashPolicy::CreateCopies).unwrap();
        assert_eq!(kept.action, ClipAction::WriteRenamed);
        assert_eq!(kept.dst, dest.join("a-b_1.mov"));

        let over = plan(&sources, &dest, ClashPolicy::Overwrite).unwrap();
        assert_eq!(over.action, ClipAction::Replace);
        assert_eq!(over.dst, dest.join("a-b.mov"));

        // `_1` taken too: the question names the number the write will
        // really use, not the first one it can think of.
        std::fs::write(dest.join("a-b_1.mov"), b"and the one before").unwrap();
        let again = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(again.keep_both_example.as_deref(), Some("a-b_2.mov"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A name built from two very long stems cannot be created on any
    /// filesystem — and finding that out at commit time would cost the
    /// user the entire write first.
    #[test]
    fn an_impossible_name_refuses_before_anything_is_written() {
        let dir = scratch_dir("clip-longname");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let long = "L".repeat(130);
        let sources = vec![
            source(
                0,
                &raw_with(&src, &format!("{long}a.ARW"), 400, 300, 1, 500),
                Some(0),
                true,
            ),
            source(
                1,
                &raw_with(&src, &format!("{long}b.ARW"), 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        match plan(&sources, &dir.join("out"), ClashPolicy::Ask) {
            Err(ClipError::NameTooLong { len, .. }) => assert!(len > MAX_NAME_BYTES),
            other => panic!("expected a refusal, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A destination that cannot hold the file refuses before the write,
    /// and the number it refuses over is the WHOLE file — samples plus
    /// header, not the samples alone.
    #[test]
    fn a_destination_that_cannot_hold_the_file_refuses_at_plan_time() {
        let dir = scratch_dir("clip-space");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        let dest = dir.join("out");
        let sized = plan_with_free(&sources, &dest, ClashPolicy::Ask, Some(u64::MAX)).unwrap();
        let need = sized.total_bytes;
        assert!(need > 1000, "samples plus header");
        // Exactly enough is enough.
        assert!(plan_with_free(&sources, &dest, ClashPolicy::Ask, Some(need)).is_ok());
        // One byte short is not — and the header is part of what must fit,
        // so the sample bytes alone are refused too.
        for free in [need - 1, sized.sample_bytes] {
            match plan_with_free(&sources, &dest, ClashPolicy::Ask, Some(free)) {
                Err(ClipError::InsufficientSpace { needed, free: f }) => {
                    assert_eq!((needed, f), (need, free));
                }
                other => panic!("expected an insufficient-space refusal, got {other:?}"),
            }
        }
        // An unanswerable volume is "unknown", never a fake number.
        let unknown = plan_with_free(&sources, &dest, ClashPolicy::Ask, None).unwrap();
        assert_eq!(unknown.free_bytes, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The plan never writes anything — not the destination folder, not a
    /// temp file, and above all not the RAWs it read (ADR 0003).
    #[test]
    fn planning_writes_nothing_at_all() {
        let dir = scratch_dir("clip-readonly");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        let before: Vec<(std::path::PathBuf, Vec<u8>)> = std::fs::read_dir(&src)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                let bytes = std::fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect();
        let dest = dir.join("never-created");
        plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert!(!dest.exists(), "the plan created the destination folder");
        for (p, bytes) in before {
            assert_eq!(std::fs::read(&p).unwrap(), bytes, "the plan touched {p:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // -------------------------------------------------------- container

    /// The header size the plan quotes is arithmetic, so it is pinned
    /// here against the layout it describes; `qt`'s own golden test then
    /// pins the bytes against this number.
    #[test]
    fn the_header_size_follows_the_sample_count() {
        // 627 fixed bytes + 12 per sample + the mdat header.
        assert_eq!(qt::header_len(3, 1000), 627 + 36 + 8);
        assert_eq!(qt::header_len(30, 1000), 627 + 360 + 8);
        // A payload that needs a 64-bit `mdat` size grows the header by 8.
        let big = u64::from(u32::MAX);
        assert_eq!(qt::header_len(3, big), 627 + 36 + 16);
        assert_eq!(qt::header_len(3, big - 8), 627 + 36 + 8);
    }

    // ---------------------------------------------------------- execute

    /// Run a plan to completion on THIS thread and collect what it said.
    /// The worker thread is not the thing under test here — what it does
    /// to the disk is.
    fn run(plan: &ClipPlan) -> (ClipReport, Vec<String>) {
        run_tampered(plan, |_| {})
    }

    fn run_tampered(plan: &ClipPlan, tamper: impl FnOnce(&Path)) -> (ClipReport, Vec<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = AtomicBool::new(false);
        run_plan_with(plan, &tx, &cancel, tamper);
        drop(tx);
        let mut report = None;
        let mut events = Vec::new();
        for e in rx {
            match e {
                ClipEvent::Frame { index, total, name } => {
                    events.push(format!("{index}/{total} {name}"))
                }
                ClipEvent::Verifying => events.push("verifying".into()),
                ClipEvent::Finished(r) => report = Some(r),
            }
        }
        (report.expect("a run always finishes"), events)
    }

    /// Everything at the destination, sorted — including the hidden temp
    /// files, which is the point of half these assertions.
    fn listing(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// A fixture folder with three frames of a 30 fps burst.
    fn burst(dir: &Path) -> Vec<ClipSource> {
        std::fs::create_dir_all(dir).unwrap();
        vec![
            source(0, &raw_with(dir, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(1, &raw_with(dir, "b.ARW", 400, 300, 1, 640), Some(33), true),
            source(2, &raw_with(dir, "c.ARW", 400, 300, 1, 480), Some(66), true),
        ]
    }

    /// The bytes of each source's embedded JPEG, in capture order — what
    /// the samples in the finished file have to be, byte for byte.
    fn source_samples(plan: &ClipPlan) -> Vec<Vec<u8>> {
        plan.frames
            .iter()
            .map(|f| {
                let raw = std::fs::read(&f.path).unwrap();
                raw[f.offset as usize..(f.offset + f.len) as usize].to_vec()
            })
            .collect()
    }

    /// The whole write, end to end: one file lands, the in-tree reader
    /// agrees it is what the plan described, and every sample in it is
    /// the camera's own JPEG.
    #[test]
    fn an_export_lands_one_file_that_reads_back_as_planned() {
        let dir = scratch_dir("clip-write");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        let expect = source_samples(&plan);
        let (report, events) = run(&plan);

        assert_eq!(report.failed, None, "{report:?}");
        assert_eq!(report.frames, 3);
        assert!(report.all_verified && report.earned_the_green_light());
        assert_eq!(report.path.as_deref(), Some(dest.join("a-c.mov").as_path()));
        assert_eq!(report.name, "a-c.mov");
        assert_eq!(report.duration_ms, 99);
        assert_eq!(report.cadence.map(|c| c.sample_ms), Some(33));
        assert!(!report.replaced && !report.cancelled);
        // The progress the dialog shows: one line per frame, in order,
        // then the verify pass (which on a 4 GB file is long enough that
        // silence would read as a hang).
        assert_eq!(events, ["1/3 a.ARW", "2/3 b.ARW", "3/3 c.ARW", "verifying"],);
        // Nothing but the one file — no `.fastcull-partial-*` left over.
        assert_eq!(listing(&dest), ["a-c.mov"]);

        let mut file = std::fs::File::open(dest.join("a-c.mov")).unwrap();
        let movie = qt::read_movie(&mut file).unwrap();
        assert_eq!(movie.samples.len(), 3);
        assert!(movie.co64 && movie.moov_before_mdat);
        assert_eq!(&movie.format, b"jpeg");
        assert_eq!(movie.sample_ms, 33);
        assert_eq!((movie.width, movie.height), (400, 300));
        let bytes = std::fs::read(dest.join("a-c.mov")).unwrap();
        assert_eq!(bytes.len() as u64, plan.total_bytes, "the quoted size");
        for (i, sample) in movie.samples.iter().enumerate() {
            let at = sample.offset as usize;
            assert_eq!(
                &bytes[at..at + sample.size as usize],
                &expect[i][..],
                "sample {i} is not the camera's JPEG, byte for byte"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A byte that changes between the write and the read-back must cost
    /// the run its file AND its green light. Driven through the real
    /// verify pass by corrupting the temp file, not by comparing two
    /// buffers a test just built.
    #[test]
    fn a_byte_that_changed_on_the_way_to_disk_is_caught() {
        let dir = scratch_dir("clip-tamper");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        let (report, _) = run_tampered(&plan, |tmp| {
            // Flip one bit in the middle of the second sample.
            let mut bytes = std::fs::read(tmp).unwrap();
            let at = bytes.len() / 2;
            bytes[at] ^= 0xFF;
            std::fs::write(tmp, bytes).unwrap();
        });
        let reason = report.failed.clone().expect("the tamper must be caught");
        assert!(
            reason.contains("does not match the bytes read from"),
            "the failure must name what went wrong: {reason}"
        );
        assert!(!report.all_verified && !report.earned_the_green_light());
        assert_eq!(report.frames, 0);
        assert_eq!(report.path, None);
        assert!(
            listing(&dest).is_empty(),
            "a file that failed verification must not exist at all: {:?}",
            listing(&dest)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The same for the INDEX: bytes that hash correctly are still
    /// useless if the header says they are somewhere else. This corrupts
    /// the sample-size table, which every hash in the file survives.
    #[test]
    fn a_moov_that_stopped_describing_the_samples_is_caught() {
        let dir = scratch_dir("clip-moov");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        let (report, _) = run_tampered(&plan, |tmp| {
            let mut bytes = std::fs::read(tmp).unwrap();
            let at = bytes
                .windows(4)
                .position(|w| w == b"stsz")
                .expect("an stsz atom");
            // Claim the first sample is one byte shorter than it is.
            let size_at = at + 4 + 8 + 4;
            let claimed = u32::from_be_bytes(bytes[size_at..size_at + 4].try_into().unwrap());
            bytes[size_at..size_at + 4].copy_from_slice(&(claimed - 1).to_be_bytes());
            std::fs::write(tmp, bytes).unwrap();
        });
        assert!(report.failed.is_some(), "a lying index must be caught");
        assert!(!report.earned_the_green_light());
        assert!(listing(&dest).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A source that disappears mid-write: the run fails honestly, the
    /// destination name is never created, and the temp file goes with it.
    #[test]
    fn a_failure_mid_write_leaves_nothing_at_the_destination() {
        let dir = scratch_dir("clip-midfail");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        // Truncate the LAST frame's RAW after planning: the write gets
        // two frames in and then cannot read what it was promised.
        std::fs::write(&plan.frames[2].path, b"gone").unwrap();
        let (report, _) = run(&plan);
        assert!(report.failed.is_some(), "a short read must fail the run");
        assert_eq!(report.path, None);
        assert!(!report.earned_the_green_light());
        assert!(
            listing(&dest).is_empty(),
            "a partial export must leave NOTHING behind: {:?}",
            listing(&dest)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Cancel: nothing lands, and nothing is left half-written.
    #[test]
    fn a_cancelled_export_leaves_nothing_behind() {
        let dir = scratch_dir("clip-cancel");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = AtomicBool::new(true); // cancelled before the first frame
        run_plan(&plan, &tx, &cancel);
        drop(tx);
        let report = rx
            .into_iter()
            .find_map(|e| match e {
                ClipEvent::Finished(r) => Some(r),
                _ => None,
            })
            .unwrap();
        assert!(report.cancelled && report.failed.is_none());
        assert_eq!(report.path, None);
        assert!(!report.earned_the_green_light());
        assert!(listing(&dest).is_empty(), "{:?}", listing(&dest));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A plan built before the clash question was answered is a QUESTION,
    /// not an instruction: it writes nothing at all, and what is already
    /// at the destination is untouched (fileops.md rule 3, inherited).
    #[test]
    fn an_unanswered_clash_question_writes_nothing() {
        let dir = scratch_dir("clip-unanswered");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a-c.mov"), b"yesterday's export").unwrap();
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(plan.action, ClipAction::Clash);
        let (report, events) = run(&plan);
        assert!(report.failed.is_some() && !report.earned_the_green_light());
        assert!(events.is_empty(), "not one frame may be read: {events:?}");
        assert_eq!(
            std::fs::read(dest.join("a-c.mov")).unwrap(),
            b"yesterday's export"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The three answers, on the disk. Nothing at the destination is
    /// replaced without the Overwrite answer — the promise this module
    /// inherits from ADR 0004 and never trades away.
    #[test]
    fn each_answer_to_the_clash_question_lands_where_it_says() {
        let dir = scratch_dir("clip-answers");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        std::fs::create_dir_all(&dest).unwrap();
        let foreign = b"another day's export".to_vec();
        std::fs::write(dest.join("a-c.mov"), &foreign).unwrap();

        // Keep both: a new number, the old file untouched.
        let kept = plan(&sources, &dest, ClashPolicy::CreateCopies).unwrap();
        let (report, _) = run(&kept);
        assert_eq!(report.name, "a-c_1.mov");
        assert!(report.earned_the_green_light() && !report.replaced);
        assert_eq!(std::fs::read(dest.join("a-c.mov")).unwrap(), foreign);
        assert_eq!(listing(&dest), ["a-c.mov", "a-c_1.mov"]);

        // Overwrite: the old file is gone, replaced by a real export.
        let over = plan(&sources, &dest, ClashPolicy::Overwrite).unwrap();
        let (report, _) = run(&over);
        assert!(report.replaced && report.earned_the_green_light());
        let now = std::fs::read(dest.join("a-c.mov")).unwrap();
        assert_ne!(now, foreign);
        assert_eq!(&now[4..8], b"ftyp");
        assert_eq!(listing(&dest), ["a-c.mov", "a-c_1.mov"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ADR 0003, which this module inherits through ADR 0004: the RAWs
    /// and their sidecars are read and NOTHING else. Byte-compared before
    /// and after a real export, sidecars included.
    #[test]
    fn the_raws_and_their_sidecars_come_out_untouched() {
        let dir = scratch_dir("clip-adr3");
        let src = dir.join("src");
        let dest = dir.join("out");
        let sources = burst(&src);
        for s in &sources {
            std::fs::write(
                crate::xmp::sidecar_path(&s.path),
                format!("<x>{}</x>", s.name),
            )
            .unwrap();
        }
        let before: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&src)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                let b = std::fs::read(&p).unwrap();
                (p, b)
            })
            .collect();
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        let (report, _) = run(&plan);
        assert!(report.earned_the_green_light());
        for (p, bytes) in &before {
            assert_eq!(
                &std::fs::read(p).unwrap(),
                bytes,
                "the export wrote to {p:?}"
            );
        }
        assert_eq!(
            std::fs::read_dir(&src).unwrap().count(),
            before.len(),
            "the export added or removed a file beside the RAWs"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The samples STREAM: a 40 MB export must not cost 40 MB of memory,
    /// or a 400-frame 4.4 GB selection would need 4.4 GB of RAM.
    ///
    /// Measured through the process's own resident size, so it is a Linux
    /// test — the streaming loop it pins is platform-independent, and
    /// this is the platform where the number is free to read.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_samples_stream_instead_of_piling_up_in_memory() {
        fn resident_bytes() -> u64 {
            let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
            let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
            pages * 4096
        }
        let dir = scratch_dir("clip-stream");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        // 40 frames of 1 MB: 40 MB of samples, four times any plausible
        // buffer, and enough that holding them all would be unmissable.
        let sources: Vec<ClipSource> = (0..40)
            .map(|i| {
                source(
                    i,
                    &raw_with(&src, &format!("f{i:03}.ARW"), 400, 300, 1, 1 << 20),
                    Some(i as i64 * 33),
                    true,
                )
            })
            .collect();
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(plan.sample_bytes, 40 << 20);
        let before = resident_bytes();
        let (report, _) = run(&plan);
        let after = resident_bytes();
        assert!(report.earned_the_green_light(), "{report:?}");
        let grew = after.saturating_sub(before);
        assert!(
            grew < 16 << 20,
            "the export grew resident memory by {} MB writing {} MB of samples — \
             they are being held, not streamed",
            grew >> 20,
            plan.sample_bytes >> 20
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What the plan left out travels into the report unchanged, so the
    /// user reads the same sentence after the export as before it.
    #[test]
    fn the_report_carries_the_plans_skips_and_cadence() {
        let dir = scratch_dir("clip-carry");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), None, false),
            source(1, &raw_with(&src, "b.ARW", 380, 285, 1, 500), None, false),
            source(2, &raw_with(&src, "c.ARW", 400, 300, 2, 500), None, false),
        ];
        let plan = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(plan.cadence.source, CadenceSource::NoTiming);
        let (report, _) = run(&plan);
        assert!(report.earned_the_green_light());
        assert_eq!(report.frames, 2);
        assert_eq!(report.mirrored, 1, "c.ARW was orientation 2");
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            skipped_text(&report.skipped),
            "skipped — 1 frame: different size (380×285)"
        );
        assert!(
            report
                .cadence
                .map(|c| c.text())
                .is_some_and(|t| t.contains("assumed 15 fps")),
            "the report must repeat the plan's own words about the cadence"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The green light is a rule, not a decoration: it belongs to a run
    /// that actually landed a verified file and nothing else.
    #[test]
    fn the_green_light_is_only_for_a_run_that_earned_it() {
        let landed = ClipReport {
            frames: 3,
            path: Some(PathBuf::from("x.mov")),
            all_verified: true,
            ..Default::default()
        };
        assert!(landed.earned_the_green_light());
        for spoiled in [
            ClipReport {
                cancelled: true,
                ..landed.clone()
            },
            ClipReport {
                failed: Some("disk full".into()),
                ..landed.clone()
            },
            ClipReport {
                all_verified: false,
                ..landed.clone()
            },
            ClipReport {
                path: None,
                ..landed.clone()
            },
            ClipReport {
                frames: 0,
                ..landed.clone()
            },
        ] {
            assert!(
                !spoiled.earned_the_green_light(),
                "green light over {spoiled:?}"
            );
        }
    }

    // --------------------------------------------------- hostile inputs

    /// Two bodies shooting at once: the frames interleave in capture
    /// order, the gaps between consecutive frames of the MERGED sequence
    /// are far shorter than either camera's own cadence, and the result
    /// is clamped and reported rather than written as a 300 fps file.
    #[test]
    fn two_interleaved_bodies_merge_in_capture_order_and_clamp() {
        let dir = scratch_dir("clip-twobodies");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // Body A at t = 0, 100, 200…; body B 3 ms behind each of them.
        let mut sources = Vec::new();
        for i in 0..6 {
            let t = i as i64 * 100;
            sources.push(source(
                i * 2,
                &raw_with(&src, &format!("A{i:02}.ARW"), 400, 300, 1, 500),
                Some(t),
                true,
            ));
            sources.push(source(
                i * 2 + 1,
                &raw_with(&src, &format!("B{i:02}.ARW"), 400, 300, 1, 500),
                Some(t + 3),
                true,
            ));
        }
        let p = plan(&sources, &dir.join("out"), ClashPolicy::Ask).unwrap();
        // Capture-sorted MERGE, not "all of A then all of B".
        let names: Vec<&str> = p.frames.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(&names[..4], ["A00.ARW", "B00.ARW", "A01.ARW", "B01.ARW"]);
        // Alternating gaps of 3 ms and 97 ms: the median is 3, which is
        // no cadence at all, so it is clamped AND said out loud.
        assert!(matches!(p.cadence.source, CadenceSource::Clamped { .. }));
        assert_eq!(p.cadence.sample_ms, MIN_SAMPLE_MS);
        assert!(p.cadence.text().contains("clamped"), "{}", p.cadence.text());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ADR 0004: never the RAW folder by DEFAULT, but allowed when the
    /// user chooses it. The copy engine refuses that destination (it
    /// would drop copies of the RAWs beside the originals); this one must
    /// not, because the file it writes is a `.mov` that cannot collide
    /// with a RAW, and "export the burst next to the shoot" is a real
    /// answer.
    #[test]
    fn the_raw_folder_is_a_legal_destination_when_it_is_chosen() {
        let dir = scratch_dir("clip-inplace");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(
                1,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        let before: Vec<Vec<u8>> = sources
            .iter()
            .map(|s| std::fs::read(&s.path).unwrap())
            .collect();
        let p = plan(&sources, &src, ClashPolicy::Ask).unwrap();
        assert_eq!(p.dst, src.join("a-b.mov"));
        let (report, _) = run(&p);
        assert!(report.earned_the_green_light());
        assert!(src.join("a-b.mov").is_file());
        // ...and the RAWs it landed beside are byte-identical.
        for (s, bytes) in sources.iter().zip(&before) {
            assert_eq!(&std::fs::read(&s.path).unwrap(), bytes, "{:?}", s.path);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hostile: a preview the walker cannot size (an EXIF orientation but
    /// no SOF header in the payload) is not a frame — it is skipped like
    /// any other unusable one, never exported at a guessed size.
    #[test]
    fn a_preview_with_no_dimensions_is_skipped() {
        let dir = scratch_dir("clip-nodims");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // A JPEG signature and an orientation tag, but no SOF: the
        // walker can find it and cannot size it.
        let mut b = TiffBuilder::new(true);
        let payload = b.add_blob(&[0xFF, 0xD8, 0xFF, 0xD9, 0x00, 0x00]);
        let ifd0 = b.add_ifd(
            &[
                (TAG_ORIENTATION, 3, 1, 6),
                (TAG_JPEG_OFFSET, 4, 1, payload),
                (TAG_JPEG_LENGTH, 4, 1, 6),
            ],
            0,
        );
        b.set_ifd0(ifd0);
        let sizeless = src.join("x.ARW");
        std::fs::write(&sizeless, b.cursor().into_inner()).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 500), Some(0), true),
            source(1, &sizeless, Some(33), true),
            source(
                2,
                &raw_with(&src, "b.ARW", 400, 300, 1, 500),
                Some(66),
                true,
            ),
        ];
        let p = plan(&sources, &dir.join("out"), ClashPolicy::Ask).unwrap();
        assert_eq!(p.frames.len(), 2);
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.skipped[0].reason, SkipReason::NoPreview);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hostile: a TRUNCATED embedded JPEG — the loupe's "no decodable
    /// preview" case. The export does not decode, so a frame whose bytes
    /// are inside the file is copied AS IS; a declared length that runs
    /// past the end of the RAW is not a frame at all and is skipped.
    ///
    /// This is the recorded behaviour, not an accident: the whole feature
    /// is "the camera's bytes, untouched", and a decode pass to
    /// pre-validate them would be the first step towards an editor. A
    /// half-written frame from a dying card therefore lands in the video
    /// looking exactly as broken as it does in the loupe.
    #[test]
    fn a_truncated_preview_is_copied_as_is_and_a_runaway_one_is_skipped() {
        let dir = scratch_dir("clip-truncated");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        // A real, sizeable JPEG header whose scan simply stops — inside
        // the file, so it is a frame.
        let cut = raw_with(&src, "cut.ARW", 400, 300, 1, 300);
        // A pointer that claims more bytes than the file holds.
        let mut b = TiffBuilder::new(true);
        let payload = b.add_blob(&tiny_jpeg(400, 300));
        let ifd0 = b.add_ifd(
            &[
                (TAG_WIDTH, 3, 1, 400),
                (TAG_HEIGHT, 3, 1, 300),
                (TAG_JPEG_OFFSET, 4, 1, payload),
                (TAG_JPEG_LENGTH, 4, 1, 1_000_000),
            ],
            0,
        );
        b.set_ifd0(ifd0);
        let runaway = src.join("runaway.ARW");
        std::fs::write(&runaway, b.cursor().into_inner()).unwrap();
        let sources = vec![
            source(0, &raw_with(&src, "a.ARW", 400, 300, 1, 300), Some(0), true),
            source(1, &cut, Some(33), true),
            source(2, &runaway, Some(66), true),
        ];
        let p = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(
            p.frames.len(),
            2,
            "the runaway pointer is not a frame; the short one is"
        );
        assert_eq!(p.skipped.len(), 1);
        let (report, _) = run(&p);
        assert!(report.earned_the_green_light(), "{report:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hostile: the user's own file names. Spaces, accents, emoji and a
    /// leading dot all travel into the video's name untouched — they are
    /// the user's business (fileops.md's rule), and the only thing that
    /// matters is that the result is a NAME and lands in the chosen
    /// folder.
    #[test]
    fn names_with_spaces_and_unicode_survive_into_the_file_name() {
        let dir = scratch_dir("clip-unicode");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let sources = vec![
            source(
                0,
                &raw_with(&src, "café brûlé 01.ARW", 400, 300, 1, 500),
                Some(0),
                true,
            ),
            source(
                1,
                &raw_with(&src, "日本 02 📷.ARW", 400, 300, 1, 500),
                Some(33),
                true,
            ),
        ];
        let p = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        assert_eq!(p.file_name(), "café brûlé 01-日本 02 📷.mov");
        assert_eq!(
            p.dst.parent(),
            Some(dest.as_path()),
            "inside the chosen folder"
        );
        let (report, _) = run(&p);
        assert!(report.earned_the_green_light(), "{report:?}");
        assert!(dest.join("café brûlé 01-日本 02 📷.mov").is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hostile: a destination the user cannot write to. The plan is happy
    /// (it is a folder), and the WRITE fails honestly with the operating
    /// system's own reason, leaving nothing behind.
    #[cfg(unix)]
    #[test]
    fn a_read_only_destination_fails_honestly_and_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch_dir("clip-readonly-dest");
        let src = dir.join("src");
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let sources = burst(&src);
        let p = plan(&sources, &dest, ClashPolicy::Ask).unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Running as root ignores the mode bits; then this proves nothing
        // and says so rather than passing vacuously.
        if std::fs::File::create(dest.join(".probe")).is_ok() {
            std::fs::remove_file(dest.join(".probe")).ok();
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::remove_dir_all(&dir).ok();
            eprintln!("read-only destination test skipped: this user can write anyway (root?)");
            return;
        }
        let (report, _) = run(&p);
        let reason = report.failed.clone().expect("a read-only folder must fail");
        assert!(!report.earned_the_green_light());
        assert!(
            reason.to_lowercase().contains("permission")
                || reason.to_lowercase().contains("denied"),
            "the failure must carry the OS reason, not a generic one: {reason}"
        );
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(listing(&dest).is_empty(), "{:?}", listing(&dest));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hostile: a 1,000-frame selection. The plan holds one entry per
    /// frame and no sample bytes, the quoted size is exact, and the
    /// header grows by exactly 12 bytes per frame — the arithmetic the
    /// free-space check and every sample offset depend on.
    #[test]
    fn a_thousand_frames_plan_without_reading_a_single_sample() {
        let dir = scratch_dir("clip-thousand");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // Ten files on disk, referenced a hundred times each: the plan
        // treats them as 1,000 independent frames, and the test does not
        // create 1,000 files to prove it.
        let paths: Vec<PathBuf> = (0..10)
            .map(|i| raw_with(&src, &format!("f{i}.ARW"), 400, 300, 1, 1000 + i))
            .collect();
        let sources: Vec<ClipSource> = (0..1000)
            .map(|i| ClipSource {
                id: i,
                path: paths[i % paths.len()].clone(),
                name: format!("DSC{:05}.ARW", 1000 + i),
                time_ms: Some(i as i64 * 33),
                has_subsec: true,
            })
            .collect();
        let p = plan(&sources, &dir.join("out"), ClashPolicy::Ask).unwrap();
        assert_eq!(p.frames.len(), 1000);
        assert_eq!(p.file_name(), "DSC01000-DSC01999.mov");
        assert_eq!(p.cadence.sample_ms, 33);
        assert_eq!(p.duration_ms(), 33_000);
        // The size line is exact, and the header is the only part that
        // depends on the frame count.
        assert_eq!(
            p.total_bytes,
            p.sample_bytes + qt::header_len(1000, p.sample_bytes)
        );
        assert_eq!(
            qt::header_len(1000, p.sample_bytes) - qt::header_len(999, p.sample_bytes),
            12
        );
        // Offsets stay in step across the whole table.
        let spec = qt::TrackSpec {
            width: p.width,
            height: p.height,
            orientation: p.orientation,
            sample_ms: p.cadence.sample_ms,
            sample_sizes: p.frames.iter().map(|f| f.len).collect(),
        };
        let offsets = qt::sample_offsets(&spec);
        assert_eq!(offsets[0], qt::header_len(1000, p.sample_bytes));
        assert_eq!(
            *offsets.last().unwrap() + p.frames.last().unwrap().len,
            p.total_bytes
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
