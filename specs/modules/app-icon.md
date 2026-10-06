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
  and python3 for its chunk audit, refuses to run without them (exit 2,
  naming what is missing), and exits non-zero when any output fails the
  audit below. A unix seat where the script refuses is red in every test
  that runs it: a refusal is never a mismatch and never a skip — the
  reproduction test skips only where `magick` is absent or lists no
  librsvg, and a seat with both but no python3 is one the maintainer
  completes, not one the tests excuse (Manager 2026-10-06, QE Q2). The
  probe-race test alone needs bash only: its stand-in folder carries a
  stand-in `python3`, since the script stops at the stand-in's render
  refusal before any audit runs (QE 2026-10-06, D3). No rendered file is
  edited by hand or produced by any other tool: a change to a drawing is a
  run of the script and a commit of what it wrote (brief 013 R1).
- The rendering is fixed: the script hands each SVG to ImageMagick as
  `RSVG:<file>`, naming the librsvg coder, and rasterises at 384 dpi,
  resizes to 512, Lanczos to N, strips and writes a 32-bit PNG; the `.ico`
  is assembled from the 16, 20, 24, 32, 48, 64 and 256 px renders, stripped
  (senior-developer plan 2026-10-06; corrected 2026-10-06, QE D1: it handed
  the SVG over by its name alone, and ImageMagick's SVG reader first runs
  the external `svg:decode` delegate its `delegates.xml` names — Inkscape,
  on Fedora's — and falls back to librsvg only when that command is absent
  or fails, so a seat with Inkscape rendered other bytes while the script
  still reported librsvg's version; named `RSVG:`, the delegate is never
  consulted, and the probe-race and reproduction tests keep their
  meaning).
- The rendered files are committed, because neither CI runner can produce
  them — the Linux image carries no ImageMagick, and the script does not
  run on Windows (Contracts) (brief 013 R1; senior-developer plan
  2026-10-06; corrected 2026-10-06, review F1 fix: it said the Windows
  image's ImageMagick is a build whose rasteriser is not librsvg, but that
  image installs ImageMagick's official Windows build, which bundles its
  own librsvg (2.40.20) — read from the image's toolset and ImageMagick's
  Windows dependency list, not measured on the runner).
- The script's output is byte-deterministic on one seat and one set of
  tools, and that is the limit of the reproduction promise. The bytes are
  shaped by the whole chain, not by ImageMagick alone: the rasteriser
  (librsvg with its cairo and pixman) fixes the pixels, and the PNG encoder
  (ImageMagick's libpng and the zlib under it) fixes how they are written;
  a move in any of them may change bytes without any regression. The
  reproduction test runs the script on any unix seat whose `magick` lists
  the librsvg delegate and compares the bytes: a match is green on any
  versions; a mismatch is red on the recorded tool versions (Contracts —
  the versions `magick` itself reports; zlib reports nothing and is the one
  link it cannot name) and passes with a printed reason on any other, where
  the tool is as likely as the drawing to be the cause. The red says which
  of two things it found, because the test decodes every differing PNG and
  compares the pixels: pixels that differ mean the drawing changed without
  a re-render, a render was edited by hand, or a rasteriser library moved
  under the recorded versions; pixels that are identical mean only the
  encoding changed — zlib moved, or a file was re-encoded by another tool —
  and the remedy for both is a re-render committed with the reason, the
  recorded versions moving only when `magick`'s own report moved (the
  `.ico` holds uncompressed members, so a difference there is never the
  encoder). When the development seat's tools move, the recorded versions
  move to the seat's in the same commit — with the re-rendered files when
  the bytes changed, alone when they did not — so that the one seat that
  can compare keeps comparing, the commit saying so (senior-developer plan
  2026-10-06; corrected 2026-10-06, senior-developer review F1: it said a
  different version "renders different bytes" and moved the recorded
  versions only with re-rendered files, which left the test comparing on no
  seat once the seat's ImageMagick moved from 7.1.2-27 to 7.1.2-32 with
  every byte unchanged; limited to unix seats (developer 2026-10-06, F1
  fix; the reason in Contracts); corrected 2026-10-06, QE D2: it named
  ImageMagick and librsvg as the whole of what fixes the bytes and recorded
  only those, while a zlib change under the same versions rewrote bytes
  with every pixel unchanged, so the red would have blamed the drawing —
  libpng is now recorded and the red reads the pixels before it speaks).

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
- The two SVG sources are bare drawings too: no `<metadata>`, `<title>`,
  `<desc>` or XML comment (the places a name or a manifest is typed into
  an SVG), no `<image>`, `href=`, `<script>` or `<foreignObject>` (an
  external or hidden payload), none of the markers above, and the root
  `viewBox` is `0 0 512 512` (the user, 2026-10-06, "c2pa free", and M7;
  widened to the sources by the Manager 2026-10-06, QE Q1, because a source
  carrying any of these would render it, or carry it into the repository,
  unnoticed).
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
  names Task Manager and Explorer's Details tab show — the `FileVersion`
  and `ProductVersion` strings are the package version as written
  (`X.Y.Z`) and the numeric `FILEVERSION` and `PRODUCTVERSION` fields the
  same version as `X.Y.Z.0`, both the crate's defaults; the language is
  en-US (0x0409), and no other string is set. A dev build's `-devel-…`
  suffix has no place in the numeric fields and stays in `--version` and
  About (senior-developer plan 2026-10-06; corrected 2026-10-06, brief
  013's build commit: it gave the two strings as `X.Y.Z.0`, which only
  the numeric fields carry in the resource script winresource 0.1.31
  writes, and read "nothing else is set" where the crate also writes its
  fixed-info defaults).
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
- The backend hands the OS that one 48×48 bitmap with its colour channels
  premultiplied by alpha twice — Slint decodes the PNG into a premultiplied
  buffer and its winit adapter premultiplies again before handing winit
  straight RGBA — and the OS scales it: on Windows both the title bar's
  small icon and the taskbar button's big icon come from it; on X11 it is
  the window's `_NET_WM_ICON`; on Wayland nothing is drawn — winit's
  Wayland `set_window_icon` is empty, and a dock there needs the app-id and
  a `.desktop`, deferred with the packaging unit. The double premultiply
  darkens only pixels that are neither opaque nor fully transparent — the
  anti-aliased rim of the plate's rounded corners — and is invisible at the
  sizes the OS draws; it is Slint's to fix, not ours (hard rule 2), and an
  upgrade re-checks fact 3 (brief 013 R5, D3, D4; the facts below;
  corrected 2026-10-06, QE D5: it said "unchanged", which the second
  premultiply makes false).
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
   returns an embedded raster image at its own size — the 64-logical-px
   target it passes applies to SVG only (`i-slint-core/graphics/image.rs`,
   `ImageInner::EmbeddedImage`) — as the premultiplied buffer the decoder
   made of it (`dynamic_image_to_shared_image_buffer`, same file);
   `icon_to_winit` premultiplies that buffer a second time before
   `winit::window::Icon::from_rgba`, which takes straight RGBA
   (`i-slint-backend-winit/winitwindowadapter.rs`), and on Windows calls
   `set_taskbar_icon` and `set_window_icon` with that one bitmap
   (`WinitWindowOrNone::set_window_icon`). winit's Windows
   `set_window_icon` sets `ICON_SMALL` only and `set_taskbar_icon`
   `ICON_BIG`; its Wayland `set_window_icon` is an empty function; its X11
   one writes `_NET_WM_ICON` (corrected 2026-10-06, QE D5).

## Contracts

- The file names, sizes and the `.ico` member order above; icon resource
  id 1; the Windows `VERSIONINFO` strings above.
- `render-icon.sh [out-dir]`: the sources beside the script, `png/` and
  `fastcull.ico` under out-dir, exit non-zero on an audit failure, the
  tool versions printed first.
- The recorded tool versions a reproduction mismatch is red on:
  ImageMagick `7.1.2-32`, quantum `Q16-HDRI`, librsvg `RSVG 2.62.3`,
  libpng `libpng 1.6.58` — the versions `magick -version` and `magick -list
  format` report, the development seat's on 2026-10-06, held as constants
  in the test and moved whenever the development seat's tools move
  (Behaviour); zlib is reported nowhere and is named only by the red
  message. Every reason the reproduction test prints when it passes
  without a verdict — a skip, or a mismatch off these versions — goes to
  stderr (visible under `--nocapture`).
- The tests that run the script are unix-only, each for its own reason.
  The probe-race, delegate and audit tests are `#[cfg(unix)]` because they
  cannot compile elsewhere: their stand-ins are executable shell scripts
  made with `std::os::unix::fs::PermissionsExt` and put first on a
  `:`-joined PATH. The reproduction test compiles everywhere and on
  Windows passes with its printed reason before anything runs, because it
  gives its child no PATH of its own, and Rust's program search for such a
  child looks in the executable's directory, then System32 — where WSL
  puts its `bash.exe` — then the Windows directory, and only then in the
  parent's PATH, where Git Bash would be (`std`'s
  `sys/process/windows.rs`, `search_paths`, Rust 1.99.0; a child given its
  own PATH is searched there first). The script is the Linux development
  seat's maintainer tool either way (corrected 2026-10-06, review N1 with
  QE's reading of `search_paths`: it gave Git Bash's handling of
  extension-less files as the reason, a claim never measured).
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

- [x] **AC1 — the set, reproducible.** `assets/icon/` holds exactly the
      files above, each PNG N×N 8-bit RGBA, the `.ico` its seven 32-bit DIB
      members in order, each `.ico` member its exact DIB length and, where a
      PNG of its size exists, its pixels — app
      `the_icon_assets_are_exactly_the_spec_set`; the
      script run into a temp dir reproduces every PNG and the `.ico` byte
      for byte, a mismatch red on the recorded tool versions and passing
      with a printed reason on any other — app
      `the_render_script_reproduces_the_committed_renders` (never compares
      on CI: no ImageMagick on the Linux runner, and the script does not run
      on Windows); its librsvg probe never refuses a seat that has the
      delegate — app
      `the_render_script_reads_the_whole_format_list_before_probing_for_librsvg`
      (unix only, Contracts); the render names the librsvg coder so no
      external SVG delegate is consulted — app
      `the_render_names_the_librsvg_coder_and_never_runs_an_svg_delegate`
      (unix only; compares on the development seat); the script's audit
      refuses a text chunk and a marker — app
      `the_render_scripts_audit_refuses_a_text_chunk_and_a_marker` (unix
      only).
- [x] **AC2 — metadata-free.** Every committed PNG walks as
      `IHDR`/`IDAT`/`IEND` only, every chunk's CRC verified, every
      PNG-encoded `.ico` member too, and no file holds a marker — app
      `every_rendered_icon_file_is_metadata_free`; red on a `tEXt` chunk
      appended to one PNG and on the bytes `c2pa` appended to the `.ico`;
      and both SVG sources bare drawings — app
      `the_svg_sources_are_bare_drawings`.
- [x] **AC3 — the Windows exe carries it.** ci: `RT_GROUP_ICON` present in
      `fastcull-app.exe`, absent in `fastcull-cli.exe`; the walker proved on
      the development seat against the pre-change artifact (both exes
      without any resource — the old red for the app assertion) and an
      iconed third-party executable (the positive).
- [x] **AC4 — the window binds it.** `MainWindow`'s block of `main.slint`
      carries the exact `icon: @image-url(…fastcull-48.png)` line at
      MainWindow's own depth, outside a comment, and the path resolves to
      the committed file — app
      `the_window_binds_the_48_px_icon`; that the OS receives the bitmap is
      review-verified from the facts above (measurable on an X11 seat:
      under `xvfb-run` with `WAYLAND_DISPLAY` unset, `xprop -name FastCull
      _NET_WM_ICON` reports one 48×48 icon; not a suite test; corrected
      2026-10-06, brief 013's build commit: it said `SLINT_BACKEND=winit-x11`,
      which Slint 1.17.1 parses as an unknown renderer named `x11`, while
      winit 0.30.13 takes Wayland whenever `WAYLAND_DISPLAY` is set).
- [x] **AC5 — the README mark.** The README's first heading line carries
      the 64 px PNG at 64×64, left of the title text, outside an HTML
      comment — app `the_readme_title_row_carries_the_64_px_mark`.
- [ ] **AC6 — the specs say so.** This spec, the architecture pointer and
      the docs sentence — review-verified; ticked at the merge.
- [ ] **AC7 — the release.** v0.15.0 is cut after the merge per
      RELEASING.md and its Windows archive's `fastcull-app.exe` shows the
      icon in Explorer, the title bar and the taskbar — the user verifies on
      the Windows machine (the PR's `fastcull-windows-x64` artifact before
      the merge, the release zip after); the CI assertion is the automated
      half. Open until the user has looked.

## History

- 2026-10-06 — brief 013, QE's fix commit (QE D1–D5, Q1, Q2; review N1):
  the script names the librsvg coder — ImageMagick's `svg:decode` delegate
  ran first and the recorded versions could not see it; the reproduction
  red reads the pixels and records libpng; the sources are under the
  metadata-free rule; the race test depends on bash alone; the `.ico`
  members' lengths and pixels, every chunk's CRC, the binding's depth, the
  README mark's position and the script's audit are pinned; the window
  icon's double premultiply recorded; the unix-only reasons corrected.
- 2026-10-06 — brief 013, the review's fix commit (senior-developer review
  F1, F2, F3, F7; D9): the reproduction test compares first and its
  recorded versions moved to 7.1.2-32 with every byte unchanged; it and the
  new probe-race test run the script on unix only; the `.ico` members' own
  headers are pinned; the premise that the Windows image's ImageMagick has
  no librsvg corrected in place.
- 2026-10-06 — brief 013, the build commit: the window's binding, the
  exe's resource icon with CI's assertion, and the README mark landed;
  AC3, AC4 and AC5 ticked beside their tests; the `VERSIONINFO` sentence
  and AC4's X11 recipe corrected in place, each saying what was wrong.
- 2026-10-06 — brief 013, the assets commit: the asset set, its render
  script and `tests/app_icon.rs` landed; AC1 and AC2 ticked beside their
  tests.
- 2026-10-06 — brief 013: written by the senior developer from the brief
  and the persona's verdicts; the spec home is this new module rather than
  a section of `01-architecture.md` because the icon has its own asset set,
  producer, tests and CI assertion, and a rule lives in exactly one spec.
  No ADR: not an interface, thread or data-flow change, and the one new
  dependency is Windows-only, build-time and reversible by deletion — the
  reasoning of "Build profiles" in `01-architecture.md`.
