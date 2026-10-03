# Brief 009 — the Settings card holds still: one height for every tab, the footer pinned, the notice line reserved, the tabs in place

Date: 2026-10-03. Follows brief 008 (PR #96). Branch `settings-fixed-size`
from `origin/main` bef5b5e (M12). A user-visible change to a shipped
dialog, so the persona gate ran (verdicts below).

## Context

The user, 2026-10-03: "The settings screen is bumping depending on its
size. I don't like that. Find a way to get its size fixed. Discuss with a
Fable 5.1 UI specialist."

What ships (settings.md "The card", brief 008): the card is 560 px wide
and "its height following its content" — `height:
min(settings-card-layout.preferred-height, parent.height - 40px)` on the
ACTIVE tab's body, the card centred in the window. Measured by the
Manager and the persona (debug build, Noto Sans, `--synthetic 6`, the
`settings card laid out … size WxH` marks), 1440×900 and 1000×700 alike:

| Moment | Before | After |
|---|---|---|
| Open, with a notice showing | 560×238 at y 338 | 560×267 at y 324 one frame later — a twitch |
| General → UI | 267 | 266; the tab labels shift 1–3 px as the bold moves (78→76, 42→43) |
| UI → Performance | 266 at y 324 | **506 at y 204** — top edge up 120 px, bottom down 120 px; Close moves from y 540 to y 660 |
| A clamped `100 GB` committed on Loupe memory | 506 | 507 — the hint wraps inside its 32 px row |

Without a notice the three tabs are 237 / 237 / 477. The persona: "the
mouse that was on Close is now over the Read workers row"; the strip
jitter "you can't un-see on the second Ctrl+Tab"; the open twitch
happens only when a notice exists (a read or write error, or a driven
run) but "is the same defect".

## Goals

- G1. The card holds still: at each open it takes ONE height, the same on
  every tab, and a tab switch never moves an edge, a button or a label.
- G2. The footer — Reset, Close and the notice line above them — is
  pinned to the card's bottom on every tab; the slack above it is empty
  card.
- G3. The notice line is reserved: always one line tall, blank when
  there is nothing to say, so a notice never moves the card either.
- G4. The strip's tabs keep constant positions and widths across a
  switch.
- G5. Nothing else about the dialog changes.

## Non-goals

- No literal pixel height (candidate B: IN-MY-WAY — on another font it
  is dead space or a scrolling settings page, and somebody re-picks the
  number every time a row lands).
- No top-anchored content-driven card (candidate C: IN-MY-WAY — Close
  still moves 240 px per switch; "the user said fixed; this isn't").
- No sidebar, no scrollbar, no moving settings between tabs to even out
  heights; Reset's label keeps the active tab's name and its per-tab
  width (persona: SHRUG, "worth more than a constant width").
- No change to the rows, the notes, the keyboard ring, apply-on-commit,
  the width (560 px), the window clamp (`window − 40 px`) or the
  1000×700 fit test's shape (slack measured, never a height pinned).
- Below the supported minimum window (1000×700) the card clips as it
  does today; if the senior developer can make the body give before the
  footer cheaply (the shortcuts card's Flickable valve is the precedent)
  it may, but it is not a requirement of this unit.

## Applicable directives

- CLAUDE.md hard rule 5 (a layout rule lives in the `.slint`; nothing
  here is business logic, and nothing moves to core), rule 6 (no budget
  row is near this change).
- settings.md "The card" (the sentence that becomes false), "Keyboard"
  and "Apply on commit" (unchanged), AC3 and AC14 (the card fits whole at
  1000×700 — slack, never a pinned height); ui-grid.md "The
  keyboard-shortcuts card" (the house rules for a card: content-driven
  tall, fits whole at 1000×700, nothing font-dependent pinned — the
  content is now the tallest tab's) and "Slint facts this module depends
  on" (a `changed` tracker is installed after `init`; an element with a
  bound height but no `y` is centred).
- test-harness.md: the `settings card laid out …`, `settings tab <name>
  laid out …`, `settings <control> laid out …` marks (they fire on
  init and on a change of position or height); the `settings` drive
  token; `key:ctrl+tab`.
- Docs page map: settings ↔ settings (one phrase on `docs/settings.md`).
- M1 (spec first), M2 (the three confirmations decided below), M7, M12.
- Rules of the gate: a user-visible change with a measured defect, so
  the old-red is the measurement above (two `settings card laid out`
  marks per switch and a Close that moves); a mutant for every new guard;
  the senior developer's veto on every test change.

## Requirements

- R1. **One height per open, every tab.** At each open the card's height
  is: the title row, the strip, the rule, the TALLEST of the three
  bodies' preferred heights — hidden bodies count, wrapped texts count
  (the environment note on Read workers, a long cache path on the readout
  row, a two-line hint) — the reserved notice line (R3) and the footer,
  clamped to `window − 40 px` as today. The same height on every tab. A
  tab switch never changes it. While the dialog is open the height is a
  high-water mark: it never shrinks (a `Clearing…` row replacing a
  wrapped path; a hint that un-wraps after a second commit), and it grows
  only when a text that affects height changes — a write error arriving,
  the environment's note, a notice that wraps — never on a tab switch.
- R2. **The footer is pinned to the bottom.** The active body is laid out
  from the top under the rule; the footer — the notice line, then Reset
  and Close — sits at the card's bottom on every tab; the slack between
  them is empty card. Close and Reset keep one position across every
  switch (Reset's width follows its label, as today).
- R3. **The notice line is reserved**: always one line tall, blank when
  there is nothing to say; a notice appearing or clearing moves nothing.
- R4. **The strip holds still**: every tab's position and width are the
  same on every switch. The jitter's cause is the active label going
  bold (`font-weight: active ? 600 : 400`, the width measured from the
  label): the developer either measures every tab at its bold width or
  drops the weight change — the white label and the accent underline
  already mark the active tab (persona: "I don't care which; the labels
  must not move"). The plan says which.
- R5. **One layout per open.** Every text that affects the height is in
  place before the card's first frame, so an open lays the card out once:
  exactly ONE `settings card laid out … size` mark per open, none on a
  switch — which is what QE reads.
- R6. **Spec and docs, same commit.** settings.md "The card": "its height
  following its content" becomes "its height following its TALLEST tab —
  one height per open, every tab, the footer pinned to the bottom, the
  notice line reserved" with the rules of R1–R5 and the provenance
  `(brief 009, 2026-10-03)`; ui-grid.md's house rule for a card stays
  ("content-driven tall" — the content is the tallest tab's); `docs/
  settings.md` says in one phrase that the dialog is the same size on
  every tab; test-harness.md notes that a settings tab switch emits no
  card mark.
- R7. **Tests.** A driven test — `the_settings_card_holds_still_across_its_tabs`
  or the shape the plan names — opens the dialog and walks General → UI →
  Performance → General (`key:ctrl+tab`), and asserts: exactly one
  `settings card laid out` mark in the run (R5); the `settings close laid
  out` and `settings reset laid out` marks never change their y (R2); the
  `settings tab <name> laid out` marks never change x or width (R4); the
  card's height from the one mark equals the Performance tab's height
  measured in a run that opens ON Performance (R1 — the tallest tab is
  the height); under `FASTCULL_NO_CONFIG` (a notice at open) the one
  mark's height equals the no-notice run's (R3; the two runs differ only
  by `FASTCULL_CONFIG_DIR`); at 1000×700 the card still fits whole with
  slack (the existing fit test, re-pointed if its tallest-state premise
  changes). Old-red: the same script on bef5b5e emits a second card mark
  on the UI → Performance switch and Close's y changes (the table above).
  Mutants: the max replaced by the active body's height (a second card
  mark on the switch); the footer not pinned (Close's y changes); the
  notice line not reserved (two card marks at open under `NO_CONFIG`);
  the strip's width rule removed (a tab mark's x changes). Existing tests
  at risk: every Settings driven test that clicks by name (unaffected —
  marks follow the elements) and the fit test
  `the_settings_card_fits_1000x700_in_its_tallest_state` (its premise —
  "tallest state" — is every state now; the plan says whether it stays as
  is).

## Acceptance criteria (they also land in settings.md)

- AC1. One `settings card laid out` mark per open; none on any tab
  switch; the same height on every tab, equal to the tallest tab's.
- AC2. Close and Reset keep their position across every switch; the
  notice line is reserved and a notice moves nothing.
- AC3. The strip's tabs keep their positions and widths across every
  switch.
- AC4. The card still fits whole at 1000×700 with slack, on every tab.
- AC5. settings.md's card sentence and `docs/settings.md` say so.

## Persona verdicts (2026-10-03, `almost-human-user`, as the UI consult)

Candidate A — one height for all tabs sized to the tallest, footer
pinned, notice line reserved, fixed tab positions: **MUST-HAVE** ("what
every preferences dialog I own does — darktable's, digiKam's, Photo
Mechanic's, Lightroom's all size to their biggest page — and the reason
is exactly the one I hit: Close and Reset live at one spot for the life
of the dialog"). General as one checkbox over ~240 px of card: SHRUG
("with the footer at the bottom it reads as a page with room; if the
footer floated right under the row with dead card beneath it, that would
read as broken — so footer at the bottom is the whole point of
'fixed'"). The reserved notice line: USEFUL ("one blank line above the
buttons is invisible; a 29-px twitch at open is not"). Fixed tab
positions: USEFUL. 477/506 px at 1000×700: fine ("between the copy
dialog and the shortcuts card"). Candidate B (a hand-picked pixel
height): IN-MY-WAY. Candidate C (content-driven, top-anchored):
IN-MY-WAY. Also: below the minimum window the body should give before
Close if the clamp ever bites (not a gate); Reset's per-tab label width:
SHRUG, leave it; do not add a sidebar, a scrollbar or rebalance the
tabs — "a culling tool's settings dialog is done when it holds still".
Gaps for the user: none.

## Decisions log

- D1 (2026-10-03, Manager, M2, the persona's confirmation 1): "get its
  size fixed" means the card holds still on a given machine — sized to
  its tallest tab, so it differs only between seats with different fonts
  — not one literal pixel height everywhere (the persona advised against
  it: dead space or a scrolling settings page on a larger font; the
  card tests forbid pinning a font-dependent height).
- D2 (2026-10-03, Manager, M2, confirmation 2): all three measured
  movements are in scope — the 240 px jump to and from Performance, the
  strip's label jitter, the open twitch when a notice exists — and the
  test measures each.
- D3 (2026-10-03, Manager, M2, confirmation 3): the footer is pinned to
  the bottom and General's empty space is accepted until #15 and #24
  land their rows there; a footer floating under the rows was refused by
  the persona as the thing that would read as broken.
- D4 (2026-10-03, Manager): the unit is bounded to R1–R7; the body
  giving before the footer below 1000×700 is the senior developer's
  call, not a requirement; "Reset <tab> to defaults" keeps its per-tab
  width.
