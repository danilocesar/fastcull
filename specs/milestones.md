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
