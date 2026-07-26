# Copy Picks — getting the keepers out

`Ctrl+E` (or **File > Copy Picks…**). The dialog shows how many files
will go, their total size, and the destination's free space — glance,
`Enter`, done. Sidecars travel with their RAWs automatically.

## What "verified" means

Every copied file is checksummed while it's written and the destination
copy is **read back and re-checksummed** before the original name is
even used. The closing line **"all checksums verified"** appears only
when every file that was copied in this run passed — if a run copied
nothing (everything skipped), no verification is claimed, because none
happened.

That line is the format-the-card signal when you cull straight off a
card mount.

Copying never touches the originals: rejects and unmarked files stay
exactly where they are, and nothing is ever deleted or moved. If a copy
fails mid-run, finished files stay, the failed file leaves no debris,
and the report says exactly what happened.

## Running it again

Added a caption or a few more picks after copying? Hit `Ctrl+E` again:

- Files already copied this session default to **skip** — only the new
  picks go.
- An image whose **sidecar changed** since the copy gets its sidecar
  re-copied alone; the big RAW isn't transferred twice.
- Name collisions at the destination default to **auto-suffix**
  (`DSC01234_2.ARW`, `_3`…), never silent overwrite. A "Skip existing"
  toggle skips collisions instead. There is deliberately no overwrite
  option — it's the one choice that could destroy an already-verified
  earlier copy.

## Renaming on the way out

The template field renames copies as they land; leave it empty to keep
original names. It uses the [same variables as metadata
templates](metadata.md#templates-templatestoml), and the template is
the WHOLE new name — include the extension via `{ext}`:

```
{date}_osprey_{seq:3}.{ext}
```

gives `2026-07-25_osprey_001.ARW`, `..._002.ARW`, … numbered in your
session's sort order (capture time unless you switched the sort). The dialog remembers your last destination and
offers last session's template as a one-click chip.

---

Next: [FAQ & troubleshooting](faq.md)
