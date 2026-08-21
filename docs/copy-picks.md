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
  picks go. "Already copied" means the copy is **still there**: if you
  deleted some copies from the destination by hand (or wiped the whole
  folder to re-export with a new name template), those picks are copied
  again — RAW and sidecar together, checksum-verified — and the dialog
  tells you: *"3 copied earlier but gone from the destination — copying
  again"*. Removing a copy by hand does not un-pick the photo; press `X`
  on it in FastCull if you want it to stay out.
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

> While a folder is **still loading**, hold off on a copy that uses
> `{seq}`. Sequence numbers follow capture time, and capture times aren't
> all known until the load finishes — so numbers assigned mid-load match
> neither the grid you're looking at (ordered by filename until then) nor
> the same copy run a few seconds later. The status bar tells you when
> loading is done.

---

Next: [FAQ & troubleshooting](faq.md)
