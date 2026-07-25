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

- Apply to selection: each **non-empty** template field overwrites that field on
  every selected image; empty template fields leave existing values untouched.
- Keyword apply is additive (union), not replacement.
- Manual panel edit on a multi-selection behaves the same way, field-by-field.
- Revert: the session keeps the pre-apply IPTC state of the **last** batch apply
  and offers "Revert last apply" (single level, cleared by the next apply or
  session close). There is no general undo stack in v1 (user decision after
  persona review, which flagged the previous wording as a dangling promise).

## Acceptance criteria (tests)

- [x] Expansion unit tests per variable, incl. `{seq}` width (batch of 120 → 3
      digits), `{{`-escaping, unknown-variable error naming field+variable
      (`every_variable_expands`, `seq_pads_to_batch_width_and_explicit_n`,
      `brace_escapes_and_errors`, `bad_seq_width_gets_its_own_error`).
- [x] Batch apply over 3 synthetic images: non-empty overwrites, empty preserves,
      keywords union, deterministic `{seq}` ordered by the active sort order
      (`batch_apply_overwrites_preserves_unions_and_orders_seq`). CAVEAT
      (recorded 2026-07-25): `{seq}` order is a CALLER CONTRACT — apply takes
      the batch slice in the active sort order; the wiring from filter.rs's
      sort lands with the panel step and must be integration-tested there.
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

Deferred to the panel step (recorded 2026-07-25): keyword writes routed
through the debounced sidecar-writer thread (today `xmp::write_keywords` has
no app caller; parallel raw calls are corruption-safe but last-writer-wins
per property); IPTC FIELD serialization to XMP (dc:title etc.); panel input
sanitization (control characters are invalid XML 1.0; keywords are trimmed
on sidecar read; NFC-normalize before casefold dedup); `ExpandContext`
wiring from exif.rs.
