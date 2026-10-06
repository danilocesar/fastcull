# Module spec: app-icon (`assets/icon/`, the application icon)

## Purpose

FastCull's mark is "Hand of three": three polaroid prints fanned like a
hand of cards — the lead upright and sharp at the right, its picture a sky
over ground in the selection blues, the two behind swept back to the left —
on the app's dark rounded plate. This spec owns the asset set under
`assets/icon/`, the script that is its only producer, the metadata-free
rule, the Windows executable's resource icon, the running window's icon and
the README mark. Linux launcher integration — an app-id, a `.desktop`,
hicolor installation — is not here: it waits for packaging (the user,
2026-10-06, brief 013).

## Behaviour

### The asset set

- `assets/icon/` holds exactly these files and nothing else: `fastcull.svg`
  (the master drawing, a 512×512 viewBox, drawn for 48 px and up),
  `fastcull-small.svg` (the 16–32 px drawing; it may be identical to the
  master and is still its own file), `render-icon.sh`,
  `png/fastcull-<N>.png` for N in 16, 22, 24, 32, 48, 64, 128, 256 and 512
  — the hicolor sizes, so the packaging unit finds its files — and
  `fastcull.ico`. No other concept, sheet or render of the three rounds of
  2026-10-05/06 enters the repository (the user, 2026-10-06, brief 013 D1
  and R7).
- The small drawing renders every size up to 32 and the `.ico`'s 20; the
  master renders 48 and up (brief 013 R1).
- Each PNG is N×N, 8-bit RGBA. The `.ico` holds seven members in this
  order — 16, 20, 24, 32, 48, 64 and 256 px — each a 32-bit DIB of its
  stated size; the 20 px render exists for the `.ico` only and is not kept
  as a PNG (brief 013 R1).

### The render script is the only producer

- `render-icon.sh [out-dir]` reads the two SVGs beside itself and writes
  `png/` and `fastcull.ico` into out-dir, which defaults to its own
  directory. It needs ImageMagick 7 (`magick`) with the librsvg delegate
  and python3 for its chunk audit, refuses to run without them, and exits
  non-zero when any output fails the audit below. No rendered file is
  edited by hand or produced by any other tool: a change to a drawing is a
  run of the script and a commit of what it wrote (brief 013 R1).
- The rendering is fixed: rasterise at 384 dpi, resize to 512, Lanczos to
  N, strip, write 32-bit PNG; the `.ico` is assembled from the 16, 20, 24,
  32, 48, 64 and 256 px renders, stripped (senior-developer plan
  2026-10-06).
- The rendered files are committed, because neither CI runner can produce
  them — the Linux image carries no ImageMagick and the Windows image a
  build whose rasteriser is not librsvg (brief 013 R1; senior-developer
  plan 2026-10-06).
- The script's output is byte-deterministic on one seat and one tool
  version, and that is the limit of the reproduction promise: a different
  ImageMagick or librsvg renders different bytes without any regression, so
  the reproduction test compares bytes only on the recorded tool versions
  (Contracts) and passes with a printed reason elsewhere; when the seat's
  tools move, the script is re-run and, if the bytes changed, the renders
  and the recorded versions are committed together, the commit saying so
  (senior-developer plan 2026-10-06).

### Metadata-free

- Every PNG holds exactly the chunks `IHDR`, one or more `IDAT`, `IEND`, in
  that order — no text chunk (`tEXt`, `iTXt`, `zTXt`), no `eXIf`, no `iCCP`,
  `pHYs`, `tIME`, `gAMA`, `cHRM`, `sRGB` or `bKGD`, and no `caBX`, the chunk
  a C2PA manifest lives in (the user, 2026-10-06: "Make sure it's c2pa
  free"; widened to metadata-free by the Manager, brief 013 D2, because the
  chunk rule is the one a test can hold).
- No PNG, and not the `.ico`, contains any of the byte strings `c2pa`,
  `jumb`, `jumd`, `urn:uuid`, `<x:xmpmeta`, `adobe:ns:meta` or `Exif`,
  matched case-sensitively — the canonical spellings every manifest writer
  emits; a case-insensitive scan of compressed pixel data would
  false-positive (senior-developer plan 2026-10-06). An `.ico` member that
  is PNG-encoded is held to the chunk rule as well; ImageMagick writes DIB
  members today.
- The script audits both rules and refuses the render; the repository test
  asserts both over the committed files, so a regenerated set cannot ship
  a manifest unnoticed (brief 013 R2).

### The Windows executable's icon

- On a Windows host building a Windows target, the app crate's `build.rs`
  compiles `assets/icon/fastcull.ico` into `fastcull-app.exe` as icon
  resource 1 — an `RT_GROUP_ICON` with its `RT_ICON` members — through the
  `winresource` crate and the Windows SDK's `rc.exe`. Explorer, the Start
  menu, a pinned taskbar and SmartScreen's dialog read this resource; the
  running window reads the property below (brief 013 R3).
- The crate is a `[target.'cfg(windows)'.build-dependencies]` entry with
  its default features off. Cargo evaluates that `cfg` against the HOST, so
  the Linux build never compiles the crate, and `build.rs` gates the call
  the same way plus on the target; the one case neither covers —
  cross-compiling a Windows exe on Linux — is a build this project does
  not make (senior-developer plan 2026-10-06).
- `winresource` writes a `VERSIONINFO` resource beside the icon whether or
  not one is asked for, so its user-visible strings are chosen rather than
  defaulted: `FileDescription` and `ProductName` are "FastCull" — the
  names Task Manager and Explorer's Details tab show — `FileVersion` and
  `ProductVersion` are the package version as `X.Y.Z.0`, the language is
  en-US (0x0409), and nothing else is set. A dev build's `-devel-…` suffix
  has no place in the numeric fields and stays in `--version` and About
  (senior-developer plan 2026-10-06).
- The compiled resource reaches the link through `cargo:rustc-link-arg`,
  which applies to every link of the package: on Windows the app crate's
  test binaries carry the resource too, harmlessly (senior-developer plan
  2026-10-06).
- `fastcull-cli.exe` carries no icon resource — an iconless CLI reads as
  "not the one you double-click" (persona 2026-10-06, brief 013 R3).
- CI's "Verify Windows artifact" step asserts both: `RT_GROUP_ICON` (type
  14) present at the root of `fastcull-app.exe`'s resource directory and
  absent from `fastcull-cli.exe`'s — the same PE walk as its subsystem
  assertion of issue #40. The two assertions are each other's control: a
  walker that finds icons everywhere fails on the CLI, one that finds none
  fails on the app (brief 013 R4).

### The running window's icon

- `MainWindow.icon` is bound to the 48 px PNG with `@image-url`. The Slint
  compiler embeds the file in the binary and the app decodes it at load;
  no file is read at run time (brief 013 R5).
- The backend hands the OS that one 48×48 bitmap unchanged and the OS
  scales it: on Windows both the title bar's small icon and the taskbar
  button's big icon come from it; on X11 it is the window's `_NET_WM_ICON`;
  on Wayland nothing is drawn — winit's Wayland `set_window_icon` is empty,
  and a dock there needs the app-id and a `.desktop`, deferred with the
  packaging unit (brief 013 R5, D3, D4; the facts below).
- 48 px is the one size bound: the taskbar's size at 150 % and a clean
  3:1 to the title bar's 16 (persona 2026-10-06: a 32 or a 48, never the
  16).

### The README mark

- The README's title row shows `assets/icon/png/fastcull-64.png` at 64×64,
  left of the title and no bigger (persona 2026-10-06, brief 013 R6). The
  release archives copy the README and the image path does not resolve
  inside them, as the screenshot paths already do not (senior-developer
  plan 2026-10-06).

### Slint and winit facts this module depends on

Read 2026-10-06 in Slint 1.17.1 and winit 0.30.13 (the senior-developer
plan); each is in the app crate's version canary (`Cargo.toml`), so an
upgrade re-checks them.

1. The Slint compiler's Rust output embeds every `@image-url` file by
   default (`EmbedAllResources` unless `SLINT_EMBED_RESOURCES` says
   otherwise, `i-slint-compiler/lib.rs` `CompilerConfiguration::new`), and
   `slint-build` emits `cargo:rerun-if-changed` for each embedded file.
2. `i-slint-core` enables the `image` crate's `png` and `jpeg` decoders in
   its own manifest, so the embedded PNG decodes in this app's feature set
   (`default-features = false`, no `image-default-formats`).
3. The winit adapter renders the icon through `render_to_buffer`, which
   returns an embedded raster image AS IS — the 64-logical-px target size
   it passes applies to SVG only (`i-slint-core/graphics/image.rs`,
   `ImageInner::EmbeddedImage`) — and on Windows calls
   `set_taskbar_icon` and `set_window_icon` with that one bitmap
   (`i-slint-backend-winit/winitwindowadapter.rs`, `WinitWindowOrNone::
   set_window_icon`). winit's Windows `set_window_icon` sets `ICON_SMALL`
   only and `set_taskbar_icon` `ICON_BIG`; its Wayland `set_window_icon` is
   an empty function; its X11 one writes `_NET_WM_ICON`.

## Contracts

- The file names, sizes and the `.ico` member order above; icon resource
  id 1; the Windows `VERSIONINFO` strings above.
- `render-icon.sh [out-dir]`: the sources beside the script, `png/` and
  `fastcull.ico` under out-dir, exit non-zero on an audit failure, the
  tool versions printed first.
- The recorded tool versions the reproduction test compares on:
  ImageMagick `7.1.2-27`, quantum `Q16-HDRI`, librsvg `RSVG 2.62.3` — the
  development seat's on 2026-10-06, held as constants in the test and
  moved only together with re-rendered files.
- The tests are `crates/fastcull-app/tests/app_icon.rs`, reading the
  repository from `CARGO_MANIFEST_DIR` two levels up as
  `tests/shortcuts_map.rs` does; their PNG and ICO walkers are test code
  with no runtime consumer and stay out of `fastcull-core` (hard rule 5
  untouched).
- `build.rs`: `embed_windows_icon()` runs before the Slint compile, only
  when the host is Windows and `CARGO_CFG_TARGET_OS` is `windows`, prints
  `cargo:rerun-if-changed` for the `.ico`, and panics with the reason when
  `rc.exe` fails.
- CI: the "Verify Windows artifact" step's Python `resource_types(path)`
  returns the type ids at the root of the resource directory (an empty set
  when the PE has no resource data directory); `14` must be in the app's
  set and not in the CLI's.
- `01-architecture.md` points here in one sentence from its Windows
  subsystems paragraph; `docs/index.md`'s Install section says the Windows
  exe carries the icon.

## Acceptance criteria

`app:` a `crates/fastcull-app/tests/app_icon.rs` test, run by `cargo test
--workspace` on both runners; `ci:` the Windows job's "Verify Windows
artifact" step.

- [ ] **AC1 — the set, reproducible.** `assets/icon/` holds exactly the
      files above, each PNG N×N 8-bit RGBA, the `.ico` its seven members in
      order — app `the_icon_assets_are_exactly_the_spec_set`; the script
      run into a temp dir reproduces every PNG and the `.ico` byte for byte
      on the recorded tool versions, and passes with a printed reason on
      any other seat — app `the_render_script_reproduces_the_committed_renders`
      (never compares on CI: no usable ImageMagick on either runner). Open:
      lands in brief 013's assets commit.
- [ ] **AC2 — metadata-free.** Every committed PNG walks as
      `IHDR`/`IDAT`/`IEND` only, every PNG-encoded `.ico` member too, and no
      file holds a marker — app `every_rendered_icon_file_is_metadata_free`;
      red on a `tEXt` chunk appended to one PNG and on the bytes `c2pa`
      appended to the `.ico`. Open: lands in brief 013's assets commit.
- [ ] **AC3 — the Windows exe carries it.** ci: `RT_GROUP_ICON` present in
      `fastcull-app.exe`, absent in `fastcull-cli.exe`; the walker proved on
      the development seat against the pre-change artifact (both exes
      without any resource — the old red for the app assertion) and an
      iconed third-party executable (the positive). Open: lands in brief
      013's build commit; the Windows runner is its first execution.
- [ ] **AC4 — the window binds it.** `MainWindow`'s block of `main.slint`
      carries the exact `icon: @image-url(…fastcull-48.png)` line and the
      path resolves to the committed file — app
      `the_window_binds_the_48_px_icon`; that the OS receives the bitmap is
      review-verified from the facts above (measurable on an X11 seat:
      `SLINT_BACKEND=winit-x11` under `xvfb-run`, `xprop -name FastCull
      _NET_WM_ICON` reports a 48×48 icon; not a suite test). Open: lands in
      brief 013's build commit.
- [ ] **AC5 — the README mark.** The README's first heading line carries
      the 64 px PNG at 64×64 — app `the_readme_title_row_carries_the_64_px_mark`.
      Open: lands in brief 013's build commit.
- [ ] **AC6 — the specs say so.** This spec, the architecture pointer and
      the docs sentence — review-verified; ticked at the merge.
- [ ] **AC7 — the release.** v0.15.0 is cut after the merge per
      RELEASING.md and its Windows archive's `fastcull-app.exe` shows the
      icon in Explorer, the title bar and the taskbar — the user verifies on
      the Windows machine (the PR's `fastcull-windows-x64` artifact before
      the merge, the release zip after); the CI assertion is the automated
      half. Open until the user has looked.

## History

- 2026-10-06 — brief 013: written by the senior developer from the brief
  and the persona's verdicts; the spec home is this new module rather than
  a section of `01-architecture.md` because the icon has its own asset set,
  producer, tests and CI assertion, and a rule lives in exactly one spec.
  No ADR: not an interface, thread or data-flow change, and the one new
  dependency is Windows-only, build-time and reversible by deletion — the
  reasoning of "Build profiles" in `01-architecture.md`.
