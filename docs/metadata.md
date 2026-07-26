# Metadata — picks, sidecars, and templates

## Where your work goes

Every mark and every metadata edit is written to an XMP sidecar next to
the RAW: `DSC01234.ARW.xmp`. That's darktable's native naming, and
what's inside is deliberately boring, standard XMP:

| In FastCull | In the sidecar | In darktable |
|---|---|---|
| Picked (`Y`) | `xmp:Rating = 1` | one star — filter with ≥★ |
| Rejected (`N`) | `xmp:Rating = -1` | the reject flag |
| Unmarked | no rating property | unrated |
| Keywords | `dc:subject` + hierarchical form | tags |
| Title, description, creator, copyright, city, … | the standard Dublin Core / IPTC properties | the matching metadata fields |

Two promises that make this safe on real folders:

- **Existing sidecars are preserved.** FastCull rewrites only the
  properties it owns; anything another tool put there survives
  byte-for-byte.
- **Re-opening a folder restores everything.** Picks and metadata are
  read back from the sidecars at open — yesterday's session is exactly
  where you left it, no database involved.

Writes happen on a background thread within a second of each change and
are flushed before Copy Picks runs. If a write ever fails (read-only
folder, full disk) the status bar says so — silence means saved.

## The IPTC panel

`I` toggles the panel; `K` jumps straight into the keyword field from
anywhere (opening the panel if needed).

- Fields apply to the **current image**, or to the whole **selection**
  if you have one (`Ctrl+A`, Shift+arrows, Ctrl/Shift+click).
- On a mixed selection a field shows *mixed*; typing overwrites all.
- **Keywords are additive**: applying adds to what each image has.
  Removing a keyword chip removes it from every selected image.
- **Revert last apply** undoes exactly the most recent template apply
  or batch edit — one level, so act on it before the next batch.
- Typing in a field never triggers shortcuts — `Enter` commits and
  returns you to the grid; clicking away also commits. (There is no
  abandon-edit gesture in this version — if you mangled a field,
  correct it and commit again, or use Revert after an apply.)

## Templates (`templates.toml`)

Templates stamp a whole set of fields in one click — pick one in the
panel's dropdown, hit **Apply**. In this version templates are edited
by hand in one TOML file (there is no save-template UI yet):

- Linux: `~/.config/fastcull/templates.toml`
- Windows: `%APPDATA%\fastcull\fastcull\config\templates.toml`

The file is re-read every time you open the panel — edit, save, toggle
the panel, apply. A complete example:

```toml
[templates.osprey-mornings]
creator = "Your Name"
rights = "© 2026 Your Name"
city = "Florianópolis"
country = "Brazil"
title = "{filename}"
job_id = "osprey-{date}"
keywords = ["osprey", "wildlife", "brazil"]
```

Available field keys: `title`, `description`, `creator`, `rights`
(copyright), `headline`, `city`, `country`, `credit`, `source`,
`job_id`, `location`, and the `keywords` list.

Values may use variables, expanded per image at apply time:

| Variable | Becomes |
|---|---|
| `{date}` | capture date, `YYYY-MM-DD` |
| `{time}` | capture time, `HHMMSS` |
| `{seq}` | position in the batch, zero-padded to the width of the batch count (120 images → 3 digits) |
| `{seq:4}` | the same, padded to exactly 4 digits |
| `{filename}` | the file name without extension |
| `{camera}` | the camera model — **currently broken: expands to an empty string; avoid it until fixed** |
| `{ext}` | the original extension, uppercase |

Literal braces are written `{{` and `}}`. An unknown variable refuses
to apply (with an error naming it) rather than stamping garbage.

**Two warnings for hand-editing**:

- `city = ""` does not mean "leave city alone" — it means **clear the
  city field on apply**. To leave a field untouched, omit the line
  entirely. The panel warns when a template contains an empty value.
- A misspelled key (`tittle = "..."`) is **silently ignored** — no
  error, the field just doesn't stamp. If a field mysteriously doesn't
  apply, check the spelling against the key list above.

---

Next: [Copy Picks — getting the keepers out](copy-picks.md)
