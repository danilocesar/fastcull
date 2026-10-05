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

## Measurements (M6)

The recipe (senior developer, 2026-10-05; the after table repeats it
exactly): a release build of the head (2e8a2fd) in the tree's own
`target/`; a folder of 60 symlinks to `testdata/raws/A1_full_compressed.ARW`
under the scratch tree; `FASTCULL_NO_CACHE=1 FASTCULL_NO_CONFIG=1
FASTCULL_TRACE=1`, `FASTCULL_CONFIG_DIR` pointing at a scratch dir whose
`settings.toml` is `[performance]` / `loupe_memory = "<n>"` (an empty dir
for the default); the drive script

```
1500:wait:load settled gen 0;2000:key:right;2200:key:right;…(ten taps 200 ms
apart, the cursor on frame 10)…;3800:key:right;4300:key:z;4700:dump.at11;
19300:key:right;20300:key:right;…(ten taps 1 s apart)…;28300:key:right;
29300:key:left;30300:key:left;31300:key:left;33800:dump.end;34000:quit
```

(`Z` from the grid is the first loupe focus of the session, at 1:1 — the
engine is focused only `at_loupe`, presenter.rs); the whole run under
bash's `time`. Full-res decodes are the `loupe ready idx N long 8640`
lines on stderr, counted between consecutive `drive:` echoes: "rest" is
`key:z` to the first `key:right`, "step k" the k-th forward tap to the next
echo, "back k" the k-th `key:left`. Every full-res ladder also lands a mid
(`long 1616`) first; those are not counted. Two runs per budget; the seat
is the i7-8665U laptop, idle, Wayland, GPU renderer.

**Before** (head 2e8a2fd):

| budget (`loupe engine started budget`) | rest, 15 s | full-res decodes per forward step (ten) | per back step (three) | total from `Z` | CPU user+sys over wall |
|---|---|---|---|---|---|
| 2 GiB default (2147483648) | 5 | 1,1,1,1,1,1,1,1,1,1 (both runs) | 0,0,0 (both) | 15 / 15 | 13.1 s / 34.2 s; 12.8 s / 34.3 s |
| 0.5 GB (536870912) | 5 | 3,3,3,2,3,2,2,3,2,3 — 2,3,3,3,2,2,3,2,2,2 | 2,2,2 — 2,3,2 | 37 / 36 | 25.4 s / 34.2 s; 24.5 s / 34.1 s |
| 0.2 GB (214748365 — the setting's 0.2 GiB, 5 MB above the engine's 200 MiB floor) | 5 | 4 at every step (both runs) | 4,4,4 (both) | 57 / 57 | 38.2 s / 34.1 s; 39.3 s / 34.2 s |

The mechanism, read in the code and visible in the indexes decoded: every
settled focus asks for the whole ±`PREFETCH` window (`focus_plan`), the
byte LRU (`evict_to_budget`) keeps `⌊budget ÷ 149 MB⌋` of it — 14, 3 and 1
frames — and brief 008 D20's hold keeps the rest quiet while the cursor
rests (the rest column is 5 and then nothing at every budget). A step
re-asks the window whole, so at 0.5 GB each step decodes the new frame
plus one or two the LRU let go (which two is the LRU's tie-break — every
re-focus stamps the cached members alike, hence 2 or 3), and at the floor
all four neighbours, every step, including the two decoded one second
earlier. Frames kept: at the default the back steps decode nothing (the
window was kept); at 0.5 GB two of the five; at the floor none but the
focused frame. The expected after, from the rule: at 0.5 GB the ring is
1/1 — rest 3, one decode per forward step, one per back step, 16 in all
(~13 s CPU); at the floor 0/0 — rest 1, one per step, 14 in all; at the
default unchanged.

**After** (head 4436eda, 2026-10-05, the developer; the same recipe, two
runs per budget; `loupe ring` marks: `rest 2/2`, `1/1`, `0/0`, `transit
2/8` at all three):

| budget (`loupe engine started budget`) | rest, 15 s | full-res decodes per forward step (ten) | per back step (three) | total from `Z` | CPU user+sys over wall |
|---|---|---|---|---|---|
| 2 GiB default (2147483648) | 5 (both runs) | 1,1,1,1,1,1,1,1,1,1 (both runs) | 0,0,0 (both) | 15 / 15 | 13.2 s / 34.3 s; 13.2 s / 34.3 s |
| 0.5 GB (536870912) | 3 (both) | 1 at every step (both runs) | 1,1,1 (both) | 16 / 16 | 13.6 s / 34.2 s; 13.4 s / 34.2 s |
| 0.2 GB (214748365) | 2 (both) | 1 at every step (both runs) | 1,1,1 (both) | 15 / 15 | 13.4 s / 34.2 s; 13.4 s / 34.2 s |

The floor's rest is 2, not the rule's 1: both backlog workers took 10 and
11 from the uncapped first queue, the first parse culled 8, 9 and 12, the
running decode of 11 completed and 10's landing evicted it, so step 1
decoded 11 again — D4's bounded residual (one neighbour, once per session,
below two frames). Perf budgets in release on the idle seat: all six
green, `budget_fullres_decode_under_350ms` median 286.4 ms,
`budget_pipeline_throughput_over_60_per_sec` green.

## Decisions log

- D1 (2026-10-04, Manager): no persona gate — invisible at the default;
  the persona accepted the follow-up at brief 008.
- D2 (2026-10-04, Manager, M12): the branch was cut while unit 011 ran its
  gate; it is rebased onto unit 011's merge before any role touches it.
- D3 (2026-10-05, senior developer's plan, confirmed by the Manager 2026-10-05):
  R1's `frame_bytes` is the bytes of the RUNG the window asks for, not one
  full-frame figure — the full-res frame for a request above what the mid
  serves (1:1; fit on a 4K display), the mid preview when the mid serves
  it (transit; fit on a ≤2K display). A transit ring asks for ~5 MB mids:
  capped by a 149 MB measure it would shrink to the focused frame at the
  floor for no memory reason (eleven A1 mids are 58 MB and fit under the
  floor's 200 MiB beside one full frame), losing the look-ahead the
  transit contract exists for. With the mid's bytes the transit window is
  unchanged at every budget above 58 MB — i.e. always.
- D4 (2026-10-05, senior developer's plan, confirmed by the Manager 2026-10-05):
  before the first header of a session is parsed the engine assumes
  nothing and asks for the uncapped window; the first parse (the focused
  frame's own, ~1 ms into its decode) sizes the ring and culls the
  focus-origin queue entries the cap excludes. Residual: what the workers
  took in that millisecond — at most one neighbour, once per session, and
  only when the budget holds fewer than two frames (the two backlog
  workers pop the focused frame and its nearest neighbour; the reserved
  lane is still in its 250 ms debounce). Rejected: assuming a size at
  start (M11 — there is none to assume); assuming "the focused frame
  alone" until a parse succeeds, which would leave a corrupt first frame's
  neighbours unprefetched — the promise `corrupt_file_reports_failed_and_
  engine_survives` pins.
- D5 (2026-10-05, senior developer's plan, confirmed by the Manager 2026-10-05):
  R4's mark is a NEW mark, `loupe ring budget <B> frame <F> rest <b>/<a>
  transit <b>/<a>`, emitted when the engine first knows a frame size and
  on every change, not an extension of `loupe engine started budget` — at
  start no size is known, and a mark printed from an assumed one would be
  constant across budgets, the D23 shape (a mark that cannot go red). AC4
  reads accordingly; the loupe memory test enters the loupe in its 0.5 GB
  session and waits on `rest 1/1`.
- D6 (2026-10-05, senior developer's plan, confirmed by the Manager 2026-10-05):
  R5's "no docs change" does not hold — docs/settings.md said the loupe
  "cannot keep all five frames … so a step at 1:1 decodes again the frames
  it had to let go", which the cap makes false; the sentence follows the
  spec in the same commit. ui-grid.md's SETTLED-AND-IDLE row ("full-res
  look-ahead on the ±PREFETCH neighbours") gets a pointer clause, since it
  is the contract for what is ASKED of the decoder.
- D7 (2026-10-05, Manager): the held-arrow (transit) prefetch ring stays
  wide — a ring of ~5 MB mids is capped only by its own rung's bytes (D3),
  so a small budget narrows the sharp 1:1 neighbours and not what a held
  arrow prefetches; the brief's non-goal and the parked #60 both point that
  way, and the Manager answers it under M2 rather than putting it to the
  user (the senior developer offered the question; the recommendation is
  this). raw-pipeline.md's D20 ledger criterion is re-opened `- [ ]` with
  its reason until the re-stated test
  (`a_budget_below_the_prefetch_window_goes_quiet_when_idle`, four phases)
  lands — a promise re-stated, not a promise dropped (M1). The AFTER
  measurement is the developer's, with the brief's recipe exactly (two
  runs per budget), recorded in the Measurements section and the commit.
