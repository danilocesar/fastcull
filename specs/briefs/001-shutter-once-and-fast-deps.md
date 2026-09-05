# Brief 001 — the screenshot shutter fires once everywhere; dependencies compile optimised in debug

Date: 2026-09-05. Issues: #77, #76 (and #33, retired by #76). Branch:
`shutter-once-and-fast-deps`. Scratch for this unit: `.qe-scratch/pipeline-001/`
(plan, review and QE reports) and `target-qe-*` build dirs. The first unit of
work through the pipeline of CLAUDE.md, "The workflow".

## Context

Both defects live in the screenshot test harness (`--screenshot` mode of the
app, `crates/fastcull-app/src/shutter.rs`; the tests in
`crates/fastcull-app/tests/screenshot.rs`). Neither changes what a user sees.

**#77.** The shutter's readiness poll is a `TimerMode::Repeated` 250 ms
timer (`shutter.rs:43-46`). After the capture it sets `shot_written` and
requests `slint::quit_event_loop()`, but the request is not an exit: on a
seat where the event loop turns once more, the poll fires again, nothing in
the poll reads `shot_written`, and a second shot is taken, emitting a second
`status at shutter` / `geometry at shutter` pair and overwriting the JPEG.
Measured 2026-09-05: two shots in 26 of 26 local traces on the 8-core
laptop; one shot in 0 of 249 CI traces (four runs, Windows debug, Windows
release, Linux release). A late settle photographed too early on CI can be
photographed correctly by the second local shot, so a local green does not
prove what a CI green proves. Every test-side reader takes the LAST
`status at shutter` line (`.lines().rev().find_map` in screenshot.rs;
corrected 2026-09-05 by the senior developer — this brief and issue #77
first said FIRST), so a local green could be a green of the SECOND capture,
taken half a second after the state CI photographs; nothing asserts the
count.

**#76.** Ten Windows CI jobs (runs 58–71, 2026-07-27 and 2026-08-01) failed
with `full-res texture never adopted for the 1:1 frame within 60 s`. The
mechanism, measured by the senior developer on 2026-09-05: the full-res rung
is `zune_jpeg` decoding the embedded 8640×5760 JPEG in `decode_oriented`
(`crates/fastcull-core/src/loupe.rs`); the workspace `Cargo.toml` has no
`[profile.dev]`, so in a debug build the decoder runs at `opt-level = 0`
and needs 26–40 s against a 60 s cap that starts at `shutter::arm`
(0.3–0.55 s in release). Same source, one config line apart,
`window_resize_keeps_the_photo` under the load recipe that reproduces the CI
refusals: stock 0/4 green, `[profile.dev.package."*"] opt-level = 2` 4/4
green. Cold debug build of the screenshot test binary: about 2m19s → about
9m on the laptop while competing with other work (ratio not clean).
Evidence of that discussion: `.qe-scratch/plan-73/` (DISCUSSION.md, the GATE
reports) — inherited evidence, not verified work.

**#73's design** (readiness budget from the end of the drive script, 85 s
cap) was rejected in the four-party discussion: in every recorded refusal
the script had finished at 1.3–4.0 s. Not to be revived here.

**The user's decisions (2026-09-05):** "go for 77. then, on 76: pick option
1; dependencies are not required to be compile at debug mode. most of the
time that's useless." Options 2 (a debug-aware cap) and 3 (no debug
screenshot pass on Windows) are not taken; the audit's open decision on the
Windows double-run stays open and is not part of this unit.

## Goals

- G1. The shutter fires exactly once per `--screenshot` run on every seat
  and profile, so a local trace and a CI trace mean the same thing.
- G2. A debug build decodes the full-res frame fast enough that the 60 s
  cap is margin again, by compiling dependencies optimised in the dev
  profile while workspace crates stay unoptimised and debuggable.
- G3. The specs say what is true afterwards: no sentence credits or relies
  on the slow debug decode without being marked as history.

## Non-goals

- The 60 s readiness cap, the 1.5 s floor, the 30 s `wait:` cap and the
  90 s child watchdog keep their values.
- What CI runs (the Windows debug + release double pass) is unchanged.
- No change to `decode_oriented` or any decode path; no upstream change to
  any dependency (hard rule 2).
- No new `[profile.dev]` settings beyond the one package override
  (`opt-level` for `"*"`); workspace crates keep the default dev
  `opt-level` (0).
- Nothing user-visible; no `docs/` page changes unless the senior
  developer's spec change finds a user-facing sentence that moves.

## Requirements

- R1 (#77). After the first capture the poll never runs again: stop the
  timer at capture or return early when `shot_written` is set — the senior
  developer's plan picks the mechanism. Exactly one `status at shutter`
  mark and one `geometry at shutter` mark per run, on the laptop and on
  CI, in debug and in release. The JPEG is written once.
- R2 (#77). The two failure paths keep their behaviour: the cap refusal
  (exit 1 with its message) and the write failure (exit 1); `finish` still
  exits 2 when the loop ends before a shot. The readiness predicate is
  untouched.
- R3 (#77). A test-side guard in `screenshot.rs`: the shared helper(s) that
  read `status at shutter` assert exactly ONE occurrence in the trace
  (count, not first-match), so every driven test enforces R1. Old-red
  first: on the pre-fix `shutter.rs` this assertion is red on the laptop
  (the 26/26 evidence predicts it); green after. Mutant: reverting the
  shutter change makes it red again.
- R4 (#76). The workspace `Cargo.toml` gains
  `[profile.dev.package."*"] opt-level = 2` with a comment that says why
  (the measured debug decode against the cap; the user's decision of
  2026-09-05; that workspace crates stay at the default). Nothing else in
  the profile changes.
- R5 (#76). The effect is measured, not assumed: the full-res decode time
  of a debug screenshot run on the laptop before and after the line (the
  trace's loupe-ready mark timing or an equivalent), and
  `window_resize_keeps_the_photo` under the #76 load recipe with the line
  (expected 4/4). Numbers go in the commit message and, where the spec
  quotes debug decode times, into the spec.
- R6 (#76). Spec truth: every sentence that quotes a debug decode time,
  credits the slow debug decode as a detector, or explains a release-only
  skip by debug decode speed is updated or marked historical with date and
  role (spec convention `(corrected <date>, <role> F<n>)`). Known sites:
  `specs/modules/raw-pipeline.md` (the ladder residual "~30 s worst case in
  debug"; the 60 s cap history), `specs/modules/ui-grid.md` (the 60 s cap
  paragraph "which in a debug build over a 50 MP frame is a real margin";
  the #61/#73 paragraphs quoting debug timings; the harness section on the
  shutter), `specs/01-architecture.md` (perf budgets "Skipped in debug
  builds where decode timing is meaningless" — still true for wall-clock
  budgets, say so), `specs/milestones.md` where it quotes #33. The senior
  developer's spec change is the authority on the full list.
- R7 (#76). The two release-only test gates justified by debug decode
  speed (`transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center`,
  `a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge`, and
  the `perf_budgets` precedent at screenshot.rs ~5236) are re-examined in
  the spec change: keep (timing pins bind in release, the precedent) or
  drop the gate, decided on the spec's promise and recorded with the reason.
- R8. CI cost is recorded: the first CI run on this PR pays a cold
  dependency build on both runners; its job durations and the following
  cached run's are noted in the PR (the user asked for measurements before
  decisions; this one is after, but still measured).
- R9. Two commits in the project's voice, #77 first, then #76; each moves
  its spec sentences in the same commit; #76's commit says it closes #33.
  Attribution trailer on both.

## Acceptance criteria

- AC1. Ten consecutive local runs of one driven screenshot test (debug)
  each show exactly one `status at shutter` line; the pre-fix binary shows
  two in each (old-red evidence).
- AC2. On the PR's CI artifacts (`screenshot-evidence-<os>`, `debug/` and
  `release/`), every trace shows exactly one `status at shutter` line, and
  the R3 assertion is what enforces it.
- AC3. `cargo test --workspace` green locally in debug; `cargo clippy
  --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`
  clean; both CI checks green on the PR.
- AC4. With the profile line, the debug full-res decode of the 8640×5760
  frame on the laptop drops from tens of seconds to at most 2 s; the
  before/after numbers are in the #76 commit.
- AC5. `window_resize_keeps_the_photo` under the #76 load recipe: 4/4 green
  with the line.
- AC6. No spec sentence contradicts the shipped behaviour; every changed
  sentence carries date and role.
- AC7. RAW checksums in `testdata/raws/` are unchanged after every test run
  (hard rule 1; QE's standard statement).

## Applicable directives

- CLAUDE.md hard rules 1–6; rule 5 is satisfied because `shutter.rs` is
  harness code in the app crate, not business logic. M1 (spec first), M5
  (model split), M7 (never name the user), M8 (uncertain questions go to
  the user).
- ADR 0001 (embedded-JPEG strategy: the full-res rung is the embedded
  JPEG, which is why the decoder is `zune_jpeg`); ADR 0003 (no RAW writes).
- Spec sections: `ui-grid.md` — the `--screenshot` harness (the shutter,
  `status at shutter`, `geometry at shutter`, the 60 s readiness cap and
  the `wait:` cap paragraph, the `resize:` acknowledgement paragraph that
  says `geometry at shutter` "fires once, at the end"); `raw-pipeline.md`
  — the decode ladder and its recorded residuals; `01-architecture.md` —
  performance budgets and the debug-skip rule.
- Tests: `crates/fastcull-app/tests/screenshot.rs` — the trace helpers, the
  90 s watchdog, `has_display()` (Windows always runs the suite),
  `--test-threads=1` on CI.
- Standing directives in the agent files: old-red-first, one mutant per
  guard, the scratch-space discipline, foreground polling, the
  test-integrity veto.

## Persona verdicts

Skipped: test and CI plumbing, nothing user-visible (workflow step 2).

## Open questions

- OQ1 (for the senior developer, in the spec change, not the user): R7 —
  do the release-only gates keep their rationale? Decide on the spec's
  promise; if it cannot be decided with good certainty from the spec, it
  comes to the Manager and then to the user (M8).
- None for the user at the time of writing: both decisions are the user's
  of 2026-09-05.

## Decisions log

- 2026-09-05, the user: fix #77; #76 by option 1 (optimised dependencies
  in debug).
- 2026-09-05, Manager: one unit of work, two commits, #77 first (the
  user's order); persona gate skipped (plumbing); #73's readiness-budget
  design stays rejected; the earlier stopped run left no commits (its
  branch `debug-fast` was deleted) and its PR draft is not evidence.
- 2026-09-05, senior developer (duty 1, agreed by the Manager): the spec
  change in `ui-grid.md`, `raw-pipeline.md` and `01-architecture.md`
  (list: `.qe-scratch/pipeline-001/SPEC-CHANGE.md`); OQ1 settled — the two
  release-only gates that rested on the debug decode are lifted, the F2
  warm-landing pin and the M1 thumb-rung pin stay release-only for reasons
  that survive; no ADR (a reversible profile line is not architecture);
  no `docs/` page; `milestones.md` untouched (it does not quote #33).
- 2026-09-05, Manager, on the senior developer's Q1: the Windows job's
  90-minute cap is left alone for the PR's first cold run; the developer
  raises it to 120 in the same PR only if that run finishes over about
  80 minutes or is cancelled (CI bookkeeping, M3).
- 2026-09-05, Manager, on Q2: the brief's context paragraph corrected
  (FIRST → LAST); issue #77's text is corrected in its closing comment.
- 2026-09-05, Manager, on Q3: every run on the PR is cold (rust-cache
  saves on main only), so the "cached job durations" placeholder in
  `01-architecture.md` is filled by the Manager in a later spec commit,
  once a cached main run exists — not a PR of its own.
