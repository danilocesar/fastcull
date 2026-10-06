# Brief 013 — The application icon

Dated 2026-10-06. Work branch `app-icon`, cut from `origin/main` at
`55927b3` (M12). Classified: feature, user-visible.

## Context

FastCull has shipped fourteen releases without an icon: the Windows exe
carries the generic executable tile in Explorer, the Start menu and a
pinned taskbar; the window shows no icon in its title bar; the README has
no mark. The user asked for one on 2026-10-05 ("I'm looking for an icon
for the application"). Three rounds of concepts were drawn, rendered at
16 px and judged (round 1: nine marks — badges, a lettermark, abstract
frames, a keycap — rejected by the user: "None of them are good";
round 2: twelve "photos moving fast, like polaroid printouts"; round 3:
thirty-nine ideas from web research into the language of fast culling and
fast photography). The user chose round 2's **"Hand of three"**
(2026-10-06): three polaroid prints fanned like cards held in a hand, the
lead print upright and sharp at the right with a sky/ground picture in
the selection blues, the two behind swept back to the left — the swept
prints are where the lead was a moment ago — on the app's dark rounded
plate. A refinement pass (three designers, a craft judge) produced the
master drawing for 48 px and up and a small-size drawing for 16–32 px.
The user then asked (2026-10-06): "create an assets folder and render
the svg on it. Make sure it's c2pa free. Then add it to the repository
(drop all the rejected ones) and generate a new release, make sure the
windows binary uses the new icon."

## Goals

- The chosen icon lives in the repository as SVG sources plus the
  rendered PNG set and the Windows `.ico`, with the script that renders
  them, and nothing else of the three rounds.
- Every rendered file is metadata-free: no C2PA/JUMBF manifest, no XMP,
  no EXIF, no text chunks — audited by the render script and pinned by a
  test.
- `fastcull-app.exe` carries the icon as its resource icon, so Explorer,
  the Start menu and a pinned taskbar show it; CI's Windows artifact
  check asserts the resource is there.
- The running window shows the icon where the platform draws
  `Window.icon` (Windows title bar and taskbar button; X11).
- The README shows the mark beside its title.
- A release (v0.15.0) ships it, with the Windows archive's exe iconed.

## Non-goals

- Linux launcher integration — an XDG app-id, a `.desktop` file, hicolor
  installation, a launcher script (the user, 2026-10-06: "Don't bother
  any Linux yet. Once we get into packaging we will deal with it"). The
  PNG set is rendered at the hicolor sizes so that unit has its files.
- A DEV badge on untagged builds (the user, 2026-10-06: skip).
- The icon in the About dialog, a symbolic/monochrome variant, a
  favicon, an icon for `fastcull-cli`, taskbar progress, and the title
  bar's text (`<folder> — FastCull`, a persona suggestion recorded as a
  follow-up candidate).
- Any change to the drawing itself beyond the refinement pass's finals.

## Requirements

- R1. **The assets folder.** `assets/icon/` holds `fastcull.svg` (the
  master, 512×512 viewBox, draws 48 px and up), `fastcull-small.svg` (the
  16–32 px drawing; may be identical to the master), `render-icon.sh`
  (ImageMagick 7 with librsvg; renders and audits), `png/fastcull-<N>.png`
  for N in 16, 22, 24, 32, 48, 64, 128, 256, 512 (the small drawing up to
  32, the master from 48) and `fastcull.ico` holding 16, 20, 24, 32, 48,
  64 and 256 px. The rendered files are committed (CI has no ImageMagick);
  the script is the only way they are produced — pixel for pixel; bytes
  follow the encoder (D11).
- R2. **Metadata-free.** Every PNG holds exactly the chunks `IHDR`, `IDAT`
  and `IEND`; no PNG or the `.ico` contains a C2PA, JUMBF (`jumb`),
  `urn:uuid`, XMP (`x:xmpmeta`, `adobe:ns:meta`) or `Exif` marker. The
  render script audits this and exits non-zero on a violation; a test in
  the repository asserts the same over the committed files, so a
  regenerated set cannot ship a manifest unnoticed.
- R3. **The Windows resource icon.** On `target_os = "windows"` the app
  crate's `build.rs` embeds `assets/icon/fastcull.ico` as the executable's
  icon resource (`winresource` or equivalent; a build dependency, used
  only on Windows); the Linux build is untouched. `fastcull-cli.exe` gets
  no icon (persona: an iconless CLI reads as "not the one you
  double-click").
- R4. **CI asserts it.** The "Verify Windows artifact" step of `ci.yml`
  (and so every CI artifact — not the release: `release.yml` is
  dist-generated and runs no artifact check, so v0.15.0's zip is parsed
  by hand after the release; this sentence said "every release" until
  2026-10-06, QE round 1 D6) additionally asserts that
  `fastcull-app.exe` contains an icon resource (`RT_GROUP_ICON`) and that
  `fastcull-cli.exe` does not — the same shape as its PE-subsystem
  assertion (issue #40).
- R5. **The window icon.** `MainWindow` sets `icon` from the 48 px PNG
  (`@image-url`, embedded at compile time), so the Windows title bar and
  taskbar button, and X11 window managers, show the mark while the app
  runs. Known and accepted: Wayland draws nothing from this property
  (winit's Wayland `set_window_icon` is empty; the dock there needs the
  app-id and a `.desktop`, which the non-goal defers).
- R6. **The README.** The README's title row shows `assets/icon/png/
  fastcull-64.png` at 64 px, left of the title, no bigger (persona).
- R7. **Only the chosen one.** No other concept, render or sheet from the
  three rounds enters the repository; the canvas and the scratch tree
  keep them.
- R8. **The specs say so.** A module spec owns the rules above (the senior
  developer places it — `specs/modules/app-icon.md` is the proposal);
  `docs/index.md`'s Install section says the Windows exe is iconed where
  it describes the zip; 01-architecture.md's app-crate section points at
  the spec in one sentence.

## Acceptance criteria

- AC1. `assets/icon/` holds exactly the files R1 lists; `render-icon.sh`
  run on the sources reproduces the committed PNGs pixel for pixel and the
  `.ico` byte for byte (D11) (the test: regenerate into a temp dir on a
  seat with ImageMagick and compare; skipped with a stated reason where
  `magick` is absent, never on CI's Linux runner if ImageMagick is
  installed there).
- AC2. A test walks every committed PNG's chunks and asserts
  `IHDR`/`IDAT`/`IEND` only, and scans every PNG and the `.ico` for the
  markers R2 names; a mutant that appends a `tEXt` chunk to one PNG turns
  it red.
- AC3. The Windows build embeds the icon: CI's artifact check asserts
  `RT_GROUP_ICON` present in `fastcull-app.exe` and absent in
  `fastcull-cli.exe`; a mutant that drops the `build.rs` embed is red on
  the Windows runner.
- AC4. `MainWindow.icon` is bound to the 48 px PNG (a grep-level test in
  the app crate, the same way the shortcuts parity test reads the
  `.slint`).
- AC5. The README's first heading row carries the 64 px PNG.
- AC6. The specs say so (R8), ticked at the merge.
- AC7. Release v0.15.0 is cut after the merge per RELEASING.md and its
  Windows archive's `fastcull-app.exe` shows the icon in Explorer (the
  user verifies on the Windows machine; the CI assertion is the
  automated half).

## Applicable directives

Hard rules 4 (every step a proper commit), 5 (no logic in the app crate —
a build script and a property binding are not logic), 6 (perf budgets
untouched: no runtime work). ADR 0004 (derived outputs: the rendered
files are derived from the SVG by a committed script). M1 (spec first),
M7 (never name the user — the README credit is the one exception; the
icon assets carry no author), M9 (clean before the unit: 21.8 GB freed),
M11 (nothing camera-specific here), M12 (branch from `origin/main`).
RELEASING.md for AC7. The gate's rule on test changes.

## Persona verdicts (almost-human-user, 2026-10-06)

Title bar/taskbar while running: USEFUL on Windows, SHRUG on Linux as
proposed (Wayland draws nothing from `Window.icon`). Windows `.ico` in the
exe: MUST-HAVE ("a generic exe icon beside a SmartScreen warning looks
like malware"; full ladder, not just 16). Linux launcher files: USEFUL,
the app-id MUST-HAVE for any Linux surface — deferred by the user.
About: SHRUG. README header: USEFUL (64 px, no bigger). CLI: none,
deliberately. Not listed: taskbar progress (later), DEV badge (asked,
declined), symbolic icon (don't draw it), favicon (no site), the title
bar's text (follow-up), dark docks need the white print border to carry
the silhouette (the refinement pass's job).

## Open questions

- Linux launch habit → the user: "Don't bother any Linux yet. Once we
  get into packaging we will deal with it." (2026-10-06) — answered, the
  non-goal.
- DEV badge on dev builds → the user: skip (2026-10-06) — answered.

## Decisions log

- D1 (2026-10-06, the user): the mark is "Hand of three" from round 2;
  the other 59 concepts are not added to the repository.
- D2 (2026-10-06, the user): the rendered assets must be free of C2PA
  manifests; the Manager widens this to metadata-free (no XMP, EXIF or
  text chunks either), since a chunk audit that allows only
  `IHDR`/`IDAT`/`IEND` is the one that can be tested.
- D3 (2026-10-06, the user): Linux integration waits for packaging; the
  Windows exe icon is the deliverable to verify.
- D4 (2026-10-06, Manager, M2): the PNG set is still rendered at the
  hicolor sizes, the window icon is still bound (it costs one property
  and shows on Windows and X11), and the README gets the mark — each a
  best-practice choice the persona rated USEFUL and none of them Linux
  packaging.
- D5 (2026-10-06, Manager): the release after the merge is v0.15.0
  (146 commits since v0.14.0: the Settings dialog, its card, the Failed
  badge tooltip, the dialogs' Tab rings, the failed-cursor fix, the loupe
  ring budget, the icon).
- D6 (2026-10-06, Manager, M2, on the senior developer's Q1): `winresource`
  writes a `VERSIONINFO` resource whether asked or not, so its strings are
  chosen: `FileDescription` and `ProductName` "FastCull", the numeric
  versions from `CARGO_PKG_VERSION`, en-US — the name Task Manager and
  Explorer's Details tab show. The crate's default would be the package
  name `fastcull-app`.
- D7 (2026-10-06, Manager, M3, on the senior developer's Q2):
  `THIRD-PARTY-LICENSES.md` is not regenerated in this unit — a
  build-dependency never links into the executable — and its drift since
  2026-07-25 (CODE-AUDIT C1) is the Manager's own bookkeeping: regenerated
  with `cargo about` in the release commit of v0.15.0, as RELEASING.md's
  archive promise requires.
- D8 (2026-10-06, Manager, on the senior developer's Q3): AC7's human half
  is asked of the user BEFORE the merge, on the PR's `fastcull-windows-x64`
  artifact (Explorer, the title bar, the taskbar while it runs), so that a
  soft or generic running-window icon is fixed in this PR — the Windows-
  only fallback that loads the window icon from the exe's own `.ico` —
  rather than after v0.15.0.
- D9 (2026-10-06, Manager, M3, on the senior developer's review F1; the
  unix limit approved in the senior developer's test-integrity verdict the
  same day): AC1's reproduction test compares first. It runs the script on
  any unix seat whose `magick` lists the librsvg delegate; a match is green
  on any tool versions, a mismatch red on the recorded versions and
  passing with a printed reason on any other; the recorded versions follow
  the development seat's tools whenever they move — to 7.1.2-32 in this
  round, every byte unchanged — so the one seat that can compare keeps
  comparing (the versions-first gate had compared nowhere since the seat's
  ImageMagick moved from 7.1.2-27). It does not run on Windows: the runner
  image installs ImageMagick's official Windows build, which by its
  dependency list bundles its own librsvg (not measured on the runner;
  review N2), and Rust's program search there finds WSL's `bash.exe`
  before Git Bash, so a delegate check alone would have run the script
  where it cannot succeed.
- D10 (2026-10-06, Manager, M3, on QE D1–D5 and Q1–Q3): the render names
  the librsvg coder (`RSVG:`); the SVG sources are under the metadata-free
  rule, `<title>`, `<desc>` and XML comments included (M7); a unix seat
  where the script refuses stays red; the reproduction promise was put to
  the user, and libpng is recorded; the CI walker asserts VERSIONINFO and
  that the CLI carries no resource at all; the window icon's double
  premultiply is recorded as Slint's, relayed to the user under hard
  rule 2.
- D11 (2026-10-06, the user): the pixel promise — option B. A byte
  mismatch whose pixels are identical passes with a printed reason naming
  the encoder; a pixel difference is red. The senior developer's refusal of
  the pass-with-reason stood until the user, as the customer, chose it with
  the trade-off stated (a pixel-identical re-encode by another tool is not
  noticed).
- D12 (2026-10-06, Manager, the record of the gate): the senior
  developer's review of the three implementation commits was
  CHANGES_REQUESTED (F1 major: the reproduction guard dead after the
  seat's ImageMagick moved 7.1.2-27 → 7.1.2-32 with every byte unchanged;
  F2 minor: the SIGPIPE race fix without its regression test; nits
  F3–F7); the fix `b7aad8c` was re-reviewed APPROVED; QE round 1 was FAIL
  (D1 major: the script handed the SVG to ImageMagick unnamed and the
  `svg:decode` Inkscape delegate runs first; minors D2–D6; proposals
  TP1–TP10); the senior developer's test-integrity review approved every
  proposal in an exact shape and refused TP2 as a pass-with-reason, a
  refusal the user then overrode as the customer (D11, option B); the
  developer's five commits `f29c231`, `23f0a95`, `571efa9`, `568b6d7`,
  `c090363` were re-reviewed APPROVED (nits N1–N3) and QE round 2 was
  PASS with three minors (D7: three PNGs never decoded on CI; D8: the
  SVG rule misses editor namespaces, hidden text and entities; D9: CI
  proves an icon, not this icon) and proposals TP11–TP13. Deferred, all
  six, to issue #112 — CI-depth improvements on guards the development
  seat already holds, none a defect in what ships — so v0.15.0 ships from
  the passing PR; the senior developer's nits N1–N3 go with them. The
  Windows artifact's `.rsrc` section is byte-identical across the PR's
  three builds (QE), so the user's D8 look at any of them stands for the
  head. CI at `c090363`: both runners green (run 37526836736).
