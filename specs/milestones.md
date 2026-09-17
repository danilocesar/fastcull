# Milestones

Each milestone lands as one or more commits with green CI. Definition of done (DoD)
includes the listed spec acceptance criteria turning into passing tests, plus the
pipeline in CLAUDE.md: every step passes the senior developer's review and QE
before it counts as complete.

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

## M2 — Grid UI (prototype risk first) — **CLOSED 2026-07-25** *(with M4's v0.1.0)*
Slint window, windowed-model virtualized grid, zoom column steps, progressive
loading, keyboard navigation, placeholder/badge visuals. DoD: 2,000-file synthetic
folder scrolls smoothly; screenshot smoke tests. The windowed-model spike held —
no escalation was needed (gate findings landed 2026-07-24, `731374f`), the smoke
tests landed 2026-07-25, and the one M2 deferral still standing is Ctrl+scroll
grid zoom: reserved in the pointer contract (ui-grid.md), `+`/`-` cover it.

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

## M5 — Filter/sort + IPTC + window chrome — **CLOSED 2026-07-25** *(`8e67cd6`; shipped in v0.2.0)*
filter.rs predicates + bar UI (pick-state filters with counts first); then IPTC
panel, multi-select apply, templates + variables, revert-last-apply, config
persistence. Window chrome (user-requested): menu bar with Open Folder… picker,
Help → Keyboard Shortcuts popup, Settings placeholder (see ui-grid.md).

## M6 — Copy picks — **CLOSED 2026-07-25** *(`f79352e`; collisions rewritten as the clash question 2026-08-21, v0.10.0)*
fileops plan/execute + dialog + report: rename templates, auto-rename collision
default, BLAKE3 verification, sidecar lockstep. (The auto-rename default was
replaced 2026-08-21 by the clash question — one question, three answers; see
fileops.md.)

## M7 — Bursts + packaging — **bursts CLOSED 2026-07-26** *(`60bcef6`)*; packaging still open below

burst.rs grouping + border rendering + in-burst filter. cargo-dist packaging:
Linux AppImage, Windows zip/MSI. DoD: full manual acceptance script in
specs/00-overview terms: open **5,000** A1 files → cull → IPTC → copy →
darktable sees everything. (That manual script was never recorded as run;
the user retired it on 2026-09-17, together with ui-grid.md's per-release
"Manual acceptance" box.)

**Packaging partially pulled forward into M5** (2026-07-25), because the user
needs a Windows executable to test the app long before M7. What already landed:

- CI uploads an unsigned Windows test build (`fastcull-windows-x64`) from every run
  of the Windows job — see the "Testing on Windows" section of README.md.
- cargo-dist ("dist") 0.32.0 is configured in `dist-workspace.toml` and generates
  `.github/workflows/release.yml`; a `v*` tag produces a GitHub Release with
  `.tar.xz` (Linux) and `.zip` (Windows) archives plus SHA-256 checksums, each
  archive carrying LICENSE, README.md and THIRD-PARTY-LICENSES.md.

Still owned by M7: **Linux AppImage** and **Windows MSI** (dist can generate an MSI
via its `msi` installer; AppImage needs separate tooling — `installers = []` in
`dist-workspace.toml` as of v0.14.0, and no release carries either). The
end-to-end verification of the release workflow is DONE: the throwaway
`v0.1.1-rc.1` (2026-07-26) was its first run, and every tag from v0.2.0 to
v0.14.0 (2026-09-12) has built both legs of `release.yml` and published a GitHub
Release with the Linux `.tar.xz`, the Windows `.zip`, their SHA-256s and
`source.tar.gz`. (Until 2026-09-17 this paragraph said no tag had been pushed
yet and the Windows leg had never executed.)

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

## M9 — Export frames as video (user decision 2026-08-27) — **CLOSED 2026-08-28** *(v0.11.0; the phone-side items stay open under CLAUDE.md's tracked decisions)*

A second exit beside Copy Picks: the selection (or the burst under the
cursor) becomes one Motion JPEG `.mov` of the camera's untouched full-res
JPEGs, at the cadence the camera's own millisecond timestamps give, no
options, no crop — the phone editor (InShot) does the rest. Spec:
`modules/video-export.md`; contract: ADR 0004. Chosen after a
three-persona review and a phone test (the untouched 8640×5760 file
imported and played in InShot). DoD: the acceptance criteria pass on BOTH
CI runners (Windows first-class — user requirement), the persona gate
before code, validator + QE with the module's hostile-input list, docs
page `docs/export-video.md` in the same commit as the behaviour, one
README bullet. Explicitly out for a year: any editing surface.


Release notes — everything since v0.4.0, newest first — are in `CHANGELOG.md`
at the repository root (moved there 2026-09-17, brief 007).
