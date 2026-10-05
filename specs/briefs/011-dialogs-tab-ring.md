# Brief 011 — the Copy Picks and Export dialogs own Tab: a keyboard ring of their own, so the window's Tab navigation never carries the keyboard behind the scrim (issue #98)

Date: 2026-10-04. Issue #98 (recorded at brief 008's plan, 2026-10-01,
by the senior developer's measurement in Slint's source). Branch
`dialogs-tab-ring` from `origin/main` after brief 010's merge (M12; the
branch was cut at e8c26ef while PR #103 waited for CI and is rebased onto
the merge before the senior developer starts). A bug fix against a rule
the spec already states — ui-grid.md "Modal keyboard containment": while
a modal is up "EVERY other key is swallowed" — so no persona gate; the
Settings dialog's ring (settings.md "Keyboard", brief 008) is the shape
the persona already accepted for the same problem.

## Context

Slint handles `Tab`/`Shift+Tab` at the WINDOW level only after the focused
item and every ancestor scope have ignored them, and then walks the whole
item tree — surfaces hidden behind a scrim included (`i-slint-core-1.17.1/
window.rs`, `process_key_input`; recorded in ui-grid.md "Slint facts this
module depends on" at brief 008). The Settings dialog's scope accepts both
keys and walks its own ring (settings.md "Keyboard": "`Tab`/`Shift+Tab`
walk the strip and the active tab's controls in order … and never leave
the dialog"). The Copy Picks and Export Frames as Video dialogs' scopes do
not: ui-grid.md's Slint-facts sentence records "the copy and export scopes
do not, a recorded gap". Pressing `Tab` in either dialog can move the
keyboard onto an element behind the scrim — the grid's key scope, an IPTC
field — against the containment rule, and a key typed next lands there.
No user report; measured, not seen in the wild.

## Goals

- G1. In the Copy Picks dialog and the Export Frames as Video dialog,
  `Tab` and `Shift+Tab` move the keyboard between the dialog's own
  controls, in a visible order, wrapping, and never leave the dialog.
- G2. Every other key the dialogs own (`B`/`O`/`N`/`Esc` on the clash
  question, `Enter`, `Esc`, the template field's typing) behaves exactly
  as today.
- G3. The spec's recorded gap closes: ui-grid.md's containment paragraph
  and its Slint-facts sentence, fileops.md's and video-export.md's dialog
  rules say what Tab does; the docs say it in one phrase each.

## Non-goals

- No change to the dialogs' layout, wording, states, plan or report
  lines, answers or keys other than `Tab`/`Shift+Tab`.
- No absorption of the hand-rolled scrims into `ModalScrim` (its own
  recorded follow-up, `main.slint`'s comment).
- No ring for About or the shortcuts card — they hold no control that
  takes the keyboard ("nothing in the card takes the pointer or the
  keyboard", ui-grid.md); `Tab` there is swallowed like every other key
  already — the plan verifies it with a driven press and records it.
- No change to the IPTC panel's own Tab order (ui-grid.md "Focus
  continuity": Tab cycling inside panel fields is review-verified).

## Applicable directives

- CLAUDE.md hard rule 5 (a keyboard ring is a `.slint` rule; nothing in
  core); the rules of the gate — old-red-first (a bug fix: a driven
  `key:tab` in each dialog on the pre-fix revision lands the keyboard
  outside the dialog — `focusowner` not `-1` — and the fix keeps it
  inside), a mutant for every new guard, the senior developer's veto on
  every test change.
- ui-grid.md "Modal keyboard containment" (issue #42's rule; "every
  dialog key scope — the copy, export and settings scopes — contains
  modals identically"), "Focus continuity" (the `-1` token for a dialog's
  own scope; a destroyed editor discards, a covered one commits), "Slint
  facts this module depends on" (the window-level Tab fact — the
  parenthesis about the copy and export scopes becomes false); settings.md
  "Keyboard" (the precedent: an explicit ring, `slot-ok`/`walk`, disabled
  controls skipped, wrapping; the matrix test
  `every_control_that_leaves_a_dirty_settings_field_commits_it_first` as
  the shape of a ring test); fileops.md "The clash question" §6 and the
  dialog's rules; video-export.md "The dialog"; test-harness.md (the
  `key:tab`/`key:shift+tab` tokens, the `copy card`/`copy buttons`/`clip
  card`/`clip buttons` layout marks, `focusowner=`).
- Docs page map: copy-picks ↔ fileops, export-video ↔ video-export (one
  phrase each: Tab moves between the dialog's controls).
- M1 (spec first), M7, M12 (the rebase before the senior developer
  starts).

## Requirements

- R1. **The Copy Picks dialog's ring.** The dialog's key scope accepts
  `Tab` and `Shift+Tab` and moves the keyboard over its own controls in
  visible order, wrapping, skipping a control that is disabled or not
  shown in the current state — the destination's Choose…, the rename
  template field, the Copy and Cancel buttons; on the clash question the
  answer rows' keys stay bare letters and `Tab` walks whatever controls
  that state shows (the plan names the ring per state, read from the
  `.slint`). The keyboard never leaves the dialog: `focusowner` stays
  `-1` through any number of presses, and a key typed after a `Tab`
  lands in the dialog or is swallowed, never in the grid or a panel
  field.
- R2. **The Export Frames as Video dialog's ring**, the same rule over its
  controls (the destination's Choose…, Export and Cancel; the report's
  Open folder; the clash question's state) — the plan names them.
- R3. **Entering a field by Tab selects its text**, as the Settings ring
  does (brief 008's QE D33: a programmatic `focus()` does not select;
  `select-all()` follows it), so a number or a name typed after `Tab`
  replaces what the field showed — the template field is the one field
  these dialogs have.
- R4. **About and the shortcuts card**: a driven `key:tab` while either is
  up changes nothing (`focusowner` unchanged, no mark, the card still up)
  — verified and recorded, no new code.
- R5. **Spec and docs, same commit.** ui-grid.md: the containment
  paragraph gains "including `Tab`: each dialog's scope walks its own
  controls" and the Slint-facts parenthesis ("the copy and export scopes
  do not, a recorded gap") is corrected in place with the retraction;
  fileops.md's dialog rules and video-export.md's "The dialog" gain one
  sentence each (the ring, its order, the wrap, the select-on-Tab);
  settings.md's "Keyboard" is unchanged but may point at the shared rule
  if the senior developer states it once in ui-grid.md (a rule lives in
  one spec); `docs/copy-picks.md` and `docs/export-video.md` gain one
  phrase each.
- R6. **Tests.** A driven test per dialog (or one table-driven test over
  both, the Settings matrix's shape): open the dialog on a real folder
  state that shows the controls (Copy Picks with picks; Export with a
  burst or a selection), press `Tab` N+2 times and `Shift+Tab` once,
  asserting after each press `focusowner=-1`, the expected control's
  focus mark (the plan names the marks — the dialogs' controls report
  their layout today; a focus mark per control may be needed, in
  test-harness.md's family), the wrap, and that a typed character after a
  Tab onto the template field replaced its text (R3) while a `Y` after a
  Tab onto a button marked nothing. Old-red on the pre-fix revision: the
  same script with `focusowner` leaving `-1` (the senior developer
  measures where it lands — the grid's scope or a panel field). Mutants:
  the scope's `Tab` arm removed (the keyboard leaves); a control left out
  of the ring (the walk skips it); the wrap removed; `select-all()`
  removed (the typed character appends). Existing tests at risk: every
  copy and export driven test that presses keys inside the dialogs
  (`Enter`, `Esc`, the answer letters) — unaffected unless one presses
  `Tab`; the plan greps for `key:tab` in those scripts.

## Acceptance criteria (they also land in ui-grid.md's ledger, and fileops.md's / video-export.md's)

- AC1. In Copy Picks, `Tab`/`Shift+Tab` walk the dialog's controls in
  order, wrap, skip disabled or hidden ones, and `focusowner` stays `-1`.
- AC2. The same in Export Frames as Video.
- AC3. A field entered by `Tab` is selected; the next character replaces
  its text.
- AC4. `Tab` under About or the shortcuts card changes nothing.
- AC5. The two dialogs' other keys and every answer behave as before
  (the existing copy and export tests stay green unchanged).
- AC6. The spec sentences and the docs say so; the Slint-facts gap is
  retracted.

## Decisions log

- D1 (2026-10-04, Manager): a bug fix against the containment rule — no
  persona gate; the Settings ring is the accepted shape.
- D2 (2026-10-04, Manager): the branch was cut from `origin/main` at
  e8c26ef while PR #103 (brief 010) waited for CI, to use the wait; it is
  rebased onto the merge commit before any role touches it (M12).
- D3 (2026-10-04, Manager, on the senior developer's measured old-red and
  plan). The old-red, head f1520b9, debug, this seat, 13 driven runs: in
  Copy Picks' plan state Tab 1 lands on Choose…, Tab 2 on the rename
  field, **Tab 3 on the grid's key scope** (`focus: keys gained`,
  `focusowner=0`) — a `Y` there marked a hidden frame (★2 → ★3, the cursor
  4/24 → 5/24), `Esc` left the dialog up, `N` rejected another; one
  `Shift+Tab` lands on `keys` at once; with the IPTC panel open the
  issue-#41 bounce belt protects the fields but nothing protects `keys`;
  the report state's third Tab lands on `keys` too; the clash question
  already swallows Tab with its nudge. In Export's plan state Tab 4 lands
  on `keys` and a `Y` marked a frame and **collapsed the selection**
  (against video-export.md's "never touches the selection"); one
  `Shift+Tab` the same. About and the shortcuts card swallow Tab (R4
  holds today — recorded, no code). Slint facts read for the plan: a
  disabled `FocusScope` refuses focus (why the greyed Copy was passed
  over), and a programmatic `focus()` on a disabled item walks on in tree
  order — so the ring never focuses a slot `slot-ok` has not approved.
  Rulings (M2): `Enter`/`Space` on a focused button press it — the
  fluent Button's own behaviour, now stated (Tab-to-Cancel-Enter cancels
  an export; Enter from the dialog's resting state still starts it); Tab
  on the clash question keeps today's "pick one of N/B/O/Esc" nudge; the
  *Use last* template chip stays pointer-only (recorded in fileops.md; a
  follow-up only if the user asks). The copy and export scopes contain
  About and the card in the bubble phase where Settings uses the capture
  phase — a follow-up issue at the merge (M3), not this unit. A
  `changed state` refocus to the scope is part of the ring: a control
  the new state destroys (Enter on the focused Copy destroys Copy; the
  worker's finish destroys Cancel) would otherwise dangle the focus.
  Commits A (copy ring + its test), B (export ring + its test), C (R4's
  press under About and the card, the canary facts, the suite-count
  sentence).
- D4 (2026-10-04, Manager, review round 1 — F1, major): the ring made a
  stale focus reachable on a normal mixed path — the keyboard on Choose…
  by Tab, a mouse click on Copy/Export starts the run, the state change
  disables Choose… and a disabled FocusScope ignores FocusOut, so after
  Esc Choose… wore a focus border while the keyboard was home. Fixed
  (fae8025): pressing Copy or Export puts the keyboard home BEFORE the run
  starts, so a control the run disables can no longer hold it; the two
  buttons report their layout (`copy copy-close`, `clip export-close`,
  6eed28b) so a test clicks them by name; the fourth canary gains fact 11.
  The `changed state` refocus's residual under an open menu was recorded
  as unmeasured (8fbb390) — see D6. Each press that activates a dialog
  button is checked at the press itself (a635228, F3).
- D5 (2026-10-04, Manager, QE round 1 — PASS with six minors, seven
  proposals; the integrity review ranked P1–P4 NOW): the small round
  landed P1 (the export ring's greyed Export driven — a third launch
  whose destination is a file; its one-token mutant had re-opened #98 in
  Export with the suite green), P2 (Ctrl+Tab and Ctrl+Shift+Tab inert in
  both dialogs, pinned), P3 (the running ring and the finish's refocus
  driven — the senior developer's Shape A: harness hold knobs
  `FASTCULL_COPY_HOLD_MS` and `FASTCULL_CLIP_HOLD_MS` hold the copy worker
  and the export writer before their first file, cancellable, through
  core's `execute_held`; spec first in 84c7785; test plumbing in
  `FASTCULL_KITCHEN_COOK_MS`'s family, not a setting — and no large
  fixture, so QE's question about sparse files on the Windows runner is
  moot), P4 (Enter in the rename field brings the keyboard home and the
  next Tab starts at Choose…). QE's D1 — the reviewer's F4: a re-plan that
  greys Copy or Export while it holds the keyboard leaves it a stale
  focus border beside the real one (the Use-last chip with a refused
  template, a Choose… whose plan fails, File › Open Folder under the
  dialog) — is cosmetic, no key goes astray; recorded as a residual in
  both ring sentences (e9e95ca) and deferred to the follow-up issue, not
  fixed here (the Manager, M2: three `.slint` paths the harness could only
  review-verify, and a Rust-ordered one).
- D6 (2026-10-04, Manager, QE round 2 — FAIL, one major in test
  construction): T1's P4 strand, under the test's own "home start removed"
  mutant, lands Return on Choose… — the native folder picker opened on the
  seat and the run hung 90 s — so the doc's mutant record ("red at
  dump.home") and its safety claim ("no script lands Enter or Space on
  Choose… under any mutant") were false; the plan's must-not was
  violated by a strand added after it. A bounded fix round: the strand
  split so no Enter or Space follows a landing no earlier launch has
  asserted, the doc corrected, the mutant re-run red where it should be;
  the export clash-question's Tab nudge guarded (a dump field for the
  nudge — the copy half is guarded, the export half was not); the spec
  corrections — the menu residual is MEASURED on Linux, where the menu
  bar is in-window (a run that ends while a menu is open takes the
  keyboard back to the dialog and the next Esc closes the report, the
  menu still drawn; with the refocus removed the pre-unit behaviour
  restored focus to the destroyed Cancel and the next Tab left the dialog
  — the refocus is the lesser evil, and a correct fix needs a design for
  the MenuBar's restore target: a follow-up issue, the Manager, M3);
  video-export.md's pre-existing "Esc in any dialog state closes" narrowed
  to the plan and report states; test-harness.md's Focus bullet gains its
  exception and the suite-count sentence is re-measured. Deferred to the
  follow-up issue with the menu residual: the rename field's slot write on
  a mouse arrival (needs a layout mark for the field), AC4 strengthened
  with the IPTC panel open, Enter on the focused plan-state export Cancel,
  and D5's stale-focus-on-re-plan residual. Relayed to the user (decided
  2026-10-05, D9): Space on a focused Close or Cancel closes on the
  key-press, and a second or held Space then picks the frame behind the
  closing dialog (measured 30 ms later) — option (a) as shipped (the mark
  is visible and undoable; Enter already behaves so on the report), (b)
  buttons acting on release, or (c) the grid ignoring a Space within a few
  hundred milliseconds of a close; the Manager recommends (a).
- D7 (2026-10-04, the user, at the circuit breaker — two consecutive QE
  FAILs at one stage): QE round 3 fixed everything round 2 asked (the
  home-start mutant red at launch 2's `dump.home` with no picker; the
  export clash-question nudge guarded by `clipnudged=`, E11 red; the menu
  residual recorded as measured on Linux; video-export.md's Esc sentence
  per state; test-harness.md's exception and suite size — 122 tests,
  1016 s) and found the same class one launch on: T1's launch 3 presses
  Return after Tabs taken from the home a STATE CHANGE leaves (click Copy →
  question → Esc → plan), a home no earlier launch asserts; under a
  one-token regression of the `changed state` reset (`slot = 0` for `-1`),
  not in the tests' own mutant lists, the Return lands on Choose… and the
  test fails as a native picker (or a 90 s Windows hang), not a clean red;
  the export test records the same exposure honestly, the copy test's
  safety note denied it. QE: FAIL, one more round (split each
  post-question path into its own launch, ~8 s per test). The Manager and
  the senior developer: merge with the risk recorded. Put to the user per
  the circuit breaker; the user chose ONE MORE ROUND, then merge. Its
  scope: QE3-P1 (each mixed path in a launch of its own, so every landing
  is asserted before any Enter or Space, in both tests; the docs made
  true; the A4/A5 regressions added to the mutant lists and shown red at a
  landing assertion with no picker); QE3-D3 (the stale "no `clipnudged=`
  field" comment reason corrected); QE3-D2 (the senior developer's sandbox
  recipe — a private session bus without service activation — is unsafe
  alone: rfd 0.15.4 falls back to `zenity`, installed on the seat; the
  recipe needs inert `zenity`, `xdg-open` and `kdialog` shims — recorded
  as the directive candidate's correction, the Manager's to carry, not
  code). Then re-review, QE round 4, merge on PASS.
- D8 (2026-10-05, Manager, at the merge): the extra round PASSED. The
  developer, checking every Enter and Space in both ring tests against
  the rule, found two more exposures beyond QE's A4/A5 — a report Space
  that a `slot-ok` regression would send to Open destination (A6), and a
  one-token forward home start that would send 2b's Enter to Choose…
  (A7) — and closed all four the same way: dry launches 3a and 2a replay
  launch 3's and launch 2's scripts with the presses swapped out, 2b
  replays launch 2's walk, so every press follows a walk an earlier
  launch drove and asserted; 34 mutants red at a landing assertion inside
  the sandbox, 0 picker, 0 shim calls, every run exiting by itself; the
  copy test 59.5 s (was 46), the export test 47.9 s (was 40). Re-review
  APPROVED (a nit: the suite-size sentence's figures — 122 driven tests at
  the head, three independent measurements from the tree's own target:
  QE 385.8 + 327.6 + 326.3 = 1039.7 s, the developer 1041.6 s, the senior
  developer 1039.1 s, in debug on the idle seat; halves would run ~520 s;
  the figures live here, not in test-harness.md's Behaviour — QE round 4
  D3 and CLAUDE.md's spec shape);
  QE round 4 PASS with two minors sent to issue #106: QE4-D1 — under a
  one-identifier regression of the export ring's refocus the PRE-EXISTING
  export clash test can reach the native picker through `Ctrl+O` on the
  grid (a harness-level hazard; QE's P1, a harness switch that makes
  native dialogs unreachable from any driven run, is the follow-up) — and
  QE4-D2 — two export ring rules without a red mutant (the first Shift+Tab
  from the plan's home; a scope gain without a state change). AC5 and AC6
  ticked at the merge (M10: the existing copy and export tests unchanged
  and green on both runners at every head; the spec and docs
  review-verified). CI green on both runners at 5923df7 (ubuntu 23m09s,
  windows 49m42s); merged after this commit's own run. Unit 011 spent
  four QE rounds; three of them on how a hypothetical regression of the
  tests' own subject would fail — a cost the Manager records for the
  user without a rule change (the user, 2026-10-03: the rule questions
  were only questions).

- D9 (2026-10-05, the user, at brief 012's closing report): the Space
  question D6 relayed is decided — option (a), accept as shipped. Space on
  a focused Close or Cancel closes on the key-press, as the toolkit's
  buttons do; a quick second or held Space that then picks the frame
  behind the dialog is a visible, undoable mark and stays. Dialog buttons
  keep acting on press, the grid takes no post-close guard window, and
  issue #106's section 4 carries no follow-up (recorded there the same
  day). Sections 1–3 and 5 of #106 stand.
