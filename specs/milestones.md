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
default, BLAKE3 verification, sidecar lockstep. (The auto-rename default was
replaced 2026-08-21 by the clash question — one question, three answers; see
fileops.md.)

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

## M9 — Export frames as video (user decision 2026-08-27)

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

## v0.11.0 (released 2026-08-28)

One feature, the first thing FastCull writes that is neither a sidecar
nor a copy: **Export Frames as Video** (M9, `modules/video-export.md`,
ADR 0004).

The idea came from the user: a 30 fps burst that produced a lovely second
of motion but no keeper is not garbage — it is a Story. Select the frames
(or just stand in the burst), `Ctrl+Shift+E`, and one QuickTime `.mov`
lands in a folder you chose. Every frame in it is the camera's own
embedded full-res JPEG, **copied byte for byte** — nothing decoded,
scaled, cropped, rotated or re-compressed — and it plays at the speed
you actually shot it, measured from the millisecond capture timestamps
the camera wrote (the median gap, so two bursts selected together do not
stretch each other). No options. Any crop, speed change or effect belongs
in the phone editor afterwards: FastCull hands frames to an editor, it is
never one. Your RAWs, sidecars and marks are not touched.

Why Motion JPEG, and why no encoder: a three-persona product review
(marketing, product owner, product manager; ~50 sources) found that no
culling tool does this, that there is no H.264 encoder a static GPL
binary can ship cleanly on Linux and Windows today (OpenH264 compiled
from source is outside Cisco's royalty cover until November 2027; the
pure-Rust AV1 encoder took 110 s for 30 frames and Meta does not accept
AV1), and that the honest first mile — which frames, straight from the
RAW folder, no develop step — is the part nobody serves. Muxing the
camera's JPEGs needs no encoder and no licence, takes half a second for
30 full-res frames, and the user tested the untouched 8640×5760 file in
InShot on the phone: it imported and played.

What the file is: `moov` before `mdat` so it plays while it copies;
64-bit offsets always, because a 400-frame selection is 4.4 GB (QE
exported a real 4.58 GB file and checked the last sample byte for byte);
portrait bursts carry the rotation in the track matrix, pixels untouched;
a frame that does not share the first frame's size or orientation is
skipped and named in the dialog, never scaled. The write goes through the
Copy Picks contract — one worker, temp name, no-clobber commit, the clash
question with its three answers, and a verification that reads the
finished file back and checks every sample against the hash taken on the
way in before the file takes its name.

Gate: persona review before code (nothing IN-MY-WAY; it chose the chord,
the seeded destination and the fallback wording), then two validator and
two QE rounds; fourteen deliberate mutations each turned exactly the
expected test red; CI green on Linux and Windows on the final commit.
Still unverified, and said so in the docs: a FastCull-made file on a
phone (the phone test used ffmpeg's file of the same shape), InShot
honouring the rotation flag on a portrait burst, other bodies' JPEG
flavours. Follow-ups filed: #55 (select this burst / Shift+`]`), #56 (an
exported badge), #58 (a Copy Picks stopwatch that should assert an
invariant, not a clock).

## v0.10.0 (released 2026-08-22)

Copy Picks. One bug report started it, and answering it properly
replaced how the whole operation deals with names that are already
taken.

**The report**: copy the picks, delete some of those copies by hand in
the destination folder, press `Ctrl+E` again — and nothing came back.
Sometimes the `.xmp` reappeared without its RAW; usually nothing at
all, and the only way out was copying to a different folder and moving
files by hand. The session remembered which images it had copied to a
folder and turned that memory into a forced skip WITHOUT ever checking
the copy was still there; the one thing it did re-check was the
sidecar, which is why the sidecar was the one thing that came back.
That memory now reads only — it feeds the ✓ badge and the "copied
earlier but gone" note, and decides nothing.

**The rule, from the user**: *"if I ask to copy the files to a folder,
you copy the files — maybe add a warning that the files already exist.
Context shouldn't matter more than that."* So the disk decides. Every
name the copy would write — the RAW **and** its sidecar, after the
rename template — is checked against the destination, and if anything
is already there you get ONE question whose answer governs the whole
run:

- **Keep both** — the clashing picks land under the first free number,
  `_1`, `_2`, `_3`… on the file-name stem before the extension, sidecar
  always sharing the number.
- **Overwrite those N** — replaces them in place. A destination RAW
  that is already byte-for-byte identical is NOT sent again: it is
  checksummed, kept, and only its sidecar is rewritten if captions
  changed, which makes a second `Ctrl+E` a free "is my export still
  bit-perfect?" pass before the card is wiped. Overwrite means
  overwrite — including a destination `.xmp`, which is where darktable
  keeps its edit history, and the question says so.
- **Cancel** — copies nothing at all, not even the files that had no
  clash. `Esc` does the same.

`Enter` deliberately does nothing on that question, and the destructive
answer is not on `Y` or `N`: those are the culling keys, and
`Ctrl+E, Enter, Enter` must never replace 148 files by reflex.

**Two picks that share a name never ask.** Two bodies producing the
same `DSC01234.ARW`, or a template that gives several frames one name:
the later pick just takes a suffix, under every answer, because
overwriting one of the user's photographs with another is not a choice
worth offering. The plan preview says how many that is.

**Nothing is written outside the destination folder** — an invariant
now, not an assumption. A rename template that produces a path (`/`,
`\`, `..`) or a name with no stem (`{camera}.{ext}` used to write a
hidden `.ARW`) is refused before anything moves, on every platform.

**`{camera}` works again** in both the rename field and the IPTC panel;
it had been handed a literal `None` since the feature shipped and
stamped an empty string.

Under the hood the copy engine stopped trusting `rename`: every commit
that is not an answered overwrite goes through a no-clobber primitive,
so a file that appears between the question and the copy fails that one
file honestly instead of being destroyed, and two same-run names that a
case-folding volume treats as one cannot eat each other. Temp files are
unique per copy after a hard-quit hazard was found: the old shared name
could alias a freshly committed RAW and truncate a copy the report had
already called verified. The plan's suffix search resumes instead of
restarting, after it turned out to freeze the UI for ~3 s per keystroke
while typing a template over a thousand picks.

Closes #14 (a `_2` copy was judged under its natural name, so a caption
refresh could land on a different camera's file — structurally
impossible now: a sidecar is only ever written beside its own RAW).
Nine gate rounds, and every claim in the spec is a test.

## v0.9.0 (released 2026-08-09)

One report drove this release (#46, reported by the user): at deep
1:1, arrowing onto a photo nothing had decoded yet flashed the ENTIRE
next frame at fit for a split second before snapping back to the
carried spot — and sometimes the next photo appeared parked at its
top-left corner with the carried position silently lost. Three
mechanisms were underneath, and fixing the third changes how the loupe
feels under the hand, which is why this is 0.9.0 and not a patch.

First, **the loupe never drops to fit in transit**. Landing on a frame
with no decoded pixels used to fall back to the whole-photo fit view;
now a rough placeholder-quality frame renders at the carried zoom and
position — with the "◌ loading" pill — until the real pixels land
moments later. During transit the eye tracks position, not detail, so
mush at the right spot beats a sharp flash of the wrong framing. A
decode that outright fails still surfaces its Failed badge instead of
a stale placeholder.

Second, **read-ahead follows the screen, not the filenames**. The
prefetch ring warmed neighbours by file order while the arrow keys
walk the order on screen — in folders where capture time interleaves
the filenames (two bodies, two cards) it warmed frames no arrow would
ever reach while every real neighbour stayed cold, which is what made
the flash so common. Every ring now works in on-screen view order, so
the frames being warmed are the frames the arrows will actually hit.

Third, **no coasting into a navigation** — this is the felt change. A
fast drag-flick used to set the image gliding, and an arrow pressed
while it still coasted rendered the next photo at the wrong spot; the
animation's writes were being misread as hand drags and folded into
the stored pan centre until the carried position was gone for good.
Dragging in the loupe now has no glide at all: the image tracks the
hand exactly and stops dead on release, and the stored position only
ever moves on a real drag. (The grid keeps its kinetic scroll —
flicking through a grid is browsing; at 1:1 the user is judging a
spot, and a glide would carry past it.)

Under the hood the drive harness learned pointer dispatch (`press.`,
`move.`, `release.`, `wheel.` tokens) and a loupe-pan dump block, so
drag/flick/navigate sequences finally have red-proven tests.

## v0.8.1 (released 2026-08-03)

Bug fixes only — two strands since v0.8.0, both from live reports.

The first is **focus continuity** (#41, #42, reported by the user):
closing the IPTC panel from the menu left the keyboard dead — no key
did anything, and at 1:1 there was no discoverable way out — and a
Help > About opened over a focused field was worse: undismissable,
with every keystroke landing invisibly in the hidden field, where a
blind-typed "keyword" could be silently committed onto an image. The
rule now is deterministic: whenever the focused editor is destroyed
(panel closed by any route, session swap) or covered (About,
shortcuts, or the copy dialog over it), the keyboard returns to the
topmost surface. A destroyed editor discards its half-typed text —
a session swap can no longer commit the old session's half-edit onto
the new session's image — while a covered one commits exactly like
clicking away always has. Esc now always closes the topmost modal
first: About over the copy dialog takes two Esc presses, and the
dialog's state survives the first one. Under the hood the drive
harness learned real key and click dispatch (`key:`, `click.`,
`dump.` tokens) plus a `FASTCULL_NO_CONFIG` sandbox, so this whole
bug class finally has red-proven tests.

The second is **the Windows console window** (#40): a double-clicked
fastcull-app.exe no longer drags a console window along, and closing
a stray terminal can no longer take the app down with it. Launched
*from* a terminal, diagnostics still work — `FASTCULL_TRACE` output
attaches to that terminal (with the standard trade-off, recorded in
the FAQ, that closing that terminal closes the app). The CLI stays a
console program on purpose, and CI now asserts both PE subsystems on
every Windows build so neither can silently flip again.

## v0.8.0 (released 2026-08-02)

Everything since v0.7.0 — two performance overhauls, a hardening pass,
and the truth-telling that closed #27.

The headline is the **texture kitchen** (#30, the user's requirement
verbatim: "No decoding should be done on the UI thread"). The
architecture spec had said it from the start — the M2-era budgeted
deviations (~32 thumb decodes per refresh, 149 MB full-res copies)
had been violating it since the beginning. One kitchen worker now owns
every pixels-to-texture conversion; the UI thread's remaining duty is
an O(1) buffer wrap, and the old paths are deleted, not bypassed.
Measured A/B on identical drives: UI stalls during a settled 1:1 walk
21–24 ms in 4 of 5 runs → **zero in all runs**; held-walk stalls of up
to 78 ms → zero; stop-to-sharp 677–731 → 627–636 ms. The gate earned
its keep again: a replace-latest dedupe was cancelling a ring
neighbour's queued fill (flaky 60 s shutter refusals), and QE's
mutation campaign found three contracts with no red test — all pinned.

Second, **full-res orientation reworked** (#27): the A1's full-res
JPEG has zero restart markers, so its ~220 ms Huffman decode is
strictly serial while seven cores idle — that dead time now pays the
page faults (decode into a pre-faulted buffer, transpose scratch built
on a spare thread during decode), and the rotate kernel routes writes
through exact chunks with the bounds check hoisted. Full-res
decode+rotate 518 → ~277 ms (three independent witnesses, ordering
preserved in every round), peak memory unchanged. The
under-350 ms budget is green on the 8-core laptop for the first time —
the machine on which #27 declared it unpassable.

Third, **the decoder stops trusting JPEG header claims** (#31). A
639-byte hostile stream claiming 30000x30000 used to decode as Ok
while committing 2.64 GB; it is now rejected at 2.3 MB peak by a
500 MP output cap, and truncated scans (which zune reports as
*successful* decodes) are detected on the raw bytes and surface as the
Failed badge instead of a giant mostly-blank frame. Residual accepted
and documented: a crafted stream with plausible dims, a valid EOI and
too-little entropy data still decodes as a bounded blank success.

With the budget genuinely green, **#27 closed as a documentation
fix**: no thresholds moved — the spec now says the truth, that they
bind on an idle run of the development machine (the 32-thread
reference hardware retired 2026-07-28 stays as provenance), and the
measured load/thermal boundary is written down so a red on a busy
machine gets re-run idle instead of read as a regression.

Also: the drive harness learned `open:PATH` (#34), so the real
Open Folder session swap — kitchen retarget, marks-flush barrier,
order-flip re-arm — has its first tests, each proven against a
mutation that turns it red.

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
