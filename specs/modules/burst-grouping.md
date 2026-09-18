# Module spec: burst grouping (`burst.rs`)

## Purpose

Group frames shot in one continuous-drive squeeze so the UI can mark the
BOUNDARIES between groups (persona redesign 2026-07-26, adopted on the
user's delegation: with 85 % of a wildlife session inside bursts, colouring
every member cell is a wall of paint that marks nothing; the information is
where one squeeze ends and the next begins). Grouping is display metadata
only — it never reorders images and has no effect on picks, sidecars or
copies.

## Behaviour

### Grouping

Input: the session's images in TRUE capture order (DateTimeOriginal +
SubSecTimeOriginal, filename as tiebreaker) — never the provisional order a
loading folder shows, because a burst is a fact about capture times.

Frames are partitioned by camera body FIRST (EXIF serial, else model;
identity-less frames share one partition): runs form within a body's own
sequence, with gaps measured between consecutive frames of the SAME body.
Two bodies shooting simultaneously therefore group independently, and group
members need not be contiguous in the capture-sorted view. A frame with no
camera identity (corrupt EXIF) sitting mid-burst does not split the body's
run — the body's frames bridge over it, and it can only ever group with
other identity-less frames. A frame with identity but no timestamp splits
its own body's run.

1. **Sony path**: the maker-note `SequenceNumber` (0 = single shot, ≥ 1 =
   position in the burst). A burst is a maximal run where the number is
   ≥ 1, each capture-time gap is within threshold (rule 3), AND the number
   did not RESET — a frame whose number is ≤ its predecessor's starts a new
   group, so back-to-back squeezes fired inside the gap window are distinct
   bursts.
2. **Generic path** (no usable sequence numbers): a maximal run of frames
   from the same body with consecutive gaps within threshold and length
   ≥ 3 (`min_run`; two quick singles are not a burst).
3. **Gap threshold**: `max_gap` (default 600 ms) when BOTH neighbours carry
   SubSec precision. When either lacks it, timestamps have 1 s granularity
   and the threshold is `max(max_gap, 1 s)`: equal timestamps are gap 0 and
   a one-second step is within-burst.
4. Groups carry a dense per-recompute index; nothing persists group
   identity in v1. A stable `BurstId` becomes necessary only if identity
   must survive recomputes — stacks, post-v1.

`max_gap` and `min_run` are config values with fixed defaults; no settings
UI in v1. Non-Sony files are never worse than time-only clustering: the
generic path is the floor for every brand, Sony included — a corrupt maker
note degrades to it, never to an error.

### Sony vs other brands

Grouping is deliberately better on Sony bodies, because the maker-note
sequence is parsed only for Sony (in-tree reader `raw/sony.rs`; rawler
exposes no maker notes). The user guide surfaces the differences
(docs/culling.md):

| Behavior | Sony (ARW) | Other brands |
| --- | --- | --- |
| Burst detection source | SequenceNumber + capture-time gaps | capture-time gaps only |
| Minimum burst length | 2 frames | 3 frames (`min_run`) |
| Back-to-back squeezes inside the gap window | split (sequence RESET) | merged into one group |
| 2-frame squeeze | grouped | shown as two singles |
| Exposure brackets (fast) | grouped (ReleaseMode2 ≠ 0 covers bracketing) | grouped only if 3+ within gaps |
| Malformed/absent maker note | falls back to the generic column | n/a (always generic) |

### The UI

- **Count badge** on each group's first frame in the grid (`×23`) — the
  boundary marker and depth gauge; visible in the loupe too (ui-grid.md's
  badge policy). MUST-HAVE.
- **`]` / `[` — next / previous burst boundary.** `]` jumps to the next
  frame in the current filtered view whose group differs from the cursor's
  (ungrouped singles count as their own territory); in a contiguous
  capture-sorted view that IS the next group's first frame; with
  non-contiguous members (interleaved bodies, non-capture sorts) it follows
  view order and may land mid-group — never jumping backwards beats landing
  "first". `[` uses the CD-player convention (persona decision 2026-07-26):
  from mid-group it first RE-ANCHORS on the current group's first visible
  frame — the compare-against-the-opener move — and only from there crosses
  to the previous group or single, landing on its first visible frame.
  Both claim the cursor, carry loupe zoom/pan persistence like the arrows,
  never mark, and clamp at the ends. A plain `[`/`]` collapses the
  selection like every unmodified move, and Ctrl+`[`/`]` jumps the same way
  with the selection kept — the selection rule of ui-grid.md (brief 002).
  MUST-HAVE: it replaces ~18 dead arrow presses per burst, ~120 times an
  evening.
- **Shift+`[` / Shift+`]` — extend the selection by whole bursts** (issue
  #55, 2026-08-28). One rule: the cursor lands exactly where `[`/`]` would,
  and every WHOLE burst between the anchor's burst and the cursor's burst is
  selected — a single counts as a one-frame burst, and in a contiguous
  capture-sorted view a burst is never taken by half. From a burst's opener
  Shift+`]` selects that burst plus the next territory in one press
  ("burst 40 plus burst 41"); again adds the next; after a burst span the
  opposite key drops a whole burst and flips past the anchor burst like
  Shift+arrows flip (from a frame-precise arrow span the first Shift+`[`
  completes the cursor's burst before it can drop anything). From
  mid-burst, Shift+`[` re-anchors on the opener like `[` does, which selects
  just this burst with the cursor on its opener. The anchor arms at the
  pre-press cursor (or stays where a live Shift gesture put it); when it
  arms FRESH the burst span is the whole selection, Ctrl-added frames
  included (brief 002); and it is widened to its burst's far edge, so a
  Shift+arrow that follows is frame-precise from the burst's edge. The
  result is always a view-order RANGE like every Shift gesture: with
  interleaved bodies or a non-capture sort the frames between come along,
  and the OTHER body's burst can straddle the range's edge (body 1 =
  {1,3,5}, body 2 = {2,4,6}: Shift+`]` from single 0 selects 0..=5 and
  leaves 6 out) — Ctrl+Shift+B is the exact tool there. Landing on the
  opener, not the selection's last frame, keeps the `]` rhythm and makes
  "look ahead with `]`, then Shift+`[` to grab the previous burst" work.
  On a US layout the keys arrive as `}` and `{`; both spellings work. Loupe
  and grid alike. Core: `Selection::extend_bursts`.
- **Ctrl+Shift+B — select this burst** (the user's proposal, 2026-08-28):
  every frame of the burst under the cursor that is in the current view
  joins the selection (a single selects itself). The cursor does NOT move —
  from frame 9/23, having compared it to the opener, one chord selects the
  burst for a caption without losing the place. ADDITIVE, like Ctrl+click
  and Ctrl+Space, so two non-adjacent bursts are Ctrl+Shift+B, Ctrl+`]`×n,
  Ctrl+Shift+B (a plain `]` would collapse the selection); IDEMPOTENT (a
  double-tap changes nothing). Members hidden by the filter stay unselected
  — what you see is what you stamp. Arms the Shift anchor on this burst.
  Listed in the shortcuts card; no menu entry. Core:
  `Selection::select_group`.
- **Esc always clears the selection**, from the loupe too (user decision
  2026-08-28, the persona's condition for the chords: the loupe shows no
  wash, so a one-press 40-frame selection made there and forgotten would
  silently take the next caption). The full rule: ui-grid.md's keyboard map.
- **Status bar**: `burst 7/23` appended when the cursor is inside a group,
  loupe and grid alike.
- **Edge strip** (optional polish): a thin 2-3 px strip along the BOTTOM of
  member cells in two alternating muted tones — adjacent-group separation
  only. Never a full-perimeter border (the cursor and the selection own
  those), never over the top-left badge corner.
- **Under non-capture sorts** groups are not contiguous: the strip is hidden
  (never fake contiguity); the badge stays (truthful per frame); `[`/`]`
  follow view order.
- **Cut from v1** (persona IN-MY-WAY, adopted): the in-burst-only filter
  chip — chips are single-choice, so it would trade away Unmarked and break
  the inbox-zero loop. Stack/collapse stays post-v1; auto-collapse is
  disqualifying for frame-by-frame culling.

## Contracts

- Pure functions in core, the app only dispatches: the grouping
  (`BurstIndex`, `next_boundary` over (view, group-of)),
  `Selection::extend_bursts`, `Selection::select_group`.
- Grouping reads `filter::view_true_sort`; `[`/`]` resolve over VIEW
  positions, so they walk oddly over a name-ordered view while a folder
  loads (ui-grid.md, *Provisional order while loading*).
- `--synthetic N --bursts` builds a fixed Sony-style pattern of singles and
  bursts for driven tests: the real test RAWs are three single shots
  (test-harness.md).

## Acceptance criteria

- [x] Synthetic EXIF sets: single shots (Seq=0) never group; a 20 fps A1
      burst (50 ms gaps, Seq 1..N) forms one group; a 700 ms pause splits;
      a sequence RESET splits two squeezes 300 ms apart into two groups;
      the generic path groups 3+ frames within gaps and not 2; mixed bodies
      interleaved group independently; a no-SubSec burst spanning
      consecutive whole seconds stays one group and a 2 s step splits;
      duplicate sequence numbers never group — `burst.rs` unit tests.
- [x] `[`/`]` over a filtered view: first visible frames only; `[`
      re-anchors before crossing; singles one per press; claims the cursor;
      clamps at the ends — `next_boundary` pure-function tests.
- [x] The real A1 test files (single shots) produce zero groups.
- [x] Shift+`[`/`]`: from an opener selects that burst plus the next
      territory; again adds; the opposite key drops a whole burst, never
      half; flips past the anchor burst; from mid-burst the anchor's burst
      is taken whole and Shift+`[` selects just this burst; a Shift+arrow
      after a burst span is frame-precise; interleaved members select the
      view range; a filtered-out anchor spans nothing —
      `Selection::extend_bursts` tests.
- [x] Ctrl+Shift+B: the burst's members in the view, cursor unmoved,
      additive, idempotent, a single selects itself, hidden members stay
      unselected, a filtered-out cursor selects nothing, Shift+`]` afterwards
      extends from it — `Selection::select_group` tests.
- [x] Driven through real key events over `--synthetic N --bursts`:
      Shift+`]`/`[` with the modifier held and as `}`/`{`, Ctrl+Shift+B,
      Esc clearing at a grid zoom and from inside the loupe, G from the loupe
      keeping it — `burst_keys_select_whole_bursts_and_esc_clears`,
      `esc_clears_a_burst_selection_from_inside_the_loupe`.
- [x] Ctrl+`[`/`]` keep the selection across a hop and a plain hop drops it
      (brief 002): Ctrl+Shift+B, Ctrl+`]`×n, Ctrl+Shift+B → both bursts; a
      plain `]` → empty; a fresh Shift+`]` after Ctrl-navigation replaces
      the Ctrl-added frames —
      `ctrl_navigation_keeps_the_selection_and_ctrl_space_toggles`,
      `a_plain_move_collapses_the_selection_in_the_grid`; core:
      `a_fresh_burst_span_replaces_ctrl_added_frames`.

## History

- 2026-09-17 — Rewritten (brief 007). The pre-rewrite text is
  `specs/history/burst-grouping.md`.
- 2026-09-06 — A plain `[`/`]` collapses the selection; Ctrl+`[`/`]` keep
  it (brief 002, user decision).
- 2026-08-28 — Shift+`[`/`]`, Ctrl+Shift+B and Esc from the loupe (issue
  #55, v0.12.0; persona USEFUL on both, the user's chord).
- 2026-07-26 — M7 (`60bcef6`): the persona's redesign to boundaries, the
  CD-player `[`, the no-SubSec and sequence-reset fixes, the in-burst chip
  cut.
