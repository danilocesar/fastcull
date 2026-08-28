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
- **Shift+`[` / Shift+`]`: extend the selection by whole bursts** (issue
  #55; persona 2026-08-28, USEFUL, shipped with Ctrl+Shift+B below). One
  rule: the cursor lands exactly where `[`/`]` would land it, and every
  WHOLE burst between the anchor's burst and the cursor's burst is
  selected — a single counts as a one-frame burst, and in a contiguous
  capture-sorted view a burst is never taken by half. From a burst's opener (where `]` leaves you) Shift+`]`
  selects that burst plus the next territory in one press — "burst 40
  plus burst 41", the heron taking off and landing. Pressing again adds
  the next burst; after a burst span the opposite key drops a whole
  burst, and flips past the anchor burst like Shift+arrows flip (from a
  frame-precise arrow span the first Shift+`[` completes the cursor's
  burst before it can drop anything — the one rule, applied). From mid-burst, Shift+`[`
  re-anchors on the opener like `[` does, which selects JUST this burst
  with the cursor on its opener. The anchor arms at the pre-press cursor
  (or stays where a live Shift gesture put it) and is widened to its
  burst's far edge, so a Shift+arrow that follows is frame-precise from
  the burst's edge ("40 plus the first two frames of 41" — persona: one
  rule for what a Shift extension is, not two). The result is always a
  view-order RANGE like every other Shift gesture: with interleaved
  bodies or a non-capture sort the frames between the two bursts come
  along, as Shift+arrows would take them (exact-member islands would be
  a selection the wash cannot show honestly) — and there the OTHER
  body's burst can straddle the range's edge (validator 2026-08-28: body
  1 = {1,3,5}, body 2 = {2,4,6}, Shift+`]` from single 0 selects 0..=5
  and leaves 6 out); only the two bursts the gesture spans are widened
  whole. Two-body shoots that need an exact burst use Ctrl+Shift+B. Landing on the opener,
  not on the selection's last frame, is the persona's call: the `]`
  rhythm survives releasing Shift, the last frame of a burst is the
  least interesting one to look at, and "look ahead with `]`, then
  Shift+`[` to grab the previous burst too" only works with symmetric
  rules. Keyboard honesty: on a US layout Shift+`]` arrives as `}`, so
  `{`/`}` are the same keys, with or without a reported Shift modifier.
  Loupe and grid alike; claims the cursor; carries loupe zoom/pan
  persistence like `]`. Core: `Selection::extend_bursts`.
- **Ctrl+Shift+B: select this burst** (user proposal 2026-08-28, persona
  USEFUL): every frame of the burst under the cursor that is in the
  current view joins the selection (a single selects itself). The cursor
  does NOT move — the point of it: from frame 9/23, having just compared
  it to the opener, one chord selects the burst for a caption without
  losing the place (Shift+`[` would throw the cursor to the opener).
  ADDITIVE (a union, like Ctrl+click — that is what makes non-adjacent
  bursts cheap: Ctrl+Shift+B on 40, `]`×7, Ctrl+Shift+B on 47) and
  IDEMPOTENT (a second press changes nothing; a toggle on a 23-frame
  chord would empty the selection on a double-tap). Members hidden by the
  filter stay unselected — what you see is what you stamp, so a Picked
  filter stamps the keepers and never the hidden rejects. Arms the Shift
  anchor on this burst, so Shift+`]` afterwards extends from it. A chord
  for the reason Ctrl+Shift+E is one: it acts on many frames. Listed in
  the shortcuts popup; no menu entry (persona: SHRUG — nobody opens a
  menu at 9 pm for this). Core: `Selection::select_group`.
- **Esc always clears the selection** — from the loupe too (user decision
  2026-08-28, the persona's one pre-condition for shipping the chords):
  the loupe shows no wash, so a one-press 40-frame selection made there
  and forgotten would silently take the next caption; the cancel key
  works where the selection was made. Full rule in ui-grid.md's keyboard
  table (G is unchanged).
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
- [x] Shift+`[`/`]` (issue #55): from a burst's opener Shift+`]` selects
      that burst whole plus the next territory; again adds the next burst;
      the opposite key drops a whole burst, never half (contiguous
      view); flips past the
      anchor burst; from mid-burst the anchor's burst is taken whole, and
      Shift+`[` selects just this burst; a Shift+arrow after a burst span
      is frame-precise from the burst's edge; interleaved members select
      the view range; a filtered-out anchor spans nothing (core:
      `Selection::extend_bursts` tests).
- [x] Ctrl+Shift+B (issue #55): the burst's members in the view, cursor
      unmoved, additive with what is held, idempotent, a single selects
      itself, hidden members stay unselected, a filtered-out cursor
      selects nothing, and Shift+`]` afterwards extends from it (core:
      `Selection::select_group` tests).
- [x] Driven through real key events over `--synthetic N --bursts` (a
      fixed Sony-style pattern — the real test RAWs are single shots):
      Shift+`]`/Shift+`[` with the modifier held, the `}`/`{` spellings,
      the Ctrl+Shift+B chord, Esc clearing the selection at a grid zoom
      AND from inside the loupe, G from the loupe keeping it; the cursor
      id and selection count are observable in the QEDUMP line (app:
      `burst_keys_select_whole_bursts_and_esc_clears`,
      `esc_clears_a_burst_selection_from_inside_the_loupe`).
