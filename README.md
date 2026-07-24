# FastCull

**Fast, open-source photo culling for working photographers.** A Photo Mechanic-class
ingest-and-select tool for Linux and Windows: open a folder of thousands of 100 MB RAW
files, see them instantly, pick the keepers, tag them with IPTC metadata, and hand the
selects to [darktable](https://www.darktable.org/) with every rating and tag intact.

## Why it's fast

FastCull never decodes RAW sensor data on the interactive path. Every modern RAW file
embeds camera-rendered JPEG previews — the Sony A1, for example, embeds a full-resolution
8640×5760 JPEG plus a 1616×1080 preview in every ARW. FastCull reads only those bytes
(~0.5 MB of a 100 MB file for the grid) and decodes them on all cores.

Measured on the reference machine (32-thread Ryzen, real A1 files): ~300 files/sec for
the thumbnail grid pipeline vs. 0.6–1.2 **seconds** per file for a full RAW decode —
the embedded-preview strategy is two orders of magnitude faster.

## Core features (v1)

- Catalog-free: open any folder, cull, done. No import step, no database to manage.
- Zoomable grid: many columns → few → single-image loupe → 1:1, one continuous gesture.
- Keyboard-first pick/reject culling.
- Burst detection: frames from the same burst get a colored border.
- IPTC metadata editing — single image or multi-select — with saved templates and
  variables (`{date}`, `{seq}`, …).
- Filter and sort: picked / rejected / unmarked / bursts, by time or name.
- Copy picks to a destination folder with rename templates; XMP sidecars travel along.
- Darktable-compatible by construction: all state is written to `<name>.<ext>.xmp`
  sidecars using the fields darktable, digiKam, and Lightroom read. RAW files are
  **never** modified.

## Camera support

Any camera supported by [rawler](https://github.com/dnglab/dnglab) works; the
**Sony A1 is the reference camera** and is covered by the test suite with real files
(compressed, lossless-compressed, and uncompressed ARW).

## Stack

Rust workspace: `fastcull-core` (all logic, UI-free, fully tested), `fastcull-app`
([Slint](https://slint.dev/) GPU-rendered UI), `fastcull-cli` (headless driver used by
integration tests). See `specs/` for the full architecture and module specifications —
this project is developed spec-first.

## Building

```sh
cargo build --release
cargo test --workspace
testdata/fetch.sh   # downloads sample RAWs (needed for integration tests)
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
