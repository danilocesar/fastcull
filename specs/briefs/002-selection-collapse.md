# Brief 002 — the file-manager selection rule: plain navigation collapses the selection, a fresh span replaces it

Date: 2026-09-06. Branch `selection-collapse` (worktree
`.qe-scratch/unit-002-selection/wt` while unit 004 holds the main checkout).
Scratch: `.qe-scratch/unit-002-selection/` (research, mechanism, synthesis,
persona report, screenshots). A feature-level change to what keys mean:
the persona gate ran, its IN-MY-WAY verdict was discussed, the user decided.

## Context

The user's report (2026-09-06): "everytime I select a set of frames to
export as a video, I get that video exported. then, when I select a
different set of frames, it get exported the old and the new frames
together. so, looks like the selection isn't cleaning between exports. what
I want you to do is: If I press ESC, a multi selection is gone. regardless
of where it is. and a multiple selection should also be gone if I press an
arrow right or left, shouldn't it? what PhotoMechanics do in those cases? i
have the impression that once the selected frame move away from a multiple
selection, the multiple selection should disappear. but I'm not an UI
specialist so you should research and discuss with an UI specialist before
agreeing to this task".

What FastCull does today, measured (`code/MECHANISM.md`): the cursor and
the selection are orthogonal channels — plain arrows move the cursor and
keep the selection (`Selection::reset_anchor` folds the live Shift-span into
`base`); a fresh Shift-span after plain navigation is UNIONED with that base
(`extend_to`), so 4 exported → Esc → Right, Right → Shift+Right gives 6
selected and the second video holds both sets. Esc already clears the
selection from anywhere except inside a modal dialog, where it closes the
dialog only (every toolkit and HIG: Esc in a dialog is Cancel). A plain
click clears the selection. The rationale recorded for the orthogonal model
(ui-grid.md ~1149-1152, docs/culling.md ~304-305): caption a run through
the IPTC panel, then walk it with arrows marking picks without losing the
selection. The union rule has no peer in any surveyed product, and the
spec's own sentence (ui-grid.md ~1203, "a new span replaces the previous
one") reads the way the user expected: it is the defect.

The research (`SYNTHESIS.md`, five reports): file managers and toolkits
(Explorer, Finder, GNOME/GTK, Qt, Bridge, digiKam, Thunderbird, Excel)
collapse a multi-selection on a plain arrow and offer Ctrl+arrow to move the
focus without touching it, Shift+arrow to extend, Space/Ctrl+Space to
toggle; the photo tools (Lightroom, Capture One, Photo Mechanic's Preview)
keep the selection and walk inside it; Photo Mechanic's contact sheet is
undocumented for the multi case; no surveyed tool clears a selection after
an export.

The persona (`PERSONA.md`): B (a fresh span replaces) MUST-HAVE; A3 (docs
say a dialog takes the Esc first) MUST-HAVE; A1 (the "· N selected" count in
the selection's blue) USEFUL; D (drop only when a key lands outside the
selection) its recommended arrow rule; **C (file-manager style) IN-MY-WAY**
— its reasons: a stray arrow after a 40-frame burst chord loses the
selection silently with no undo; Ctrl+arrow is undiscoverable; the Y/N
advance forces a choice with no good answer; the `]`×7 route to two
non-adjacent bursts needs a modifier. E-b (a finished export's report
consumes the selection) USEFUL only if arrows stayed as they were.

**The user's answers (2026-09-06), verbatim:**
1. "when I move the arrow or press ] to a new burst after an export, I
   expected the older frames to be not selected."
2. "no. i usually already know what's the because i used the fast moving
   forward key Press to "watch the frames from the burst"."
3. "yes. that's the expected behavior."
4. "file manager style."
5. "I don't think auto deselecyting is intuitive."
6. "I don't have photo mechanics"

So: C over the persona's IN-MY-WAY (the user's decision, recorded); B
confirmed; E-b rejected; the peek workflow does not exist for this user.

## Goals

- G1. Plain navigation collapses the selection: after any unmodified cursor
  move the selection is empty and the cursor is the batch, so "export,
  `]`, export" and "caption, `]`, Ctrl+Shift+B, caption" act on the new
  burst only.
- G2. A fresh Shift-span replaces the whole selection; a span that continues
  the live anchor still shrinks and flips; Ctrl+click and Ctrl+Shift+B stay
  additive.
- G3. The file-manager companions exist, so the keyboard can still build
  and walk a selection: Ctrl+arrows (and Ctrl+`[`/`]`) move the cursor
  without touching the selection; Ctrl+Space toggles the frame under the
  cursor.
- G4. The selection is visible where it lives: the status count in the
  selection's blue; the docs say what Esc does in a dialog.

## Non-goals

- Esc inside any dialog keeps closing the dialog only; no dialog and no
  export consumes the selection (the user's answer 5).
- G from the loupe keeps the selection; G at a grid zoom, Esc and a plain
  click keep clearing it (unchanged).
- No fence (arrows confined to the selection), no loupe chip (A2), no
  deselect chord (A4), no re-select key.
- No change to what the IPTC batch, the video export or Copy Picks take.

## Requirements

- R1. Collapse on plain navigation. Every unmodified cursor move — Left,
  Right, Up, Down, PgUp, PgDn, Home, End, `[`, `]` — and the Y/N/U mark
  auto-advance clear the selection (empty selection: the cursor is the
  batch, the count is silent, no wash), in the grid and in the loupe, at
  every zoom. The clear happens whether or not the move actually changes
  the cursor (a Right at the last frame still clears: the intent is the
  same). Rationale for the advance (Manager, best practice, 2026-09-06):
  it is a cursor move; Lightroom's auto-advance collapses to the next
  photo; a special case would make Y and Right disagree about the
  selection.
- R2. A fresh Shift-span replaces the selection. `Shift+arrows` and
  `Shift+[`/`]` starting after a reset anchor replace the whole selection,
  Ctrl-added frames included (answer 3); a span continuing the live anchor
  replaces only the live span (today's shrink/flip). `Ctrl+click`,
  `Ctrl+Shift+B` and `Ctrl+A` unchanged. Under R1 the reset-anchor case
  arises after Ctrl-navigation and after a click; the rule is stated for
  all of them.
- R3. Ctrl-navigation keeps the selection. Ctrl+Left/Right/Up/Down,
  Ctrl+PgUp/PgDn/Home/End and Ctrl+`[`/`]` move the cursor exactly as the
  plain key would, reset the anchor, and leave the selection alone. Any
  existing binding of these chords is resolved in the spec change (the
  senior developer checks the key table and the Slint key scopes); a
  conflict goes to the Manager.
- R4. Ctrl+Space toggles the cursor frame's membership (additive, like
  Ctrl+click), arms the anchor on the cursor, cursor unmoved. Resolved
  against existing bindings as in R3.
- R5. The status fragment "· N selected" is drawn in the selection accent
  (`selection-wash` hue at full opacity, or the accent the plan names), in
  the grid and in the loupe; an empty selection stays silent.
- R6. Docs: `docs/culling.md` "Working on several photos at once" and the
  key table say the new rules (arrows collapse, Ctrl+arrows walk, Ctrl+Space
  toggles, Shift starts a fresh span, Esc in a dialog closes the dialog and
  a second Esc clears); `docs/export-video.md` where it describes selecting
  frames. The shortcuts card (`?`/F1) gains the Ctrl+arrow and Ctrl+Space
  rows and its Esc row stays true.
- R7. Spec: `ui-grid.md` — the orthogonal-channels paragraph (~1149-1152)
  rewritten, the deselect gestures (~856-862), the key table rows for
  arrows, Shift+arrows, Shift+`[`/`]`, new rows for Ctrl+arrows and
  Ctrl+Space, the Esc/G rows re-read, the pointer contract row for click;
  `burst-grouping.md` ~134-153 (Ctrl+Shift+B's `]`×7 rationale rewritten
  with Ctrl+`]`); `video-export.md` where it relies on a surviving
  selection (~54-70, the "select frames or stand in a burst" fallback now
  the normal rhythm); `iptc-templates.md` if it names the walk-and-mark
  workflow. Every changed sentence dated and tagged; the persona's
  IN-MY-WAY and the user's decision recorded where the orthogonal model was
  recorded.
- R8. Tests: driven tests for R1 (plain arrow, `]`, PgDn and a Y-advance
  each empty a live selection, in the grid and in the loupe, count read at
  the shutter), R2 (span, Ctrl+Right ×2, Shift+Right → exactly the new
  span; the export plan line names only the new frames), R3 (Ctrl+Right
  keeps the count), R4 (Ctrl+Space adds and removes), R5 (the count's
  pixels are the accent, by the wash-pair method), plus the core unit tests
  in `selection.rs` rewritten for the new rules (`toggle_and_anchor_reset`
  asserts the union today: it must assert the replacement). Old-red first
  for R2 against the pre-fix `selection.rs`. Existing tests that encode
  today's rules are listed by the senior developer's plan and rewritten,
  never gated.
- R9. Business logic in `fastcull-core` (`Selection` gains the collapse and
  toggle operations and the replacing span; `nav.rs` calls them); the app
  crate stays a bridge (hard rule 5). Commits in the project's voice, spec
  and docs moving with the code, both trailers.

## Acceptance criteria

- AC1. The user's scenario: 4 selected → export → Esc → Right, Right →
  Shift+Right → **2 selected**, and the plan dialog names only those two;
  4 selected → export → Esc → `]` → Ctrl+Shift+E → the burst under the
  cursor, no clash question for the old file.
- AC2. Caption a burst (Ctrl+Shift+B, IPTC commit) → `]` → Ctrl+Shift+B →
  commit lands on the second burst only.
- AC3. Ctrl+Shift+B on burst 40, Ctrl+`]` ×7, Ctrl+Shift+B on 47 → both
  bursts selected; plain `]` anywhere in that sequence → empty.
- AC4. Ctrl+Space on three separate frames → 3 selected; Ctrl+Space again on
  one → 2.
- AC5. A Y with auto-advance on a frame inside a live selection → the
  selection is empty afterwards and the mark landed on that frame only.
- AC6. Esc in the export dialog closes the dialog and the selection is
  intact; a second Esc clears it (unchanged, now documented).
- AC7. "· N selected" renders in the accent colour in the grid and in the
  loupe; absent when empty.
- AC8. The full suite green on both CI runners; the shortcuts-map test
  covers the new rows; RAW checksums unchanged.

## Applicable directives

- CLAUDE.md hard rules 1-6 (rule 5: `Selection` semantics in core); M1, M2
  (the two conventions decided above), M5, M7, M8 (the user answered the
  persona's questions), M9 (cleanup ran at the unit's start).
- Specs: `ui-grid.md` pointer contract (~519-528), deselect gestures
  (~856-867), selection wash and count (~1134-1180), key table
  (~1191-1213), the shortcuts card section; `burst-grouping.md` (~120-160);
  `video-export.md` (~52-72, 250-264); `iptc-templates.md` (~65-72);
  `docs/culling.md`, `docs/export-video.md`, `docs/metadata.md`.
- Evidence: `.qe-scratch/unit-002-selection/` — `SYNTHESIS.md` (options C,
  B, A1, A3 with their touch lists and the tests at risk), `code/MECHANISM.md`
  (§7 tests that assert selection behaviour), `PERSONA.md`, the five
  research reports, the screenshots.

## Persona verdicts

B MUST-HAVE, A3 MUST-HAVE, A1 USEFUL, D USEFUL (recommended), **C
IN-MY-WAY**, E-b USEFUL-if-arrows-stay, A2/A4/B′/E-a/F rejected. Discussed
with the user; the user chose C (answer 4) and rejected E-b (answer 5).

## Open questions

- OQ1 (senior developer, spec change): existing bindings of Ctrl+arrows,
  Ctrl+`[`/`]`, Ctrl+PgUp/PgDn/Home/End and Ctrl+Space in `main.slint` and
  the key table; any conflict comes to the Manager.
- OQ2 (plan): whether Ctrl+A's "select all" should also arm the anchor;
  best-practice default: no change.
- None for the user.

## Decisions log

- 2026-09-06, the user: answers 1-6 above; C over the persona's IN-MY-WAY;
  B confirmed; E-b rejected.
- 2026-09-06, Manager (M2, best practice): the mark auto-advance collapses
  like any cursor move; Ctrl+arrows and Ctrl+Space ship with C as its
  standard companions; collapse means an empty selection (the cursor is the
  batch), matching the plain click; the status count in the accent colour
  and the docs correction ship in the same unit.
- 2026-09-06, senior developer (duty 1): OQ1 — no existing binding of the
  Ctrl chords or Ctrl+Space (the main scope's Ctrl block rejects everything
  but O/Q/A/E/Shift+E/Shift+B; the dialog scopes' PgUp/PgDn/Home/End arms
  are siblings; the winit backend delivers Ctrl+Space as a space with the
  control modifier); OQ2 — Ctrl+A arms no anchor, a Shift+arrow after it
  starts fresh (Explorer/GTK). `iptc-templates.md` needs no change. The
  count is drawn in `selection-wash`'s hue at full opacity; the card gains
  two SELECT rows (a measured limit: three would clamp at 1000x700 on Noto
  Sans). Plan: three developer commits — chords and card, then the core rule
  with old-red tests, then the count's colour.
- 2026-09-06, Manager rulings: Q1 the card rows and the parity pairings
  land in the developer's commit 1 (the spec commit carries specs and docs
  only; the parity test is red for exactly that commit, which CI never runs
  alone); Q2 `U` leaves the selection alone unless its mark removed the
  frame and the cursor moved ("collapse = a cursor move", one rule); Q3 the
  anchor resets after Ctrl-navigation.
