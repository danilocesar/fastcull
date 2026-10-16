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
  every driven run read the user's real `templates.toml`). Read on
  panel open and on Apply — no file watcher (2026-07-25); `templates.toml`
  is hand-edited in v1.
- Errors: a corrupt entry (`[templates.x]` with wrong types) is surfaced per
  entry and the other templates still load; an unparseable file (broken
  syntax, duplicate keys) is a hard error — nothing is safe to load from a
  file the parser cannot segment. Unknown keys inside an entry are ignored
  by serde, so a typo like `tittle` silently does nothing (known
  limitation).
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
all three models whatever their content, at the first refresh after it —
the swap's own refresh with the panel open, the refresh the reopen runs
before its first frame with it closed (issue #114, brief 014 D3,
2026-10-16). The template dropdown's selection survives the swap like the
dock's visibility — the user's stationery, not the folder's data — by
index, clamped to the re-read list, which Slint's ComboBox does on its own
at a model replacement (ui-grid.md, *Slint facts*; brief 014 D4,
2026-10-16).

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

## History

- 2026-10-16 — Brief 014 (issue #114): after a session swap the panel's
  models show the new session and nothing else, with the panel open across
  the swap and reopened after it; the change-only rebuild rule and the
  chip rule, shipped with M5 and never written into this spec, are stated
  under *The panel's models*; the template dropdown's selection is
  recorded as kept (D4). Until the fix the chip model was replaced only
  when the new content differed from a cache the swap had reset to empty,
  so a new folder with no keywords kept the old folder's chips and counts
  until its first keyword landed — with the panel open across the swap and
  reopened after it alike.
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
