# Brief 005 — a fourth answer to the clash question: copy only the new picks, leave what is already there untouched (issue #86)

Date: 2026-09-12. Issue #86. Branch `copy-only-new` from `main` 0209d8f.
Scratch: `.qe-scratch/pipeline-005/` (persona hand-off and report, plan,
review and QE reports). A feature — a fourth answer on an existing
question — so the persona gate ran (verdicts below).

## Context

The user's report (issue #86, 2026-09-12), verbatim: "On the Copy Pick
screen, in case there are files that are already in the folder, add a new
option to only copy files that doesn't exist into the new folder yet.
Call it option N. Use case: I just remember I needed to copy 4 more files
to the archive after editing the photos already there. If I pick
override, my darktable changes will be gone. And I don' t want to
repopulate the folder with new files, so I need a new option to just copy
the new ones."

What FastCull does today (fileops.md, "The clash question", implemented
2026-08-21): after the flush barrier and the final replan, every name the
plan would write — RAW and sidecar, after template expansion — is checked
against the destination; if any is occupied the dialog asks once for the
whole run, with three answers: `B` Keep both (every clashing pick lands
under the first free `_k` suffix, RAW and sidecar in lockstep), `O`
Overwrite those N (replaced in place; a byte-identical destination RAW is
re-verified rather than re-sent, and its sidecar rewritten if it
differs), `Esc` Cancel (nothing copied at all). `Enter`, `Space`, `Y` and
`N` are inert on the question; every accelerator is swallowed; a
non-answer key flips the "Pick one: B, O or Esc." line.

The gap is already on record. fileops.md carries it as "the persona's
blocker" of 2026-08-21: a destination sidecar that differs is
byte-replaced under Overwrite, darktable's history stack lives in a file
of exactly the name FastCull writes (`DSC01234.ARW.xmp`), and "with 'skip'
gone there is no other answer that adds new picks to that folder". The
user's ruling of 2026-08-22 was "overwrite means overwrite" — no sidecar
merge, the warning line is the whole mitigation, Keep both or a fresh
folder are the escapes. The user has now met the case those escapes do
not cover: Keep both would duplicate the 144 edited frames as `_1` copies
beside the originals; a fresh folder splits the archive. Neither adds
four picks to an archive and leaves it alone.

Why the answer is new and not a revival: v1's `ExistsMode::Skip` was a
FORCED skip decided by session memory, and that memory is what caused the
2026-08-21 bug (issue #14: a folder emptied by hand — the sidecar came
back as a refresh, the RAW never did). The clash question's rule — the
disk decides, the user answers once, per run — is unchanged by this unit:
the new answer is the user's explicit per-run choice about names the
plan found occupied on disk. `SessionCopies` still reads and never
decides.

## Goals

- G1. A fourth answer on the clash question, key `N` (the user's choice),
  that copies every clash-free pick exactly as today and touches nothing
  at the destination for a clashing pick — not its RAW, not its sidecar,
  not a read of either.
- G2. The dialog, the report, the spec and the docs say what that answer
  does and does not do: what copied, what was left, and that "all
  checksums verified" covers only what this run copied.
- G3. The three existing answers, their keys, their wording and every
  guard on the question (Enter/Space inert, accelerators swallowed, no
  default styling, Esc back to the plan) are unchanged.

## Non-goals

- No change to the video-export dialog's own clash question (one `.mov`
  file: for a single file, "skip" is Cancel).
- No verification (no read, no hash) of the skipped files — Overwrite
  remains the "is my export still bit-perfect?" pass.
- No sidecar merge (the user's ruling of 2026-08-22 stands).
- No change to "two picks, one name" (a name claimed by another pick in
  the same run is suffixed under every answer and never a clash), to the
  plan-preview line, to the free-space rule before the answer, or to
  session memory's read-only role.
- No per-file list, no per-file mode selector (the no-148-row-table rule).

## Applicable directives

- CLAUDE.md hard rules 1 (never write to a RAW — copies only, sidecars
  only, ADR 0003), 5 (business logic in `fastcull-core`; the app crate is
  a thin Slint bridge — the policy, the plan action, the report counts
  and the green-light rule are core's), 6 (budgets).
- fileops.md: the pair is the unit; one question per run, the answer is
  a whole-run policy and only a plan built with it executes (rule 3);
  nothing is replaced unless the user answered Overwrite (rule 4);
  session memory reads, never decides (rule 5); wording and keys (§6 —
  bare letters only, every accelerator swallowed, no default styling,
  counts in picks, Esc back to the plan); `earned_the_green_light` is
  core's rule.
- ui-grid.md key table: `N` is the reject key in the grid; the
  question's scope owns every key while it is up (issue #42, one key
  scope, no new stacking surface).
- docs page map (CLAUDE.md, M8): copy-picks ↔ fileops; faq ↔ everything
  else — both pages move in the same commit as the behaviour.
- M1 (spec first), M2 (UX choices decided on best practice after the
  gate, dated in the spec), M7 (never name the user), M8 (what no role
  can answer with certainty goes to the user verbatim).
- Rules of the gate: old-red-first is not applicable (a feature, not a
  bug fix); a mutant for every new guard; the senior developer's veto on
  every test change; deferring a spec acceptance criterion needs the
  user's OK.

## Requirements

Counts below use the persona's evening (4 new picks, 144 clashing) as the
example; every count is in picks, never files, and the singular forms
follow the question's existing habit ("The other 1 copies normally").

- R1. **A fourth `ClashPolicy` variant** in `fastcull-core` (the senior
  developer names it), the app's answer to the `N` key. Under it a pick
  whose destination pair is occupied — RAW name or sidecar name, the
  existing pair-is-the-unit rule — is not written, not read and not
  hashed: neither member of the pair is opened. Every clash-free pick
  copies exactly as under the other answers (temp name, BLAKE3-verified,
  no-clobber commit). A name claimed by another pick in the same run
  stays what it is today: suffixed, counted in `shared_name`, never a
  clash — and therefore copied under this answer.
- R2. **A sidecar-only clash is skipped like any clash** (a stray `.xmp`
  beside no RAW makes the pick a clash; landing a RAW beside a sidecar
  that describes another photograph is the one thing the module must
  never produce) and is **counted apart** in the report (R5), because
  under this answer it is a photograph that did not land.
- R3. **Free space**: only the clash-free bytes must fit — the same rule
  as before the answer and under Overwrite (fileops.md rule 3).
- R4. **Progress counts the new picks only.** The `CopyEvent::File`
  total is the number of picks this run copies, and no event is emitted
  for a skipped pick: the line reads `Copying 1 / 4`, never
  `Copying 1 / 148` and never a "Skipping" flicker (persona G1 — the
  misread §6 already records).
- R5. **The report** counts the skipped picks, and the sidecar-only
  ones among them, in `CopyReport` (core), and `report_lines` (pump.rs)
  prints, in this order after the copied line:
  `144 already had files with these names here — left untouched, not
  re-checked` (singular: `1 already had a file with this name here —
  left untouched, not re-checked`), then, only when it happens, `1 of
  those is a stray .xmp with no RAW beside it — that pick was not
  copied` (plural: `N of those are stray .xmp files with no RAW beside
  them — those picks were not copied`). "All checksums verified" stays
  on the copied line; `earned_the_green_light` is unchanged (copied +
  identical > 0): a run that skipped everything and copied nothing
  prints the skipped line and no green light, and never "Nothing needed
  copying". The wording says the NAMES were taken, never that the
  photographs are there (two bodies, one name: they are not).
- R6. **The ✓ badge**: a skipped pick is not in `report.landed` (this
  run verified nothing about it); a record the session already holds
  for it survives, because `SessionCopies::refresh` finds the file still
  there. Session memory still reads and never decides.
- R7. **The dialog** (copy-state 3): a fourth `AnswerRow`, key `N`,
  FIRST, label `New only — copy the 4, leave the 144 already here
  untouched`; then `B`, `O`, and `Esc` set apart, in that order
  (increasing consequence, the §6 rule extended by one row). The nudge
  reads `Pick one: N, B, O or Esc.` The amber warning line gains one
  clause: `Overwriting also replaces those files' .xmp sidecars — edits
  made at the destination by another app (darktable) are lost. New only
  leaves them alone.` `N`/`n` answers only as a BARE letter (the same
  modifier guard as `B`/`O`); `Enter`, `Space`, `Y`, `P`, `X` stay inert;
  no row carries default styling, the N row's label is the ordinary
  colour; the row answers on click like the others. **N is always
  offered**: with zero clash-free picks the row reads `New only — nothing
  new to copy, leave the 148 already here untouched`, and answering it
  runs a copy of nothing whose report is R5's skipped line (Manager
  decision D6 — a row that appears and disappears moves `B` and `O`
  under the mouse on exactly the destructive question). The video-export
  dialog's question is untouched.
- R8. **The `{seq}` note** (the user, 2026-09-12, on the persona's
  question 1: "Warn on the plan line + docs"): when the template in play
  contains `{seq}` and the plan has at least one clash, the plan preview
  carries a note in the spirit of `{seq} numbers the whole session —
  the names already here may now belong to other frames`, beside the
  existing `4 new · 144 already exist here — Copy will ask what to do`.
  The fact ("this template contains `{seq}` and there are clashes") is
  core's to compute (a `CopyPlan` field); the sentence is the bridge's
  to print. No refusal: every answer stays available.
- R9. **Docs, same commit** (page map: copy-picks ↔ fileops, faq ↔
  everything else): `docs/copy-picks.md` — the question block gains the
  N row, the bullet list a **New only** entry, the amber darktable box
  says to answer New only, "Running it again" flips (a few more picks →
  New only, which adds them and touches nothing; captions changed on
  frames already there, or the bit-perfect check before wiping the card
  → Overwrite, which re-verifies and replaces sidecars — darktable edits
  included), with the `{seq}` caveat beside the recommendation;
  `docs/faq.md` — the "will FastCull touch my edits?" answer names New
  only as the answer that adds picks and leaves edits alone.
  `docs/export-video.md` is unchanged.
- R10. **Spec, first** (fileops.md): §2 gains the fourth answer; §3's
  "which total has to fit" names it; §6 records the four rows, the key,
  the nudge, the warning clause, and moves `N` out of "as are `Y`/`N`"
  (`Y` stays inert); the "Recorded consequence, and the persona's
  blocker" paragraph records its resolution; the `ClashPolicy` doc
  comment ("There is no 'skip the clashing files' answer") and the
  `recopied` comment in `plan()` ("there is no skip any more") become
  false and change in the implementation commit; the acceptance criteria
  below land in the spec's list.
- R11. **Tests.** Core: the new policy leaves both members of a clashing
  pair byte-for-byte and mtime-unchanged, including a destination
  sidecar that DIFFERS from the source (the darktable case), while the
  clash-free picks copy and verify; a sidecar-only clash is skipped and
  counted apart; the free-space check follows the answer; the `File`
  total equals the clash-free count and no event names a skipped pick;
  a destination file under a clashing name made unreadable (unix:
  `chmod 000`) does not fail the run (the pair was never opened); the
  #14 shape under the new policy — a hand-emptied folder holds no clash,
  so the gone copies are new and copy again, RAW and sidecar together;
  `report_lines` for the skipped line, the stray-sidecar line, the
  all-skipped run and the green light. App: the question shows the N
  row first with its counts, `N` copies the new pick and leaves the
  differing destination sidecar byte-for-byte, the report carries the
  skipped line, `Y` stays inert, and the `{seq}` note appears on the
  preview when a `{seq}` template meets a clash — driven through the
  real dialog like `copy_picks_asks_once_and_each_answer_does_what_it_says`.
  A mutant for the guard: an executor that writes (or reads) the skipped
  pick's sidecar must turn the sidecar-bytes (or the `chmod 000`)
  assertion red.

## Acceptance criteria (also land in fileops.md)

- AC1. Under the new answer, no clashing pick's RAW or sidecar is
  opened: both are byte-for-byte and mtime-identical after the run, a
  differing destination sidecar included, and an unreadable one does
  not fail the run.
- AC2. Every clash-free pick copies and verifies exactly as under the
  other answers; a same-run shared name is suffixed and copied.
- AC3. A sidecar-only clash is skipped and reported apart.
- AC4. The progress total equals the number of picks copied; no event
  for a skipped pick.
- AC5. The report prints the skipped line (and the stray-sidecar line
  when it applies); the green light attaches to copied files only; an
  all-skipped run prints the skipped line and no green light.
- AC6. Free space: only the clash-free bytes must fit.
- AC7. The dialog: N row first, wording as R7, `N` bare-letter only,
  `Y`/Enter/Space/accelerators inert, the nudge and the warning clause
  as R7, N always offered.
- AC8. The `{seq}` note appears on the preview when a `{seq}` template
  meets a clash, and only then.
- AC9. The docs pages of R9 say what the dialog does, in the same commit.

## Persona verdicts (almost-human-user, 2026-09-12; report in
`.qe-scratch/pipeline-005/PERSONA.md`)

MUST-HAVE: the N answer ("the first time the copy re-run and my darktable
folder have been allowed to coexist"); skip the whole pair, never read or
hashed; free space counts the new bytes only; the progress line counts the
new ones only (G1). USEFUL: the report's "left untouched, not re-checked"
with the green light on the copied line only; sidecar-only clashes
counted apart (G2 — "without that line I believe the photo is archived
and it is not; that is a data-trust failure"); N first in the row order;
the warning line pointing at N; the docs flip. SHRUG: no badge for a
skipped pick; hiding the N row when nothing is new (leaning USEFUL);
the video-export question unchanged; `Y` inert. IN-MY-WAY: a read-only
checksum of the skipped picks — cut. Gaps: G1 and G2 (both in the
requirements), G3 the `{seq}` re-run trap (question 1 to the user,
answered — R8), G4 the NAS plan-time stat cost (pre-existing, recorded
in fileops.md "What that fix does NOT do", not this unit).

## Open questions

- OQ1 (the persona's question 1, relayed verbatim 2026-09-12): "When
  you add a few picks to a folder that already holds an earlier copy, is
  a rename template with `{seq}` ever in the field? If yes, the numbers
  shift with the new picks and 'New only' would copy the wrong four and
  skip the right ones under a green report — then we need a guard
  (refuse, or warn on the plan line, when a `{seq}` template meets a
  folder that already holds files). If you always keep original names
  when you add to a folder, one sentence in the docs is enough. What
  turns on it: a guard in the plan versus a line in the docs." **The
  user, 2026-09-12: "Warn on the plan line + docs"** — no refusal (R8).

## Decisions log

- D1 (the user, issue #86): the key is `N`. Recorded consequence: §6's
  "as are `Y`/`N`" moves — `N` becomes an answer, `Y` stays inert. Not
  IN-MY-WAY at the screen (persona C1): the question is a stop, not a
  rhythm, so the reject cadence never reaches it, and N is the least
  consequential answer that still does something.
- D2 (Manager, 2026-09-12, persona C3): the label begins with "New", so
  `N` reads as New, not No: `New only — copy the 4, leave the 144
  already here untouched`.
- D3 (Manager, persona C2): row order N, B, O, Esc — increasing
  consequence, Cancel set apart.
- D4 (Manager, persona C5): report wording says the names were taken,
  not that the photographs are there; "not re-checked" said out loud.
- D5 (Manager, persona C6): skipped picks are never opened — no
  read-only verification pass; Overwrite remains the bit-perfect pass.
- D6 (Manager, best practice over the persona's SHRUG-leaning-USEFUL):
  N is always offered; with nothing new its row says so and answering it
  is an honest no-op with the skipped line as its report. A row that
  disappears moves the other rows under the pointer on the destructive
  question, and a stable layout outweighs one row of noise.
- D7 (Manager, persona C4): the warning line keeps "(darktable)" and
  gains "New only leaves them alone."
- D8 (Manager, persona 4): no badge for a skipped pick; an existing
  badge stays because the file is still there.
- D9 (Manager): the video-export question is unchanged, and
  `docs/export-video.md` with it.
- D10 (the user, OQ1): the `{seq}` note on the plan line plus the docs
  sentence; no refusal.
- D11 (Manager): G4 (plan-time stat cost on a NAS) is pre-existing and
  recorded in fileops.md; not this unit.
- D12 (Manager, 2026-09-12, review round 1): the senior developer's two
  minors — the video planner's compile-forced `NewOnly => Clash` arm had
  no test; a New only run whose every copy failed lost its "Nothing was
  copied" headline — fixed in 77f98c7 rather than deferred.
- D13 (Manager, 2026-09-12, QE round 1): QE's four test proposals (the N
  row's counts pinned on unequal numbers; a cancelled run's left counts
  at the executor; `copyprogress=` in the dump with the rendered line
  asserted; `N` inert on the video question), all APPROVED by the
  integrity review, implemented in cb501ec; the four minors close with
  them. No product code changed in that commit.
- D14 (deferred, not this unit; QE observation 2026-09-12): `human_bytes`
  has no KB tier, so the Keep both row reads `+1029480 B` for a ~1 MB
  clash. Pre-existing and cosmetic; a bookkeeping issue for a later unit.

## Outcome (2026-09-12)

Commits on `copy-only-new`: 6cd287f (this brief), e0c3071 (the spec
change), dc96f1c (core: the policy, the plan, the report, T1-T8),
0750b33 (the dialog: the N row first, the key arm, the layout marks, the
`{seq}` note, T9, the mouse round by name), 77f98c7 (review round 1's two
minors), cb501ec (QE round 1's four proposals). PR #87. Verdicts: senior
developer APPROVED (round 1 with two minors, fixed; round 2 APPROVED;
integrity review: four proposals APPROVED; round 3 APPROVED); QE PASS
(round 1 with four minors, all closed; round 2 PASS, no defects). CI green
on both runners at every head; the un-gated mouse rounds answer by name
on the Windows runner in debug and release.

Untested, carried (agreed by QE and the senior developer): the ✓ copied
badge surviving a New only run (no dump field reads the badge); a
sidecar-only clash on a name a same-run twin also wants (both halves
covered separately); the video question's nudge on an inert key
(`clipstate` asserted, the line is not); a real darktable round trip and
a NAS destination (fileops.md's NOT-VERIFIED list); 2,000 picks under New
only (1,000 reached; the arm calls no `occupied()`); Windows-specific
hostile names (issue #10); a mid-run sample of `copyprogress` (the
assertions read the final line by design, now a contract in ui-grid.md).
