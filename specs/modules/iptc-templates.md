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
  `templates.toml`, written atomically (temp + fsync + rename). Read on
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
  capture-time sort key) — the model alone, since burst grouping prefers
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

## Contracts

- The IPTC field list is one core table; the panel, the writer and the
  reader all read it.
- `SidecarWriter::iptc` routes the writes; keyword-only messages merge into
  a pending full write, so fields are never dropped (xmp-sidecars.md).
- `Selection::batch` is the apply input, and the video export's
  (ui-grid.md, video-export.md).
- Copy Picks' rename templates are this engine (fileops.md).

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

## History

- 2026-09-17 — Rewritten (brief 007). The pre-rewrite text is
  `specs/history/iptc-templates.md`.
- 2026-08-30 — The rebuild discard is deterministic and the keyboard
  returns to the same row (issue #63).
- 2026-08-22 — `{camera}` expands from the session's EXIF model (both
  bridges had handed it `None` since the feature shipped); a no-stem
  template is refused at plan time (fileops.md).
- 2026-08-03 — The field exits settled: no commit-on-destroy,
  generation-stamped edits across a swap (issue #41, user decision).
- 2026-07-25 — Tri-state apply (user decision after the PM research), the
  single revert slot, live-reload on open and Apply, the error semantics;
  the panel step's ledger — writer routing, IPTC serialization,
  sanitization, the immediate clear control.
