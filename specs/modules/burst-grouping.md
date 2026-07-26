# Module spec: burst grouping (`burst.rs`)

## Purpose

Group frames shot in one continuous-drive squeeze so the UI can mark the
BOUNDARIES between groups (persona redesign 2026-07-26, adopted on the
user's delegation: with 85% of a wildlife session inside bursts, coloring
every member cell is a wall of paint that marks nothing and collides with
the cursor/selection borders — the information is where one squeeze ends
and the next begins). Grouping is display metadata only — it never
reorders images and has no effect on picks, sidecars, or copies.

## Algorithm

Input: session images sorted by capture time (DateTimeOriginal +
SubSecTimeOriginal; filename as tiebreaker).

Frames are partitioned by camera body FIRST (EXIF serial, else model;
identity-less frames share one partition): runs form within a body's own
frame sequence, with gaps measured between consecutive frames of the
SAME body. Two bodies shooting simultaneously — interleaved in capture
order — therefore group independently (criterion 4), and group members
need not be contiguous in the capture-sorted view. Consequence of the
partition (deliberate): a frame with NO camera identity (corrupt EXIF)
sitting mid-burst does not split the body's run — the body's frames
bridge over it, and the identity-less frame can only ever group with
other identity-less frames. A frame with identity but no timestamp
still splits its own body's run (unjoinable).

1. **Sony path**: maker-note `SequenceNumber` (0 = single shot, ≥1 =
   position in burst). A burst = maximal run where `SequenceNumber` ≥ 1,
   the capture-time gap to the previous frame is within threshold (rule
   3), AND the SequenceNumber did not RESET — a frame whose sequence
   number is ≤ its predecessor's starts a NEW group (persona algorithm
   fix: back-to-back squeezes fired inside the gap window are distinct
   bursts, and the reset is right there in the data).
2. **Generic path** (no usable sequence numbers): maximal run of frames
   from the same camera body (EXIF serial, else model) with consecutive
   gaps within threshold and run length ≥ 3 (avoids labeling two quick
   singles a burst).
3. **Gap threshold**: `max_gap` (default 600 ms) when BOTH neighbors
   carry SubSec precision. When EITHER lacks SubSec, timestamps have 1 s
   granularity and the effective threshold is `max(max_gap, 1 s)` —
   equal timestamps are gap 0 and a 1-second step is within-burst
   (persona algorithm fix: the old rule split every no-SubSec burst at
   each second boundary).
4. Group identity: groups carry a dense per-recompute index (nothing
   persists group identity in v1). A stable `BurstId` (e.g. hash of the
   first frame's path) becomes necessary only if identity must survive
   recomputes — stacks, post-v1.

`max_gap` and min run length are config values, not constants (fixed
defaults, no settings UI in v1).

## Behavior differences: Sony vs. other brands

Grouping quality is deliberately better on Sony bodies because the
maker-note sequence data is only parsed for Sony (in-tree reader,
`raw/sony.rs`; rawler exposes no maker notes). The observable
differences, which the M8 user guide must surface:

| Behavior | Sony (ARW) | Other brands |
| --- | --- | --- |
| Burst detection source | SequenceNumber + capture-time gaps | capture-time gaps only |
| Minimum burst length | 2 frames | 3 frames (`min_run`) |
| Back-to-back squeezes inside the gap window | split (sequence RESET) | merged into one group |
| 2-frame squeeze | grouped | shown as two singles |
| Exposure brackets (fast) | grouped (ReleaseMode2 ≠ 0 covers bracketing) | grouped only if 3+ within gaps |
| Malformed/absent maker note | falls back to the generic column | n/a (always generic) |

Non-Sony files are never worse than "time-only clustering" — the
generic path is the floor for every brand, Sony included (a corrupt
maker note degrades to it, never to an error).

## UI contract (persona-scoped v1)

- **Count badge** on each group's FIRST frame in the grid ("×23") — the
  boundary marker and depth gauge. MUST-HAVE.
- **`[` / `]` keys: previous/next burst boundary** — `]` jumps to the
  next frame in the current filtered view whose group differs from the
  cursor's (a group is "different" by group index; ungrouped singles
  count as their own territory). In a contiguous capture-sorted view
  that IS the next group's first frame; when members are non-contiguous
  (interleaved bodies, non-capture sorts) `]` deliberately follows view
  order and may land mid-group — never jumping backwards beats landing
  "first". `[` uses the CD-player convention (persona
  decision 2026-07-26): from mid-group it first RE-ANCHORS on the
  current group's first visible frame — the compare-against-the-opener
  move that happens several times per burst; only when already on the
  group's first visible frame does it cross to the previous
  group/single (also landing on its first visible frame). Reaching the
  previous group from mid-burst costs a free double-tap; the overshoot
  case after `]` is unchanged since `]` always lands on a first frame.
  Claims the cursor (untouched-cursor rule), carries
  loupe zoom/pan persistence exactly like arrows, never marks (G1
  untouched), clamps at the ends. MUST-HAVE — this is the feature: it
  replaces ~18 dead arrow presses per burst, ~120 times an evening.
- **Status bar**: "burst 7/23" appended when the cursor is inside a
  group (loupe and grid alike). USEFUL.
- **Edge strip** (optional polish): a thin 2-3px strip along the BOTTOM
  edge of member cells in TWO alternating muted tones — adjacent-group
  separation only; a cycling per-group palette buys nothing when groups
  are contiguous. Never a full-perimeter border (cursor/selection own
  those), never over the top-left badge corner.
- **Under non-capture sorts** groups are not contiguous: the strip is
  HIDDEN (never lie with fake contiguity); the count badge stays (still
  truthful per-frame); `[`/`]` follow view order.
- **CUT from v1** (persona IN-MY-WAY, adopted): the in-burst-only filter
  chip — filter chips are single-choice by recorded decision, so the
  chip would trade away Unmarked and break the inbox-zero loop; no
  workflow moment needs singles hidden. Revisit only as an orthogonal
  AND-toggle if ever requested.
- Stack/collapse stays post-v1; auto-collapse is disqualifying for
  frame-by-frame culling (the assumed style per the user's delegation).

## Acceptance criteria (tests)

- [x] Synthetic EXIF sets: single shots (Seq=0) never grouped; a 20 fps A1
      burst (50 ms gaps, Seq 1..N) forms one group; a 700 ms pause splits.
- [x] SequenceNumber RESET splits: two squeezes 300 ms apart (Seq 1..8,
      then Seq 1..5) form TWO groups.
- [x] Generic path: 3+ frames within gaps → group; 2 frames → no group.
- [x] Mixed bodies interleaved (two cameras shooting simultaneously)
      group independently.
- [x] No-SubSec: a burst spanning consecutive whole-second timestamps
      stays ONE group; a 2 s step splits.
- [x] `[`/`]` navigation over a filtered view: lands on first visible
      frames only; `[` re-anchors on the current group's first visible
      frame before crossing to the previous group; singles traversed one
      per press; claims the cursor; clamps at ends (core: next_boundary
      over (view, group-of) — pure function tests).
- [x] Integration: the real A1 test files (single shots) produce zero
      groups.
