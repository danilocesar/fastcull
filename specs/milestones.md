# Milestones

Each milestone lands as one or more commits with green CI. Definition of done (DoD)
includes the listed spec acceptance criteria turning into passing tests.

## M0 — Scaffold ✔ (this commit series)
Repo, GPL-3.0, workspace (core/cli/app), specs tree, CLAUDE.md, testdata fetcher,
Linux+Windows CI. DoD: `cargo test`/`clippy -D warnings` green on both OSes.

## M1 — Core pipeline
catalog scan, raw extraction (incl. A1 full-res extractor), pipeline priority pool,
SQLite cache, EXIF read. `fastcull-cli scan|thumbs` subcommands. Criterion benches
with thresholds. DoD: raw-pipeline + catalog-cache acceptance criteria pass against
the 3 real A1 files.

## M2 — Grid UI (prototype risk first)
Slint window, windowed-model virtualized grid, zoom column steps, progressive
loading, keyboard navigation, placeholder/badge visuals. DoD: 2,000-file synthetic
folder scrolls smoothly; screenshot smoke tests. **Start with the windowed-model
spike — if Slint can't hit 60 fps here, escalate to the user before proceeding.**

## M3 — Culling
Pick/reject session state, sidecar writer thread, XMP serializer. `fastcull-cli
cull` for scripted marking. DoD: xmp-sidecars acceptance criteria incl. sandboxed
darktable-cli round-trip.

## M4 — Loupe
Fit + 1:1 zoom, pan, DCT-scaled fit decode, ±2 prefetch, auto-advance on mark.

## M5 — IPTC
Panel, multi-select apply, templates + variables, config persistence.

## M6 — Filter/sort + bursts
filter.rs predicates + bar UI; burst.rs grouping + border rendering.

## M7 — Copy picks + packaging
fileops plan/execute + dialog + report. cargo-dist packaging: Linux AppImage,
Windows zip/MSI. DoD: full manual acceptance script in specs/00-overview terms:
open 2,000 A1 files → cull → IPTC → copy → darktable sees everything.
