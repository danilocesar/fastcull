# Brief 014 — The IPTC panel forgets the old folder at a swap

Dated 2026-10-16. Work branch `iptc-swap-stale-chips`, cut from
`origin/main` at `aacf45c` (M12). Classified: bug fix, user-visible
(issue #114).

## Context

The user opened issue #114 on 2026-10-16, "tags stays after changing to a
new folder":

> After culling some shots, adding some tags and exporting the shots, I
> then open a new folder with new pictures.
>
> When I'm seeing the IPTC pannel, it keeps showing the last tags I added,
> from the previous folder.
>
> I says tagname X/Y (where x and y are numbers from the tags in the last
> folder). That information is invalid in this new folder and should be
> zeroed.

Reproduced by the Manager on `main` at `aacf45c` with the drive harness
(debug build, `FASTCULL_TRACE=1 FASTCULL_NO_CACHE=1 FASTCULL_NO_CONFIG=1`,
folder A holding two symlinks to `A1_full_compressed.ARW`, folder B one):

```
2400:wait:load settled gen 0;2500:select-all;2600:key:k;3000:key:w;
3100:key:i;3200:key:p;3400:key:return;3800:dump.tagged;4000:open:<B>;
5200:wait:load settled gen 1;5500:dump.swapped;6000:dump.end
```

The `tagged` dump carries `revert="Revert: keywords on 2 image(s)"`; the
`swapped` dump carries `status="three.ARW (1/1) …"` and `revert=""`; the
screenshot taken after the script shows B's one image, the panel header
`three.ARW`, every field row empty — and the keyword row still showing
the chip `wip 2/2`, the old folder's keyword with the old folder's
coverage. A second run that closes the panel (`iptc`) before the `open:`
and reopens it after B settles shows the same stale chip: the Slint `if
root.iptc-visible` rebuilds the panel from the models, and the model is
what is stale.

The mechanism (`crates/fastcull-app/src/iptc_bridge.rs`,
`refresh_iptc_panel`; `crates/fastcull-app/src/state.rs`, `PanelCache`
and `IptcPanelState::begin_session`): the panel's three Slint models —
field rows, keyword chips, template names — are replaced only when the
freshly computed contents differ from a cached copy of what was last set,
a rule that exists because an unconditional rebuild tore the field
editors down on every 33 ms engine tick (the #63/#64 family). A session
swap resets that cache to its default — three empty vectors — and leaves
the window's models untouched. The new folder's chip list is empty until
one of its sidecars lands, so empty compares equal to empty, the chip
model is never replaced, and the old chips stay on screen. The field rows
are rebuilt (eleven rows never equal an empty cache), which is why only
the chips go stale; the template list carries the same latent aliasing,
visible only when `templates.toml` has no templates left at the swap. The
sibling change-detection caches in the presenter (`last_pan_write`,
`last_badge`, `last_view_generation`, …) default to `None` or to a
counter and cannot alias a real result; the panel cache is the one that
can.

## Goals

- After a folder swap, everything the IPTC panel shows describes the new
  session and nothing else — with the panel open across the swap and with
  it closed before and reopened after.
- A driven test pins it, red on the pre-fix build and green after, and the
  harness can read the chip row so the proof is the model, not a picture.

## Non-goals

- Carrying keywords, fields or templates from one folder to the next in
  any form ("recent keywords", a sticky template); any new control or key.
- Any change to the panel's commit, discard or focus rules
  (iptc-templates.md "The panel's field exits", ui-grid.md "Focus
  continuity"), to the change-only rebuild rule within a session, or to
  the in-place row update recorded as a follow-up in ui-grid.md.
- Issue #68 (window deactivation commits a half-typed field).

## Requirements

- R1. **Every panel model follows the session.** After a session swap
  (`session::open_folder_at`: File > Open Folder, `Ctrl+O`, the harness's
  `open:`), the field rows, the keyword chips and the template names the
  window holds describe the new session: the chip row is empty until a
  sidecar of the new batch lands and then shows the new batch's union with
  the new batch's coverage; the template list is the `templates.toml` the
  swap re-read; the batch label, the warning strip, the revert label and
  the revert button are the new session's (these four are set on every
  refresh the panel runs while it is visible, and a reopen's synchronous
  refresh sets them before the panel's first frame — corrected
  2026-10-16, senior-developer spec review: `refresh_iptc_panel` returns
  before them while the panel is closed, so a dump taken with the panel
  closed still reads the window's old `revert=` until the reopen).
- R2. **Both paths.** R1 holds with the panel open across the swap and
  with the panel closed across the swap and opened afterwards: a model
  the panel's `if` re-instantiates from is as visible as one on screen.
- R3. **The change-only rebuild stands.** Within a session the three
  models are still replaced only when their contents change — the rows
  model never on a tick that changes nothing (the #63/#64 rule), and the
  chip and template models likewise. The swap is the one event that
  forces a replacement regardless; the fix does not reintroduce
  unconditional rebuilds, and the counts of `iptc rows rebuilt (gen N)`
  marks the existing focus tests rely on do not change.
- R4. **The harness reads the chip row.** The `QEDUMP` line carries the
  keyword chips as the window holds them — text and coverage per chip, in
  panel order — appended per test-harness.md ("new fields are APPENDED"),
  so a driven test asserts the model the user sees rather than a
  screenshot. The senior developer names the field and its encoding.
- R5. **The specs and docs say so.** iptc-templates.md carries the rule
  in Behaviour and the criterion in its ledger (a rule lives in exactly
  one spec; ui-grid.md's swap sentences point at it in one sentence if
  they need to at all); test-harness.md's dump section lists the new
  field; docs/metadata.md's "The IPTC panel" says in one clause that
  opening another folder empties the panel of the old folder's data (page
  map: metadata ↔ iptc-templates).

## Acceptance criteria

- AC1. **Open across the swap.** The reproduction above, asserted on the
  dump: after `load settled gen 1` the chip row is empty and `revert=""`.
  Red on the pre-fix behaviour with the harness's `keywords=` field in
  place — the harness commit lands ahead of the fix commit (D6) — where
  the dump reads `keywords=[("wip", "2/2")]`; on `aacf45c` itself the
  field does not exist and the record is the reproduction above
  (corrected 2026-10-16). Green after the fix.
- AC2. **Closed across the swap.** The same with `iptc` before the `open:`
  and `iptc` after `load settled gen 1`: the chip row is empty.
- AC3. **The new folder fills the row.** B's image carries a prepared
  sidecar with the keyword `bee`: after B settles the chip row is exactly
  `bee` with no coverage (a single image), never `wip` — the panel is
  emptied of the old folder, not emptied for good. Green on the pre-fix
  tree too (the row corrected itself the moment a keyword entered the new
  batch — the persona's observation, confirmed in the code by the senior
  developer): it guards the over-fix — a swap that empties the models and
  then bypasses the change-only comparison — not the defect (corrected
  2026-10-16).
- AC4. **Nothing else rebuilds more.** The focus tests that read or
  order the `iptc rows rebuilt` marks keep them, count for count
  (`session_swap_mid_field_edit_discards_and_keeps_the_keyboard`,
  `a_cursor_move_rebuild_keeps_the_keyboard_in_the_field`,
  `a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard`), and
  the within-session chip path keeps its promise
  (`session_swap_mid_keyword_edit_never_writes_into_the_new_session`,
  `keyword_enter_commit_still_writes_and_returns_focus`); a mutant that
  drops the swap's forced replacement turns AC1 and AC2 red — AC3 stays
  green by design (this line said "AC1–AC3" and listed the Enter-commit
  test among the mark readers until 2026-10-16, the senior developer's
  spec review).
- AC5. **The specs and docs say so** (R5) — ticked 2026-10-16 at the
  record's close (D15), PR #115 merged on both runners green right after.

## Applicable directives

Hard rules 1 (sidecars only — the test tags throwaway symlinked copies,
as the existing swap tests do), 4 (every step a proper commit), 5 (the
fix is model bridging in the app crate — which models the window holds
and when they are replaced — not business logic; no core rule moves), 6
(no perf budget touched: nothing runs per tick that did not). M1 (spec
first), M7 (never name the user), M9 (clean before the unit: 7.6 GiB
freed from the workspace crates, no leftover worktree or `target-qe-*`),
M11 (nothing camera-specific), M12 (branch from `origin/main`). The
gate's rule on test changes; old-red-first for the bug fix; a mutant for
the new guard. test-harness.md "Rules for script authors" (the panel
opens behind `wait:load settled gen 0`; a mark emitted more than once
carries what differs, `gen N`).

## Persona verdicts (2026-10-16, `almost-human-user`, as the UI consult)

The persona reproduced the defect itself and ran one variant the Manager
had not: folder B holding one pre-tagged image, where the row corrects
itself the moment a real keyword enters B's batch (`osprey 1/3`) — so the
ghost lives exactly as long as the batch has zero keywords, which on a
fresh card is the whole evening. The cost it named is not the chip but
the doubt: "is anything I type tonight actually being written?", and a
detour into a text editor to check a sidecar.

The rule — after a swap the panel shows the new session and nothing
else: label, rows, chips with their counts, template list, warning
strip, revert button: **MUST-HAVE** ("a chip with a count is a claim
about the files in front of me; `wip 2/2` under a header that says `3
images selected`, in a folder with no sidecar at all, is a count that
cannot be true, and the next thing I doubt is whether my keywords are
being written at all"). The same rule with the panel closed across the
swap and reopened: **MUST-HAVE** ("closing and reopening the panel is
the first thing I'd do to 'refresh' it; if the ghost survives that, I
conclude the data is really there"). The chip row empty for the moment
between the swap and the batch's sidecars landing: **fine, SHRUG-level
visible** ("the field rows have done exactly this since the panel
existed and I have never once noticed; the chips landed 390 ms behind
the header on my seat, inside the time the thumbnails were still
arriving"). The template list rebuilt alongside: **SHRUG** (the same
names come back from the same file; the dropdown's selection survives a
model replacement — Slint clamps the index and re-derives the value on
`changed model`). No new control, no key, no carry-over of keywords into
the next folder: **MUST-HAVE as a non-goal** ("the template is the
carry-over and it already exists"). The dock staying open across the
swap: keep. Gaps for the user: none. Nothing the fix would take away: the
ghost is not a "what did I type last" aid (it is the previous batch's
union with a wrong count, and the keyword field's own text is already
wiped at the swap) and its × is a dead control in the new folder (the
handler resolves the chip against the new batch, which is empty) —
annoying, never dangerous; the bug cannot touch a RAW or a sidecar.

Wording the persona asked for: not "the chip row is empty immediately
after the swap" (true only with the panel open across it; opened after
the load settled there is no empty moment at all) but "at every moment
the chip row shows the new session's keywords and nothing else", the
empty row a consequence, not a promise. Everything else on the panel and
around it already resets or is deliberately kept (header, rows, warning
strip, revert slot, the keyword field's text; filter, selection, cursor,
dialogs and badges per ui-grid.md, fileops.md, video-export.md).

Follow-up candidate, explicitly not this fix: keyword completion in the
add field from keywords already seen in this session's sidecars (what
digiKam and Photo Mechanic offer on the second folder of the same event).

## Open questions

None — the persona raised no question for the user, and the Manager has
none: the correct behaviour is the panel's existing rule applied at the
one moment it was skipped.

## Decisions log

- D1 (2026-10-16, Manager): classified as a bug fix with a spec-defined
  correct behaviour — the panel shows the current batch of the current
  session — so no UX choice is open; the persona is consulted as the UI
  consult, as briefs 005 and 009 did for user-visible fixes.
- D2 (2026-10-16, Manager, M3): the release after the merge is v0.15.1 —
  a patch, one bug fix since v0.15.0 — cut the way v0.15.0 was: a
  `release-0.15.1` branch carrying one release commit (version bump,
  `Cargo.lock`, the CHANGELOG entry, the brief's closing line), merged by
  PR on green, the tag on the merge commit (`v0.15.0` sits on `aacf45c`,
  the merge of PR #113).
- D3 (2026-10-16, Manager, on the persona's wording): the spec sentence
  is stated as an invariant — at every moment after a swap the panel's
  models show the new session's data and nothing else — with the empty
  chip row before the first sidecar lands as its consequence, never as a
  promise of an empty moment.
- D4 (2026-10-16, Manager, M2, on the persona's walk): the template
  dropdown's SELECTION survives the swap, like the dock's visibility — it
  is the user's stationery, not the folder's data — and Slint's ComboBox
  keeps it across the model replacement on its own (clamped index, value
  re-derived on `changed model`); the fix neither forces nor forbids it,
  and the spec records it as kept. Narrowed by QE's measurement (D15,
  2026-10-16): it survives the swap WHILE THE PANEL STAYS OPEN; closing
  the panel resets the dropdown to the first template, swap or no swap,
  before and after the fix — the ComboBox lives under the panel's `if`.
- D5 (2026-10-16, Manager): the persona's follow-up candidate (keyword
  completion from this session's sidecars) is recorded here and not
  opened as an issue; the unit does not grow toward it.
- D6 (2026-10-16, Manager, on the senior developer's Q2): two
  implementation commits — the harness's `keywords=` dump field first, on
  its own, so the old-red has a real tree (the repo's "pre-fix build plus
  the drive-harness commit" shape); the fix with its tests second. QE
  reproduces the old behaviour on the harness commit.
- D7 (2026-10-16, Manager, on Q1): ui-grid.md's Slint-facts sentence on
  the ComboBox is kept — the fact is verified in Slint's source and that
  section exists for exactly such facts.
- D8 (2026-10-16, Manager, on Q4): AC5 lives in this brief's ledger only;
  the module ledger carries the four failable criteria, since a criterion
  whose only test is the merge is decoration there.
- D9 (2026-10-16, Manager, on Q3): docs/metadata.md names 0.15.1 (D2);
  the release commit corrects it if the number moves.
- D10 (2026-10-16, Manager, on the senior developer's plan Q1): commit 1
  (the harness field) is verified by fmt, clippy, the build, the unit
  tests and the eight named driven tests — the field is appended to the
  end of a line no test reads to the end, grep-verified by the senior
  developer — and the full driven suite runs once, before commit 2; the
  developer's rule is "the tests the plan names green before every
  commit", and the plan names them.
- D11 (2026-10-16, Manager, on plan Q2): test-harness.md's `keywords=`
  sentence gains the half-sentence that with the panel closed across a
  swap the field lags until the reopen replaces the model before the
  panel's first frame — a clarification landed in commit 2, so no later
  test author tightens AC2's `closed` dump into a false red.
- D12 (2026-10-16, Manager, on plan Q3): the `key:+` / `dump.end` probe
  stays in the three new tests — the forced replacements sit in the
  swap's reclaim pass, and the probe costs nothing.
- D13 (2026-10-16, developer, the fix commit's record): the old-red, the
  mutants and the suite, measured on the development seat (debug unless
  named). Old-red, on the harness commit `002c8c5` with the three tests
  added and the fix not yet applied — `cargo test -p fastcull-app --test
  screenshot --locked -- --test-threads=1 --exact` and the three names:
  AC1 red at its `swapped` read and AC2 at its `reopened` read, both
  `left: "[(\"wip\", \"2/2\")]"`, `right: "[]"`; AC3 green, as designed.
  On the fix, the three green three runs in three. Mutants, each run once
  on the fix and restored: A, `let swapped = false;` (the defect back) —
  AC1 and AC2 red at the same two reads, AC3 green; B, the chip block's
  condition reduced to `if swapped {` — all three red, and earlier than
  the plan predicted, at the shared `tagged` read (`left: "[]"`, `right:
  "[(\"wip\", \"2/2\")]"`): the keyword commit in folder A is a non-swap
  refresh too, so dropping the comparison empties the row before any
  swap; B2, the same comparison dropped after a swap only (`if swapped ||
  (session_gen == 0 && …)`) — AC1 and AC2 green, AC3 red at its `bee`
  read (`left: "[]"`), the reading the plan expected of B and the one
  that shows AC3 guarding the over-fix on its own; C, the `seen_gen`
  stamp deleted (every refresh a "swap") — all three red at the
  rows-rebuild count, `iptc rows rebuilt (gen 0)` emitted twice (the
  panel's open and the Enter), and
  `a_cursor_move_rebuild_keeps_the_keyboard_in_the_field` red, its
  `wait:row 0 (gen 4)` never satisfied (QE's re-run, D15: so is
  `a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard`, at
  :5379). The whole driven suite once,
  before the fix commit (D10): 126 tests in three `--exact` thirds of 38,
  46 and 42, 301 s + 420 s + 336 s, all green; the eight named tests
  green in release in 47 s.
- D14 (2026-10-16, Manager, on the senior developer's review gap G1): the
  template model's forced replacement at the swap — the `swapped ||` on
  the names block of `refresh_iptc_panel` — ships reviewed but without a
  test of its own, and is recorded here as the unit's one untested line,
  not as acceptance. The reviewer's reading, re-run by the reviewer: with
  `swapped ||` removed from that block all three swap tests stay green,
  because every driven run is `FASTCULL_NO_CONFIG` — the names are empty
  in both sessions and the window's model is Slint's default empty list,
  correct by accident as on the pre-fix tree — and the dump has no
  template field. The consequence of the line's absence: a `templates.toml`
  that had templates at the launch folder and none at the swap (deleted
  or emptied mid-session) leaves the dropdown offering names the file no
  longer has, with Apply a no-op — the aliasing this brief named, in its
  rarest form. The proof has a known shape and a known cost: a
  `templates=` dump field (one harness argument and its sentence in
  test-harness.md "The dump", a spec change) and a driven test modelled on
  `templates_and_ui_prefs_are_read_from_the_one_config_dir`
  (`FASTCULL_CONFIG_DIR` with a one-template file, `key:i`, the file
  deleted from `shoot_env_stderr_watching`'s `on_line` at the first
  `templates loaded from` line — the issue #50 pattern — then `open:` B
  and `templates=[]` after `load settled gen 1`; red with the line
  removed, green with it). There is no unit-level pin: the app's unit
  tests have no headless `MainWindow`. Ruling: this unit is the chips the
  user reported, and a harness field for a dropdown's rare stale names is
  the next template unit's first item, not this release's blocker; the
  line ships, the module ledger carries the open box with this reason,
  and the report names the deferral.
- D15 (2026-10-16, Manager, on QE round 1 — PASS): the old behaviour
  reproduced on `002c8c5` by the worktree overlay — the one-liner's
  `swapped` dump reading `keywords=[("wip", "2/2")]` under the header
  `three.ARW`, the three tests `2 passed; 2 failed` (AC1 at :4436, AC2 at
  :4537, both `left: "[(\"wip\", \"2/2\")]"`) — and `d1977cf` green:
  the driven suite in debug thirds 302 s + 420 s + 336 s and in release
  564 s + 158 s + 250 s (126 tests), the perf budgets 6 of 6, the new
  tests 3 of 3 under load, fmt and clippy clean, the RAWs' checksums
  unchanged. QE's mutants in the worktree: A as D13; A-chips (the
  chips line alone) — AC1 and AC2 red, so the tests pin that exact line;
  N (the names line) — all green, G1 confirmed; R (the rows line) — no
  observable effect, eleven rows never equal the reset cache, the line
  documents intent; C — the second casualty added to D13. The `keywords=`
  encoding read back through a keyword typed as `a] b"c\d`. D14's line
  shown working by hand: a `templates.toml` deleted after the panel read
  it, then a swap — the dropdown still offered `solo` with Apply enabled
  on `002c8c5` and on mutant N, empty and disabled on the fix. D4 by
  hand with the panel open: `beta` before and after the swap, clamped to
  `gamma` when the file shrank to one template. Probes on the fix:
  A → B → A refills the row from A's sidecar; a failed open keeps A's
  chips and revert; a zero-byte `.ARW` and a JPEG named `.ARW` give `[]`
  without a crash; a swap from a scrolled grid adds no refresh. Three
  minor findings, none a defect of the fix: (1) the dropdown's selection
  does not survive CLOSING the panel, swap or no swap, before and after
  the fix — the ComboBox lives under the panel's `if` — so D4's sentence
  and the docs line promised more than is true; narrowed in this commit
  (D4, iptc-templates.md, docs/metadata.md — the senior developer's
  wording); whether the choice should survive a close is a follow-up
  candidate for the persona, not this unit's; (2) D4's survival now rests
  on the ComboBox clamp at every swap with the panel open, verified by
  the canary's reading and QE's probe only — D14's open box carries it,
  and the `templates=` field pins it once it also carries the selected
  index; (3) outside the diff and older than the unit: iptc-templates.md
  says templates are read "on panel open and on Apply", but Apply has not
  re-read the file since M5 (`8e67cd6`) — the marks show reads at the
  panel's open and at the swap, none after an Apply, and the user guide
  describes what the code does. A promise found unimplemented is recorded
  and put to the user, never quietly deleted (M10, M8); the sentence
  stands until the user answers QE's question, relayed verbatim: "The
  templates spec says templates.toml is re-read when you open the panel
  *and when you click Apply*. Since the panel shipped, it has only been
  re-read when you open the panel or open a folder. So if you edit
  templates.toml while the panel is open and click Apply, you get the old
  version until you close and reopen the panel; the user guide already
  tells you to toggle the panel. Should Apply re-read the file, so a
  saved edit applies at once? Or should the spec be corrected to 'panel
  open and folder open', matching what you have today?" Directive
  candidates for the curation after the release: qe.md's "~460 s serial"
  for the release suite (QE: ~970 s on this seat, a 60-test half 564 s);
  a worktree has no `testdata/raws` (symlink before a driven run, unlink
  before `git worktree remove` — the plan's old-red command missed it);
  developer.md's "two halves of 87" (thirds — the senior developer's
  measurement agrees). QE's visual note — an emptied chip row moves the
  add-keywords field down, as any keyword-free folder does — is not a
  defect. No test change proposed for this unit; QE endorses D14's test
  shape, dump-read, never pixels.
- D16 (2026-10-16, Manager, the release): PR #115 merged into main at
  `9dedb4b` with both runners green on `b97315b` (CI run 38887211918);
  v0.15.1 cut from it per D2 — the version bump, Cargo.lock, the
  CHANGELOG entry and this line in one "Release 0.15.1" commit;
  THIRD-PARTY-LICENSES.md untouched, no dependency having moved since
  v0.15.0 (Cargo.lock's only change is the workspace crates' version).
  The three directive candidates of D15 are promoted in PR #116,
  independent of the release. D15's question for the user stays open.
