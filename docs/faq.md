# FAQ & troubleshooting

**My picks don't show up in darktable.**
Check the sidecar exists next to the RAW (`DSC01234.ARW.xmp`). If the
folder was already imported in darktable before you culled, darktable
may be showing its database copy — select the images and use
*load sidecar file* / re-import the folder. Picks arrive as ratings
(★ = pick, reject flag = reject); this round-trip is exercised against
a real darktable in FastCull's test suite.

**Does Lightroom read the sidecars?**
Honest answer: the *contents* are standard XMP that Lightroom
understands, but the *file name* follows darktable's convention
(`NAME.ARW.xmp`), while classic Lightroom looks for `NAME.xmp`.
Verified today: darktable (automatically, in CI). digiKam and Lightroom
read the same properties but haven't been round-trip tested — for
Lightroom you may need to rename sidecars to `NAME.xmp` on import. If
this matters to your workflow, say so in an issue.

**I opened my shoot folder and it says "No images".**
FastCull reads one folder, not subfolders. Open the folder that
actually contains the RAW files (e.g. `.../2026-07-25/card1/`).
"No folder open" is different — that just means no folder was chosen
yet (`Ctrl+O`).

**What's this cache folder?**
Decoded previews are cached (Linux: `~/.cache/fastcull/`; Windows:
`%LOCALAPPDATA%\fastcull\fastcull\cache`) so the second open of a
folder is instant. It's capped around 2 GiB with
least-recently-used eviction, and deleting it is always safe — it just
rebuilds thumbnails on the next open. After some upgrades the app
rebuilds it once by itself; the only cost is a slower first open.

**Thumbnails load slowly from my NAS / slow card.**
Set `FASTCULL_MAX_READERS=4` (or 2) in the environment. It caps how
many files are read at once — slow media thrashes when too many reads
compete.

**Something misbehaves — what should I attach to a bug report?**
Run with `FASTCULL_TRACE=1` from a terminal and attach the output: it
timestamps every slow UI phase and loupe state change. If the issue
looks cache-related, try once with `FASTCULL_NO_CACHE=1` to rule it in
or out. (You may also see `FASTCULL_DRIVE` mentioned in the source —
it's a test-automation hook that can mark real files; not for everyday
use.)

**Where do I read about how it works inside?**
The developer specs in [`specs/`](../specs/) are the source of truth —
[architecture](../specs/01-architecture.md), per-module contracts in
[`specs/modules/`](../specs/modules/). This guide is deliberately the
short version.

---

Back to: [Getting started](index.md) ·
[Culling](culling.md) ·
[Metadata](metadata.md) ·
[Copy Picks](copy-picks.md)
