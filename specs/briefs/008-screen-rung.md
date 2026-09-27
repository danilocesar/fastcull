# Brief 008 — a screen-sized rung so a held arrow stays sharp on 4K (issue #60)

Date: 2026-09-26. Issue #60 (HIGH, feature-request; parked 2026-08-29,
reopened by the user today: "Work on problem issue #60"). Branch
`screen-rung`, rebuilt on `main` ee99067 the same day — the first cut
came from a stale local `main` 5f67f00 and numbered this brief 007, a
number `main` had already given to the spec-readability unit (decisions
log). Requirements R4–R7 and criteria A1, A4, A5 and A12 carry the
user's redesign of 2026-09-26 (decisions log); the brief describes what
ships. The persona's reports, the user's answers and the benchmark harness
live in the unit's scratch, which is not versioned; everything the
decisions rest on is quoted in this brief. A feature and a
user-visible change, so the persona gate ran (verdicts below).

## Context

The user's words (2026-08-29): *"When fast culling between several images
(press and hold the right arrow) while in fit or 1:1 screen, the quality
gets bad very quickly, maybe within 2 or 3 frames. I want to be able to
move quickly between more shots before actually seeing the image
degradation."* Setup: a 4K screen (3840×2160), at fit and at 1:1; "fast"
is the held key, ~30 frames/s.

The mechanism, verified in code and spec: while the arrow is held the loupe
is in TRANSIT and asks the decoder for the 1616 px mid rung only
(ui-grid.md "Transit vs settled", user requirement 2026-08-01). On a 4K
viewport the fit view of a 3:2 frame is 3240×2160, so the mid is shown
upscaled 2×. The A1 file embeds exactly two JPEGs, the 1616 preview and the
8640×5760 full; there is nothing between. A full decode costs 225–269 ms on
the development laptop through the shipped path (`loupe::decode_oriented`,
zune-jpeg 0.4.21; the perf budget is 350 ms), ten times the key repeat,
across three loupe workers. The first two or three frames of a hold are
sharp only because they were prefetched (±2) while the cursor rested; every
frame after that is the upscaled mid — and at fit it is shown UNFLAGGED,
because the render ladder exits at fit (`transit::render_rung` returns
`Drop { BelowLadder }` when the overlay is not wanted, and the fit cell
shows whatever texture it has). That contradicts ui-grid.md's own quality
rule ("never show upscaled pixels UNFLAGGED") on wide displays, and
ui-grid.md records that "the visible softness of an upscaled mid on a 4K
monitor during a hold has not been eyeballed by the user". It has now. The
user's original ask — a bigger cache (24 slots, 8 GiB) — stores nothing
new: this is a decode-rate problem, not an eviction problem (issue #60
explains why; the persona and the Manager agree).

**The benchmark (M6, 2026-09-26).**
A throwaway harness on the development laptop (i7-8665U, 4 cores /
8 threads, 32 GB, thermally saturated after the first second of every
phase), libjpeg-turbo 3.1.0 built from source with SIMD (AVX2 proven on:
`JSIMD_FORCENONE=1` is 1.95× slower), zune-jpeg pinned to the lock file's
0.4.21, the app's own extraction path for the bytes, 11 interleaved samples
per cell, two independent reviewers (one read the harness line by line, one
re-ran it: every latency cell within 3.4 %, the throughput cells within
6.6 %, the SIMD probe within 1.5 %), verdict not refuted. Reference file
`A1_full_lossless_compressed.ARW` (9.75 MB embedded JPEG), single thread:

| per decode | portrait (o8, shipped shape) | landscape |
|---|---|---|
| today: zune-jpeg, `decode_oriented` | 268.8 ms | 225.3 ms |
| libjpeg-turbo, full size, same shape | 226.7 ms | 185.7 ms |
| libjpeg-turbo, 1/2 scale (4320×2880, 37 MB) + rotate | 146.7 ms | ~137 ms |
| libjpeg-turbo, 3/8 scale (3240×2160, 21 MB) | portrait not measured | 127.8 ms |
| libjpeg-turbo, 1/8 scale (the serial Huffman floor) | — | 90.8 ms |

Throughput, frames/s, three fixtures round-robin: 1/2-scale 7.5 / 13.7 /
18.8 / 22.3 / 23.3 / 24.3 at 1 / 2 / 3 / 4 / 6 / 8 workers; today's shipped
path 3.7 / 6.8 / 9.2 / 10.9 / 11.7 / 12.3. Four workers is the knee on four
physical cores (2.97× at N=4); hyperthreads add 5–9 % at doubled per-frame
latency. The two heavier fixtures (12.3 MB) decode at 152–155 ms at 1/2
scale hot, ~140 cool; decode time follows the byte count (+26 % bytes →
+15 % time). The gain is 1.6–1.8× over today's decode, not the ~3× the
issue estimated, and about half of it is the decoder swap itself; the 1/8
floor is 68 % of the half-scale time. The reviewers' material notes are
recorded here and in ADR 0005: the rule holds single-threaded (at 3–4 workers a
half-scale decode is 166–184 ms per frame — the throughput table, not a
different verdict); the 3/8 rung's portrait rotate runs single-threaded
below `orient.rs`'s 32 MiB parallel threshold (a 21 MB rung), so its
portrait cost is likely ~150 ms unless the threshold moves. The recorded
rule — half-scale ≤ 150 ms build the rung, > 220 skip it — was written
against the single-threaded 305 ms baseline; read on the same footing it
says **build**.

**The user's answers (2026-09-26, verbatim in scratch).** The culling
machines are not the laptop: "A desktop. 64gb RAM, nvidia 5800. pretty
powerful. I also run the cull in a ryzen ai+ max 395." Both operating
systems: "it really depends. Sometimes is linux, sometimes is windows.
Right now I'm on windows." On pacing the held key: **"No. the application
needs to move smooth. the softness is acceptable, but I need to maximize
the situations where the softness isn't there."** At 1:1: "I usually tap.
but sometimes I hold too." Files: "Usually I copy to an ssd before start
culing." Folders: "One event." The dependency: "Fine regarding libjpeg."
Release: "Once implemented, can you not release this? I would like to get
the CI version and test it on windows with real folders and see how it
behaves."

Consequences. The held key keeps its repeat rate; the unit maximises the
sharp frames inside a hold and never slows the hold. On the user's
machines decode stops being the limit once workers follow the core count
(a 16-core Zen 5 at ~130 ms per half-scale decode is over 100 frames/s
aggregate); the limit moves to the display path, where a full-size frame
is a 149 MB kitchen copy plus a GPU upload per swap and a fit-sized rung is
21–37 MB. So the rung is what lets sharp frames keep up with the key on the
strong machines, and what makes them cheaper to decode on the weak one.
Workers and ring depth derive from the machine; nothing is configured.

## Goals

- G1. A held arrow at fit on a wide viewport shows sharp frames for as long
  as the decoders keep ahead of the key — a whole burst on the user's
  machines, a dozen-plus frames from rest on the laptop — instead of two or
  three. The hold itself never slows down.
- G2. The frames the decoders cannot keep ahead of are the best rung in
  hand and honestly flagged, at fit as above fit.
- G3. Decode capacity follows the machine: decoders from physical cores,
  the cache from total RAM, the ring fixed; no knob beyond a test switch.
- G4. A short hold at 1:1 stays sharp (the user taps, sometimes holds).
- G5. A stop at fit on a wide viewport is served by the rung, and `Z`
  after it stays as fast as today.

## Non-goals

- N1. Pacing the held key (issue part 3) — rejected by the user 2026-09-26.
- N2. 1:1 at held speed by crop upload (part 4) — phase 2, its own unit.
- N3. Removing the full-res texture copy (part 6) — persona SHRUG under
  this branch; unchanged, recorded.
- N4. Runtime shrink-only memory polling and the thumbnail cap (the second
  half of part 5) — a later brief.
- N5. A settings UI or persistence (#39 parked; "a toggle you can't see in
  the UI is worse than none").
- N6. Card-reader culling — the user copies to SSD first; the existing
  slow-media notes stand as they are.
- N7. Swapping the thumb/mid pipeline's decoder — the senior developer
  decides in the spec change whether zune-jpeg stays there; either way
  recorded, neither way a goal.
- N8. Any upstream contribution (hard rule 2).
- N9. A release. The CI Windows artifact is the user's test build.

## Requirements

- R1 (decoder). The loupe's full-res decode uses libjpeg-turbo ≥ 3.0
  through the `turbojpeg` crate, built with SIMD; `require-simd` is on so
  a SIMD-less build fails instead of shipping at half speed. The public
  `decode_oriented` contract holds (the perf budget measures it).
- R2 (hostile inputs unchanged). Every existing bound applies on the new
  path: `MAX_DECODED_PIXELS` checked from the header before any
  allocation, `scan_is_terminated` on the bytes, a sub-KB 30000×30000 SOF
  rejected before a pixel is allocated, a scan cut before EOI → `Failed`.
  libjpeg-turbo's own truncation behaviour (a synthetic EOI and a warning)
  never yields a silent blank success — the byte check runs first, and
  warnings are errors on the decode.
- R3 (screen rung). A new loupe rung between mid and full: the embedded
  full JPEG decoded with DCT scaling at the smallest N/8 factor whose
  output serves the viewport's fit view under the existing 1.25 `serves`
  rule (4K → 3/8 = 3240×2160; QHD → 2/8; ≤ 2K → no rung, the mid serves
  and behaviour is unchanged; 5K → 4/8). Orientation is applied to the
  rung like every rung. The factor follows the viewport: a resize or a
  move to another display re-derives it, and a cached rung that no
  longer serves is a soft rung, never presented as sharp.
- R4 (the ring; revised 2026-09-26 by the user). One ring shape: 2
  behind / 15 ahead, fixed, leaning the way of travel by the existing
  latch, never re-derived. At fit it requests the fit box — the screen
  rung on a wide viewport, the mid below ~2K — while travelling and while
  settled alike. Above fit, R7. The `revive_deferred` gate follows the
  ring in force (the trap raw-pipeline.md recorded); a reversal culls the
  queued decodes that fall outside the new ring. (Was: 6 behind / 12
  ahead, sized down by a budget derived from free memory.)
- R5 (decoders; revised 2026-09-26). Loupe decode workers = the
  machine's physical cores, not its threads, floor 3 (two backlog workers
  and the focus-reserved lane, which stays), cap 16, and at most half the
  total RAM in GiB (Manager ruling Q4).
  `FASTCULL_DECODERS=N` replaces the count for testing, in the mould of
  `FASTCULL_MAX_READERS` — an override, allowed above the cap. (Was:
  capped by a derived budget's transient term; no override.)
- R6 (the pixel cache; revised 2026-09-26 by the user). The loupe's RAM
  cache of decoded pixels is the smaller of 10 GiB and a quarter of the
  machine's TOTAL RAM, never below today's 2 GiB. Total RAM comes from
  `/proc/meminfo` `MemTotal` on Linux and `GlobalMemoryStatusEx` on
  Windows (the one `unsafe` block ruled Q1); an unreadable figure means
  2 GiB. The app prints the cache size, the ring and the decoder count
  — and, on Linux, the mmap threshold it set — once on stderr at startup. Screen rungs count toward the cache like
  mids do. The app's texture copies stay bounded by the rings, and the
  spec states the whole-app peak per RAM class. Withdrawn: the
  `MemoryBudget` derivation from free memory and its eleven-row table,
  `FASTCULL_MEMORY_MB`, `fastcull-cli budget`. Runtime shrink and the
  thumbnail cap: a later brief.
- R7 (above fit; revised 2026-09-26 by the user). Above fit the ring is
  FULL-RES, 2 behind / 15 ahead, clamped so it fits in the cache, while
  settled and while tapping — a tap forward at 1:1 lands on a sharp
  frame. During a HOLD above fit, full-res is requested for the ring
  while the decoders keep up with the key; when they fall behind, the
  fit-box rung is requested instead until they catch up. The hold never
  slows; the render shows the best rung in hand, cued. The guard is one
  measured quantity, not a table, and its switch rule is the one the
  decisions log records from the persona's screen verdict. It never
  outranks the focused frame's own work. (Was: a settled look-ahead of
  2 behind / 6 ahead, and transit capped at the fit box at every factor.)
- R8 (render ladder). full-res (sharp) → screen rung (sharp at or below
  its own scale; soft above it, cue on) → mid (soft) → thumb (soft) →
  residual hold → drop. `transit::render_rung` gains the rung's rows and
  stays total. At fit on a wide viewport a frame rendered from a rung
  that does not serve fit shows the cue pill: the sentence "never show
  upscaled pixels UNFLAGGED" becomes true at fit (persona G6; today the
  fit cell shows an upscaled mid unflagged on 4K).
- R9 (stop at fit). On settle at fit on a wide viewport the target is the
  rung, and the cursor frame's full-res still cooks behind it when idle,
  so `Z` after a stop is served as today (persona G4).
- R10 (threading). Every pixel step of the rung — decode, rotate, texture
  fill — runs off the UI thread; the kitchen fills, the UI thread wraps.
- R11 (build and CI). Both CI jobs and the release workflow install nasm
  (cmake is already on both runner images); libjpeg-turbo builds from
  source and links statically on both targets. Linux may link a system
  libjpeg-turbo ≥ 3.0 through pkg-config (Fedora 44 ships 3.1.3; Ubuntu's
  2.1.x is too old, so CI builds from source). The build documentation
  names the requirement: cmake + nasm, or a system libjpeg-turbo ≥ 3.0.
- R12 (docs, same commit). `docs/culling.md` "Holding the arrow" tells the
  new promise: sharp while the decoders keep ahead, how far that reaches
  on a modest laptop and on a strong desktop, the cue when they fall
  behind, and that the hold never slows. `docs/faq.md` gains the memory
  entry (how much RAM FastCull takes, why, and that it follows the
  machine). `docs/index.md` gains the build requirement.
- R13 (numbers recorded). The benchmark is recorded in this Context and
  in ADR 0005 (the decoder swap, the rung, the Huffman floor, the
  throughput knee, the reviewers' two material notes); 01-architecture.md's
  perf table gains a landscape full-res row (the SIMD canary, < 280 ms)
  and two 4K screen-rung rows whose thresholds are kind guards at 0.9 × the
  idle landscape full-res median (< 150 ms, measured idle in step 1).
  (Revised 2026-09-26: was "raw-pipeline.md records the benchmark",
  which the spec shape forbids, and "~2× the idle median".)

- R14 (CMYK and YCCK on the loupe path; Manager ruling 2026-09-26).
  libjpeg-turbo refuses CMYK and YCCK to RGB ("Unsupported color
  conversion request", measured in step 1); zune-jpeg converts them. A
  loupe stream whose header says CMYK or YCCK — a bare JPEG session,
  issue #8, e.g. a print-ready export — decodes through zune-jpeg as
  before, at full scale, with no rung. No regression; the A1's embedded
  JPEGs are YCbCr and unaffected.
- R15 (licence notices; Manager ruling 2026-09-26). A test pins that
  `THIRD-PARTY-LICENSES.md` lists `turbojpeg-sys` under the IJG, the
  BSD-3-Clause and the zlib licences with their notice texts: cargo-about
  0.9.2 falls back silently, exit 0, when a clarification's checksum no
  longer matches, and then lists the crate under MIT with an unrelated
  notice (measured in step 1).

## Acceptance criteria (the module specs carry the binding form)

- A1. Clock-free: the ring plan — forward → (2 behind, 15 ahead); at
  fit the fit-box rung, above fit full-res clamped by the cache; a
  reversal re-leans on the very next call; edges clamp; a folder shorter
  than the ring → the whole folder. Fails on the old 2-behind / 8-ahead
  transit shape and the old settled ±2. (Revised 2026-09-26.)
- A2. Clock-free: rung factor per viewport — a table over (viewport,
  frame dims, orientation): 3840×2160 → 3/8; 2560×1440 → 2/8; 1920×1080 →
  none; 5120×2880 → 4/8; a portrait frame uses its rotated dims.
- A3. Clock-free: `render_rung` extended and total; rows for rung-in-hand
  at fit → Sharp without cue, rung-in-hand at 1:1 → Soft with cue,
  mid-only at fit on a wide viewport → Soft with cue (the G6 row).
- A4. Clock-free: the cache rule over 4 / 8 / 16 / 32 / 64 GB of total
  RAM (2, 2, 4, 8, 10 GiB), an unreadable, zero or absurd total → 2 GiB;
  the decoder rule over 2 / 4 / 16 / 32 physical cores (3, 4, 16, 16 on
  32 GiB and more; the RAM cap gives 4 on 8 GiB and 8 on 16 GiB),
  an unreadable count → 4, `FASTCULL_DECODERS` wins and a value that is
  not a positive integer is ignored with a stderr line. (Revised
  2026-09-26: was the `MemoryBudget` derivation table.)
- A5. Driven (release, real A1 files, a 3840×2160 window): during a
  400-key hold at 40 ms at fit, no frame is rendered below the rung once
  its ring entry has landed, and the first fifteen frames of a hold that
  starts after a rest longer than the ring's fill time are all at the
  rung; the on-screen sharp-frame rate and p90 frame interval are
  recorded per seat in this brief's Outcome — numbers for humans, not a
  gate; A5's gates bind on any seat that grants a 3840×2160 window.
- A6. Driven: the hold never slows — frames on screen per key stays at
  the current table's level (the 787-of-800 class); no pacing crept in.
- A7. Old red first: a mutant that keeps transit on the mid fails A5; a
  mutant that keeps `revive_deferred` at ±`PREFETCH` fails the widened-ring
  revival test; a mutant that derives direction per call fails the
  existing backward-hold test.
- A8. Hostile inputs on the new path: the existing tests pass, and the
  30000×30000 SOF and the cut-before-EOI stream are exercised through the
  scaled decode too.
- A9. Perf budgets: the full-res row stays green with more headroom; the
  new rows are green; `tests/zoom_walk.rs` (the mandatory zoom-quality
  gate) passes in release against the real A1 files.
- A10. CI: both checks green; the Windows artifact runs; libjpeg-turbo
  is compiled optimised on both targets (the CMake-cache check in the
  Windows verify step; added 2026-09-26 after the step-1 review). The user's test
  on the desktop with real folders is outside this brief and precedes
  any release.
- A11. Hard rule 1: the RAW-write tests unchanged and green; QE records
  the sample RAWs' checksums before and after its runs.
- A12. Memory: at the seat's cache size (about 7.8 GiB on the laptop's 31.1 GiB), a
  5k-symlink walk at fit and at 1:1 holds `VmHWM` ≤ the cache + the
  decoders' transient buffers + 200 MB (the issue's RSS ceiling test),
  skipped when available RAM is under the cache + 2 GiB. (Revised
  2026-09-26; revised 2026-09-27: the walk runs under the app's mmap
  threshold via `GLIBC_TUNABLES`, its fit phase at 2560×1440.)
- A13. The hold above fit (R7): engine-level and clock-free where it can
  be — with decoders that keep up, every frame of a hold lands full-res;
  with decoders that fall behind, the requests fall to the fit-box rung
  and follow the recorded switch rule; the hold's frames on screen per
  key stay at A6's level either way.
- A14. A CMYK and a YCCK bare JPEG open in the loupe with pixels, never
  a Failed badge (R14); red on step 1's code.
- A15. The licence test (R15); red on a regenerated file whose
  clarification fell back.

## Applicable directives

- The shape of a module spec (CLAUDE.md, brief 007 spec-readability,
  2026-09-17): five sections, a rule in one spec, evidence — run ids,
  mutant readings, seat measurements — in this brief or the commit,
  never in a spec's Behaviour; M9 as amended (stale is deleted); M10.

- Hard rules 1 (never write a RAW), 2 (no upstream), 5 (business logic in
  `fastcull-core`; the kitchen stays presentation plumbing), 6 (the perf
  budgets are regression-tested).
- ADR 0001 (embedded-JPEG strategy — the rung is a scaled decode of the
  same embedded JPEG), ADR 0003 (sidecar-only writes). A new ADR for the
  C dependency (libjpeg-turbo, cmake + nasm on every build seat) — the
  senior developer writes it in the spec change.
- ui-grid.md "Transit vs settled" (request states; the geometry never
  changes; direction latched at the index change; the revival trap; the
  measured table), the quality rule and the `transit` module record;
  raw-pipeline.md (the ladder, the workers and the reserved lane, the
  hostile-input bounds, the memory budget, the pooling record);
  01-architecture.md (threading model — one kitchen worker, all pixel work
  off the UI thread; perf budgets; build profiles, #76).
- M1 (spec first), M5 (model split), M6 (numbers before architecture —
  recorded above), M7 (never name the user), M8, M9.
- Rules of the gate: old-red-first, a mutant for every new guard, the
  senior developer's veto on every test change; the driven suite runs as
  two `--exact` halves from `--list` (directive 7337a0c).
- Scratch: the unit's scratch directory and `target-qe-007*` build trees;
  combined cap 10 GB; the benchmark harness and the build tools stay for
  the unit.

## Persona verdicts (2026-09-26, this branch; full report in scratch)

| Part | Verdict | From the screen |
|---|---|---|
| 1. Screen-sized rung | MUST-HAVE | the only item that makes a hold at fit on 4K sharp; a stop at fit becomes ~free |
| 2. Workers + ring 6 behind / 12 ahead | MUST-HAVE (with 1) | the ring turns the runway into the whole burst; 1–3 back by hold is the compare loop |
| 3. Pace the held key | USEFUL (with a bounded wait) | moot: rejected by the user |
| 4. 1:1 crop upload | USEFUL | phase 2; would be MUST-HAVE for whole-burst holds at 1:1 — the user taps, sometimes holds |
| 5. Memory budget, no UI | USEFUL | noticed once a season, its absence badly; one number if a setting ever ships |
| 6. Remove the 149 MB copy | SHRUG now | ~20–30 ms with a 21–37 MB rung |

Gaps adopted: G2 → R7, G4 → R9, G6 → R8, G7 → R12, G8 → R3, G10 → A5.
Moot after the user's answer (pacing): G1, G3, G5. G9 (shrink never takes
the cursor's rungs) → a later brief.

## Open questions (all answered 2026-09-26)

1. Which machine, RAM, drive, what else is open → the desktop, 64 GB,
   "nvidia 5800", and a Ryzen AI Max+ 395; Linux or Windows by the day,
   Windows now. Files on an SSD after copying.
2. Paced held key → **no**: "the application needs to move smooth. the
   softness is acceptable, but I need to maximize the situations where
   the softness isn't there."
3. Hold at 1:1 → "I usually tap. but sometimes I hold too."
4. Card reader → "Usually I copy to an ssd before start culing."
5. Folder size → "One event."
6. The dependency → "Fine regarding libjpeg."
7. Release → no; the CI Windows artifact is the test build.
8. The persona answered with authority: 1–3 frames back by hold (6 behind
   is right); no knob now, one number if ever.

## Decisions log

- 2026-09-26 (benchmark, M6): half-scale decode 133.5 ms on the reference
  file hot, 121 cool — the recorded rule says build. The Manager reads the
  rule on the reference file, single-threaded, as it was written against
  the 305 ms baseline; the heavier fixtures (152–155 hot, ~140 cool) and
  the 3–4-worker per-frame figures (166–184) are the throughput table,
  not a different verdict. Nothing approaches 220.
- 2026-09-26 (the user): the C dependency is accepted.
- 2026-09-26 (the user): no pacing — the held key keeps its repeat rate;
  soft frames are acceptable; maximise the sharp ones. Part 3 dropped.
- 2026-09-26 (the user): no release; the CI Windows artifact is tested on
  the desktop with real folders first.
- 2026-09-26 (Manager, M2): the cue pill at fit on a wide viewport when the
  shown rung is upscaled — the spec sentence is honoured within the
  ladder's 1.25 `serves` tolerance at fit (a ≤ 25 % upscale is unflagged
  everywhere on the ladder), strict above fit; not carved out.
- 2026-09-26 (Manager, M2): the rung's factor derives from the viewport,
  never hardcoded.
- 2026-09-26 (Manager, M6): workers = physical cores, from the benchmark's
  knee; the persona's "cores − 1" is noted and A5's p90 is where jitter
  would show.
- 2026-09-26 (Manager): part 5 split — the startup-derived struct here,
  runtime shrink-only and the thumb cap in a later brief.
- 2026-09-26 (Manager): the thumb/mid pipeline decoder is the senior
  developer's call in the spec change (N7).
- 2026-09-26 (Manager, agreeing the senior developer's spec change after
  four review rounds — two adversarial readers per round; the senior
  developer's own record of what each round corrected is in the spec
  sentences dated "fix round N"; the last round's verifier found no
  material flaw and four wording minors, applied by the Manager in the
  verifier's words). The rulings, which the module specs carry in their
  binding form:
  - Perf rows (R13): three rows join the table, not two — the landscape
    full-res row at 1.5× its median (the SIMD canary must fire on a 1.73×
    regression) and the two 4K rung rows (3/8 landscape; 2/8 portrait plus
    its rotate) at most 0.9× the IDLE landscape full-res median, < 155 ms
    provisional until the implementation commit measures the four rows
    idle. R13's "~2× the idle median" applies to the re-measured full-res
    row only.
  - The factor rule is the BOX rule: a portrait frame on QHD is served by
    the mid and on 4K by 2/8; R3's and A2's "QHD → 2/8" is the landscape
    case. The 32 MiB parallel-rotate threshold in `orient.rs` does not
    move.
  - A5 is three gates read off the trace marks, with the budget pinned by
    `FASTCULL_MEMORY_MB=16384` in the driven run: (1) a rung once adopted
    is never displaced — anchored on the adoption mark, the
    render-monotone form kept as 1b; (2) at least `ring_ahead` screen
    rungs decoded from a TRANSIT-state request land during the hold — 0
    under the mid-capped mutant by construction, on every seat; (3) after
    a rest that waited for each of the twelve adoptions ahead, the first
    twelve frames of the next hold render at the rung. The count of
    frames a hold SEES at the rung is a decode-rate figure — a number for
    humans per seat, never a gate (issue #27). A5 skips, printing the
    geometry it got, on a seat that cannot host a 3840×2160 window — the
    Windows runner — and is review-verified there: the platform skip the
    gate rules require in writing, here.
  - A6's ≥ 98 % is provisional on the laptop as on CI until the software
    renderer's 4K render-mark rate is measured there; a re-base is the
    floor in ui-grid.md A6, with the run's count, p90 and run id in this
    brief's Outcome (the spec shape keeps evidence out of the spec), never
    a test-side margin.
  - R5: `workers` = min(16, max(3, physical cores)), reduced by the
    budget's transient term — the cap because the 19-entry transit ring
    bounds what more decoders could pop and a 32-core seat would otherwise
    derive 30. R6: `mids_cap` stays 64 in the struct, not derived (brief
    008). The `Ready` event carries the request state beside the rung's
    kind — an instrument for A5, not behaviour.
  - R9: the cursor's idle full-res cook at fit is the reserved lane's
    second manufactured job, on wide viewports only in this unit (M2:
    nothing changes on the screens the mid serves; extending it to every
    viewport is a follow-up if the user asks — one background decode per
    stop).
  - R2, measured rather than reasoned (the harness's `truncation` mode,
    `run-truncation.log`): `tj3Decompress8` returns −1 on any warning with
    or without `TJPARAM_STOPONWARNING`, and the safe crate maps it to
    `Err`, so a truncated stream is an `Err` over a grey-bottomed buffer,
    never a blank success. Neither `TJPARAM_STOPONWARNING` nor
    `TJPARAM_MAXPIXELS` is set: the safe crate cannot set them, and a
    raw-FFI decompressor would be core's first `unsafe` block for an abort
    13–24 ms earlier on a crafted stream. The loupe uses the safe
    `Decompressor` as published. A8's mutant is of our code — the
    decoder's `Err` swallowed — never of a library parameter.
  - Also ruled: `fastcull-cli budget` prints the derived line; the
    full-res texture ring widens to the look-ahead ring (9 by default)
    within the budget, with issue #60 part 6 as the lever; ADR 0001's
    consequence carries a dated in-place narrowing; README and the ADR
    0002 line land in the spec commit; the kitchen's Full-before-Wrap
    order stays, recorded and measured; the G6 sentence is honoured at fit
    within the ladder's 1.25 `serves` tolerance and strictly above fit.
  - The reviewers' agreement on the benchmark, re-derived from the two
    logs: every latency cell within 3.4 %, the throughput cells within
    6.6 %, the SIMD probe within 1.5 %.
- 2026-09-26 (Manager, the plan's seven open questions — the plan is
  the plan, six commits):
  - Q1, the Windows memory probe: ONE `#[cfg(windows)]` `unsafe` block in
    `budget.rs` over `windows-sys` (`GlobalMemoryStatusEx`, `dwLength`
    set, zeroed struct), with a SAFETY comment — the spec's own mechanism.
    The rule against `unsafe` in core stands for everything else: it was
    set against duplicating a guard a safe crate already gives (the
    decoder parameters), and no safe std API reports available memory on
    Windows. `sysinfo` is refused: a large dependency on every seat for
    one number.
  - Q2: a box-less engine keeps the ±`PREFETCH` settled ring as well as
    the mid transit cap — the behaviour before this unit exactly; R7's look-ahead
    needs the fit box to know what "above fit" is. The clause lands in
    raw-pipeline.md "The factor rule" in the step that implements it.
  - Q3: `loupe::serves_box` is added and ui-grid.md's `serves_dims`
    sentence is corrected in place (a naming correction; the 1.25 rule
    keeps its one home).
  - Q4: physical cores come from `num_cpus::get_physical` on both OSes;
    raw-pipeline.md's sysfs / `GetLogicalProcessorInformationEx` sentence
    is corrected in place to name the crate and its sources.
  - Q5: A5 cycles the three fixtures and models the view order from their
    known capture times, pinned by two dumps.
  - Q6: two amended test promises the spec's at-risk list missed are
    agreed — the backward-hold flip threshold 14 → 18 (derived from
    `ring_behind` as the old one was from `TRANSIT_BEHIND`), and
    `transit_at_zoom_stays_soft_never_drops_to_fit`'s fixture widened to
    ten files and eight keys so its premise holds by ring geometry; no
    assertion loosened; each recorded at the test, in the commit body and
    (the second) in a dated sentence in ui-grid.md.
  - Q7: the rung's kind rides inside the `Ready` event's `FullImage`
    rather than as a separate field — the event carries it either way.
  - Seat: the development laptop has no system cmake, nasm or Xvfb; all
    three run from the unit's scratch tools directory (Xvfb verified to
    serve a 3840×2160 root screen, 2026-09-26), which the scratch GC must
    not remove during this unit. Installing them as system packages is
    the user's call and blocks nothing.
  - Models (the user, 2026-09-26: "I reached my token limit on Fable.
    Resume the work with Opus 5.5, including the agents if necessary."):
    from the plan's checks on, every role of this unit runs on Opus 5.5,
    the senior developer and any persona call included. Task-scoped; the
    agent files' pins are unchanged (M5).
- 2026-09-26 (Manager, the plan checks re-run on Opus: three rounds, the
  plan amended twice; the last round left one material flaw, ruled
  below; the questions the fix rounds raised, Q8–Q15, ruled on the
  senior developer's recommendations):
  - Q8: the two app texture rings evict by the engine's LEANED window —
    `evict_ring(held, cursor, view, window)` evicts entries outside the
    window first, then by distance; the window comes from core
    (`LoupeEngine::texture_windows()`), so the travel latch stays in core,
    and each capacity is its window's size, so the budget is unchanged.
    Symmetric eviction could not hold a 6 / 12 ring: after a forward hold
    it settled at −9..+9 and evicted +10..+12 as each landed (and the
    full-res ring discarded +5 / +6), which made A5's rest and gate 3
    unpassable on a correct build and docs/culling.md's "a dozen ahead …
    kept" false. The four in-place spec corrections the plan names (§7
    Q8) land in the steps that implement them.
  - Q9: `a_backward_hold_keeps_leaning_backward_across_refocus`'s reach
    bound moves ≤ 6 → ≤ 4 (= 11 − `ring_behind` − 1), farther from the
    cursor, so the third A7 mutant (direction per call) is red; nothing
    loosened; recorded at the test and in the commit body with Q6's.
  - Q10: the truncation sentences in raw-pipeline.md and ADR 0005 are
    corrected to the order the code has and the tests need — the byte
    check runs after the header read and the pixel cap, before any buffer
    is sized or any scan byte decoded (the header read allocates
    nothing).
  - Q11: A5 waits for the capture sort before the 4K resize and reads
    gates 1 / 1b after the settle; the 1:1 short hold starts after six
    look-ahead adoption waits; `FASTCULL_A5_REQUIRE_4K=1`, read only by
    the test, turns A5's skip into a failure — set on the Linux CI release
    step and on every local measurement and mutant run.
  - Q12: the fit cue is masked in a `--synthetic` session (no files,
    nothing is ever loading), with the clause added to ui-grid.md's
    quality rule at fit.
  - Q13, Q15: the wording and precision corrections the plan names, in
    the steps named.
  - Q14: the "wide viewport" predicate is decided per VIEWPORT against the
    reference landscape A1 mid (one predicate for the settled ring, the
    revival gate and the idle cook); the residual — portrait frames on a
    portrait-rotated QHD-class screen — is recorded in raw-pipeline.md's
    Settled bullet. M2: a best-practice ruling on a corner no culling
    seat on record has; revisited if the user ever culls on a rotated
    screen.
  - The last material flaw — A5 on the Windows runner: the spec's
    premise that "the Windows runner's desktop" cannot host a 3840×2160
    window is unverified and probably false, and the plan's fixture
    folder (under the CI temp dir, on C:) cannot hard-link the checkout's
    RAWs (on D:). Ruled: A5's 480-file fixture folder lives on the RAWs'
    volume, under the target dir with a drop guard (the `perf_budgets`
    `target_dir()` pattern), so it links on every OS; A5 binds on ANY
    seat that grants the geometry and skips, printing the geometry it
    got, only on one that does not. ui-grid.md's A5 sentence is corrected
    in step 5 to that conditional rule, without naming a seat as fact.
    The PR's CI run of step 5 is the measurement of what the Windows
    runner grants; if it binds there and A6's count is under 98 % with
    the p90 far above the key, A6's written CI-floor clause applies (a
    spec sentence with the run id), never a test-side margin.
  - The A10 `turbojpeg.dll` import check's mutant is review-verified, not
    run: seeing it red needs a Windows MSVC build linked dynamically,
    which no local seat produces, and CI runs only on pull requests and
    `main`; the static build provably cannot contain the string (vendored
    sources and both crates read). Recorded here as the decision the
    gate rules require.
  - M14b (the idle cook's memo-clause mutant) reds as a hang under the
    state lock; `timeout 120 cargo test … --exact`, exit 124, is accepted
    as its recorded red.
- 2026-09-26 (Manager, the stale base — the Manager's error): the unit's
  branch was cut from a local `main` at 5f67f00 that had not been fetched;
  `origin/main` was at ee99067 (PR #92, merged 2026-09-18: every module
  spec rewritten into the five-section shape, the harness contract moved
  to `specs/modules/test-harness.md`, history to `specs/history/`, the
  CLAUDE.md shape directive, M9 amended, M10 — and brief 007 given to the
  spec-readability unit). The developer found it at the end of step 1,
  when PR #93 showed as conflicting and CI would not run. Rebuilt on
  ee99067: this brief renumbered 008 (it was `007-screen-rung.md` on the
  first branch — commits 41b7219, 4e9b7d6 and 1b22e4d — and the follow-up
  it called "brief 008" is now "a later brief"); the agreed spec change
  (048bcc9 on the first branch) re-ported into the new shape together
  with the redesign below, by the senior developer; step 1's code
  (bbbbb45) cherry-picked, its spec edits re-applied in the new shape.
  The first branch's history is replaced on PR #93 by a force-push with
  lease; nothing of it was merged. Directive candidate for the user:
  step 0's session check fetches `origin` and cuts the unit's branch from
  `origin/main`.
- 2026-09-26 (the user, the redesign; verbatim, in order): "What if we
  do 2 behind and 15 in front? I feel we are overly complicating things."
  — "The memory cache can grow more than that. Would a 10G limit help to
  have more textures live so it would be faster to move between images?"
  — "The look ahead is my main problem. When I'm at fit view or full zoom
  in and I'm pressing and hold the arrow forward, the images loading get
  soft. Even when o star tapping images forward they get soft. When i
  stop, takes some few ms and the sharp does. What can i do to have more
  sharp images there?" — "Is your proposal also increase the decoders to
  follow the threads?" — "Go with your suggestion then. Forget about the
  GPU request." The suggestion he accepted, now R4–R7: one ring of 2
  behind / 15 ahead everywhere; screen-sized at fit; full-res above fit,
  kept coming during a hold while the decoders keep up and falling back
  to the fit-box rung when they do not; a pixel cache of the smaller of
  10 GiB and a quarter of RAM, never below 2 GiB; decoders = physical
  cores with a test switch; the startup memory derivation dropped. It
  supersedes the persona's G2 (a 6-frame full-res runway at 1:1) and the
  spec change's `MemoryBudget`, `FASTCULL_MEMORY_MB`, `fastcull-cli
  budget` and eleven-row table. Q1's one `unsafe` block now reads total
  RAM rather than available. GPU decoding was discussed and dropped; no
  issue is opened for it.
- 2026-09-26 (Manager, the developer's step-1 questions): CMYK and YCCK
  loupe streams go through zune-jpeg as before (R14, option c of three —
  no regression at the least code; converting CMYK in core, option b,
  would be a new colour path for files no camera writes); the licence
  canary becomes a test (R15); "regenerate the licence file on every
  Cargo.lock change, or have CI check it" is a directive candidate for
  the user, since it costs CI time (M3); step 1's two tests beyond the
  plan and its three-text licence clarification are the senior
  developer's call in review.
- 2026-09-26 (persona, the redesign, on Opus; no questions for the user):
  1. one ring, 2 behind / 15 ahead, fixed — MUST-HAVE (the earlier "6
     behind" withdrawn: frames behind stay in the cache and the first
     backward step re-leans);
  2. the screen-sized ring at fit — MUST-HAVE;
  3a. the full-res ring at 1:1 while stopped or tapping — MUST-HAVE;
  3b. full-res during a hold at 1:1 while the decoders keep up —
      MUST-HAVE, and IN-MY-WAY only if it pumps sharp / soft or stutters
      (it also decides whether taps faster than 4/s and fast Y/N chains
      at 1:1 are sharp);
  4. the cache — USEFUL;
  5. decoders = physical cores with a switch — SHRUG;
  6. the fit pill, the idle cook for `Z`, frames behind kept — MUST-HAVE.
  Gaps G1–G6 below, each adopted by the Manager (M2).
- 2026-09-26 (Manager, M2, on the persona's redesign check):
  - The switch rule for R7's hold above fit, adopted as the persona
    wrote it: step DOWN to the fit-box rung as soon as a full-res frame
    cannot reach the screen before the cursor does, decided before that
    frame's decode starts, so the change is one clean step and never dips
    through a mid or a thumb; step back UP only when the rung ring ahead
    is complete and the decoders are idle, from one boundary ahead of the
    cursor, everything beyond it full-res; if it must step down again
    within one ring (15 frames) of stepping up, it stays on the rung until
    the user stops or slows below four frames a second.
  - G1: "keeping up" is measured at the SCREEN — the frame's full-res was
    ready to draw when the cursor reached it — so a display path that
    cannot take 149 MB per key (the kitchen copy, the GPU upload) steps
    the hold down even while the decoders keep up. A6's frames-on-screen
    criterion is measured at 1:1 as well as at fit. The recorded lever if
    the upload is the limit is the 1:1 crop upload (#60 part 4), never a
    slower hold.
  - G2: R7's full-res ring is clamped from its far end to what the cache
    holds (the "ring that is a lie" rule), so a machine under ~11 GB never
    decodes a ring it then evicts.
  - G3: the startup line and `docs/faq.md` state the whole-app peak the
    user will see — the cache, the texture copies, the decoders' buffers
    and the thumbs — not only the cache figure; "a quarter of RAM" is the
    TOTAL the OS reports.
  - G4: the first backward step re-leans the texture windows as well as
    the decode ring, and the kitchen drops queued full-res texture fills
    for frames outside the texture window, so a revisit never waits
    behind copies of frames already passed — a change to the kitchen's
    "Full requests are not culled" contract, specified with its own test.
  - G5: time-to-sharp on the frame landed on — after a stop and after `]`
    — is measured on the idle laptop against v0.14.0 and must be no
    worse; a number QE records, since wall clocks bind only on the idle
    laptop (issue #27).
  - G6: the spec and docs follow the redesign, including retiring the
    sentence "a bigger LRU stores nothing a hold can use" (false once
    revisits at 1:1 are full-res) and telling the user that a fast Y/N
    chain is now judged at full quality at 1:1 and at fit quality at fit.
  - The pill rule the persona left to the Manager: ADOPTED in part. While
    travelling the pill, once on, stays on for at least ~250 ms, and it
    clears the moment a sharp frame is on screen after the key is
    released — a minimum on-time errs toward flagging a sharp frame, never
    toward hiding a soft one. REFUSED: "the pill never lights for a single
    frame" — that would show a soft frame unflagged, against the quality
    rule's "never show upscaled pixels UNFLAGGED" (user-approved
    2026-07-27); the switch rule's clean steps are what keep single soft
    frames rare.
  - Recorded for a later unit, not this one: preparing the `]` target (the
    next burst's first frame) while the user rests.
  - For the user's Windows test of the CI build: hold the arrow at 1:1
    through a long burst and report whether it is as smooth as at fit, and
    whether, when it goes soft, it steps down once or flickers.
- 2026-09-26 (Manager, the spec port): the senior developer ported the
  agreed change into the five-section shape with the redesign; three
  check rounds by two readers (brief coverage; the shape rules and
  contradictions), the last with no material flaw. The machine froze and
  was rebooted during the second fix round; the log shows an orderly
  reboot after network errors, no out-of-memory kill, GPU hang or lockup,
  and the round was resumed from the workflow's journal. Agreed from the
  senior developer's port: the thumbs stay on zune-jpeg (N7); lossless
  streams decode full-scale with no rung (a step-1 finding); the requests
  per position and state have one home, raw-pipeline.md's "The ring"
  table; during a hold the focused frame and the members behind are
  re-planned to the fit box whatever the cache holds; an engine with no
  fit box keeps the behaviour before this unit; `LoupeEngine::start`
  keeps three decoders; three driven tests keep their promises under
  fixture changes, no assertion loosened, each recorded at the test and
  in its commit — `transit_at_zoom_stays_soft_never_drops_to_fit` pins one
  backlog decoder (`FASTCULL_DECODERS=2`) and holds at 60 ms on a folder
  longer than the hold, and
  `transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center` and
  `a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge` grow
  their folders to at least `RING_AHEAD` + 2 frames, because their End
  targets now sit inside a 1:1 rest's full-res ring.
- 2026-09-26 (Manager, the port's open questions):
  - The GPU upload (Q1): the switch rule sees the decode, the kitchen copy
    and the adoption; femtovg's upload at a texture's first draw is a
    recorded residual no automated test can see (the suite renders in
    software). Ruled: ship as designed; the user's Windows test of the CI
    build — a long hold at 1:1 — is the check, and nothing is released
    before it; if it stutters, the recorded lever is the 1:1 crop upload
    (#60 part 4). Put to the user in the report, who may choose the
    alternatives instead: time the GPU draw as a second input, or keep a
    1:1 hold on the screen-sized frame.
  - The kitchen's full-fill order (Q2): the cursor's fill first, then the
    nearest by view distance, ties toward the lean — core's
    `transit::next_fill`.
  - The switch rule's binding form (Q3): agreed as written.
  - Memory on small machines (Q4): the port's own table put the whole-app
    worst case at 113 % of RAM on an 8 GiB, 16-core machine (72 % at four
    cores) against 3.8 GiB before this unit — a swap or an out-of-memory
    kill on a machine the release reaches. NOT accepted as stated. Two
    one-line clamps, specified by the senior developer before the plan:
    the full-res ring above fit counts its texture copies against the
    cache — floor(cache / (2 × 149,299,200)) frames, far end first — and
    the decoders are capped at half the machine's RAM in GiB, floor 3.
    Target: the whole-app worst case at most 60 % of total RAM on every
    row of the table. Neither clamp changes the user's 32 and 64 GB
    machines.
  - `FASTCULL_DECODERS` down to 2, with 1 read as 2 (Q5): agreed.
  - The step-1 carry (Q7): its old-shape spec edits dropped, "brief 007"
    renamed "brief 008" and `"The rung"` renamed `"The screen rung"` in its
    code comments, raw-pipeline.md A8 ticked in that commit with its tests
    (M10).
  - CLAUDE.md (Q9, Q10): the Commands block names the new build
    requirement, and the open-decisions entry for #60 records the
    reopening.
  - The build seat (Q9) is put to the user: the laptop's cmake and nasm
    live in the unit's scratch, which the cleanup rule deletes at the
    unit's end; after that a build there fails on purpose unless the two
    are installed as system packages.
- 2026-09-26 (the user installed the build tools): at the user's request
  ("Can you install it for me?") the Manager installed `cmake` 4.3.0,
  `nasm` 3.02 and `xorg-x11-server-Xvfb` as system packages on the
  development laptop. A plain `cargo build` works there without the
  unit's scratch tools, which the cleanup rule may now delete at the
  unit's end.
- 2026-09-26 (Manager, Q4's outcome, from the plan's checks): the
  whole-app formula now counts the kitchen's full-res fill in flight and
  each decoder's input JPEG; with the two Q4 clamps every row from 16 GiB
  up is within 60 %, the nominal 8 GiB row reads 61.7 %, and a real 8 GB
  machine (which reports less than 8 GiB and runs 3 decoders) stays
  within 60 % only from a reported 7.74 GiB. Rulings below complete it.
  Q-D: the decoder cap uses the floor of half the reported RAM, so a
  16-core machine reporting 31.x GiB runs 15 decoders; the claim "no
  change on the user's machines" is narrowed to that.
- 2026-09-26 (Manager, M10): every earlier ruling in this log that names
  the memory budget, `FASTCULL_MEMORY_MB`, `fastcull-cli budget`, the
  6 / 12 transit ring or the 2 / 6 look-ahead is superseded by the
  redesign; where it and the module specs disagree, the specs hold. The
  list: the A5 pin `FASTCULL_MEMORY_MB=16384`; A5's twelve adoptions and
  frames (now fifteen); "A5 skips on the Windows runner" (A5 binds on any
  seat that grants the geometry); R5's "reduced by the budget's transient
  term"; `fastcull-cli budget`; the 9-frame full-res texture ring (the
  texture window is the full-res ring); Q6's two test amendments and Q9's
  reach bound (moot: `RING_BEHIND` is 2); Q8's 6 / 12 figures (its rule
  stands at 2 / 15); Q11's six waits (one wait per full-res member
  ahead).
- 2026-09-26 (Manager, the plan v2 questions; recommendations accepted
  unless stated): Q-G (i), the full-res ring clamp counts the kitchen's
  fill, ⌊(cache − F) ÷ 2F⌋ frames; Q-C, an unreadable, zero or absurd
  total reads as 8 GiB for the RAM cap too; Q-H (b), rule 2 steps up when
  the members ahead are held or in flight, nothing waits, and a backlog
  worker is free; Q-I (A), the reserved lane asks for the settled ring
  once per settle; Q-K (a), a time-to-screen stamp ends only on facts the
  app reports (`note_adopted`, `note_dropped`), no position-based cull;
  Q-L (a), the step-down delay is measured on the kitchen leg it can see;
  Q-A (a), at the loupe the mids inside the rung window are kept; Q-B,
  `CUE_MIN_ON` counts from the last soft frame. HELD: Q-J (machines that
  report under 8 GiB) waits on the allocator measurement below, since
  both move the same numbers.
- 2026-09-26 (Manager, the step-1 review, CHANGES_REQUESTED): the blocker
  F1 — on Windows the `cmake` crate picks the Visual Studio generator and
  strips every `/O` flag, so libjpeg-turbo's C code ships at `/Od`; the
  PR run's advisory Windows perf step was red on three rows (the 3/8 rung
  slower than the full decode) inside a green job. Ruled: the fix names
  the generator for the MSVC target in a workspace `.cargo/config.toml`
  (`CMAKE_GENERATOR_x86_64_pc_windows_msvc = "Visual Studio 17 2022"`,
  the reviewer's option A — a seat without VS 2022 fails loudly, revisited
  when the runner images move); a CMake-cache guard in "Verify Windows
  artifact" (`/O2` in `CMAKE_C_FLAGS_RELEASE`, `WITH_SIMD = 1` in the
  build output) is pushed ONE COMMIT AHEAD of the fix, so the gate sees
  it red on the real Windows runner (the reviewer's option A for the
  guard, ~70 min of CI); both perf steps gain `--nocapture` so their
  medians reach the log; the false sentences about the C decoder being
  optimised in every build are corrected in place and A10 gains "compiled
  optimised", through M1. F2 (a lossless test), F3 (the NASM comment),
  F4 (a scratch row citation) are fixed in the same round. R14's CMYK
  route lands at the start of step 2. The progressive scan limit is
  recorded as a residual in the hostile-input bounds, for a later brief.
  Practice for this unit, and a directive candidate: the Manager reads
  the Windows perf medians of every PR run.
- 2026-09-26 (Manager, the Linux allocator, M6): the plan's feasibility
  check measured that glibc keeps freed 21 MB rung, 12 MB JPEG and 5 MB
  mid buffers in its arenas once its mmap threshold has risen, so after a
  fit session switches to 1:1 the laptop's peak read 12.2 GiB against the
  planned 9.1 (Windows returns this memory). Before choosing among a
  one-line `mallopt` threshold in the app, a trim after eviction, another
  allocator, or recording the term in the accounting, the options are
  measured on the same probe; a new dependency goes to the user with the
  numbers.
- 2026-09-26 (the user, on other cameras; verbatim): asked whether the
  user culls files from bodies other than the A1, the user answered "Yes
  I do. I mean, I might not actually, but the software should be able to
  handle more files as, at some point, I will want more users with
  different cameras." Ruled (Manager): the relaxation lands IN THIS UNIT,
  in step 2 with the stderr line — a benign libjpeg-turbo warning (any
  warning outside the truncation class, which `scan_is_terminated` and the
  short-scan test already pin) no longer leaves a frame on its lower rung:
  the decode's completed buffer is used and one stderr line names the file
  and the warning; the truncation warnings stay fatal, so a damaged file
  still shows the Failed badge. The spec moves first (the senior
  developer's pass after amendment 1), including 00-overview.md's "Other
  cameras: best-effort" line, which the user's answer upgrades. Promoted
  to CLAUDE.md as directive M11.
- 2026-09-26 (Manager, spec amendment 1, committed 21ab0e3 after two
  check rounds, the last with no material flaw):
  - The scan limit, re-ruled on a corrected premise: zune-jpeg 0.4's
    default refused more than 100 progressive scans, and the swap dropped
    that bound unnoticed, so R2 ("every existing bound applies on the new
    path") was not met. Restored: `set_scan_limit(100)` on the loupe
    decode, with a generated 101-scan test and a canary item, in the
    step-1 fix round; the earlier "residual for a later brief" ruling is
    withdrawn.
  - The other-cameras relaxation needs a design pass, not a sentence: the
    safe crate reports errors as text only, and libjpeg-turbo's header
    read also fails on a warning. The Manager's preferred direction for
    that pass: any libjpeg-turbo failure outside the truncation class
    (`JWRN_JPEG_EOF`, `JWRN_HIT_MARKER`, and whatever else the pass finds
    in that class) decodes the file through zune-jpeg, full scale, no
    rung — the CMYK route of R14 — with one stderr line; no second
    `unsafe` block. The senior developer may propose otherwise with the
    reason.
  - M11 against the July rule for a RAW whose full JPEG is damaged but
    whose mid is good: the rule stands (the frame stays on its mid, cued;
    from step 2 a stderr line names it), and M11's wording is corrected
    to say so. A distinct "full size unavailable" cue is an idea for a
    later unit, with the persona.
  - The stderr line's test reads the child process's stderr over a
    synthetic RAW with a good mid and a truncated full, so the box keeps
    its promise.
  - Bookkeeping: CLAUDE.md's Commands block names the Windows generator
    requirement; A10 gains "compiled optimised on both targets"; step 6
    adds VS 2022 on the release runner to RELEASING.md and the dist
    plumbing. (Superseded the same day by the generator re-ruling below:
    step 6's RELEASING.md names ninja, preinstalled on `windows-2022`; no
    Visual Studio version is named and dist gets no ninja dependency.)
- 2026-09-26 (senior developer, the other-cameras spec pass: the
  measurements behind raw-pipeline.md "The decoder's complaints", for the
  Manager's ruling on where the pass departs from the preferred direction.
  Synthetic 1024×768 streams through the vendored libjpeg-turbo 3.1.0
  (turbojpeg 1.5.1) and zune-jpeg 0.4.21 in its default, strict, options;
  no RAW file read):
  - Header gaps: with three non-FF bytes before the first DQT, our byte
    check called the stream "truncated" and the SOF sniff found nothing;
    libjpeg-turbo's header read failed ("3 extraneous bytes before marker
    0xdb"), after which the safe crate offers no header; zune-jpeg refused
    two or more such bytes ("[strict-mode]: Extra bytes between headers")
    and accepted one.
  - Harmless streams, each decoded by libjpeg-turbo byte-identical to the
    same stream intact, at 8/8 and at 3/8: an SOS whose Se byte is 0
    ("Invalid SOS parameters for sequential JPEG"); junk left after a scan —
    1 to 7 bytes before EOI with no warning at all (the Huffman decoder's
    bit buffer had read them; 8 bytes gave "2 extraneous bytes", 64 gave
    "59"), 3 and 64 bytes before a restart marker (every byte counted), and
    3 bytes between a progressive stream's DHT and its next SOS, each under
    an "extraneous bytes" warning; zune-jpeg decoded each of them
    byte-identical to its own decode of the intact stream.
  - Header complaints: a JFIF APP0 of revision 2 and an ICC chunk with
    sequence number 0 fail libjpeg-turbo's header read, and zune-jpeg
    decodes both. An Adobe APP14 with transform 5 on a three-component
    stream decodes in libjpeg-turbo with no complaint when a JFIF APP0 is
    present and fails its header read without one; zune-jpeg refuses
    transform 5 in both modes, with JFIF or without.
  - Restart markers: RST1 renumbered RST5 (four ahead) decodes in
    libjpeg-turbo byte-identical to the intact stream; renumbered RST2 (one
    ahead) or RST0 (one behind) it does not; zune-jpeg, which resets at any
    restart marker whatever its number, decodes all three byte-identical to
    its intact decode.
  - Corruption campaign, 1,000 single-byte flips of the scan data per
    stream shape, sorted by libjpeg-turbo's first message. Baseline without
    restarts: HIT_MARKER 348, all damaged; bytes left before EOI 39, all
    visibly damaged, zune-jpeg decoding all 39 as a success; no warning 613,
    of which 506 damaged (309 visibly). Restart every 64 MCUs: HIT_MARKER
    373; bytes left before a marker 312; no warning 315 (235 damaged); no
    damage above 0.5 % of the pixels. Progressive: HIT_MARKER 324;
    HUFF_BAD_CODE 69, all refused by zune-jpeg too; bytes left before a
    marker 55, all damaged, zune-jpeg a success on 54; no warning 546 (350
    damaged); fatal header errors 6, refused by both. NOT_SEQUENTIAL,
    BOGUS_PROGRESSION and MUST_RESYNC never came first. Arithmetic coding,
    200 flips: ARITH_BAD_CODE 103, no warning 36, other messages 57.
  - Behind a kept message: an SOS with Se = 0 over a scan cut to a quarter
    and closed with EOI decodes with 69 % of its pixels grey, the kept
    message hiding the short scan.
  - Corrected by the pass's second round: its first draft refused bytes
    left over after a scan as damage, on the campaign alone (406 of 406
    such streams damaged); the padding rows above raise the same message
    over byte-identical images, so the spec keeps libjpeg-turbo's image and
    records the damaged case as a residual.
- 2026-09-26 (Manager, the relaxation pass, after two check rounds the
  last with no material flaw; the senior developer's recommendations
  accepted): the damage class that is refused is the truncation pair
  plus `HUFF_BAD_CODE`, `ARITH_BAD_CODE` and `MUST_RESYNC` — messages only
  damage raises; the leftover-bytes warning (`EXTRANEOUS_DATA`) and the
  scan-parameter pair are decoded past with libjpeg-turbo's completed
  image (padding raises the same text over byte-identical pixels);
  anything else gets zune-jpeg's second opinion at full scale, no rung.
  The Manager's first direction ("outside the truncation class, zune-jpeg")
  is refined by the measurements above, not overruled. Also accepted: the
  header-gap pre-pass and its SOF sniff, on the grid-thumb path too
  (inside N7: zune-jpeg itself is unchanged); the Adobe, missing-EOI and
  full-scale-only-at-fit residuals as recorded; README's camera-support
  paragraph; step 2 as three commits reviewed together.
- 2026-09-26 (Manager, the generator, re-ruled on the developer's
  evidence): the F1 ruling named "Visual Studio 17 2022", but CI's
  `windows-latest` is now the `windows-2025-vs2026` image (VS 2026 only)
  while dist's release plan builds on `windows-2022` (VS 2022 only);
  naming either version breaks the other build. The developer disputed
  the ruled value with that evidence and committed nothing for it — a
  sound dispute, so no user arbitration is needed. Re-ruled: the
  workspace `.cargo/config.toml` names `Ninja` for the MSVC target, which
  is preinstalled on all three images and independent of the Visual
  Studio version, so CI and the release build one way; it is proven by
  the Windows guard going green and the Windows perf rows (the 3/8 rung
  faster than the full landscape decode). If the cmake crate's
  Ninja-with-MSVC path fails on the runner, the fallback is the
  developer's option (c): VS 2022 named and CI's Windows job on the
  `windows-2025` image, keeping the check name. Spec first: 01-architecture
  "Native dependencies", ADR 0002, ADR 0005, docs/index.md and CLAUDE.md's
  Commands block change with it. The guard reading every turbojpeg-sys
  cache, debug builds included, is confirmed; so are the three new
  BUDGET-MEDIAN prints.
- 2026-09-27 (Manager, step 1 APPROVED): the senior developer's re-review
  of e1b488a + d4cc7b7 + 53a4248 + 82f69ff approved it with minors F5–F11.
  The Windows-optimisation evidence is CI run 36294797235 (82f69ff): the
  guard green on all four turbojpeg-sys caches, each naming
  `CMAKE_GENERATOR=Ninja`, the release flags `/O2 /Ob2 /DNDEBUG` and
  `WITH_SIMD = 1`, after being red on the same runner at d4cc7b7 and
  53a4248 (runs 36280732156 and 36285260233, every cache "-nologo -MD
  -Brepro -W0" under Visual Studio 18 2026); the Windows perf medians
  before → after: full-res portrait 339.9 → 226.6 ms, landscape 289.2 →
  185.4, the 2/8 rung 239.2 → 131.5, the 3/8 rung 302.8 → 147.8 (under
  the 150 ms threshold with 2.2 ms to spare on the shared runner — a
  watched row: a later red is diagnosed, never re-based); the reviewer's
  disassembly of `jpeg_idct_3x3` in the artifact went from 403
  instructions, 182 stack operands and 5 multiplications by zero to 186 /
  17 / 0. `fastcull-windows-x64` from run 36294797235 or later is the
  first artifact with the decoder compiled optimised; every earlier PR
  #93 artifact is not to be judged for speed. A10 stays open for step 6's
  release plumbing. F7 (a cargo-install residual clause), F8 (a comment's
  source), F11 (two unlabelled BUDGET-MEDIAN lines) ride with step 2's
  first commit; F5, F6 and F9 are M10 corrections carried by spec
  amendment 2; F10 is done above.
- 2026-09-27 (Manager, the Linux allocator, on the measurement): glibc's
  default keeps freed buffers under its risen mmap threshold (at most
  32 MiB) in its arenas; on the probe linking the real core, a fit
  session followed by 1:1 peaked at 12.2 GiB (4K landscape), 15.9 (QHD's
  2/8 rung), 14.4 (portrait on 4K) and 12.0 (the mid on ≤ 2K) against
  A12's 9.07 GiB ceiling at the laptop's cache — and the build before
  this unit leaked too (3.29 GiB against 3.02 with the old 2 GiB cache).
  Measured options: `mallopt(M_MMAP_THRESHOLD, 4 MiB)` at start held
  every shape within the ceiling (8.40–8.96 GiB) at a decode-rate cost
  within noise; 16 MiB failed three of four shapes; `malloc_trim` after
  eviction worked but needs `unsafe` in core and cost 5.7 % at fit;
  mimalloc (v2 and v3) stayed over the ceiling; anonymous-mapped buffers
  worked but change the pixel buffer type through core. RULED: one
  `mallopt(M_MMAP_THRESHOLD, 4 << 20)` as the first line of the app's
  `main` under `cfg(all(target_os = "linux", target_env = "gnu"))`, with
  `libc` as a Linux target dependency of fastcull-app (already in the
  lock) — the app's one Linux `unsafe` call; core's rule is unchanged. A12
  runs under the same threshold through `GLIBC_TUNABLES`
  (`glibc.malloc.mmap_threshold=4194304`, measured identical), its value
  one constant, its mutant the variable dropped (red at 12.2 GiB); the
  startup line reports the threshold `mallopt` accepted and a driven test
  asserts that line; A12's fit phase walks a 2560×1440 box (2/8), the
  shape that tells 16 MiB from 4 MiB. Recorded residuals: fit rungs under
  4 MiB (no A1 viewport makes one); a portrait 1:1 session at about 13
  decoders or more exceeds A12's ceiling as written by arithmetic; the
  Windows heap's return of large blocks is documented, not measured
  here; the app itself was not driven (no rung on HEAD yet).
- 2026-09-27 (Manager, Q-J, on the premise that the threshold ships):
  option (a) — the band below a reported 8 GiB is accepted and recorded,
  docs/faq.md gains its sentence, and the user's rule R6 (the 2 GiB
  floor) stands. Spec amendment 2 carries the allocator rule, A12's
  changes, Q-J's provenance and the M10 corrections F5, F6 and F9; step 3
  waits for it.
- 2026-09-27 (Manager, step 2 APPROVED in its first review round): 2a
  d2d5941 (CMYK and YCCK through zune-jpeg; F7, F8, F11), 2b c73297e (the
  screen rung and its kinds, in core; the app inert), 2c 9211580 (the
  other-cameras relaxation); CI run 36303917663 green on both runners,
  the Windows guard green on all four caches, every Windows perf row
  green (3/8 rung 144.7 ms, 2/8 135.9, landscape full 185.4, portrait
  full 232.0). Spec amendment 2 (f7ac661) committed after one check
  round with minors only, hand-merged at the A10 / A15 boundary. Rulings
  on the review and the amendment: the review's F1 (tests for the stderr
  lines on the screen-rung path), F2 (a stale comment) and F3 (the
  reserved lane's abandon check before the plain decode) ride with step
  3's first commit; the pre-existing re-decode loop on an IFD that
  over-claims its JPEG's size is fixed in step 3 — the ladder memoizes the
  decoded long edge, with its spec sentence in the same commit (M11); a
  damaged mid over an intact full showing Failed is recorded in the spec
  as a known gap for a later unit (unchanged from main); the header-gap
  list's growth is recorded as a residual; Q-M — step 3 measures the
  thumb and throughput perf rows with and without the Linux threshold,
  and inside +2 ms / −10 % the rows keep timing glibc's default; no ADR
  for the one `mallopt` call (issue #40's precedent); A12 stays off CI as
  before. 2a and 2b need no separate driven re-run (their app code is
  unchanged from 9211580 and 0a178cc).
- 2026-09-27 (Manager, steps 3 and 4 APPROVED): step 3 (ca473f5, eb9de78,
  9df84ab) in one review round — CI run 36315775611 green on both
  runners; the laptop's real startup line: "loupe cache 7.8 GiB (a quarter
  of 31.1 GiB total RAM), ring 2 behind / 15 ahead, full-res 15 ahead at
  1:1, 4 decoders (physical cores, 3 to 16), worst case 12.2 GiB for A1
  frames on a 4K screen plus ~0.2 GB per 1,000 thumbnails; glibc mmap
  threshold 4 MiB"; A12 under the threshold read VmHWM 8,575 MiB against
  its 9,287 MiB ceiling; the IFD over-claim loop fixed with its old red.
  Q-M measured inside the ruled bounds (+0.5 to +0.9 ms per thumb;
  throughput −5.5 % in the developer's rounds, −7.5 to −9.6 % in the
  reviewer's) — the perf rows keep timing glibc's default; the lever, if a
  many-core seat ever minds, is a reused per-worker thumb buffer. Both CI
  runners derive the floor configuration (2 physical cores → 3 decoders,
  a 4 GiB cache, full-res 10 ahead), so A5, A6 and A13 on CI measure the
  floor. Step 4 (ecad74a, fd73755, 75d0fdf; fix round 7064694, 9123d9b)
  after one CHANGES_REQUESTED round; its F5 landed in step 5a (c8445f3).
  The video-export perf row read 840–2110 ms on this PR's ubuntu runners
  with no change to its code (a runner-disk row, advisory on CI, ~23 % of
  budget on the idle laptop): watched; a second red on this PR opens an M3
  bookkeeping issue. The #[cfg(test)] AFTER_SCREEN_DECODE hook stays (the
  only race-free red for its guard; fileops.rs's PROBES is the precedent).
- 2026-09-27 (Manager, step 5's block): CI run 36350758741 on 33e5e8d is
  red on Windows debug only — `transit_to_a_cold_frame_keeps_the_overlay_at_
  the_carried_center` read the thumb's aspect because its midgap dump runs
  80 ms after End on the clock, and step 5's Q-A mid window makes the End
  refresh free about 14 mids at once, a per-pixel drop loop that debug
  compiles at opt-level 0 (52–62 ms on the laptop, 166–189 ms on the
  Windows runner; about 3 ms in release, where the ubuntu pass is green).
  Rulings: (1) the fix is the test's, not the product's — the dump is
  gated on the End refresh's own mark instead of a clock (the project's
  rule that driven tests wait on events, not timers), proposed by the
  senior developer as a test change under its integrity review, the
  test's promise unchanged; freeing textures off the UI thread is not
  done, since release frees them in about 3 ms; (2) the soft-transit test
  (`transit_at_zoom_stays_soft_never_drops_to_fit`) is found vacuous — its
  first assertion is met by the cold start's soft line before any key —
  and is made to prove its claim, scoped after the first key with a
  fixture or profile under which the premise holds, as a test change under
  the integrity review; (3) the three enlarged fixtures get the drop guard
  (cold-frame, failgate, soft-transit: ~2 GB per Windows pass otherwise);
  (4) step 6 carries the kitchen's fill-cost measurement under the 4 MiB
  threshold; (5) the step-5 review confirms, or has the fix round close,
  step 3's reviewer's gap — `Z` to 1:1 and back to fit leaving the
  full-res ring queued at fit for the next hold to pop; (6) step 5's five
  plan deviations are the review's to judge. Also promoted (directive
  9131a37): every local driven run under Xvfb with WAYLAND_DISPLAY cleared
  — the agents' test windows had been opening on the user's live desktop.
