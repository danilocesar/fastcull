# Milestones

Each milestone lands as one or more commits with green CI. Definition of done (DoD)
includes the listed spec acceptance criteria turning into passing tests, plus the
step validation gate in CLAUDE.md: every step is reviewed by the `validator` and
`qe-engineer` subagents before it counts as complete.

## M0 — Scaffold ✔ (this commit series)
Repo, GPL-3.0, workspace (core/cli/app), specs tree, CLAUDE.md, testdata fetcher,
Linux+Windows CI. DoD: `cargo test`/`clippy -D warnings` green on both OSes.

## M1 — Core pipeline
catalog scan, raw extraction (incl. A1 full-res extractor), pipeline priority pool,
SQLite cache, EXIF read. `fastcull-cli scan|thumbs` subcommands. Perf budgets
enforced by release-mode tests (criterion benches provide the numbers). DoD:
raw-pipeline + catalog-cache acceptance criteria pass against the 3 real A1 files —
except the sidecar-at-open criterion, which is deferred to M3 where XMP parsing
lands (deferral approved by the user, 2026-07-24). **M1 closed 2026-07-24.**

## M2 — Grid UI (prototype risk first)
Slint window, windowed-model virtualized grid, zoom column steps, progressive
loading, keyboard navigation, placeholder/badge visuals. DoD: 2,000-file synthetic
folder scrolls smoothly; screenshot smoke tests. **Start with the windowed-model
spike — if Slint can't hit 60 fps here, escalate to the user before proceeding.**

## M3 — Culling — **CLOSED 2026-07-25** *(built after M4 per the swap)*
Pick/reject session state (Y/N/U + badges + auto-advance everywhere), sidecar
writer thread, XMP serializer with preservation, sidecar-at-open,
`fastcull-cli cull`, EXIF orientation on all rungs, I/O gate + bounded
shutdown. DoD met: xmp-sidecars acceptance criteria pass incl. the sandboxed
darktable-cli round-trip (ratings verified in darktable 5.4.1's library.db);
keyword halves moved to M5 (scope split approved by the user).

## M4 — Loupe *(order swapped with M3 by user decision 2026-07-25: full-res
zoom quality first, culling marks second — implemented 2026-07-25)*
**M4 closed 2026-07-25**: loupe quality + walk behavior confirmed by the user
on real shoot folders; v0.1.0 tagged. Open M2/M4 stragglers: screenshot
smoke tests DONE 2026-07-25 (grid/placeholder/badge/loupe-fit/1:1);
Ctrl+scroll decision still open.
Fit + 1:1 zoom, pan, ±2 prefetch via the dedicated loupe engine (single
full-res asset, see raw-pipeline.md recorded deviation). Auto-advance-on-mark
moves to M3 with the marks themselves. DoD: **tag a runnable 0.x release**
once the user confirms loupe quality (the earliest genuinely usable build) —
done: v0.1.0.

*(M5–M7 reordered after the persona review + user decisions: filter before IPTC
because the IPTC pass starts with "filter to picked, select all"; copy-picks
before bursts because copy is the exit move of every session and bursts are
decoration.)*

## M5 — Filter/sort + IPTC + window chrome
filter.rs predicates + bar UI (pick-state filters with counts first); then IPTC
panel, multi-select apply, templates + variables, revert-last-apply, config
persistence. Window chrome (user-requested): menu bar with Open Folder… picker,
Help → Keyboard Shortcuts popup, Settings placeholder (see ui-grid.md).

## M6 — Copy picks
fileops plan/execute + dialog + report: rename templates, auto-rename collision
default, BLAKE3 verification, sidecar lockstep.

## M7 — Bursts + packaging

burst.rs grouping + border rendering + in-burst filter. cargo-dist packaging:
Linux AppImage, Windows zip/MSI. DoD: full manual acceptance script in
specs/00-overview terms: open **5,000** A1 files → cull → IPTC → copy →
darktable sees everything.

**Packaging partially pulled forward into M5** (2026-07-25), because the user
needs a Windows executable to test the app long before M7. What already landed:

- CI uploads an unsigned Windows test build (`fastcull-windows-x64`) from every run
  of the Windows job — see the "Testing on Windows" section of README.md.
- cargo-dist ("dist") 0.32.0 is configured in `dist-workspace.toml` and generates
  `.github/workflows/release.yml`; a `v*` tag produces a GitHub Release with
  `.tar.xz` (Linux) and `.zip` (Windows) archives plus SHA-256 checksums, each
  archive carrying LICENSE, README.md and THIRD-PARTY-LICENSES.md.

Still owned by M7: **Linux AppImage** and **Windows MSI** (dist can generate an MSI
via its `msi` installer; AppImage needs separate tooling), and the end-to-end
verification of the release workflow itself — no tag has been pushed yet, so the
Windows leg of `release.yml` and the GitHub Release step have never executed.

## M8 — Documentation (user decision 2026-07-26)

A `docs/` usage guide distilled from the specs (issue #9): plain Markdown,
web-readable (GitHub renderer; Pages-ready later). Task-oriented pages —
quick start, culling & keyboard, metadata & templates, copy-picks, FAQ —
written as simply as possible; each page is reviewed whenever its source
module spec changes. DoD: a newcomer can go from "downloaded the app" to
"copied verified picks" using docs/ alone.

DELIVERED 2026-07-26: five pages (index, culling, metadata, copy-picks,
faq); QE executed the full DoD path from the docs against the release
binary. Release-note debt RESOLVED with v0.3.0: the index install note was
removed and the culling callout pinned to "Changed in 0.3.0". The docs-follow-specs binding lives in CLAUDE.md.

## v0.4.0 (released 2026-07-27)

Everything since v0.3.0: the import-performance overhaul (EXIF summaries
via the in-tree TIFF walker — the rawler whole-file mmap serialized all
import workers on mmap_lock; a real 1,450-ARW folder on an ntfs-3g
backup drive went from 99–133 s to ~1–3 s, local NVMe 5k from ~14.5 s to
~3.1 s, per-file EXIF 1.71 ms → 5 µs), the loupe soft-transit contract
(#21 — the view never strobes to fit during held-arrow transit) with the
loupe-engine scheduling fixes it exposed (ring-gated deferred revival +
the debounced focus-reserved worker), the loupe state badge (#20 — mark
visible without leaving the loupe; rejects no longer dimmed at fit), the
About dialog (#23 — build-composed version string, X.Y.Z-devel-<hash>
off-tag) with full modal keyboard containment for both popups, the #18
anchor closure (verified fixed by the #16 relayout work, regression-
pinned), and the drive/shutter harness determinism work. No docs
release-note debt: culling.md gained its sections with the features.
