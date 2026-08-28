# Copy Picks — getting the keepers out

`Ctrl+E` (or **File > Copy Picks…**). The dialog shows how many files
will go, their total size, and the destination's free space — glance,
`Enter`, done. Sidecars travel with their RAWs automatically.

## What "verified" means

Every copied file is checksummed while it's written and the destination
copy is **read back and re-checksummed** before the original name is
even used. The line **"all checksums verified"** appears only when this
run really did verify something — either files it copied, or files it
found already identical at the destination and re-checked (see
*Overwrite* below). A run that moved and checked nothing claims nothing.

That line is the format-the-card signal when you cull straight off a
card mount.

Copying never touches the originals: rejects and unmarked files stay
exactly where they are, and nothing is ever deleted or moved. If a copy
fails mid-run, finished files stay, the failed file leaves no debris,
and the report says exactly what happened.

If you **force-quit** the app in the middle of a copy (or the machine
loses power), the file that was in flight can leave a hidden half-written
file behind, named like `.fastcull-partial-8421-3`. It is never mistaken
for one of your photos and never reused — but nothing sweeps it up
either, so if you go looking with hidden files shown, those are safe to
delete. Cancelling a copy from the dialog leaves none.

## When the names are already taken

Before anything moves, FastCull looks at **the destination folder** —
not at what it remembers copying. If any name it is about to write is
already taken (the RAW **or** its `.xmp` sidecar; a folder or a dangling
symlink counts too), it asks once, for the whole run:

> 12 of your 148 picks already have files with these names in
> …/2026-08-21-osprey/selects
> The other 136 copy normally. Choose once for the whole run:
>
> **B** — Keep both: the 12 land as `DSC01234_1.ARW`  (+590 MB)
> **O** — Overwrite those 12: identical files are re-checked, not re-sent
> **Esc** — Cancel: copy nothing at all, not even the 136

- **Keep both** gives each clashing pick the first free number — `_1`,
  then `_2`, … — with the sidecar in lockstep, so a pair never splits
  across two numbers and nothing already there is touched. The button
  shows the name it will really use, so a second "keep both" into the
  same folder says `_2`.
- **Overwrite** replaces those files. A destination file that is already
  **byte-for-byte identical** is *not* sent again: FastCull checksums
  what's there, keeps it, and rewrites only the sidecar if your captions
  changed. The report then says *"145 already identical — re-verified in
  place"* — which makes a second `Ctrl+E` a free "is my export still
  bit-perfect?" pass before you wipe the card.
- The Overwrite promise has one hair-splitting exception, worth knowing
  if you copy onto a card: on a filesystem with no hard links (FAT32,
  exFAT, some network mounts) FastCull has to check the name and then
  write it as two steps instead of one, so a file that appears in that
  split second is replaced. On any ordinary disk the two are a single
  operation and cannot be raced.
- **Cancel** copies **nothing at all** — not even the files that had no
  clash. `Esc` does the same and leaves the dialog on your plan, so
  pointing it at another folder is one step.
- `Enter` deliberately does nothing on this question: `Ctrl+E`, `Enter`,
  `Enter` must never replace or duplicate 148 files by reflex. Answer
  with `B`, `O`, `Esc` — or click.

**Two of your own picks sharing a name** — two bodies, two cards, the
same `DSC01234.ARW` — never raise this question at all: the second one
lands as `DSC01234_1.ARW` whatever you answer, because overwriting one of
your photographs with the other is not something worth offering. The plan
line says so before you copy: *"1 pick shares a name with another — it
gets a suffix"*. The same goes for a rename template that gives several
frames the same name — they land `same.ARW`, `same_1.ARW`, `same_2.ARW`,
in your sort order.

> ⚠ **Overwrite replaces sidecars too.** If you have already started
> developing the copies in darktable, its edit history lives in
> `DSC01234.ARW.xmp` — the same file name FastCull writes — and
> Overwrite replaces it. This is deliberate: overwrite means overwrite,
> and nothing is merged behind your back. Answer **Keep both**, or copy
> into a fresh folder, when the destination has been edited elsewhere.

If a caption refresh cannot land — something else is sitting under the
`.xmp` name, or the destination filled up — the RAW beside it is still
yours and still verified, and the report says both things: the file counts
in *"already identical — re-verified in place"* **and** appears in the
failed list with the reason. Nothing there was damaged; only the sidecar
update was lost.

The other way round, Overwrite never *deletes* anything at the
destination: if a pick has no sidecar of its own (its sidecar could not be
written — a locked card, a full disk), overwriting the RAW leaves the
`.xmp` that was already there, describing a different photo. The report
says so — *"1 destination sidecar left in place — that pick has no sidecar
of its own"* — and **Keep both** is the answer that avoids the pairing
entirely.

Nothing is ever replaced unless you answered Overwrite: each file is
copied to a temporary name, checksum-verified, and only then given its
final name — and if something else grabbed that name in the meantime,
that one file fails with a clear reason and the rest of the run
continues.

## Running it again

Added a caption or a few more picks after copying? Hit `Ctrl+E` again.
The folder still holds your earlier copies, so you get the question
above — **Overwrite** is the answer that adds the new picks and
re-verifies the old ones instead of re-sending them.

Before you press Copy, the dialog already shows the split: *"3 new · 148
already exist here — Copy will ask what to do"*. That line is also your
only reminder the morning after, when the ✓ badges from yesterday's
session are gone.

If you deleted copies from the destination by hand (or wiped the folder
to re-export under a new name template), nothing clashes any more, so
there is no question: those picks are copied again — RAW and sidecar
together, checksum-verified — and the dialog says *"3 copied earlier but
gone from the destination — copying again"*. Removing a copy by hand does
not un-pick the photo; press `X` on it in FastCull if you want it to stay
out.

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

Next: [Export Frames as Video — a burst you can post](export-video.md)
