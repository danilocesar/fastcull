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
  one file `templates.toml`, atomic writes, live-reloadable.
- Variables, expanded at apply time per image:

| Variable | Value |
|---|---|
| `{date}` | capture date `YYYY-MM-DD` (EXIF DateTimeOriginal; file mtime fallback) |
| `{time}` | capture time `HHMMSS` |
| `{seq}` | 1-based position in the current apply batch, zero-padded to batch width |
| `{seq:N}` | as `{seq}` padded to N digits |
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

- [ ] Expansion unit tests per variable, incl. `{seq}` width (batch of 120 → 3
      digits), `{{`-escaping, unknown-variable error naming field+variable.
- [ ] Batch apply over 3 synthetic images: non-empty overwrites, empty preserves,
      keywords union, deterministic `{seq}` ordered by the active sort order.
- [ ] Template TOML round-trip incl. Unicode; corrupt TOML → error surfaced, other
      templates still load.
- [ ] All-or-nothing: a failing expansion on image 2 of 3 leaves images 1 and 3
      unmodified.
- [ ] Revert-last-apply restores exact pre-apply state (incl. keyword lists) and
      is single-level: a second revert is a no-op.
