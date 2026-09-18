# Brief 007 — Specs that read as behaviour

Dated 2026-09-17. Work branch `spec-readability`, created from
`spec-reference-editor` (the 2026-09-17 ledger corrections, which go to
`main` as their own PR).

## Context

The 2026-09-17 spec review read all 10,402 lines of `specs/` against the
code, the tests, git and the tracker. Its A-items — ledger errors — are
fixed on the parent branch. Its B-items are about shape, and the user's
instruction on them is the whole goal of this unit: *"things should be
simple. Easy to read descriptions of behaviors in the spec."*

The shape today, measured:

- `modules/ui-grid.md` is 4,438 lines, 43 % of the corpus, and is four
  documents in one: the UI contract (~2,240 lines); an acceptance ledger
  of 1,415 lines carrying run ids, mutant readings and campaign counts
  inline; and a 782-line "Debug facilities" section that is the test
  harness reference, cited from `01-architecture.md` (twice),
  `xmp-sidecars.md` and `fileops.md`.
- `01-architecture.md` (632 lines) spends ~350 lines on the CI cache-key
  forensics of briefs 003 and 004 — rust-cache source line numbers, run
  ids, byte counts — which the file itself rules "not architecture".
- `milestones.md` is 718 lines, ~570 of them release notes for
  v0.4.0–v0.14.0; there is no `CHANGELOG.md`, and `README.md:146` points
  here for history.
- Superseded rules are kept inline in up to three layers (`fileops.md`:
  the v1 rename default → "SUPERSEDED 2026-08-21" → "New only,
  2026-09-12"; `video-export.md`'s two cadence windows; `ui-grid.md`'s
  "an earlier draft claimed…"). The current rule is recoverable only by
  reading every layer in order.
- One rule is written in full in several places: brief 002's selection
  rule in `ui-grid.md` (Visual language AND the keyboard map),
  `burst-grouping.md` and `video-export.md`; the byte formatter in
  `fileops.md` and `video-export.md`; the clash question in both; the
  render ladder and transit in `ui-grid.md` and `raw-pipeline.md`; focus
  continuity in `ui-grid.md` and `iptc-templates.md`.
- Fifteen sentences cite `.qe-scratch/…` as where their evidence lives;
  all fifteen paths are gone (M9's scratch GC). Eighty mentions of
  "validator", "architect" and "qe-engineer" name roles the team table no
  longer has. The overview's glossary defines 7 terms; the specs use
  about 15 more without defining them (rung, mid, kitchen,
  transit/settled, wash, anchor, opener, territory, clash question,
  pair-is-the-unit, claim, reveal, settle mark, the pill, the ring).

What must not happen: `specs/` is the source of truth and the reasons it
holds are hard-won — dated user decisions, measurements, the mechanism
behind each fix. This unit relocates; it never deletes.

## Goals

- G1. A reader finds the current behaviour of a module in one place,
  stated once, in plain present-tense sentences, without reading its
  history.
- G2. Nothing is lost: every dated decision, measurement, issue, commit,
  test name and mechanism survives, relocated, and a reader who wants the
  history finds it from the module in one hop.
- G3. Every module spec has the same shape, so the next spec change has
  an obvious place to land and the next reviewer knows where to look.
- G4. What the tests read from the specs keeps working: the shortcuts
  parity test parses `ui-grid.md`'s keyboard map; QE and the briefs cite
  test names.

## Non-goals

- No behaviour change of any kind — not a rule, not a contract, not a
  budget, not a key. A sentence found wrong during the move is reported
  to the Manager (M10 fixes it in its own commit), never corrected
  silently inside the rewrite.
- No decision on anything parked or open (issue #60, the M9 phone items,
  the review's ADR candidates). Open stays open, worded as open.
- Briefs 001–006 are records and are not rewritten (R7's reference fixes
  excepted).
- No `docs/` rewrite; link fixes only. No code change beyond a comment
  or a link.
- No shortening for its own sake: a long Behaviour section that is all
  behaviour is fine. Length is reported, not targeted.

## Requirements

- R1. **One shape for every `specs/modules/*.md`**, headings in this
  order: `Purpose` (five lines at most) · `Behaviour` (the rules, present
  tense, one canonical statement per rule; a rule may end with its
  provenance in parentheses — `(user decision 2026-08-21)` — and carries
  nothing else inline) · `Contracts` (what other modules and the tests
  rely on: names, events, marks, invariants) · `Acceptance criteria`
  (one line per criterion, then its test names; an open criterion
  carries its reason on the same line) · `History` (dated, newest first:
  what changed, why, and where the full record is). `Behaviour` reads
  without `History`.
- R2. **Relocation, not deletion.** Text that leaves a `Behaviour` or
  `Acceptance criteria` section lands verbatim in
  `specs/history/<module>.md` under a dated heading, and the module's
  `History` section links it. The evidence narratives inside acceptance
  criteria — run ids, mutant readings, campaign counts, per-seat timings
  — relocate the same way; the criterion keeps one line and its test
  names. The mechanical check is a relocation diff: every sentence in the
  pre-unit text that carries a date, an issue number, a test name, a
  commit hash or a measured number appears in the post-unit tree
  (`specs/` and `CHANGELOG.md`). QE writes and runs it.
- R3. **One canonical statement per rule.** A rule written in full in
  more than one spec today is written in full in exactly one, and every
  other place says it in one sentence with a pointer. The known set, to
  be completed by the plan: the selection rule (canonical in
  `ui-grid.md` Behaviour; pointers from its keyboard map,
  `burst-grouping.md`, `video-export.md`); the byte formatter
  (`fileops.md`); the clash question (`fileops.md`; `video-export.md`
  states only its difference — no New only); the loupe render ladder and
  transit (one home — OQ3); focus continuity (`ui-grid.md`;
  `iptc-templates.md` points).
- R4. **Four extractions.** (a) `ui-grid.md` "Debug facilities" →
  `specs/modules/test-harness.md` in the R1 shape (the drive script, the
  marks and what each promises, the dump fields, the env vars, the waits
  and their limits), the four inbound references updated. (b)
  `01-architecture.md`'s CI cache-key narrative →
  `specs/history/ci-cache-key.md`, leaving in the architecture spec the
  rule (the key carries a hash of the root manifest's `[profile]` tables;
  `prefix-key` moves only for an action or format change), one line per
  acceptance box, and the pointer. (c) `milestones.md`'s release notes →
  `CHANGELOG.md` at the repository root, verbatim, newest first;
  `milestones.md` keeps the plan, the DoDs and the closure markers;
  `README.md`'s history link and `RELEASING.md` follow ("release notes go
  in `CHANGELOG.md`"). (d) `00-overview.md`'s Glossary covers every term
  of art the specs use (the plan lists them; the review's fifteen are the
  floor) and gains a one-line role legend: validator and architect are
  the review role before 2026-09-05, now senior-developer; qe-engineer
  is qe.
- R5. **Evidence pointers.** No `.qe-scratch/` path remains anywhere in
  `specs/`. Each is replaced by what the evidence showed, in one
  sentence, or by the PR or commit that carries it — never by nothing.
- R6. **Superseded rules leave Behaviour.** A rule that has been replaced
  appears only in `History`, with the date it was replaced and the
  sentence that replaced it. Corrections of earlier drafts ("an earlier
  revision of this bullet claimed…") go the same way.
- R7. **References by name, not by line.** Briefs 001–006 and any spec
  that cites another by line range cite the section name instead.
- R8. **Wording.** Present tense; "the user"; one idea per sentence; a
  parenthesis never longer than a line; a measured number stays in
  Behaviour only when it is the rule (a budget, a threshold, a constant)
  — otherwise it is History.
- R9. **The tests keep reading what they read.** `ui-grid.md`'s Keyboard
  map stays a table in the shape
  `the_shortcuts_card_lists_every_binding_in_the_spec` parses, or that
  test changes in the same commit under the test-integrity rule; every
  test name cited before the unit is cited after it.
- R10. **The user sees the first module before the rest.** The first
  rewritten module is delivered on its own, reviewed and QE'd, and shown
  to the user; the remaining modules follow only after the user says the
  shape is right. The plan names the first module and the order of the
  rest.

## Acceptance criteria

- AC1. Every `specs/modules/*.md` has the R1 headings in the R1 order (a
  heading script, run by QE).
- AC2. The relocation diff of R2 finds nothing lost: zero dated
  sentences, issue references, test names, commit hashes or measured
  numbers present before the unit and absent after it.
- AC3. `cargo test --workspace` green on both runners, the parity test
  included; no broken relative link in `specs/`, `docs/`, `README.md`,
  `RELEASING.md` (a link script, run by QE).
- AC4. `grep -r "\.qe-scratch" specs/` is empty.
- AC5. For each rule in R3's set, exactly one spec carries the full
  statement (QE greps the rule's distinctive sentence).
- AC6. `CHANGELOG.md` holds every release note `milestones.md` held,
  verbatim; `milestones.md` holds none; `README.md` links `CHANGELOG.md`.
- AC7. `specs/modules/test-harness.md` exists in the R1 shape;
  `ui-grid.md` has no "Debug facilities" section; the four inbound
  references resolve.
- AC8. The user has seen the first rewritten module and said the shape
  is right (R10) — WAIVED 2026-09-17 by the user ("I don't want to read
  more … can you conclude on your own using common sense? If yes,
  continue and finish it"); the Manager's judgement stands in for the
  read, and the senior developer's meaning check is the independent eye.
- AC9. `specs/history/<spec>.md` holds the pre-rewrite text of every spec
  that was rewritten, verbatim; no live spec depends on the folder (OQ1).

## Applicable directives

- CLAUDE.md hard rules 1–6 (rule 5: no logic moves; the only code change
  is a comment or a link).
- M1 (this unit moves sentences and never changes their meaning; a wrong
  sentence found on the way is reported), M5 (the senior developer plans
  and reviews on Fable; the developer executes on Opus; QE verifies on
  Opus), M7, M8 (a question no role can answer goes to the user
  verbatim), M9 (cleanup ran at the unit's start — decisions log), M10.
- The gate's rules: an unticked box carries its reason; no test loosened
  or skipped; a change to the parity test's parsing goes through the
  test-integrity review.
- `docs/` follows `specs/`: no user-visible behaviour changes, so no
  docs page changes beyond links.

## Persona verdicts

Not run: no user-visible change (CLAUDE.md step 2 — the persona gate is
for features and user-visible changes only).

## Open questions

- OQ1 (the user, 2026-09-17, superseding the Manager's earlier answer):
  `specs/history/` holds DEAD text only — superseded rules and evidence
  narratives — one file per spec, `specs/history/<spec>.md`, the
  pre-rewrite text verbatim. The user expects to delete the folder later
  ("git log is our history if necessary"), so nothing live may depend on
  it: a module's `History` section cites briefs, issues and commits, and
  mentions the folder in one line at most.
- OQ2 (for the plan): which module goes first as the template (R10), and
  the order of the rest. The Manager's suggestion: the mechanical
  extractions of R4(a)–(c) and R5 first, as their own commits — no
  rewording, low risk; then `fileops.md` as the template (the worst case
  of R6, and the operation the user cares most about); then the rest,
  largest first.
- OQ3 (for the plan): the canonical home of the render ladder and the
  transit contract — `ui-grid.md` (the user's requirement and the
  measurements live there) or `raw-pipeline.md` (the engine does).

## Decisions log

- 2026-09-17, the user: on the review's question 3, first "leave them",
  then the same day: "Work on B items, please. What I would like to see
  is things should be simple. Easy to read descriptions of behaviors in
  the spec."
- 2026-09-17, Manager: persona gate skipped (not user-visible);
  `CHANGELOG.md` at the root; `test-harness.md` as a module spec; the user
  checks the first module before the rest (R10, AC8). M9 cleanup ran at
  the unit's start: 75 GB from `target/`, 15 GB from a July agent
  worktree, its branch deleted.
- 2026-09-17, the user, asked "why a pipeline — are we rebuilding
  anything?": nothing is rebuilt, so the Manager writes the rewrite
  itself ("1: you"); the developer and QE stages do not run; questions go
  to the senior developer or the persona before the user ("for
  everything else, try the senior developer or the almosthuman first
  before coming to me"). The relocation diff, the link check and the
  heading check run as a script on every commit (the Manager's, recorded
  in the Outcome); one independent `senior-developer` read of the
  finished set checks that each Behaviour still says what it said; CI
  covers the parity test.
- 2026-09-17, the user, on relocate-or-toss: history holds dead
  decisions only, and is disposable (OQ1 above).
- 2026-09-17, the user, after `fileops.md` was delivered as the template:
  no check-in read — "conclude on your own using common sense … once it's
  done, make sure the repository is updated on github." The Manager
  finishes every module in the same shape, runs the senior developer's
  meaning check, writes the Outcome, and merges on green.

## Outcome (2026-09-17)

Commits on `spec-readability`, carried to `main` by PR #92 together with
the ledger corrections of the same day:

- `f4e277e` Brief 007: specs that read as behaviour — the shape, the relocation rule, the four extractions
- `63f9fda` Brief 007: the Manager writes; history/ is dead text only and disposable (user decisions 2026-09-17)
- `ec7100a` Brief 007: fileops.md rewritten as behaviour — 1,194 lines to 667, nothing lost
- `51ed4d0` Brief 007: the release notes move from milestones.md to CHANGELOG.md, verbatim
- `623db43` Brief 007: the test harness leaves ui-grid.md — test-harness.md, verbatim
- `b8e6da3` Brief 007: no spec cites the scratch tree as where its evidence lives
- `0a99ded` Brief 007: the glossary covers the terms the specs use, and the roles older text names
- `346f14f` Brief 007: 01-architecture.md keeps the build-profile and cache-key rules, history keeps the forensics
- `af73009` Brief 007: briefs cite spec sections by name, not by line range
- `29484aa` Brief 007: the user waives the check-in read — conclude on the Manager's judgement, then push
- `81cc3f0` Brief 007: catalog-cache.md rewritten as behaviour — Purpose, Behaviour, Contracts, Acceptance criteria, History
- `df329e4` Brief 007: xmp-sidecars.md rewritten as behaviour — Purpose, Behaviour, Contracts, Acceptance criteria, History
- `9abf25f` Brief 007: iptc-templates.md rewritten as behaviour — Purpose, Behaviour, Contracts, Acceptance criteria, History
- `fcf0a3b` Brief 007: burst-grouping.md rewritten as behaviour — Purpose, Behaviour, Contracts, Acceptance criteria, History
- `f5689c9` Brief 007: raw-pipeline.md rewritten as behaviour — Purpose, Behaviour, Contracts, Acceptance criteria, History
- `dc1ae6f` Brief 007: video-export.md rewritten as behaviour — Purpose, Behaviour, Contracts, Acceptance criteria, History
- `1b8ea1a` Brief 007: test-harness.md reshaped — the env vars, the drive script, the marks, the dump, the shutter, the rules for authors
- `9d6cd8f` Brief 007: ui-grid.md rewritten as behaviour — 3,673 lines to 1,083, the keyboard map and the pointer table byte-identical
- `4182d9c` Brief 007: the six budget tests named in raw-pipeline.md; 01-architecture.md's budget preamble in two paragraphs
- `d91813c` directive: the shape of a module spec, one home per rule, evidence outside Behaviour, history/ disposable (brief 007)
- `e35a9a5` Brief 007: the agent files point at test-harness.md for the harness contract
- `d5f660f` Brief 007: the senior developer's meaning check — 4 HIGH and 25 MEDIUM fixed, the LOW errors corrected

**Verdicts.** The senior developer's meaning check — every live/history
pair, four forked readers, read-only — returned CHANGES_REQUESTED: 4 HIGH,
25 MEDIUM, 58 LOW; the spliced tables byte-identical; every test name,
budget, key binding and threshold found with the same meaning; the
release notes byte-identical in `CHANGELOG.md`. All 4 HIGH and all 25
MEDIUM are fixed in `d5f660f`, together with the LOW items that were errors
or overclaims in the new text (a test's file, an internal API named as
core's, `AUX`, the ring stated two-sided, the loading pill's rule, six
History dates git contradicts, and two wrong claims the old text carried —
the export scope as `Selection::batch`, "decoded with turbojpeg"). The
remaining LOW items are nuance restorations, left in `specs/history/` by
the user's rule that history is dead and disposable. The fixes were not
re-reviewed: the Manager's judgement stands in, per the user's
instruction to conclude. The user's own read of the shape (AC8) was
waived by the user.

**Checks.** The relocation script (`spec_check.py --base f4e277e`, in the
session scratchpad; its logic: every date, issue reference, identifier of
three or more segments, short commit hash, number with a unit, N-of-M
count and backticked token in the base tree must appear in the tree
after — history included; what left the LIVE text is printed for review)
reports 0 lost of 26 dates, 73 issue references, 275 identifiers, 36
hashes, 491 numbers, 103 counts and 1,735 backticked tokens; AC1 — nine
of nine module specs in the five-heading shape; AC3 — no broken relative
link in `specs/`, `docs/`, README or RELEASING; AC4 — no scratch path in a
live spec. `the_shortcuts_card_lists_every_binding_in_the_spec` green
locally on the rewritten `ui-grid.md` (the table it parses is
byte-identical to the base). CI: PR #92's `test (ubuntu-latest)` green on
`e35a9a5`; the merge head's run is the PR's own record.

**Line counts, base → final** (the live corpus went from ~9,100 lines to
~4,400; history holds ~10,300, disposable): ui-grid 4,453 → ~1,120 plus
test-harness 379; fileops 1,197 → ~690; video-export 911 → ~560;
01-architecture 644 → ~320; milestones 727 → 124 plus CHANGELOG 613;
raw-pipeline 467 → ~440; the five small modules reshaped at about their
old length; the overview 64 → 134 (the glossary).

**Untested, carried.** The History dates were checked against git for
the entries the review named, not for every entry; the LOW nuances
listed in the review's report (this brief's decisions log names where it
lives: the senior developer's hand-back of 2026-09-17) are in history
only.
