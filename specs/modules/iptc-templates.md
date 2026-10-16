# Module spec: IPTC model & templates (`iptc.rs`)

## Purpose

The IPTC data model the side panel edits, and the saved templates
("stationery pads") with variable expansion, applied to one image or to a
multi-selection.

## Behaviour

### Data model

`IptcData`: the fields of the xmp-sidecars mapping table — title,
description, creator, rights, headline, city, country, credit, source, job
id, location — and `keywords: Vec<String>`. All fields optional. Keywords
are ordered and deduplicated case-preservingly: the first spelling wins,
the comparison is Unicode-casefolded. Input is sanitized at the commit
boundary (`iptc::sanitize_text`: NFC, control characters stripped, trimmed)
before the casefold dedup.

### Templates

- Named `IptcTemplate`s with the same fields; values may contain variables.
- Persisted as TOML in the user config dir (`directories` crate), one file
  `templates.toml`, written atomically (temp + fsync + rename). The
  directory is the one `settings::config_dir()` resolves for every config
  file, so `FASTCULL_NO_CONFIG` hides it and `FASTCULL_CONFIG_DIR` moves
  it exactly as for `settings.toml` (brief 008, 2026-10-01; until then
  every driven run read the user's real `templates.toml`). `templates.toml`
  is hand-edited in v1, and the app never writes it — `save_templates` has
  no caller — so the editor is its only writer and a re-read can clobber
  nothing (brief 015, 2026-10-16).
- **The three read moments** (user decisions 2026-10-16, brief 015 D1 and
  D2): `templates.toml` is read when the panel opens, at every folder
  open, and every time Apply is clicked — before anything is applied —
  through core's one config-dir resolver, each read marked `templates
  loaded from <path>` from the very path it read (test-harness.md); under
  `FASTCULL_NO_CONFIG` there is no path, no read and no mark. Never
  otherwise: no file watcher, no timer, no Reload button, no re-read when
  the dropdown opens or the window regains focus; a template added while
  the panel is open is in the dropdown after the next of the three. This
  bullet said "read on panel open and on Apply — no file watcher
  (2026-07-25)" while the app, since M5, read the file on panel open and
  folder open only — Apply applied the list as last read (corrected
  2026-10-16, brief 015: QE's finding 3 on brief 014, the user choosing to
  make the app keep the promise).
- Errors, the loader's (core, unchanged by brief 015): a corrupt entry
  (`[templates.x]` with wrong types) is surfaced per entry and the other
  templates still load; an unparseable file (broken syntax, duplicate
  keys) is a hard error — nothing is safe to load from a file the parser
  cannot segment; a missing file is a successful read of zero templates,
  not an error. Unknown keys inside an entry are ignored by serde, so a
  typo like `tittle` silently does nothing (known limitation).
- **One rule for a bad file, at every read moment** (brief 015 R4, Manager
  D5, 2026-10-16): a read that fails as a whole keeps the LAST GOOD list —
  the result of the last read in this process that succeeded, carried
  across a folder swap — on screen and in the dropdown, and the warning
  strip shows the parse error followed, on its own line, by `the
  templates shown are the last read` whenever that list is non-empty; at
  launch there is no last good list, so a broken file at launch is an
  empty list with the error alone, as before. A corrupt entry is not a
  failed read: it is skipped with its per-entry warning and the rest
  replace the list. A missing file is not a failed read either: it is a
  successful read of zero templates and replaces the list with nothing,
  no warning — a file deleted while the panel is open empties the
  dropdown at the next read moment, which disables Apply. The read's
  warnings — per-entry errors and CLEAR warnings — are replaced at every
  read; an apply-time expansion error is APPENDED to them, never put in
  their place (R6, D7).
- Variables, expanded at apply time per image:

| Variable | Value |
|---|---|
| `{date}` | capture date `YYYY-MM-DD` (EXIF DateTimeOriginal; file mtime fallback) |
| `{time}` | capture time `HHMMSS` |
| `{seq}` | 1-based position in the current apply batch, zero-padded to the batch width |
| `{seq:N}` | as `{seq}`, padded to N digits (N = 1..=32; out of range is its own error, distinct from unknown-variable) |
| `{filename}` | original stem (no extension) |
| `{camera}` | EXIF model string, whitespace-normalized |
| `{ext}` | original extension, uppercase |

- An unknown variable is an apply-time error naming the variable and the
  field; nothing is applied (all-or-nothing per batch). Literal braces are
  `{{` and `}}`.
- `{camera}` is the EXIF model from the session
  (`SessionState::camera_models`, filled from `MetadataReady` beside the
  capture-time sort key) — the model alone — not `FrameMeta::camera`, since burst grouping prefers
  the serial. It is empty for an image whose metadata has not landed yet,
  exactly as the capture-time sort is provisional during a load.

### Applying to a selection

- **Tri-state per field** (user decision 2026-07-25, after the Photo
  Mechanic research): an ABSENT field preserves the existing value; a
  NON-EMPTY field overwrites; an EMPTY string CLEARS the field on every
  selected image — PM's ticked-but-empty case. Clearing removes the XMP
  property, never writes an empty value. Whitespace-only counts as empty
  everywhere: it clears, and it warns.
- The empty-string encoding is the TOML wire format ONLY. In the panel,
  bare emptiness always preserves, and clearing is the explicit per-field
  control (⌫), which clears immediately across the batch, revert-covered —
  there is no pending-clear state. Because `field = ""` in a hand-edited
  file means clear, template load emits a warning naming template and field
  for every empty-string field it finds.
- Keywords apply additively (union), never as replacement.
- A manual panel edit on a multi-selection behaves the same way, field by
  field. A value-unchanged commit is a strict no-op: it arms no revert and
  touches no sidecar.
- `{seq}` follows the active sort: apply takes the batch slice in view
  order (`Selection::batch`) and the app builds each `ExpandContext` from
  the sort key (`ExpandContext::from_sort_key`) — a caller contract.
- **Apply re-reads first and applies by name** (brief 015 R1–R3; user
  decision D1, Manager D3 and D4, 2026-10-16): a click on Apply reads
  `templates.toml` — the third read moment — and applies the template
  whose NAME the dropdown showed at the click, found in the re-read list
  by exact, case-sensitive match on the TOML table key (`Osprey` and
  `osprey` are two templates), never by position: the dropdown is
  alphabetical by byte (`toml 0.8` without `preserve_order`, a
  `BTreeMap`), so a position drifts under every edit that inserts,
  renames or deletes a name. A name not in the re-read list — renamed or
  deleted in the editor, or the file gone — applies NOTHING: the revert
  slot is untouched, the strip says `template '<name>' is no longer in
  templates.toml — nothing was applied`, and the dropdown shows the
  re-read list on its first entry, Apply enabled unless that list is
  empty — never the first template as a fallback, never a silent return
  (the persona's two MUST-HAVE conditions; until brief 015 the handler's
  `else { return }` was that silence, unreachable while nothing could
  change the list between the click and the lookup). A re-read that fails
  as a whole applies nothing either — what is on disk is not what would
  be applied — the last good list stays and the strip's last-read line
  ends `— nothing was applied`. Every click ends in a mark naming its
  outcome (test-harness.md, `template apply`).
- **Re-applying an edited template** overwrites the fields it still sets
  and leaves the others: a line removed from the template preserves what
  its previous version stamped (ABSENT preserves); to drop that value,
  clear the field in the panel or set it to `""` in the template (brief
  015 R8, the persona's docs sentence).
- **Revert**: one shared single-level slot, armed by every batch mutation
  from the panel — a template Apply, a manual commit to a multi-selection, a
  keyword-chip removal — labelled with what it will revert, and cleared by
  the next batch mutation or session close. There is no general undo stack
  in v1 (user decision after the persona review).
- Batch-apply performance target: picks-scale (hundreds), not whole-folder.

### The panel's field exits

Enter commits and returns the keyboard to the grid; click-away commits and
the keyboard stays where it was clicked; a covering surface — a Help modal,
the copy dialog — commits like click-away; DESTRUCTION — panel close,
session swap, or the field rows rebuilt under the editor as the folder's
metadata lands — DISCARDS the uncommitted text (user decision 2026-08-03:
no commit-on-destroy; a swap also generation-stamps edits, so the old
session's text can never land on the new session's images). The rebuild
case is deterministic since 2026-08-30 (issue #63): a rebuild generation
stamped on focus gain decides it, and the keyboard goes back to the SAME
field row, never to the grid, so the next caption character can never
become a cull command. Esc inside a field is not an abandon gesture — the
Slint LineEdit offers no Esc hook in v1 (recorded deviation). Focus
continuity in full: ui-grid.md, *Focus continuity*.

### The panel's models

The panel shows the batch of the session that is open — the selection in
view order, or the cursor (`Selection::batch`) — through three window
models: the field rows (the tri-state mapping above, one row per field),
the keyword chips and the template names. A chip is one keyword of the
batch's union, in first-seen order across the batch in view order, with
its coverage `n/total` after it on a multi-selection and nothing on a
single image (M5, 2026-07-25; written here by brief 014, 2026-10-16).
Within a session each model is replaced only when its content changes —
the rows never on a tick that changes nothing, because a replacement
destroys the editors (ui-grid.md, *Focus continuity*) and an unconditional
rebuild tore them down on every engine tick (gate finding at the panel
step, 2026-07-25; written here by brief 014).

After a session swap (`session::open_folder_at`: File › Open Folder…,
`Ctrl+O`, the harness's `open:`) everything the panel shows describes the
new session and nothing else, at every moment — with the panel open across
the swap and with it closed across the swap and opened afterwards: the
rows, the chips with their coverage, the template list, the batch label,
the warning strip and the revert slot. The chip row holds nothing until a
sidecar of the new batch lands and then holds that batch's union with that
batch's coverage — the empty row is a consequence of the batch having no
keywords yet, never a promise of an empty moment; the template list is the
`templates.toml` the swap re-read. The swap is the one event that replaces
the rows and the chips whatever their content, at the first refresh after it
— the swap's own refresh with the panel open, the refresh the reopen runs
before its first frame with it closed (issue #114, brief 014 D3,
2026-10-16). The names model follows the change-only rule at every moment,
the swap included: its cache is what the window holds, which no swap resets,
so the model is replaced exactly when the re-read names differ from the
window's — at a swap, at a reopen, at an Apply (brief 015 plan, D15,
2026-10-16; this sentence said "all three models", and brief 014 D14
recorded the names model's forced replacement as that unit's one untested
line — the line is deleted, not tested: with the cache kept across the swap
the comparison alone is the rule, and the aliasing 014 fixed for the chips,
a cache reset to empty against an empty new list, cannot touch a cache that
is never reset). **The
dropdown follows the name at every replacement of the names model** —
Apply's re-read and the folder swap alike: its selection is the entry
carrying the name selected before the replacement, at that name's new
position, and the first entry when the name is gone (brief 015 R5,
Manager D6, 2026-10-16). The rule rests on the bridge, not on the widget:
the bridge sets the dropdown's index with the replacement — through a
window property two-way bound to the dropdown's `current-index`, which is
also what the harness reads — and Slint's own deferred clamp leaves an
in-range index where it was put (ui-grid.md, *Slint facts*). This
rewrites brief 014 D4's sentence, "by index, clamped to the re-read list,
which Slint's ComboBox does on its own at a model replacement": true of
the widget, wrong as the rule — the index kept is the wrong template
whenever a name is inserted, renamed or deleted ahead of it, a drift
nobody noticed at a swap of an unchanged file and that every Apply would
show (rewritten 2026-10-16, brief 015 D6). **The selection survives
closing and reopening the panel**, by name, for as long as the app runs:
the reopen is a read moment, so the names model it builds follows the
rule above — the entry carrying the name selected before the close, at
its position in the re-read list, the first entry when it is gone — and
at launch the dropdown starts on the first entry (brief 015 D11,
2026-10-16, the Manager under M2 on the persona's words: the one control
that carries the user's intent for the whole evening must not re-decide
itself). This replaces the sentence "closing the panel resets the
dropdown to the first template, swap or no swap — the dropdown is created
with the panel and dies with it" (brief 014 D15, QE finding 1, itself the
correction of D4's "like the dock's visibility"): true of the widget
until brief 015, whose index lives in a window property that the panel's
close does not touch (ui-grid.md, *Slint facts*; retracted 2026-10-16,
brief 015 D11 — 014 D15's follow-up is resolved here, not deferred).

## Contracts

- The IPTC field list is one core table; the panel, the writer and the
  reader all read it.
- `SidecarWriter::iptc` routes the writes; keyword-only messages merge into
  a pending full write, so fields are never dropped (xmp-sidecars.md).
- `Selection::batch` is the apply input (ui-grid.md); the video export
  takes the selected ids in view order WITHOUT `batch`'s cursor fallback
  (video-export.md).
- Copy Picks' rename templates are this engine (fileops.md).
- The keyword chips the window holds are the dump's `keywords=` field
  (test-harness.md, *The dump*): text and coverage per chip, in panel
  order — what a driven swap test reads instead of a screenshot.
- The template names the window holds and the dropdown's selected index
  are the dump's `templates=` and `tplsel=` fields, the warning strip its
  `iptcwarning=` field (test-harness.md, *The dump*); every Apply ends in
  a `template apply '<name>': <outcome>` mark; the dropdown and the Apply
  button are clicked by name (`click:template dropdown`, `click:template
  apply`) — brief 015, 2026-10-16.
- Hard rule 5's split for brief 015: the name lookup (exact,
  case-sensitive; a missing name is the error `IptcError::TemplateMissing`,
  whose text is the strip's sentence) and the selection after a
  replacement (the old name's new position, else 0) are pure rules in
  `fastcull-core` (`iptc.rs`, `template_by_name` and
  `selection_after_replacement`) with unit tests beside them (corrected
  2026-10-16, the developer at brief 015's core commit: this said "`None`
  for a missing name" — the plan's lookup returns the error that carries
  the strip's sentence, so the app never composes it); the
  keep-last-good is app state around the unchanged loader
  (`session::reload_templates`), carried across `begin_session`; the
  strip's two sentences above are the exact strings the tests pin.

## Acceptance criteria

- [x] Expansion per variable; `{seq}` width (a batch of 120 → 3 digits);
      `{{`-escaping; the unknown-variable error naming field and variable;
      the `{seq:N}` range error — `every_variable_expands`,
      `seq_pads_to_batch_width_and_explicit_n`, `brace_escapes_and_errors`,
      `bad_seq_width_gets_its_own_error`.
- [x] Batch apply over three synthetic images: non-empty overwrites, absent
      preserves, empty clears (the load-time warning tested with it),
      keywords union, `{seq}` ordered by the active sort —
      `batch_apply_overwrites_preserves_unions_and_orders_seq`; the
      view-order wiring by the `selection::batch` tests and
      `ExpandContext::from_sort_key`. The full click-to-apply path belongs
      to the user's manual pass (recorded 2026-07-25: Wayland offers no
      input injection; the driven harness came later).
- [x] Template TOML round-trip with Unicode; corrupt-entry resilience per
      the error semantics —
      `templates_toml_roundtrip_unicode_and_partial_corruption`.
- [x] All-or-nothing: a failing expansion mid-batch leaves every image
      unmodified — `all_or_nothing_on_mid_batch_failure`. Today every
      expansion error is template-wide, so the test cannot tell two-phase
      apply from a fail-fast loop; the two-phase structure is load-bearing
      the day a per-image variable appears — do not simplify it away.
- [x] Revert restores the exact pre-apply state, keyword lists included, and
      is single-level — `revert_restores_exact_state_and_is_single_level`.
- [x] `{camera}` stamps the EXIF model end to end —
      `camera_template_stamps_the_exif_model` (two A1 frames and a
      `{camera}.{ext}` rename → `ILCE-1.ARW` and `ILCE-1_1.ARW`, the in-batch
      suffix included).
- [x] **The panel forgets the old folder with it open across the swap**
      (brief 014 AC1): a keyword tagged on folder A's two images, `open:`
      folder B (one image, no sidecar), and after `load settled gen 1` the
      dump reads `keywords=[]` and `revert=""` — app
      `a_session_swap_empties_the_keyword_chips_with_the_panel_open`; red
      on the pre-fix behaviour with the dump field in place (the harness
      commit lands before the fix commit), where it reads
      `keywords=[("wip", "2/2")]` — on `aacf45c` itself the field does not
      exist and the record is the brief's screenshot.
- [x] **…and with it closed across the swap and reopened** (AC2): the same
      with `iptc` before the `open:` and `iptc` after `load settled gen 1`,
      the dump after the reopen reading `keywords=[]` — app
      `a_session_swap_empties_the_keyword_chips_with_the_panel_closed_and_reopened`;
      red on the same pre-fix behaviour, where the reopened panel is
      instantiated from the model the swap never replaced.
- [x] **The new folder fills the row** (AC3): B's image carries a prepared
      sidecar with the keyword `bee`, and after `load settled gen 1` the
      dump reads exactly `keywords=[("bee", "")]` — no coverage on a single
      image, never `wip` — app
      `the_new_folders_sidecar_fills_the_keyword_chips_after_a_swap`. Green
      on the pre-fix tree too (the row corrected itself the moment a
      keyword entered the new batch): it guards the over-fix — a swap that
      empties the models and then bypasses the change-only comparison —
      not the defect.
- [x] **Nothing else rebuilds more** (AC4): the focus tests that read or
      order the `iptc rows rebuilt` marks keep them, count for count —
      `session_swap_mid_field_edit_discards_and_keeps_the_keyboard`,
      `a_cursor_move_rebuild_keeps_the_keyboard_in_the_field`,
      `a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard` — and
      the within-session chip path keeps its promise —
      `session_swap_mid_keyword_edit_never_writes_into_the_new_session`,
      `keyword_enter_commit_still_writes_and_returns_focus`; the mutant
      that drops the swap's forced replacement turns AC1 and AC2 red (run
      on the fix and recorded in brief 014 D13 and the fix commit).
- [x] **An edited template applies as saved** (brief 015 AC1): panel open,
      a template selected, its `title` rewritten on disk after the open
      (the issue #50 `on_line` rewrite, anchored on the panel open's rows
      mark, `iptc rows rebuilt (gen 0)`, which follows the open's read in
      the same refresh), Apply — the sidecar carries the NEW value and
      `revert=` names the template — app
      `apply_re_reads_templates_toml_and_stamps_the_saved_edit`; red before
      the fix, on `a084df2` (the harness and core commits, the Apply flow
      the old one) with the test added, at its title read, where the
      sidecar held `Z`, the OLD value (the persona's fact 2) (corrected
      2026-10-16, the developer at brief 015's fix commit: this box said the
      rewrite was anchored on the open's `templates loaded from` mark — a
      run's first such mark is the launch folder's read, before the panel
      opens, and an edit made there would reach the open's read and leave
      the test green on the old tree).
- [x] **A vanished name applies nothing, visibly** (AC2): the selected
      template renamed in the file before Apply — no sidecar, `revert=""`,
      `iptcwarning=` carries `is no longer in templates.toml — nothing was
      applied`, `tplsel=0` over the re-read `templates=` — app
      `apply_of_a_vanished_template_name_applies_nothing_and_says_so`; red
      on `a084df2` with the test added at its first read, `one.ARW.xmp`
      written — the stale template applied; the warning deleted from the
      vanished-name path turns it red at the strip read.
- [x] **By name, not by position** (AC3): a name that sorts before the
      selected one inserted in the file before Apply — the selected name's
      fields land, not the inserted template's — app
      `apply_finds_the_template_by_name_not_by_position`; green on
      `a084df2` (no re-read at all), as designed: it guards the over-fix, a
      re-read followed by a lookup at the old index, which its mutant (the
      click's index looked up in the re-read list) turns red at the title
      read, `M` — the inserted template's.
- [x] **A broken file at Apply keeps the list and applies nothing** (AC4):
      the file made unparseable before Apply — no sidecar, `iptcwarning=`
      carries the parse error and `the templates shown are the last read —
      nothing was applied`, `templates=` still lists the previous names
      (Apply's `enabled` is bound to that list's length in main.slint, so
      the list is the button's state); after the file is repaired a second
      Apply applies — app
      `a_broken_templates_toml_at_apply_keeps_the_last_good_list_and_applies_nothing`;
      red on `a084df2` with the test added at the first Apply's revert
      read, `Revert: apply 'zebra' to 1 image(s)` — the stale template
      applied; the failed re-read ignored and the kept list applied turns
      it red at the same read.
- [x] **The dropdown follows the name** (AC5, D11): after AC3's insertion
      and Apply, `templates[tplsel]` is still the selected NAME, at its new
      position; with the panel open across a folder swap, another name
      inserted ahead of it before the `open:`, `templates[tplsel]` after
      `load settled gen 1` is again that name at its new position (brief
      014 D4 by name — a swap of an UNCHANGED file cannot tell by-name from
      by-index, so the test changes the file first); and after
      `iptc;…;iptc` — the panel closed and reopened with yet another name
      inserted ahead — `templates[tplsel]` is still that name (D11: the
      reopen is a read moment) — app
      `the_template_dropdown_follows_the_name_at_apply_at_a_swap_and_at_a_reopen`;
      red on `a084df2` with the test added at the first read, `templates=`
      unchanged by the Apply and `tplsel=1`. The mutant that keeps the
      index (the bridge's index write deleted) turns the first two reads
      red — measured, all three read `tplsel=1`; a reset of the window
      property at the panel's close turns the third red alone, `tplsel=1`.
- [x] **The list at a swap, as brief 014 D14 shaped it** (AC6): the file
      deleted after the panel read it, then a swap — `templates=[]` after
      `load settled gen 1`, which is Apply disabled — app
      `a_session_swap_replaces_the_template_list_even_when_the_file_is_gone`;
      green on `a084df2` (014's forced replacement still in place), red with
      `names` dropped from the cache `IptcPanelState::begin_session` keeps —
      the 014 aliasing shape, the old names still listed after the swap
      (014 G1's forced-replacement line is deleted, see *The panel's
      models*). Brief 014's open box — "the template list is replaced at
      the swap too", with the selection's survival by index — closes into
      this box and AC5's: its first half is this test, its second is
      rewritten by R5 and pinned by AC5's swap read; the selection no
      longer rests on the ComboBox's clamp (ui-grid.md, *Slint facts*).
- [x] **Warnings add up** (R6, brief 015 AC9): a template with a `""` field
      and an unknown variable — after Apply `iptcwarning=` carries the
      CLEAR warning AND, after it, the expansion error — app
      `an_apply_time_expansion_error_joins_the_read_warnings`; red on
      `a084df2` with the test added at the CLEAR read, the strip holding
      the expansion error alone; the error put back in place of the read's
      warnings turns it red at the same read, and inserted ahead of them at
      the order read.
- [x] **The pure rules in core**: `Osprey` is not `osprey`, and a missing
      name is the error whose text is the strip's sentence, never another
      template; the selection after a replacement is the old name's new
      position, 0 when the name is gone, nothing was selected or the list
      is empty — core `template_by_name_is_exact_and_case_sensitive`,
      `template_selection_follows_the_name_across_a_replacement`; a
      case-folded lookup (`eq_ignore_ascii_case`) turns the first red at
      the `osprey` read, a case-folded `position` the second at its case
      line, which holds the other spelling at position 1 so that the folded
      match cannot hide behind the gone-name answer 0 (corrected
      2026-10-16, the developer at brief 015's core commit: this box said a
      missing name is `None`).
- [ ] **Nothing else moves** (AC7):
      `templates_and_ui_prefs_are_read_from_the_one_config_dir`, the core
      template tests, the five focus tests and the three swap-chip tests
      of brief 014 AC4 keep their promises, count for count, and the
      change-only rule stands within a session (the names model replaced
      only when the names changed; the selection set without a
      replacement). Ticked when the full suite is green on the fix commit
      on both runners.

## History

- 2026-10-16 — Brief 015: Apply re-reads `templates.toml` and applies the
  selected template BY NAME; a vanished name applies nothing and the strip
  says so; a read that fails as a whole keeps the last good list at every
  read moment, carried across a swap; the dropdown follows the name at
  every replacement of the names model, the swap included — brief 014 D4's
  "by index, clamped" is rewritten (D6) and 014 D14's open box closes into
  AC5 and AC6 (D8); warnings append (R6); the selection survives closing
  and reopening the panel, by name (D11 — brief 014 D15's close-reset
  sentence retracted and its follow-up resolved). The Templates bullet
  said "read on panel open and on Apply" from 2026-07-25 while the app
  read the file
  on panel open and folder open only (M5, `8e67cd6`); the user chose to
  make the app keep the promise (D1), Apply the only new read moment
  (D2). The read moments, the bad-file rule and the by-name rule are
  stated here once; test-harness.md carries the `templates=`, `tplsel=`
  and `iptcwarning=` dump fields, the `template apply` marks and the two
  click names; ui-grid.md the Slint fact the dropdown's follow-the-name
  rests on; docs/metadata.md the user's words.
- 2026-10-16 — Brief 014 (issue #114): after a session swap the panel's
  models show the new session and nothing else, with the panel open across
  the swap and reopened after it; the change-only rebuild rule and the
  chip rule, shipped with M5 and never written into this spec, are stated
  under *The panel's models*; the template dropdown's selection is
  recorded as kept (D4) — narrowed the same day to "while the panel stays
  open" (QE finding 1, D15: the dropdown is created with the panel, so a
  close resets it, before and after the fix), its survival by index joining
  the ledger's open box. Until the fix the chip model was replaced only
  when the new content differed from a cache the swap had reset to empty,
  so a new folder with no keywords kept the old folder's chips and counts
  until its first keyword landed — with the panel open across the swap and
  reopened after it alike. The template list's own forced replacement
  ships without a test, the ledger's open box (D14).
- 2026-10-01 — `templates.toml` resolves through the one config-dir
  resolver of settings.md, hermetic under `FASTCULL_NO_CONFIG` (brief 008).
- 2026-09-17 — Rewritten (brief 007). The pre-rewrite text is
  `specs/history/iptc-templates.md`.
- 2026-08-30 — The rebuild discard is deterministic and the keyboard
  returns to the same row (issue #63).
- 2026-08-22 — `{camera}` expands from the session's EXIF model (both
  bridges had handed it `None` since the feature shipped); a no-stem
  template is refused at plan time (fileops.md).
- 2026-08-03 — The field exits settled: no commit-on-destroy,
  generation-stamped edits across a swap (issue #41, user decision).
- 2026-07-25 — Tri-state apply (user decision after the PM research,
  superseding the earlier "empty preserves" rule), the
  single revert slot, live-reload on open and Apply, the error semantics;
  the panel step's ledger — writer routing, IPTC serialization,
  sanitization, the immediate clear control.
