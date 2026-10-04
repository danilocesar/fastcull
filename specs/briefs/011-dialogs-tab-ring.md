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
