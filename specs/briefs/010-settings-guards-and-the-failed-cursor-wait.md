# Brief 010 — the Settings guards QE listed, two hand-edited writer shapes, and the failed-cursor test's wait on the app's own mark (issues #100, #101)

Date: 2026-10-03. Issues #100 (brief 008's and 009's recorded deferrals)
and #101 (a one-off red of a driven test at brief 008's merge). Branch
`settings-guards-and-flake` from `origin/main` e8c26ef (M12). Test and
harness work with two small writer corrections — no persona gate (the
only behaviour that changes is the settings writer's handling of two
hand-edited file shapes nobody meets in normal use, and the spec already
promises what they should do). The user, 2026-10-03: "work on the open
issues from 98 to above"; this unit takes #100 and #101 together because
both are test work on `tests/screenshot.rs` and one CI cycle.

## Context

Brief 008 shipped the Settings dialog with QE's PASS and a list of
promises that work but nothing pins (issue #100, the unit's explicit
deferrals), plus two writer edge cases in hand-edited shapes; brief 009
added three more deferrals to the same issue. Issue #101 records that
`a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge` went
red once on the Windows debug run of a Markdown-only commit (1 of 13
runs): the test's dump landed 143 ms BEFORE the app's `loupe overlay
dropped idx 11 (decode failed)` mark, so the cursor was not yet
KNOWN-failed when the assertion read it — the script gates its second
`End` on `wait:thumb landed idx 11`, not on the failure's own mark. The
harness's rule (test-harness.md): wait on the app's mark, never on a
clock guess.

## Goals

- G1. Every promise issue #100 lists has a test that goes red when the
  promise is broken, or an explicit, recorded reason it cannot.
- G2. The two writer shapes behave as settings.md's "Writing" promises —
  the user's comments survive, every created key carries its note — or
  the sentence says exactly what TOML cannot hold.
- G3. The failed-cursor test waits on the event its assertion depends on,
  so it cannot read the cursor before the app knows it failed.
- G4. The driven suite grows as little as these guards allow: strands
  inside existing launches where the integrity review allowed it, one
  table-driven test where the shape fits; its size and chunk times are
  re-measured and reported.

## Non-goals

- No change to any Settings behaviour beyond the two writer shapes (R4);
  no change to the dialog, the card, the keyboard ring, apply-on-commit.
- No change to what the failed-cursor test asserts — only to what it
  waits on (R1).
- Issues #97, #98 and #99 are their own units.
- No new environment variable for a setting (brief 008 D42's rule); a
  harness hold knob for the Clear worker (R3's "never blocks the UI
  thread" guard) is test plumbing in `FASTCULL_KITCHEN_COOK_MS`'s family
  if the senior developer judges it the honest proof — announced on
  stderr, owned by test-harness.md — and brief 008 D13 stays (no
  `FASTCULL_CACHE_DIR`).

## Applicable directives

- CLAUDE.md hard rule 5 (the writer's two corrections are core's, with
  unit tests beside them); the rules of the gate — old-red-first for a
  bug fix (#101 is a bug in a test: the old-red is the recorded trace,
  and a forced reproduction under load if the senior developer can make
  the race happen on demand — `taskset` plus spinners, the diagnostic
  method), a mutant for every new guard, the senior developer's veto on
  every test change (this unit IS test changes: every one is reviewed
  under duty 3's shapes — nothing loosened, no step moved later on the
  clock, a wait on the event the assertion depends on is not a loosening).
- settings.md: "Writing" (the note on every created key; the exception
  sentence for replaced shapes — D40's rule, brief 008), AC7 and AC12
  (narrowed at brief 008's merge to what their tests prove — re-widened
  when the guards exist), AC18 (brief 009: the never-shrinks rule), the
  Clear cache row; test-harness.md (the marks and dump fields; the wait
  rule; the geometries and chunk sizes); ui-grid.md (the failed-cursor
  ledger line, if its test's doc moves).
- M1 (spec first for the two writer sentences), M3 (the harness knob and
  the suite's shape are the Manager's bookkeeping, decided here), M7,
  M12.

## Requirements

- R1. **#101 — the failed-cursor test waits on the failure's own mark.**
  In `a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge`
  the step that makes the cursor "known-failed" (the first `End`'s decode
  failure) is gated with a `wait:` on the app's own mark for that failure
  (`loupe overlay dropped idx 11 (decode failed)`, or the `failed badge
  11 laid out` mark — the senior developer names which, and whether any
  other clock-based step in the script carries a premise the assertion
  depends on); the assertions are unchanged. Old-red: issue #101's trace
  (the dump 143 ms before the mark), and — if the senior developer can
  force the race (the runner was slow: a cold debug build in parallel,
  `taskset -c 0,1` plus spinners) — the unmodified test red under that
  load with the rate, then green under the same load with the wait. The
  test's doc records the mechanism; test-harness.md's wait rule is cited.
- R2. **#100's six NOW guards**, each a test that goes red under the named
  mutant: (1) the status line for a PLAIN failed write (no read error
  standing) reads ` — ⚠ settings.toml could not be written`; (2) Clear is
  DISABLED under `FASTCULL_NO_CACHE`, read from the button's own mark;
  (3) the locked Limit field's SHOWN value is the environment's (a
  creation mark and a reader); (4) `Space` on each checkbox commits it;
  (5) brief 008 D42's option A — a hand edit made while the dialog is open
  loses to the next save (with the `edited` premise the integrity review
  required); (6) the read workers' Limit has no ceiling of its own (core).
- R3. **#100's eight DEFERRABLE guards**: `Ctrl+,` inert while a keyword
  field holds the keyboard (a measured red mutant is the condition); the
  active tab's accent underline (a comparative pixel check, the underline
  only — the integrity review refused the "brighter label" half); the
  open session keeps its painted thumbs after Clear (a cursor-cell mark,
  Linux only); Clear never blocks the UI thread (a harness hold knob for
  the worker, test-harness.md's family, if that is the honest proof —
  the senior developer's call, recorded); the Tab ring reaches Clear when
  the cache is on; the Reset button names the active tab (read from its
  own label mark); auto-advance off holds in the loupe at 1:1 (a NEW
  test, not a strand in the existing one — refused shape); core's test
  scratch dirs removed on `Drop` keeping a red test's evidence (refused
  with a guard that deletes while panicking — the approved shape keeps
  the evidence).
- R4. **The two writer shapes** (spec first, M1): QE's D47 — a key the
  writer CREATES inside an inline table (`performance = { cache_cap =
  "2 GB" }` written by hand) carries no note: the senior developer
  recommends and the Manager decides between stating the exception (TOML
  cannot comment inside an inline table) and expanding the inline table
  into a standard table so the note applies — the Manager's lean is the
  exception sentence, because expanding rewrites the user's shape, which
  D5 forbids more than a missing comment does; QE's D48 — a two-element
  `[[general]]` array where the `[general]` table belongs keeps only its
  FIRST header's comments when replaced: every element's comments are
  carried above the replacing table, in order (D5: a hand-edited config
  is the user's data), and the D40 sentence says so. Core tests with the
  `clear()`/first-only mutants.
- R5. **Brief 009's deferrals**: TP-1 — the never-shrinks rule guarded on
  every face and on the Windows runner, inside the growth test's first
  open (the integrity review's approved shape, in issue #100's comment);
  TP-3 — T3's premise asserts no `window geometry WxH` other than
  `1000x400` between the landing and the last dump (the approved shape;
  compare the WxH prefix only); D6 — the growth test's doc stops saying
  "GREEN on bef5b5e" (TP-1's strand makes the record true).
- R6. **The contrived move-aside wording** (#100): after one move-aside, a
  hand edit breaks the fresh file while the dialog is open, the config dir
  turns read-only, the user commits — the status line and the notice name
  the EARLIER aside as "the file that would not read". The bridge keeps
  the write error's KIND (`WriteError::MoveAside`), not only its text, and
  the two lines say "the file that would not read could not be moved
  aside" when that is what happened; a bridge unit test with the old
  wording as the mutant. settings.md "Writing" gains the sentence.
- R7. **Spec and docs, same commits**: settings.md's AC7 and AC12 widened
  back to the shown value and the button's state once R2(2) and R2(3)
  exist; AC18 names TP-1's strand; the ledger lines for every new guard;
  test-harness.md gains every new mark, dump field and the hold knob if
  one lands, and its chunk-size and test-count sentences are re-measured
  (the suite is 118 tests before this unit); `docs/settings.md` only if a
  user-facing sentence changes (R4's exception, if chosen, says in one
  phrase that a setting written inside an inline table carries no
  comment).
- R8. **Suite economy** (G4): the plan says which guards are strands in an
  existing launch (where the integrity review already allowed it), which
  share one new table-driven test, and which must be their own launch;
  the developer reports the suite's test count and chunk times before and
  after; the halves (or thirds) are re-split from `--list`.

## Acceptance criteria (they also land in settings.md's ledger and ui-grid.md's)

- AC1. The failed-cursor test gates the known-failed premise on the
  app's own failure mark; its assertions are unchanged; the forced-race
  old-red (if achieved) and the recorded trace are in the commit.
- AC2. Each of #100's six NOW guards exists, is named in the ledger, and
  is red under its mutant.
- AC3. Each of the eight DEFERRABLE guards exists in the approved shape
  or is recorded, with its reason, as still deferred (a condition not
  met — e.g. no measurable mutant — is a reason; a wish for less work is
  not).
- AC4. D47 and D48 behave as the corrected "Writing" sentences say; core
  tests red under the named mutants.
- AC5. Brief 009's TP-1 and TP-3 strands exist; the growth test's doc is
  true.
- AC6. The contrived move-aside state names what happened; the bridge
  test's mutant is the old wording.
- AC7. settings.md's AC7, AC12 and AC18 read as what their tests now
  prove; test-harness.md's counts and marks are current.
- AC8. Issues #100 and #101 can be closed by this unit's PR (every item
  done or recorded with its reason).

## Decisions log

- D1 (2026-10-03, Manager): #100 and #101 are one unit — both are test
  work on `tests/screenshot.rs`, and the suite's CI cycle (ubuntu ~20
  min, Windows 40–75 min per push) is the unit's largest fixed cost.
- D2 (2026-10-03, Manager, M3): no persona gate — test and harness
  plumbing; the two writer corrections implement sentences the spec
  already carries for shapes no normal use produces.
- D3 (2026-10-03, Manager): the harness hold knob for the Clear worker,
  if the senior developer judges it the honest proof of "never blocks the
  UI thread", is test plumbing in `FASTCULL_KITCHEN_COOK_MS`'s family —
  announced on stderr, owned by test-harness.md — and not a setting; brief
  008 D13 (no `FASTCULL_CACHE_DIR`) and D42 (no new environment variable
  for a setting) stand.
- D4 (2026-10-03, Manager): the deferrable guards are IN this unit — the
  user asked for the issues to be worked, not triaged — with the suite
  economy rule R8 keeping the cost down; a guard whose approved shape has
  a condition that cannot be met is recorded, not forced.
- D5 (2026-10-03, Manager, on the senior developer's plan): D47 is the
  exception sentence — a key created inside a hand-written inline table
  carries no note and the braces stay the user's shape — because the
  alternative, expanding the table, produced an unparsable file and moved
  the group in the measurement (toml_edit 0.22.27, `into_table()`), and
  D5 of brief 008 (a hand-edited config is the user's data) weighs a
  rewritten line heavier than a missing comment; no code change, a
  pinning test. AC30 (auto-advance off holds at 1:1 — "at every zoom") and
  AC31 (the core tests' scratch dirs removed on `Drop`, keeping a red
  test's evidence; 1,086 leftover dirs measured on the seat) are IN. The
  #101 gate is `wait:failed badge 11 laid out` in front of `dump.t1`, not
  the overlay-drop mark, which is emitted only while the overlay is up
  and would have hung the run that went red; the race did not reproduce
  on this seat under load (0 of 10 on either binary), so the recorded
  Windows trace is the old-red. The driven suite runs as thirds re-split
  from `--list` on this seat (118 tests, 910 s) — a directive candidate
  for the agent files' stale "87 tests, 318 s + 288 s", not a rule
  change. AC22 stays ticked with TP-3's clause (the claim is pinned; TP-3
  improves the premise). AC27 is the strand in the keyword test. The
  harness hold knob `FASTCULL_CLEAR_HOLD_MS` is test plumbing (D3).
- D6 (2026-10-04, Manager, at the merge): the verdict trail — review
  round 1 CHANGES_REQUESTED (a blocker: AC28's underline strand red on
  windows-latest on correct code — a 3 px band flush with the tab's mark
  held one underline row of two there; two minors), fixed in three
  commits and APPROVED; QE round 1 FAIL on two majors in test power, each
  with a fix QE measured in a worktree — TP-3's revert diagnostic ranged
  from the geometry wait's echo, not the window's landing, and named 0 of
  16 reverts under load (the brief's and the issue's wording said
  "landing"; AC22's sentence corrected, `(QE 2026-10-03, D1)`), and AC27's
  "keeps its text" had no reader (a `revert=""` premise at the typed and
  inert dumps, red under the commit-and-clear mutant, `(QE 2026-10-03,
  D2)`) — plus a seat figure in test-harness.md's Behaviour (dropped,
  D3) and a second load residual the failed-cursor test's doc now names
  (D4); re-review APPROVED (two nits: a retraction's wording, this log's
  entry); QE round 2 PASS — one minor, D5, the test doc's "a few ms"
  wording, fixed here as a comment fixup. The Manager's rulings of round
  1 recorded: the two load residuals' clock-based steps (the fixed 15 s
  first `End`, the corrupter's 12 s deadline) are a follow-up issue, not
  this unit's; the other seat and CI figures in test-harness.md's
  Behaviour are an M10 sweep after this unit; the keyword editor's own
  text has no dump reader (a typed comma is hidden by the commit's split)
  — a recorded limit, in the follow-up issue; the failed-cursor test
  leaves its scratch (pre-existing hygiene) — the same issue. Directive
  candidates for the user: the driven suite is 120 tests, thirds of ~327
  + 280 + 326 s in debug on the idle seat; a third runs as `cargo test
  --workspace --locked -- --test-threads=1 --exact <names>` — CI's one
  feature resolution — because `--test screenshot` builds a second
  variant of the app. CI green on both runners at 3467cc0 (ubuntu
  19m27s, windows 44m57s). #100 and #101 close with PR #103.
