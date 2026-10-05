# Brief 012 — the loupe's prefetch ring fits the memory budget: a small budget never decodes a frame it will not keep (issue #99)

Date: 2026-10-04. Issue #99 (recorded at brief 008's QE round 1 and its
decisions D20, by the senior developer). Branch `loupe-ring-fits-budget`
from `origin/main` (cut at f99a7ac while unit 011 ran its gate; rebased
onto unit 011's merge before any role touches it — M12). An engine change
in `fastcull-core` whose effect is visible only under a deliberately small
loupe memory setting — nothing changes at the 2 GB default — so no persona
gate (the persona accepted the follow-up at brief 008); a measured
before/after is the unit's evidence (M6).

## Context

Brief 008 made the loupe memory budget a setting (`performance.loupe_memory`,
default 2 GB, floor 200 MB). QE's round 1 then measured that a budget below
the ±PREFETCH window — five decoded A1 frames, ~746 MB — made the engine
re-decode window members for as long as the cursor rested at 1:1 (101
full-res decodes in 15 idle seconds at 0.5 GB, two cores busy); brief 008
D20 fixed the loop in the engine (a member evicted under a settled focus
waits for the next step). What remains, per the senior developer (brief
008's review, issue #99): every step at 1:1 still asks for the whole
window — ±`PREFETCH` (2) at rest, `TRANSIT_BEHIND`/`TRANSIT_AHEAD` (2/8)
while the user holds a key — and evicts what does not fit, so at 0.5 GB
each step decodes up to three frames and keeps two, and at the 200 MB floor
four frames keeping none but the one in focus: CPU and I/O spent on frames
the budget cannot hold. The setting's hint already tells the user how many
A1 frames the budget holds ("≈ 14 A1 frames" at the default).

## Goals

- G1. The ring asks only for frames the budget can keep: its size follows
  the budget, the focused frame first, the nearest neighbours next in the
  direction of travel.
- G2. At the default budget nothing changes — the ±2 and 2/8 windows fit
  with room — and the perf budgets do not move.
- G3. The change is measured before and after with QE's recipe (M6).

## Non-goals

- Nothing about the held-arrow pace or the ring's shape at the default
  (issue #60, parked by the user; the user reopens it or nobody does).
- No change to the budget's floor, its default, the setting, its hint or
  its note; no new setting.
- No change to which rung is shown (the render ladder) or to the kitchen.

## Applicable directives

- CLAUDE.md hard rule 5 (the rule is core's, a pure function with unit
  tests beside it); rule 6 (the perf budgets are regression-tested — the
  full-res row and the throughput row must not move); M6 (data before the
  decision: the before/after decode counts); M11 (the frame's size is the
  frame's — an A1 full-res frame is 149 MB, another body's differs — so
  the ring is sized from the bytes a decoded frame of THIS folder takes,
  never from an A1 constant).
- raw-pipeline.md "The loupe engine" (the ring in view order, ±PREFETCH,
  the transit window, the deferred-upgrade rule, the budget LRU floored at
  `BUDGET_FLOOR_BYTES`), "Memory" (the setting), the Contracts
  (`PREFETCH`, `TRANSIT_BEHIND`, `TRANSIT_AHEAD`, `LoupeEngine::start`);
  ui-grid.md "Transit and settled" (what is ASKED of the decoder, never
  what is displayed); settings.md "Loupe memory" (the hint, the note);
  test-harness.md (the `loupe engine started budget <bytes>` mark, the
  `loupe ready idx N long L` marks a test counts).
- M1 (spec first), M7, M12 (the rebase before the senior developer
  starts).

## Requirements

- R1. **A pure ring rule in core.** `loupe.rs` gains a function the senior
  developer names — inputs: the budget in bytes, the bytes one decoded
  full-res frame of this folder takes (R2), the window the engine would
  ask for (behind, ahead); output: the window it asks for, with
  `1 + behind + ahead ≤ max(1, floor(budget / frame_bytes))` — the focused
  frame always, then the neighbours nearest the focus, the travel
  direction's side first when the window is asymmetric, shrinking the far
  side first. At the floor (200 MB, one A1 frame) the ring is the focused
  frame alone. At the default (2 GiB, 14 A1 frames) both windows (5 and
  11 frames) are unchanged.
- R2. **The frame size is the folder's, not a constant** (M11): the bytes
  of one decoded full-res frame come from the frames the engine knows —
  the largest decoded size seen in this session, or the dimensions the
  EXIF summary already carries, before any decode — never from
  `A1_FRAME_BYTES`; the senior developer names the source and what the
  engine assumes before the first decode lands.
- R3. **The engine asks for that ring** at every focus and `set_view` (rest
  and transit), and the deferred-upgrade revival respects it too (a
  neighbour outside the budget's ring is dropped, never revived). The
  eviction rule of brief 008 D20 stands.
- R4. **The mark says what was asked**: `loupe engine started budget <bytes>`
  gains the ring the budget allows at start (`ring <behind>/<ahead>` at
  rest and in transit, for the frame size assumed), and a per-focus mark
  or the existing `loupe ready` marks let a test count decodes per step.
- R5. **Spec and docs, same commit**: raw-pipeline.md's ring paragraph and
  Memory bullet state the rule once; settings.md's "Loupe memory" row
  points at it in one sentence (the note is core's text — unchanged unless
  the senior developer finds it now false); test-harness.md's mark; no
  docs page changes unless a user-facing sentence does (docs/settings.md
  says the budget is "memory for decoded full-size frames … so that
  stepping back does not re-decode" — true before and after).
- R6. **Tests and measurement.** Core: the pure rule's table (default →
  unchanged windows; 0.5 GB with A1 frames → 3 frames: focus + the two
  nearest, travel side first; the floor → focus alone; a body whose frame
  is 60 MB → more frames per GB; a 100 MP body's → fewer); the engine with
  a tight budget decodes no frame outside the ring (count `Ready` events
  per step against the ring size; the mutant: the ring rule bypassed →
  more decodes than the ring holds). App: the existing loupe memory test
  reads the new mark. Measurement (M6), before and after, recorded in the
  brief or the commit: QE's recipe — release build, 60 real-size frames,
  0.5 GB and the 200 MB floor, idle at 1:1 on one frame and tap-stepping
  ten frames — decodes per step and CPU seconds; and at 2 GB, identical
  counts before and after. The perf budgets green in release on the idle
  seat.

## Acceptance criteria (they also land in raw-pipeline.md's ledger)

- AC1. The ring rule holds for the table in R6; at the default both
  windows are unchanged.
- AC2. With a tight budget the engine decodes no frame outside the ring;
  the mutant is red.
- AC3. The frame size comes from the folder's frames, never from an A1
  constant.
- AC4. The start mark names the ring; the loupe memory test reads it.
- AC5. The before/after measurement is recorded: fewer decodes per step at
  0.5 GB and at the floor, the same at 2 GB; the perf budgets green.
- AC6. raw-pipeline.md and settings.md say so.

## Decisions log

- D1 (2026-10-04, Manager): no persona gate — invisible at the default;
  the persona accepted the follow-up at brief 008.
- D2 (2026-10-04, Manager, M12): the branch was cut while unit 011 ran its
  gate; it is rebased onto unit 011's merge before any role touches it.
