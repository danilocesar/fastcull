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

## v0.7.0 (released 2026-08-02)

Everything since v0.6.0. The headline is the **transit quality model** —
the user's requirement verbatim: "I don't need the image to be as good
as possible, I need it to move fast, feeling almost like a video. But
when I release the key, then I want quality to be high." The loupe used
to request the full-res rung for every frame even under a held key
(~390 ms decode vs ~120 ms repeat: two of three frames never seen).
Three request states now govern what is ASKED of the decoder, never
what is displayed: TRANSIT (frame changes < 250 ms apart — mid rung
only, over a wide ring leaning the direction of travel), SETTLED
(150 ms of quiet — the real target), then the pre-existing full-res
look-ahead. Measured on the 8-core dev machine, interleaved A/B vs the
old code: frames reaching the screen during a 20-key hold 14 → 37; CPU
during a 30 s hold 59.9 s → 3.1 s; grid thumbnails after exiting a hold
mid-flight 3.5–4.1 s → 15 ms; 3-minute-marathon peak RSS 2.37 GB →
517 MB; fast-cull chains stop producing BLANK frames (the old code left
2–5 of 20 undecoded above ~6 marks/s). Accepted cost, chosen
motion-first: sharpness-on-stop 710 → 840 ms median. The gate caught a
real bug before merge (the prefetch ring leaned FORWARD during a
backward hold — the app's same-index re-focus storm re-derived the
direction every call; now latched at the real index change) and two
review rounds re-measured every published number. The fast-Y/N
deferral is closed as intended, with data: at 4 marks/s nothing
changes; only above ~4.2/s do frames get judged from the mid — where
the old code judged them from nothing.

Also: **dark-only means dark-only** — on a light-mode desktop the
menu bar's labels were invisible (native fluent MenuBar text follows
the platform colour scheme; the app's surfaces are hand-picked dark).
As old as the menu bar itself, surfaced by the user's desktop theme.
The palette is now pinned dark at the root window (user decision: "I
don't want a light mode. I don't want a toggle. Keep the design as
is"), with a deterministic regression test that forces the failing
scheme via an unreachable session bus — QE proved the pin holds even
across a LIVE mid-session theme flip, and that the screenshot suite
had been silently capturing whichever scheme the desktop happened to
be in (the real blind spot behind two different one-off Windows CI
reds, both also fixed: the loupe-fit shutter now waits for the
mid-or-better texture instead of a 1.5 s clock).

## v0.6.0 (released 2026-07-31)

Everything since v0.5.0 — two issues, both of which turned out to be
bigger than their titles.

- **#25: the load stops moving the photo under your hands.** EXIF is read
  inside the per-file THUMBNAIL job, so the metadata sweep runs for the
  whole load (~15 s for 3,000 files locally, far longer off a card), and
  for all of it the capture sort put keyed images ahead of keyless ones —
  so the view order, and the head with it, changed on almost every
  arrival whenever filename order ran contrary to capture order. The
  issue called this "cursor lands off-by-N". Measured, one `right` at
  1 ms landed 870 frames away; worse, MARKS write to the cursor, and a
  `Y` typed 4 s after opening wrote the sidecar for a file the user had
  never seen. The view now holds FILENAME order until every job finishes,
  then sorts once. Per the user's decision, whatever is selected stays
  selected through that flip and every engine event after it — which
  narrows issue #4 knowingly: an untouched cursor can end up mid-grid
  rather than at the start of the shoot.
- **#26: a dev build's version says how old it is.**
  `X.Y.Z-devel-YYYYMMDD-<hash>`, using the commit date so the string is
  reproducible. The fix that matters most is smaller than the feature:
  `build.rs` did not watch tag refs, so `git tag && cargo build` left a
  `-devel-` string inside a release binary — which happened at 0.5.0 and
  made every version string suspect.

Also: `loupe_survives_a_vertical_resize` was flaking 4-in-20 on main and
is fixed; the About card holds its extra line; and a `rerun-if-changed`
path that did not exist was costing ~4.7 s on every no-op build.

## v0.5.0 (released 2026-07-30)

Everything since v0.4.0. The selection wash (a multi-selection is
readable at a glance, and the status bar states its size — the IPTC
panel stamps that selection, so its reach had to be visible), and the
issue #11 closure, which turned out to be two real defects rather than
a sign-off:

- **Double-click never reached 1:1 above fit** — the headline gesture of
  the pointer contract, dead since it shipped. The bridge's proximity
  guard compared two clicks as image fractions taken either side of the
  first click's re-centre, so the "distance" it measured was the click's
  own offset from the view centre; anything beyond ~12 px was vetoed. It
  worked from fit (where a click re-centres nothing), which is why two
  gates passed it. The guard is deleted — Slint's own 10 px repeat gate
  already enforces the rule it was written for.
- **The loupe "fit" view was a crop.** The one-column grid cell is 3:2 and
  spans the grid width, so it was taller than the viewport on every normal
  window: 16.6 % of the frame height hidden at 1440×900, 23.4 % fullscreen
  on 1080p, with nothing on screen to say so — and after #11 gave the wheel
  to zoom and made drag inert, unreachable by any input. The cell is now
  bounded by the viewport at N=1 (persona MUST-HAVE, user-approved); the
  photo renders ~20 % smaller with pillarbox bars, and the whole frame is
  there. The `✓ copied` and `×N burst` badges, anchored below the fold,
  came back with it.

Also: an unbounded optimistic zoom climb that reached 1e38 and poisoned
the pan centre with NaN; three `zoompan` functions that could panic on
non-finite geometry (7,992,116 panicking combinations in a 108.8 M-case
sweep, now zero); and four acceptance criteria that had no real test —
every pointer assertion sat at dead centre, where the pointer anchor and
the centre anchor coincide. A `FASTCULL_DRIVE dblclick:X,Y` action makes
bridge-level pointer defects reachable from a test for the first time;
pointer ROUTING remains review-verified only (issue #13).

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
About dialog (#23 — build-composed version string,
X.Y.Z-devel-<hash> off-tag; the commit DATE joined it in #26) with full modal keyboard containment for both popups, the #18
anchor closure (verified fixed by the #16 relayout work, regression-
pinned), and the drive/shutter harness determinism work. No docs
release-note debt: culling.md gained its sections with the features.
