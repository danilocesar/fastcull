# Module spec: IPTC model & templates (`iptc.rs`)

## Purpose

The IPTC data model edited in the side panel, plus saved templates ("stationery
pads") with variable expansion, applicable to one image or a multi-selection.

## Data model

`IptcData`: the fields listed in the xmp-sidecars mapping table (title, description,
creator, rights, headline, city, country, credit, source, job id, location,
keywords: `Vec<String>`). All fields optional. Keywords are ordered, deduplicated
case-preservingly (first spelling wins; comparison is Unicode-casefolded).

## Templates

- Named `IptcTemplate`s: same fields, values may contain variables.
- Persisted as TOML in the user config dir (`directories` crate conventions),
  one file `templates.toml`, atomic writes (temp + fsync + rename, the
  project standard), live-reloadable (v1 reads the file on panel open /
  Apply — no filesystem watcher; recorded 2026-07-25).
- Error semantics (recorded 2026-07-25, implementation decision): a corrupt
  ENTRY (`[templates.x]` with wrong types) is surfaced per-entry and the
  other templates still load; an unparseable FILE (broken TOML syntax,
  duplicate keys) is a hard error — there is nothing safe to partially
  load from a file the parser cannot segment. Unknown KEYS inside an entry
  are currently ignored by serde (a typo like `tittle` silently does
  nothing — known limitation, revisit with the panel).
- Variables, expanded at apply time per image:

| Variable | Value |
|---|---|
| `{date}` | capture date `YYYY-MM-DD` (EXIF DateTimeOriginal; file mtime fallback) |
| `{time}` | capture time `HHMMSS` |
| `{seq}` | 1-based position in the current apply batch, zero-padded to batch width |
| `{seq:N}` | as `{seq}` padded to N digits (N = 1..=32; out-of-range is its own error, distinct from unknown-variable) |
| `{filename}` | original stem (no extension) |
| `{camera}` | EXIF model string, whitespace-normalized |
| `{ext}` | original extension, uppercase |

- Unknown variable → apply-time error naming the variable and template field;
  nothing is applied (all-or-nothing per batch).
- Literal braces escaped as `{{` / `}}`.

## Apply semantics

- Apply to selection — tri-state per field (user decision 2026-07-25 after
  the Photo Mechanic research; supersedes the earlier "empty preserves"
  rule): an **absent** field preserves existing values; a **non-empty**
  field overwrites; an **empty string** CLEARS the field on every selected
  image (PM's ticked-but-empty case — "cover our asses"). Clearing REMOVES
  the XMP property, never writes an empty value (interop). The empty-string
  encoding is the TOML wire format ONLY: in the panel UI, bare emptiness
  always preserves, and clearing is an explicit per-field control with an
  unmistakable visual state (persona IN-MY-WAY on making emptiness the
  gesture). Because this flips the meaning of `field = ""` in hand-edited
  files, template load emits a WARNING naming template and field for every
  empty-string field it finds. Whitespace-only values count as empty
  everywhere in this rule (validator: "   " was neither a clear nor a
  meaningful overwrite, and the sidecar reader drops whitespace-only
  values on round-trip — so they clear, and they warn).
- Keyword apply is additive (union), not replacement.
- Manual panel edit on a multi-selection behaves the same way, field-by-field.
- Revert: ONE shared single-level slot (user decision 2026-07-25, persona
  recommendation) armed by EVERY batch mutation from the panel — template
  Apply, a manual field commit to a multi-selection, and keyword-chip
  removal alike; the button label reflects what it will revert. Cleared by
  the next batch mutation or session close. There is no general undo stack
  in v1 (user decision after persona review, which flagged the previous
  wording as a dangling promise).

## Acceptance criteria (tests)

- [x] Expansion unit tests per variable, incl. `{seq}` width (batch of 120 → 3
      digits), `{{`-escaping, unknown-variable error naming field+variable
      (`every_variable_expands`, `seq_pads_to_batch_width_and_explicit_n`,
      `brace_escapes_and_errors`, `bad_seq_width_gets_its_own_error`).
- [x] Batch apply over 3 synthetic images: non-empty overwrites, absent
      preserves, empty CLEARS (tri-state; warning test covers the load-time
      notice),
      keywords union, deterministic `{seq}` ordered by the active sort order
      (`batch_apply_overwrites_preserves_unions_and_orders_seq`). CAVEAT
      (recorded 2026-07-25): `{seq}` order is a CALLER CONTRACT — apply takes
      the batch slice in the active sort order; the wiring is covered by
      `selection::batch` view-order tests + `ExpandContext::from_sort_key`
      (the app builds ctxs from `Selection::batch` output directly); the
      full click-to-apply path is headless-untestable (recorded — Wayland
      offers no input injection) and belongs to the user's manual pass.
- [x] Template TOML round-trip incl. Unicode; corrupt-entry resilience per the
      recorded error semantics above
      (`templates_toml_roundtrip_unicode_and_partial_corruption`).
- [x] All-or-nothing: a failing expansion mid-batch leaves every image
      unmodified (`all_or_nothing_on_mid_batch_failure`). NOTE: today every
      expansion error is template-wide (no ctx-dependent failure exists), so
      the test cannot distinguish two-phase apply from a fail-fast loop; the
      two-phase structure is load-bearing the day a ctx-dependent variable
      appears — do not simplify it away.
- [x] Revert-last-apply restores exact pre-apply state (incl. keyword lists) and
      is single-level: a second revert is a no-op
      (`revert_restores_exact_state_and_is_single_level`).

Panel-step ledger (updated after the panel gate, 2026-07-25):
- DONE: writer-thread routing (SidecarWriter::iptc; keyword-only messages
  MERGE into a pending full write — fields never dropped); IPTC field
  serialization (write_iptc: clear removes the property, never writes
  empty values; rewrites are byte-stable — no whitespace accumulation);
  input sanitization at the commit boundary (iptc::sanitize_text: NFC +
  control-strip + trim; add_keywords sanitizes before the casefold dedup).
- Tri-state UI, v1 reading (recorded): the per-field clear control (⌫)
  clears IMMEDIATELY across the batch, revert-covered — there is no
  pending-clear badge state; bare emptiness always preserves, and a
  value-unchanged commit is a strict no-op (must not arm revert or touch
  sidecars).
- CLOSED 2026-08-22: `{camera}` used to expand EMPTY from both the panel
  and the rename field, because each bridge handed `ExpandContext` a
  literal `None`. The EXIF model now rides with the session
  (`SessionState::camera_models`, filled from `MetadataReady` alongside
  the capture-time sort key) and both bridges pass it. It is the MODEL
  alone — `FrameMeta::camera` is not the source, since burst grouping
  prefers the serial number. It stays empty for an image whose metadata
  has not landed yet, exactly as the capture-time sort is provisional
  during a load; driven end to end by
  `camera_template_stamps_the_exif_model` (two A1 frames + a
  `{camera}.{ext}` rename → `ILCE-1.ARW` and `ILCE-1_1.ARW`, which also
  exercises the in-batch suffix that one camera over two picks creates).
  Before the fix the same template wrote `.ARW`, a hidden file with no
  name of its own — now refused at plan time (fileops.md). Esc inside a field is NOT an abandon gesture (Slint LineEdit
  offers no Esc hook in v1) — the exits are: Enter (commit + focus to
  grid), click-away (G7 commit, focus stays where clicked), a covering
  surface — a Help modal or the copy dialog — which commits like
  click-away (preserving the shipped G7 semantics; the covering scope
  takes the keyboard — issue #41), and destruction — panel close, session
  swap, or the panel's field rows being REBUILT under the editor as the
  folder's metadata lands — which
  DISCARDS the un-committed text (issue #41, user decision 2026-08-03:
  no commit-on-destroy; a swap also generation-stamps edits so the old
  session's text can never land on the new session's images).
  Deterministic since 2026-08-30 (issue #63): the rebuild case used to be
  a timing coin flip — the dying editor's blur handler ran, and committed,
  in 2 runs of 20 — and a rebuild generation stamped on focus gain now
  decides it outright. The same fix keeps "focus stays where clicked"
  true across a rebuild: the keyboard goes back to the SAME field row,
  not to the grid, so the next character of a caption can never become a
  cull command. Focus
  continuity across all of these is specified in ui-grid.md; recorded
  deviation on Esc stands.
