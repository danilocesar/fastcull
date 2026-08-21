//! Application state: the one `AppState` every controller borrows, plus the
//! app-level constants (window margins, texture caps, selection wash).
//!
//! # The shape
//!
//! `AppState` is seven groups and two survivors, not a flat field list.
//! Each group is the state of ONE thing, so a controller's footprint on the
//! state is visible at a glance — `st.copy.…` is the copy dialog, and
//! nothing else is:
//!
//! | group | what it is |
//! |---|---|
//! | [`SessionState`] | the open folder: its images, what the user has said about them, the engines producing data for them |
//! | [`GridViewState`] | what the grid shows and where the cursor and selection are inside it |
//! | [`LoupeViewState`] | the loupe overlay: its engine, the desired factor and pan, what it last drew |
//! | [`TextureStore`] | every UI-side texture (thumbs, mids, full-res) — a cache, re-derivable from the engines |
//! | [`BurstIndex`] | burst grouping outputs, indexed by image id |
//! | [`IptcPanelState`] | the IPTC dock: its visibility, its model cache, the revert slot |
//! | [`CopyState`] | the Copy Picks dialog: plan, destination, running worker, what was copied |
//!
//! The two survivors are not state at all: `cells` is the model the window
//! is bound to, and `kitchen` is a worker thread. Both outlive every
//! session; both say so at their declaration.
//!
//! # The reset rule
//!
//! A session swap is [`AppState::begin_session`]: each swap-scoped group is
//! REPLACED wholesale, never reset field by field. That is the whole point
//! of the shape. The old code reset ~45 fields by hand in `load_folder` and
//! the list had already drifted — three fields it should have reset were
//! missing (`last_pan_write`, `last_overlay_cursor`, `last_view_geometry`).
//! With whole-struct replacement there is no list to fall out of: a field
//! added to a group is forgotten on a swap because it is part of the group.
//!
//! So when you add a field, the question is not "where do I reset this?" —
//! it is "which group is this a fact about?". Only if the answer is "none"
//! does it belong at the top level, and then it owes a comment saying why
//! it survives a session.
//!
//! Three groups DO carry something across a swap, and each names it in its
//! own `begin_session`: the grid keeps the zoom step (the launch flags set
//! it before the first folder loads) and the filter bar's visibility, the
//! IPTC panel keeps the dock's visibility, and the copy dialog keeps the
//! remembered destination (fileops.md). Everything else in those structs
//! goes back to its default.
//!
//! # Per-image vectors
//!
//! Six vectors in [`SessionState`] and three in [`BurstIndex`] are indexed
//! by image id — index i is the same photograph in all nine. Their length
//! is decided in exactly two constructors ([`SessionState::new`] and
//! [`BurstIndex::new`]), and `begin_session` asserts the invariant in debug
//! builds where the count is known.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use fastcull_core::grid;
use fastcull_core::pipeline::{Pipeline, SessionEvent};
use slint::VecModel;

use crate::kitchen;
use crate::CellData;

/// Rows kept alive around the viewport in the windowed model.
pub(crate) const MARGIN_ROWS: usize = 1;

/// Belt-and-braces cap on mid textures (~5 MB each) beyond the prune-to-
/// visible-window bound (recorded decision: 4K + 6 columns worst case).
pub(crate) const MIDS_CAP: usize = 64;

/// Longest the residual HOLD (issue #46) may keep the PREVIOUS image's
/// pixels on screen when not even the cursor's own thumb exists: one
/// settle window (persona condition — "don't ship an unbounded lie"). In
/// any healthy session the thumb or mid lands well inside this; the cap
/// exists for the wedged-decode pathology, where the view then drops to
/// fit honestly rather than showing photo N−1 labeled photo N forever.
///
/// The VALUE lives here, with its sibling UI tuning constants; the rule it
/// bounds is `fastcull_core::transit::render_rung`, which takes it as
/// `hold_cap` (A3). That split is deliberate — an elapsed time and a cap
/// passed IN are what make the bound a table row rather than a stopwatch.
pub(crate) const OVERLAY_HOLD_CAP: std::time::Duration = std::time::Duration::from_millis(250);

/// Selection wash hue: the SAME accent blue as the cursor outline, so the two
/// indicators stay one visual family — filled means selected, bright border
/// means cursor, and the two compose instead of competing.
pub(crate) const SELECTION_WASH_RGB: [u8; 3] = [0x4d, 0xa3, 0xff];

/// Selection wash strength (user decision 2026-07-28, chosen by eye on his own
/// A1 frames against 12% and 18% renders). Held here rather than inlined in
/// the UI because the user's stated plan is to promote it to a user setting —
/// a settings pane then writes the `selection-wash-opacity` property and no
/// other code changes. The `.slint` literals are inert fallbacks: Rust
/// overwrites both properties at construction, so THIS is the one default.
pub(crate) const SELECTION_WASH_OPACITY: f32 = 0.25;

/// Clamp for whatever eventually writes the wash strength. `with-alpha` has no
/// defined behavior outside 0..=1, and the stated plan is to expose this to a
/// settings pane — a stray 5.0 or -1 must not reach the renderer. Applied at
/// the single write site so a future settings path inherits it for free.
pub(crate) fn clamp_wash_opacity(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        SELECTION_WASH_OPACITY
    }
}

/// Last-set IPTC panel model contents (field rows, keyword chips,
/// template names): models rebuild ONLY when these change.
#[derive(Default, PartialEq)]
pub(crate) struct PanelCache {
    pub(crate) rows: Vec<(String, String, bool)>,
    pub(crate) chips: Vec<(String, String)>,
    pub(crate) names: Vec<String>,
}

/// M6 Copy Picks (fileops.md): the previewed plan, the running worker, and
/// what landed WHERE this session (the re-run skip default + the copied
/// badge — core owns the rule that a copy counts only while it is still
/// on disk).
///
/// The handle and its receiver are ONE thing — a running copy — and live
/// together so they can never be set or cleared apart.
#[derive(Default)]
pub(crate) struct CopyState {
    /// The previewed plan (rebuilt by `copy_replan`).
    pub(crate) plan: Option<fastcull_core::fileops::CopyPlan>,
    /// The chosen destination. Remembered ACROSS sessions (fileops.md:
    /// "destination and rename template survive across sessions") — see
    /// [`CopyState::begin_session`].
    pub(crate) dest: Option<std::path::PathBuf>,
    pub(crate) handle: Option<fastcull_core::fileops::CopyHandle>,
    pub(crate) rx: Option<std::sync::mpsc::Receiver<fastcull_core::fileops::CopyEvent>>,
    /// Image id -> the RAW path(s) it landed at, per destination, this
    /// session; re-checked against the disk by `copy_replan`.
    pub(crate) copies: fastcull_core::fileops::SessionCopies,
}

/// Burst grouping outputs (M7, burst-grouping.md), all indexed by IMAGE
/// id and all rebuilt together by `recompute_bursts` — at most once per
/// pump tick while metadata streams (`dirty`).
///
/// The three vectors are parallel and MUST stay the same length as the
/// session's per-image vectors; [`BurstIndex::new`] is the one place that
/// length is decided.
#[derive(Default)]
pub(crate) struct BurstIndex {
    /// Which group each image belongs to (None = a single).
    pub(crate) group_of: Vec<Option<usize>>,
    /// Badge count on the group's FIRST frame (0 = no badge).
    pub(crate) badge: Vec<usize>,
    /// "7/23" position inside the group.
    pub(crate) pos: Vec<Option<(usize, usize)>>,
    /// Set when frame metadata landed and the grouping is stale.
    pub(crate) dirty: bool,
}

impl CopyState {
    /// Everything a session swap must forget — which is everything EXCEPT
    /// the destination. fileops.md remembers that across sessions (the
    /// user copies a card at a time into the same shoot folder), so it is
    /// carried explicitly. Written as "replace the struct, naming what
    /// survives" rather than "clear four fields" so that a field added
    /// later is forgotten by default: forgetting is the safe direction.
    pub(crate) fn begin_session(&mut self) {
        *self = Self {
            dest: self.dest.take(),
            ..Self::default()
        };
    }
}

impl BurstIndex {
    /// A fresh index for a session of `count` images: no groups yet, and
    /// nothing to regroup until metadata arrives.
    pub(crate) fn new(count: usize) -> Self {
        Self {
            group_of: vec![None; count],
            badge: vec![0; count],
            pos: vec![None; count],
            dirty: false,
        }
    }
}

/// The IPTC panel's own state (M5, iptc-templates.md): whether the dock is
/// up, what its models currently show, and the one shared revert slot.
///
/// Not here: the per-image IPTC DATA (`iptc`, `touched_iptc`) and the
/// templates — those belong to the session, not to the panel, and they
/// outlive the panel being closed.
#[derive(Default)]
pub(crate) struct IptcPanelState {
    /// Is the dock on screen? Survives a session swap (see
    /// [`IptcPanelState::begin_session`]).
    pub(crate) visible: bool,
    /// Last-set panel model contents: the models are ONLY rebuilt when
    /// these differ (gate finding: rebuilding on every engine event tore
    /// down the field editors mid-typing).
    pub(crate) cache: PanelCache,
    /// ONE shared single-level revert slot (user decision): armed by every
    /// batch mutation from the panel; the ids the snapshots belong to ride
    /// alongside so revert lands on the right images even after re-sorts.
    pub(crate) revert: fastcull_core::iptc::RevertSlot,
    pub(crate) revert_ids: Vec<usize>,
    pub(crate) revert_label: String,
}

impl IptcPanelState {
    /// Everything a session swap must forget. The dock's VISIBILITY
    /// survives — it is a window layout the user chose, and a folder swap
    /// is not a reason to open or close a panel under their hands.
    pub(crate) fn begin_session(&mut self) {
        *self = Self {
            visible: self.visible,
            ..Self::default()
        };
    }
}

/// Every UI-side texture the app holds, plus the bookkeeping that decides
/// which rung each image is currently showing at.
///
/// This is a CACHE, not data: everything in here can be re-derived from
/// the engines (the SQLite thumb cache and the loupe LRU keep the pixels),
/// which is why a session swap can drop the whole thing without asking
/// anyone's permission.
#[derive(Default)]
pub(crate) struct TextureStore {
    /// Encoded thumbs by index (30–60 KB each); decoded lazily per window,
    /// bytes dropped after decode (the SQLite cache keeps the encoded copy).
    pub(crate) thumb_jpegs: HashMap<usize, Vec<u8>>,
    /// Decoded thumb textures, kept for the session (spec: thumbs are cheap).
    pub(crate) images: HashMap<usize, slint::Image>,
    /// Images whose decode failed (the strip's failed badge).
    pub(crate) failed: HashSet<usize>,
    /// Mid-rung textures (1616x1080, ~5 MB each) for intermediate zooms
    /// whose cells outgrow the 320 px thumb; pruned to the visible window.
    pub(crate) mids: HashMap<usize, slint::Image>,
    /// Bookkeeping for `mids` (core-side, tested by tests/zoom_walk.rs):
    /// which rung each held texture is, and what must be adopted from the
    /// engine cache when no event will fire.
    pub(crate) va: fastcull_core::viewassets::ViewAssets,
    /// UI-side textures for the focused image ± neighbors: sized to the
    /// prefetch ring (5) and cursor-protected on eviction (see
    /// insert_fullres); the core LRU holds the pixel data for rebuilds.
    pub(crate) fullres: Vec<(usize, slint::Image)>,
    /// Images whose best rung is mid-class-or-smaller but TERMINAL (the
    /// file's native size — bare JPEGs, issue #8): their small texture
    /// counts as the top rung for the zoom ceiling.
    pub(crate) terminal_native: HashSet<usize>,
}

/// The grid surface: which images are on screen, in what order, where the
/// cursor and the selection are, and the "what did the last refresh see"
/// bookkeeping that tells a re-sort apart from a scroll.
///
/// The cursor is an IMAGE id, never a position — positions change under it
/// every time the view re-sorts (issue #22, bug #46).
pub(crate) struct GridViewState {
    /// Index into `grid::ZOOM_COLUMNS`; the last step IS the loupe.
    pub(crate) zoom: usize,
    /// Grid zoom to return to when leaving the loupe with G/Esc.
    pub(crate) last_grid_zoom: usize,
    /// The cursor, as an image id.
    pub(crate) cursor: usize,
    /// False until the user first moves the cursor or marks (issue #4):
    /// while untouched, the cursor tracks the view's FIRST image through
    /// the progressive metadata re-sorts, so a folder never opens with
    /// the cursor stranded mid-grid (name order vs capture order).
    pub(crate) cursor_touched: bool,
    /// M5 filter/sort state: the grid binds to `view` (image ids passing the
    /// filter, in sort order); `cursor` remains an IMAGE id throughout.
    pub(crate) query: fastcull_core::filter::ViewQuery,
    pub(crate) view: Vec<usize>,
    /// Bumped on every recompute_view (membership or order change).
    pub(crate) view_generation: u64,
    /// The generation the last refresh saw: a mismatch means the view
    /// mutated under the cursor — re-sorts are never scrolling (issue
    /// #22).
    pub(crate) last_view_generation: u64,
    /// Whether the cursor's cell was ON SCREEN at the end of the previous
    /// refresh. The load-settled re-anchor consults it so that it restores a
    /// cursor the user was looking at, and leaves a BROWSING user's scroll
    /// alone — "scrolling is browsing, the cursor stays where the user
    /// parked it; it may leave the viewport" (cursor contract). Computed on
    /// the previous pass because the flip changes the cursor's POSITION, so
    /// asking after the re-sort answers a different question.
    pub(crate) last_cursor_visible: bool,
    /// Whether the previous refresh saw a fully loaded session (issue #25).
    /// The false->true edge is the ONE moment the view re-sorts from the
    /// provisional filename order into the user's chosen sort, and it is
    /// the only view mutation that reorders the WHOLE grid at once.
    pub(crate) last_metadata_complete: bool,
    /// Grid-area geometry (grid_width, viewport_h) at the last refresh:
    /// a change means RELAYOUT (panel toggle, window resize), not user
    /// scrolling — the loupe follow-scroll claim must not fire (issue
    /// #16: marks landed on a photo the user already left).
    pub(crate) last_view_geometry: Option<(f32, f32)>,
    /// Multi-selection (Shift+arrows, Ctrl+A; batch = selection in view
    /// order or the cursor — core model, tested).
    pub(crate) selection: fastcull_core::selection::Selection,
    /// Is the filter bar on screen? Survives a session swap.
    pub(crate) filter_bar_visible: bool,
}

/// Hand-written rather than derived: three of these do NOT start at the
/// type's zero. `zoom` starts at the 8-column step (the launch default),
/// `last_cursor_visible` starts TRUE so a folder's first refresh treats the
/// cursor as one the user is looking at, and the filter bar starts visible.
impl Default for GridViewState {
    fn default() -> Self {
        Self {
            zoom: 1, // 8 columns
            last_grid_zoom: 1,
            cursor: 0,
            cursor_touched: false,
            query: fastcull_core::filter::ViewQuery::default(),
            view: Vec::new(),
            view_generation: 0,
            last_view_generation: 0,
            last_cursor_visible: true,
            last_metadata_complete: false,
            last_view_geometry: None,
            selection: fastcull_core::selection::Selection::default(),
            filter_bar_visible: true,
        }
    }
}

impl GridViewState {
    /// Everything a session swap must forget. Two fields survive:
    ///
    /// - `zoom`, because the zoom step is decided by the LAUNCH path, not
    ///   by the folder: `--start-loupe` sets it before the first folder is
    ///   ever loaded, and File > Open Folder resets it to the grid right
    ///   after this returns. Defaulting it here would open every
    ///   `--start-loupe` run in the grid.
    /// - `filter_bar_visible`, for the same reason as the IPTC dock: it is
    ///   window layout the user chose, not session state.
    ///
    /// The filter itself does NOT survive (the query goes back to
    /// default): a hidden active filter on a fresh folder would look like
    /// missing files.
    ///
    /// Everything else — including `last_view_geometry`, which the old
    /// hand-written reset list had drifted into forgetting — goes back to
    /// its default.
    ///
    /// `last_grid_zoom` resets DELIBERATELY, even though its companion
    /// `zoom` is one of the two survivors above. The asymmetry is the
    /// point: `zoom` is where the view IS (decided by the launch path),
    /// while `last_grid_zoom` is the step to come BACK to when the loupe
    /// is left — a memory of the folder that just closed. Carrying it
    /// would exit the loupe in folder B at folder A's remembered step.
    /// Nothing observes this today (every path into a new folder is at
    /// the grid with it already at the default, and `open_folder_at`
    /// overwrites both right after this returns), so it is here for the
    /// swap-while-at-the-loupe caller that does not exist yet.
    pub(crate) fn begin_session(&mut self) {
        *self = Self {
            zoom: self.zoom,
            filter_bar_visible: self.filter_bar_visible,
            ..Self::default()
        };
    }

    /// Is the view at the loupe (the last zoom step, one column)?
    ///
    /// The zoom INDEX is the authority, not the column count the layout
    /// happens to produce: `GridLayout::new` derives columns from this
    /// same index (`ZOOM_COLUMNS[zoom.min(len-1)]`), and every writer of
    /// `zoom` keeps it inside the ladder, so `layout.columns == 1` — the
    /// second idiom this replaces — is exactly this predicate.
    pub(crate) fn at_loupe(&self) -> bool {
        self.zoom == grid::ZOOM_COLUMNS.len() - 1
    }

    /// The cursor's position in the current view (None = cursor image is
    /// filtered out or the view is empty).
    pub(crate) fn cursor_pos(&self) -> Option<usize> {
        self.view.iter().position(|id| *id == self.cursor)
    }
}

/// The loupe overlay: the decode engine that feeds it, WHERE the user is
/// looking (the desired factor and pan anchor), and the bookkeeping of what
/// was last drawn there.
///
/// Every field here is about ONE session's photographs — which is why a
/// session swap replaces the whole struct rather than resetting fields one
/// by one (see `load_folder`).
pub(crate) struct LoupeViewState {
    /// Full-res loupe assets (real sessions only).
    pub(crate) engine: Option<fastcull_core::loupe::LoupeEngine>,
    /// Decode announcements from the engine (drained by the pump).
    pub(crate) rx: Option<std::sync::mpsc::Receiver<fastcull_core::loupe::LoupeEvent>>,
    /// DESIRED loupe zoom factor relative to fit (ui-grid.md zoom ladder):
    /// 1.0 = fit, `f32::INFINITY` = 1:1 wanted before the full-res texture
    /// (and thus the real ceiling) is known. Clamped to the 1:1 ceiling at
    /// render time; the overlay shows only when the clamped factor > 1.
    pub(crate) zoom_factor: f32,
    /// Pan anchor as a fractional image coordinate (0..1). Persists across
    /// image navigation (contract: lock 1:1 on the eye, arrow through the
    /// burst); resets to center when returning to fit.
    pub(crate) pan_center: (f32, f32),
    /// The loupe offsets we last WROTE — trace-dedup bookkeeping ONLY
    /// since issue #46: Rust is the single writer of the offsets (the
    /// overlay has no Flickable), and a drag arrives as a `loupe-dragged`
    /// callback that mutates `pan_center` at the source — there is
    /// nothing to read back and no drag to infer. None while the overlay
    /// is hidden.
    pub(crate) last_pan_write: Option<(f32, f32)>,
    /// Residual HOLD (issue #46, spec'd in ui-grid.md): `(cursor, since)`
    /// while the overlay is keeping the PREVIOUS image's pixels because
    /// not even the cursor's own thumb exists yet. Bounded: a decode
    /// failure or `OVERLAY_HOLD_CAP` elapsing drops to fit honestly —
    /// never an unbounded wrong-pixels hold (persona condition).
    ///
    /// Since A3 this field is the hold's RECORD, not its rule: the
    /// presenter reduces it to `transit::HoldState` (is it this cursor,
    /// how long has it run) and `transit::render_rung` decides.
    pub(crate) overlay_hold: Option<(usize, std::time::Instant)>,
    /// `(cursor, was_thumb)` of the last SOFT render — trace dedup for
    /// the soft branch. Keyed on the rung, not just the cursor, so the
    /// thumb→mid upgrade of one image still traces (it is a visual
    /// change, and `transit_at_zoom_stays_soft_never_drops_to_fit`
    /// asserts the soft render is observable).
    pub(crate) last_soft_rung: Option<(usize, bool)>,
    /// The last FINITE factor the sharp overlay rendered at: during a
    /// transit whose desired factor is the INFINITY pin (Z), the soft
    /// view carries this value — visual continuity is the whole point
    /// of issue #21 (the carried magnification, not the sentinel).
    pub(crate) last_resolved_factor: Option<f32>,
    /// (cursor, mark) the loupe badge last traced — dedupes the trace
    /// line, not the property write (issue #20; the property is set
    /// every refresh, atomically with the image swap).
    pub(crate) last_badge: Option<(usize, i32)>,
    /// Which image the overlay last showed (trace bookkeeping only).
    pub(crate) last_overlay_cursor: Option<usize>,
}

/// Hand-written: the two fields that describe WHERE the user is looking do
/// not start at zero. Fit is 1.0 (0.0 would be a degenerate magnification)
/// and the pan anchor starts at the middle of the frame, not its top-left
/// corner.
impl Default for LoupeViewState {
    fn default() -> Self {
        Self {
            engine: None,
            rx: None,
            zoom_factor: 1.0,
            pan_center: (0.5, 0.5),
            last_pan_write: None,
            overlay_hold: None,
            last_soft_rung: None,
            last_resolved_factor: None,
            last_badge: None,
            last_overlay_cursor: None,
        }
    }
}

/// One folder's worth of session: the images, everything the user has said
/// about them, the engines producing data for them, and how far that
/// production has got.
///
/// The per-image vectors (`labels`, `paths`, `picks`, `capture_keys`,
/// `frame_meta`, `iptc`) are PARALLEL: index i means the same photograph in
/// all six, and an image id IS an index into them.
pub(crate) struct SessionState {
    pub(crate) labels: Vec<String>,
    /// RAW paths for real sessions (empty for --synthetic).
    pub(crate) paths: Vec<std::path::PathBuf>,
    /// Pick state per image (mirrors sidecars; synthetic = in-memory only).
    pub(crate) picks: Vec<fastcull_core::catalog::PickState>,
    /// EXIF capture sort keys, filled by MetadataReady events (None until
    /// metadata loads; keyless images sort after keyed ones by name).
    pub(crate) capture_keys: Vec<Option<String>>,
    /// Burst inputs per image (M7), from MetadataReady summaries.
    pub(crate) frame_meta: Vec<fastcull_core::burst::FrameMeta>,
    /// Per-image IPTC state (M5 panel): seeded from sidecars at open,
    /// edited by the panel, persisted via SidecarWriter::iptc.
    pub(crate) iptc: Vec<fastcull_core::iptc::IptcData>,
    /// Images whose pick the user changed this session: sidecar-at-open
    /// events must not overwrite fresh user intent.
    pub(crate) touched: HashSet<usize>,
    /// Images whose IPTC the user edited this session: a stale sidecar
    /// read racing the debounced write must not revert fresh intent
    /// (same guard as `touched` for picks — gate finding).
    pub(crate) touched_iptc: HashSet<usize>,
    /// The serialized sidecar writer. Dropping it FLUSHES pending marks
    /// (xmp-sidecars.md: flushed on session close), which is why the swap
    /// drops the old one before starting the new session's.
    pub(crate) writer: Option<fastcull_core::sidecar_writer::SidecarWriter>,
    /// Failed sidecar writes this session (surfaced in the status bar).
    pub(crate) sidecar_failures: usize,
    pub(crate) sidecar_errs:
        Option<std::sync::mpsc::Receiver<fastcull_core::sidecar_writer::WriteFailure>>,
    /// The thumbnail/metadata pipeline and its events. Receivers live in
    /// state so File > Open Folder can swap the whole session without
    /// restarting the event pump.
    pub(crate) pipeline: Option<Pipeline>,
    pub(crate) pipeline_rx: Option<std::sync::mpsc::Receiver<SessionEvent>>,
    /// Finished pipeline jobs (ThumbReady AND Failed both count).
    pub(crate) thumbs_done: usize,
    /// Templates + load warnings (templates.toml, read at session open —
    /// live-reload is read-on-open per spec).
    pub(crate) templates: Vec<fastcull_core::iptc::IptcTemplate>,
    pub(crate) template_warnings: Vec<String>,
    /// True for --synthetic sessions: cells get distinct placeholder hues;
    /// real folders use the spec's neutral gray.
    pub(crate) synthetic: bool,
    /// False until a session exists (folder opened or --synthetic). The
    /// folderless launch (issue #5) shows "No folder open" — a different
    /// message from "folder opened but it has no images".
    pub(crate) session_open: bool,
}

impl Default for SessionState {
    /// The pre-folder state: no images, no engines, and `session_open`
    /// false so the empty window says "No folder open" rather than "no
    /// images" (issue #5).
    fn default() -> Self {
        Self {
            labels: Vec::new(),
            paths: Vec::new(),
            picks: Vec::new(),
            capture_keys: Vec::new(),
            frame_meta: Vec::new(),
            iptc: Vec::new(),
            touched: HashSet::new(),
            touched_iptc: HashSet::new(),
            writer: None,
            sidecar_failures: 0,
            sidecar_errs: None,
            pipeline: None,
            pipeline_rx: None,
            thumbs_done: 0,
            templates: Vec::new(),
            template_warnings: Vec::new(),
            synthetic: false,
            session_open: false,
        }
    }
}

impl SessionState {
    /// A fresh session over `labels` (and `paths`, empty for --synthetic).
    /// The one place the per-image vectors' LENGTH is decided: picks,
    /// capture keys, frame metadata and IPTC are all sized from the label
    /// count here, so they cannot be sized differently by two callers.
    pub(crate) fn new(labels: Vec<String>, paths: Vec<std::path::PathBuf>) -> Self {
        let count = labels.len();
        // Synthetic sessions have no files, hence no paths; a real folder
        // must have exactly one path per image.
        debug_assert!(
            paths.is_empty() || paths.len() == count,
            "session has {} labels but {} paths",
            count,
            paths.len()
        );
        Self {
            picks: vec![fastcull_core::catalog::PickState::Unmarked; count],
            capture_keys: vec![None; count],
            frame_meta: vec![fastcull_core::burst::FrameMeta::default(); count],
            iptc: vec![fastcull_core::iptc::IptcData::default(); count],
            labels,
            paths,
            // A session exists the moment this is built; the empty-state
            // message distinguishes "no folder" from "empty folder".
            //
            // Note the direction change against the old `load_folder`,
            // which set this LAST, once the engines were up: the flag now
            // goes true at the TOP of a swap, before the sidecar writer,
            // the pipeline and the loupe engine are started. Nothing can
            // observe that window today — `load_folder` holds ONE mutable
            // borrow of the AppState across the entire swap, so no
            // refresh and no callback can run until the engines are in
            // place. That single borrow is the invariant this relies on:
            // an early return or a `?` inserted between `begin_session`
            // and `Pipeline::start` would release it with a session
            // claiming to be open and nothing behind it.
            session_open: true,
            ..Self::default()
        }
    }

    /// The `--synthetic N` session: N placeholder images, no files, no
    /// pipeline — and therefore no job that will ever complete, so the
    /// finished-job count is pre-filled or the status bar would claim
    /// "0/N loaded" forever on the very frames the screenshot suite
    /// captures.
    pub(crate) fn synthetic(n: usize) -> Self {
        Self {
            synthetic: true,
            thumbs_done: n,
            ..Self::new(
                (0..n).map(|i| format!("SYN{i:05}.ARW")).collect(),
                Vec::new(),
            )
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.labels.len()
    }

    /// Has every image's metadata job finished? (Issue #25: until it has,
    /// the view is ordered by filename — see `filter::view`.)
    ///
    /// `thumbs_done` counts BOTH `ThumbReady` and `Failed`, so it is the
    /// count of finished jobs, and EXIF is read inside that same job. A file
    /// that fails to READ therefore cannot strand the session in the
    /// provisional order — which counting `MetadataReady` alone would do,
    /// since the pipeline emits it only when the EXIF read succeeds.
    ///
    /// A read that never RETURNS still can: one worker wedged on a dying
    /// card or a stalled network mount leaves this one short forever, and
    /// the whole session stays in filename order with no way to force the
    /// sort. Accepted for now and recorded in ui-grid.md — the fix is a
    /// per-file give-up or a "sort anyway" affordance, not a smaller
    /// predicate.
    pub(crate) fn metadata_complete(&self) -> bool {
        self.thumbs_done >= self.labels.len()
    }
}

pub(crate) struct AppState {
    /// The open folder: its images, the user's judgements about them, and
    /// the engines producing data for them.
    pub(crate) session: SessionState,
    /// What the grid is showing and where the cursor is inside it.
    pub(crate) grid: GridViewState,
    /// The loupe overlay: its engine, its desire, and what it last drew.
    pub(crate) loupe_view: LoupeViewState,
    /// Every UI-side texture (thumbs, mids, full-res) and its bookkeeping.
    pub(crate) textures: TextureStore,
    /// Burst grouping outputs (M7), indexed by image id.
    pub(crate) bursts: BurstIndex,
    /// The IPTC panel surface (M5).
    pub(crate) iptc_panel: IptcPanelState,
    /// The Copy Picks dialog's state (M6).
    pub(crate) copy: CopyState,
    /// SURVIVOR: the one VecModel the window binds. It is not session
    /// data at all — replacing it would unbind the grid from the window,
    /// so refresh mutates it in place for the life of the process.
    pub(crate) cells: Rc<VecModel<CellData>>,
    /// SURVIVOR: the texture kitchen — a worker THREAD (plus its queue),
    /// not state. Every pixels->texture conversion happens on it
    /// (01-architecture.md: the UI thread never decodes). A session swap
    /// retargets it (dropping queued work and orphaning late completions
    /// via the generation fence) rather than replacing it, because
    /// restarting a thread per folder would be pure cost.
    pub(crate) kitchen: kitchen::Kitchen,
}

impl AppState {
    /// The state a freshly launched app starts in: no session, the grid at
    /// its default zoom, no textures. The two things it cannot default are
    /// passed in — the cell model the window is already bound to, and the
    /// texture kitchen, whose completion nudge needs a handle on the window.
    ///
    /// Launch flags (`--start-loupe`, `--start-11`) are applied by the
    /// caller through `enter_loupe`, not through more constructor
    /// arguments: they are a request to enter a view, and the app already
    /// has a word for that.
    pub(crate) fn new(cells: Rc<VecModel<CellData>>, kitchen: kitchen::Kitchen) -> Self {
        Self {
            session: SessionState::default(),
            grid: GridViewState::default(),
            loupe_view: LoupeViewState::default(),
            textures: TextureStore::default(),
            bursts: BurstIndex::default(),
            iptc_panel: IptcPanelState::default(),
            copy: CopyState::default(),
            cells,
            kitchen,
        }
    }

    /// Swap in a new folder: every group that describes ONE session is
    /// replaced wholesale, and the handful of things that outlive a
    /// session are named here rather than left implicit.
    ///
    /// This replaces the old hand-written list of ~45 field resets in
    /// `load_folder`. The difference that matters is not length: a field
    /// added to a group from now on is reset because it is PART OF THE
    /// GROUP, not because someone remembered to add a line here. The three
    /// fields the old list had already drifted into forgetting
    /// (`last_pan_write`, `last_overlay_cursor`, `last_view_geometry`) are
    /// covered by construction.
    ///
    /// Ordering: everything old goes down before anything new comes up.
    /// The old session's engines stop here and its sidecar writer is
    /// dropped — which FLUSHES its pending marks (xmp-sidecars.md) — so
    /// the caller's `SidecarWriter::start()` afterwards is on the far side
    /// of that barrier. The loupe engine and the writer have no ordering
    /// between them: they share nothing.
    pub(crate) fn begin_session(&mut self, labels: Vec<String>, paths: Vec<std::path::PathBuf>) {
        let count = labels.len();
        self.session = SessionState::new(labels, paths);
        self.loupe_view = LoupeViewState::default();
        self.textures = TextureStore::default();
        self.bursts = BurstIndex::new(count);
        self.grid.begin_session();
        self.iptc_panel.begin_session();
        self.copy.begin_session();
        // SURVIVOR: the kitchen is a worker thread. Retargeting bumps its
        // generation so queued work is dropped and late completions are
        // orphaned, without paying to restart a thread per folder.
        self.kitchen.retarget();
        // SURVIVOR: `cells` — the model the window is bound to.
        //
        // The per-image vectors are parallel by contract; assert it where
        // the count is actually known.
        debug_assert_eq!(self.session.picks.len(), count);
        debug_assert_eq!(self.session.capture_keys.len(), count);
        debug_assert_eq!(self.session.frame_meta.len(), count);
        debug_assert_eq!(self.session.iptc.len(), count);
        debug_assert_eq!(self.bursts.group_of.len(), count);
        debug_assert_eq!(self.bursts.badge.len(), count);
        debug_assert_eq!(self.bursts.pos.len(), count);
    }

    /// How many images the open session has. Shorthand for the session's
    /// own count — the call sites read the same as before.
    pub(crate) fn count(&self) -> usize {
        self.session.count()
    }

    /// Is the view at the loupe (the last zoom step, one column)? The
    /// predicate itself lives on [`GridViewState`]; this is the shorthand
    /// every controller already calls.
    pub(crate) fn at_loupe(&self) -> bool {
        self.grid.at_loupe()
    }

    /// Remember the grid zoom to come back to when the loupe is left.
    /// Saved on BOTH ways in: the jump (`enter_loupe`) and the last
    /// zoom-in step across the boundary.
    pub(crate) fn remember_grid_zoom(&mut self) {
        self.grid.last_grid_zoom = self.grid.zoom;
    }

    /// Climb from a grid zoom into the loupe at `factor` (fit = 1.0,
    /// `INFINITY` = "1:1 as soon as the ceiling is known"). Returns false
    /// when the view was already at the loupe — nothing was touched, and
    /// the caller's own already-there branch applies.
    pub(crate) fn enter_loupe(&mut self, factor: f32) -> bool {
        if self.at_loupe() {
            return false;
        }
        self.remember_grid_zoom();
        self.grid.zoom = grid::ZOOM_COLUMNS.len() - 1;
        self.loupe_view.zoom_factor = factor;
        true
    }

    /// Drop back to the remembered grid zoom, at fit and un-panned. A
    /// no-op when the view is not at the loupe. `last_grid_zoom` is
    /// clamped below the loupe step so a stale value can never park the
    /// exit back on the loupe itself.
    pub(crate) fn exit_loupe(&mut self) {
        if !self.at_loupe() {
            return;
        }
        self.loupe_view.zoom_factor = 1.0;
        self.loupe_view.pan_center = (0.5, 0.5);
        self.grid.zoom = self.grid.last_grid_zoom.min(grid::ZOOM_COLUMNS.len() - 2);
    }

    /// The cursor's position in the current view (None = cursor image is
    /// filtered out or the view is empty). Shorthand for the grid's own.
    pub(crate) fn cursor_pos(&self) -> Option<usize> {
        self.grid.cursor_pos()
    }

    /// Has every image's metadata job finished? Shorthand for the
    /// session's own predicate (issue #25).
    pub(crate) fn metadata_complete(&self) -> bool {
        self.session.metadata_complete()
    }
}
