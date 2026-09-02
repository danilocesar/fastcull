# Module spec: export frames as video (`clip.rs`)

## Purpose

A second exit beside Copy Picks. The user selects frames — usually a burst
that produced "interesting cinematic, but no good shot" — and FastCull
writes ONE video file from the camera's own embedded full-res JPEGs, so the
burst can be edited and posted from a phone video editor. In the user's
words (2026-08-27): *"select some shots, usually burst, then a new menu:
'export frames as video'. All the frames selected, their high resolution
JPEGs, exported in video format. Any edits, crops, effects should be done by
an external editor."* And: *"I would like to see the whole full image. No
crop here. Crops, if done, will be done by the video editor in a secondary
step."*

Design principle, signed by a three-persona review (marketing / product
owner / product manager, 2026-08-27): **FastCull may hand frames to an
editor; it is never one.** The export has no options.

## The decision that makes it cheap: Motion JPEG, bytes untouched

The file is a QuickTime `.mov` whose video track is **Motion JPEG**: every
sample is the RAW's embedded full-res JPEG **copied byte-for-byte** — no
decode, no scale, no crop, no re-encode, no colour conversion. Recorded
reasons:

- There is no H.264 encoder that a static GPL binary can ship cleanly on
  Linux and Windows today: OpenH264 compiled from source is not covered by
  Cisco's patent royalty (only Cisco's own binaries are, until the last US
  patent expires 29 Nov 2027); the developer's own Fedora ffmpeg has no
  `libx264`; the pure-Rust AV1 encoder (rav1e) measured 110 s for 30 frames
  and AV1 is not on Meta's accepted list. Motion JPEG needs no encoder and
  no licence.
- The user tested both a 2880×1920 MJPEG `.mov` and the real artefact — the
  30 untouched 8640×5760 camera JPEGs muxed at 15 fps, 328 MB — in
  **InShot** on the phone: *"all worked."*
- Cost: muxing 30 full-res frames took **0.3 s** with ffmpeg stream-copy on
  the dev laptop; extracting them is a byte copy (measured 0.21–0.24 s for
  30 frames, 327 MB). The whole export is I/O bound. For comparison a
  decode → crop → re-encode path measured 3.2 s and 1.7 GB peak.
- The Sony full-res JPEG is 4:2:2 chroma (`yuvj422p`); the phone decoded
  it. Other bodies may embed 4:2:0 or progressive JPEGs; Motion JPEG
  decoders take baseline JPEG of any subsampling, and a body whose full-res
  JPEG a phone will not play is a support question, not a design one.

Consequences accepted with it: the file is large (~11 MB per A1 frame —
328 MB for a 2 s clip, and a 400-frame selection is 4.4 GB), and the
frames are exactly what the camera rendered (ADR 0001's trade-off, same as
the loupe). A 4K-scaled variant is deliberately NOT offered: it would be a
re-encode, i.e. the first step into the editor.

## Scope: what gets exported

- **The selection**, when there is one (`Selection::batch`, the same input
  the IPTC batch uses). With no selection, **the burst under the cursor**
  (burst-grouping.md `Grouping`). Neither → the menu item is disabled with
  its reason in the tooltip/status ("select frames or stand in a burst").
  One frame is not a video: the item is disabled for a single frame too.
- **Capture order**, always: the selection is the SET; the file is ordered
  by the capture-time sort key (filename as tiebreaker), regardless of the
  grid's current sort. A video that plays backwards because the grid was
  sorted by name descending is a bug.
- Pick state is irrelevant and untouched: the export reads marks like Copy
  Picks does and never writes them; by definition the frames are usually
  rejects. Filter state is irrelevant (the selection is explicit).
  **The filter's two halves, settled at implementation time
  (2026-08-27)**: a SELECTION means the selected frames that are in the
  view — `Selection`'s own rule, "what you see is what you stamp", and
  the number the status bar prints — while a BURST means the whole burst,
  including members the filter is hiding. A selection is a set the user
  built by pointing at frames they could see; a burst is a fact about
  capture times, and "this burst" means the burst.
- **Every frame keeps its whole image.** No crop, no scale, no rotation of
  pixels.

## Frame rate: the camera's own cadence (user decision 2026-08-27)

*"Detect the FPS from the video frames themselves… calculate the natural
FPS. It won't necessarily be 2 s. Could be more, could be less."*

Sony writes `SubSecTimeOriginal` with **millisecond** precision (the three
reference A1 files carry `943`, `959`, `691`); the app already turns
`DateTimeOriginal` + `SubSecTimeOriginal` into `FrameMeta::time_ms` for
burst grouping (burst.rs). The export uses that timestamp:

1. **Constant frame rate, from the median gap.** Sort the frames by
   capture time; take the gaps between consecutive frames; the sample
   duration is the MEDIAN gap, in milliseconds (MOV timescale 1000). A
   30 fps A1 burst reads ~33 ms gaps → 30.3 fps; the file plays in real
   time, 1 s for 30 frames. The median, not the mean, so that one pause
   (the gap between two squeezes selected together) does not stretch every
   frame. Only a pair of CONSECUTIVE frames that BOTH carry SubSec
   precision contributes a gap (implementation 2026-08-27): a whole-second
   timestamp dropped into a burst would otherwise add a spurious 1000 ms
   sample to the population the median is taken from.
2. **Gaps are not preserved.** A gap larger than the median plays as one
   frame step: two bursts selected together play back to back, the pause
   between them dropped. Recorded alternative, rejected: true per-frame
   durations (QuickTime allows it) would make a 5 s pause a 5 s freeze
   inside the clip and produce variable-frame-rate files that some editors
   mishandle; the phone editor is where pacing is decided.
3. **Fallbacks, reported in the completion line.** No pair of frames with
   SubSec precision (1 s granularity, or no timestamp at all) → the
   cadence cannot be measured → **15 fps**, and both lines say
   "timing not in the files — assumed 15 fps". A median gap outside the
   playable window (two bodies interleaved, or a selection of singles) →
   clamped, and both lines say so: "gaps of 4.0 s — clamped to 10 fps".
   Duration and fps are shown in the plan line before the user confirms,
   so a wrong cadence is visible before a byte is written — and the plan
   line carries the SAME wording as the report (persona: it must be
   impossible to miss before Enter). When the cadence was measured,
   neither line says where it came from: the line is just "30.3 fps".

   **The window, settled at implementation time (2026-08-27).** This
   paragraph used to name two different windows — a trigger of "below
   10 ms or above 1000 ms" and a clamp target of "[10 fps, 120 fps]" —
   which disagree: a 500 ms median gap trips neither trigger and is
   2 fps, well outside the fps window. The fps window wins, because it is
   the one that guarantees a file an editor can use. So: **the sample
   duration is the median gap clamped into [9 ms, 100 ms]**, and the
   clamped wording appears whenever the clamp actually moved it. 100 ms
   is exactly the promised 10 fps floor; 9 ms is 111 fps, the fastest
   whole millisecond inside the promised 120 fps ceiling (8 ms would be
   125 fps, i.e. outside it) — 120 fps itself is not representable at
   timescale 1000.

## The container

QuickTime File Format, one video track, `jpeg` sample description (the
layout ffmpeg produces for `-c:v copy` from a JPEG sequence, which the
phone accepted; the golden file below pins it). Timescale 1000. `moov`
BEFORE `mdat` (every sample size is known from the plan, so the file plays
while it is still being transferred). **64-bit chunk offsets (`co64`)
always** — a 4 GB+ file is routine here. `ftyp` major brand `qt  `. No
audio track. The track's display matrix carries the EXIF orientation of the
frames (see "Orientation"). Written by an in-tree, dependency-free muxer
(`clip/qt.rs`): no crate on crates.io writes a `jpeg` sample entry, and the
atom set is small; it is golden-file tested like the XMP serializer.

## Uniformity rules — skip, never scale

All samples in a Motion JPEG track must share one frame size and one
orientation. The first frame in capture order sets both.

- A frame whose embedded JPEG has **different dimensions** (a crop-mode
  shot, a different body, a file whose full-res JPEG is missing so only a
  smaller preview exists) is **skipped and reported** ("2 frames skipped:
  different size (5616×3744)"). Scaling would be a re-encode; padding would
  be an edit. If skipping leaves fewer than 2 frames, the export refuses at
  plan time.
- **The sentence is bounded: at most THREE reasons are named** (issue #62).
  Every distinct pixel size is its own reason, so a mixed selection —
  thirteen frames in twelve sizes — used to word itself as twelve clauses,
  nine wrapped lines in a card built for one. The three biggest groups are
  named and the rest fold into one tail that keeps the arithmetic honest.
  Measured from the app, twenty skipped frames in twelve groups (4 + 3 +
  2 + 11 = 20): *"skipped — 4 frames: different size (390×400) · 3 frames:
  different size (380×400) · 2 frames: no usable embedded JPEG · 11 more
  frames in 9 other sizes"*. The tail names the KIND it folded (`in N other sizes`,
  `in N other orientations`) only when the whole tail is that kind, and
  says `for N other reasons` otherwise — a shorter sentence may never
  become a wrong one. `skipped_text` is the one place this happens, so the
  plan line, the refusal and the report are bounded together.
- A frame whose **EXIF orientation** differs from the first frame's is
  skipped and reported the same way.
- A frame with **no usable embedded JPEG** (the loupe's `no usable embedded
  preview` badge) is skipped and reported.
- The fullres source is `EmbeddedPreviews::fullres()` — the largest
  embedded JPEG, the loupe's own source. **Corrected at implementation
  time (2026-08-28, validator finding)**: this line used to add "for
  CR3/RAF the rawler fallback applies (raw-pipeline.md), whatever it
  yields", and that fallback cannot serve this module. It is a half-size
  RAW *decode* — it produces pixels, and what an export needs is a byte
  range inside the file. So a container the in-tree preview walker cannot
  read (CR3 and RAF are ISO-BMFF and Fuji's own format, not TIFF) has no
  frame here for the same reason it has no picture in the loupe: those
  frames are skipped and reported as "no usable embedded JPEG", and a
  selection made entirely of them refuses at plan time. Giving those
  bodies a video export means giving them a loupe first.

## Orientation

Pixels are never rotated (that is a re-encode). Portrait frames (EXIF
orientation 6/8) stay in sensor orientation inside the samples and the
**track matrix** in `tkhd` rotates the display, exactly as phone cameras
record portrait video. Orientation 1 → identity matrix. Mirrored
orientations (2/4/5/7) are treated as their unmirrored counterparts and
noted in the report. QE verifies the matrix with ffprobe and a player;
whether InShot honours it on import is recorded in the acceptance criteria
as user-verified or NOT VERIFIED.

## Files: naming, destination, the Copy Picks contract (ADR 0004)

- **Name**: `<first stem>-<last stem>.mov` in capture order, e.g.
  `DSC05010-DSC05039.mov`; one frame per stem, so it doubles as the
  frame-range record. Rename templates do not apply (there is one file).
  Three details settled at implementation time (2026-08-27): the stems
  are the FIRST AND LAST FRAME IN THE FILE, never a skipped one, or the
  range would name frames the user cannot find inside it; two equal stems
  (the same name with two extensions) collapse to `<stem>.mov` rather
  than `a-a.mov`; and a name over **255 bytes** — the per-name limit of
  every mainstream filesystem, reachable from two long stems — is refused
  at PLAN time, because discovering it at commit time would cost the user
  the whole write first.

  The length is checked on **the name this plan would write**, `_k`
  suffix included — not on the natural name before the clash question is
  resolved (validator finding, 2026-08-28: checking early accepted a
  255-byte plan that then wrote 257 under "keep both" and failed at the
  commit with the file already on disk). Each answer is therefore judged
  on its own: a 255-byte name whose destination is occupied still gets
  the question, Overwrite still works because it writes that same name,
  and only "keep both" is refused — by the replan that answer itself
  triggers, still before a byte is written, dropping back to the plan
  where Overwrite is one keystroke away. Refusing the whole plan instead
  would take away an answer that works.
- **Destination**: a folder the user chooses, remembered across sessions
  as `clip_dest` in `ui.toml`. **Seeded from the Copy Picks destination
  until a clip folder is first chosen** (persona decision 2026-08-27: on an
  ordinary evening the selects folder is where today's output goes; a
  second remembered path would land the video in a three-week-old job's
  folder). Once chosen, remembered separately. Never the RAW folder by
  default; allowed if chosen (ADR 0004).
- **Clash**: the same question as Copy Picks (fileops.md "The clash
  question"): a name already there → Keep both (`_1`, `_2`, …) / Overwrite
  / Cancel. Nothing is ever replaced without the Overwrite answer.
- **Write**: the copy-engine shape — one worker thread, a progress event
  per frame, cancel between frames, unique temp name, no-clobber commit
  (`hard_link` + unlink, rename only for an answered Overwrite), **never a
  partial file under the final name**. A hard quit leaves at most one
  hidden `.fastcull-partial-*` file (documented, as for Copy Picks).
- **Verified**: every sample's bytes are BLAKE3-hashed while read from the
  RAW and the finished file is re-read and each sample range re-hashed;
  "all checksums verified" appears only when that passed for every frame.
  The `moov` is re-parsed from the finished file by the in-tree reader and
  must describe exactly the samples written.
- **Free space**: sum of the JPEG lengths + the header, checked at plan
  time like Copy Picks. The header is not an allowance but an exact
  number: every sample size is known before the write, so the size the
  dialog quotes is the size the file ends up having.
  A destination whose filesystem cannot hold a file of that size (FAT32
  above 4 GB) fails honestly at write time with the OS error and the temp
  file removed — recorded, not pre-detected (there is no portable way to
  ask).

## Dialog (minimums)

Menu: **File › Export Frames as Video…** (the wording is the user's;
"video", not "clip", in the menu), keystroke **Ctrl+Shift+E** — beside
Copy Picks' Ctrl+E as "the other exit"; a modifier chord so it cannot
fire from a fat finger mid `]`/`N` (persona 2026-08-27). Disabled with its
reason in the status line, never a silent grey item. One dialog in the
Copy Picks style:
destination row with a Choose… button and the remembered path; one plan
line — *"30 frames · 8640×5760 · 30.3 fps · 1.0 s · 328 MB →
DSC05010-DSC05039.mov · 358 GB free"*, whose cadence field carries the
fallback wording instead of the bare rate when there is one; a skipped
line
when there is one; **Export** (Enter, when the plan is clean) and Cancel
(Esc); progress "n / N" with Cancel while writing; the report with the
verified line and an Open folder action. No other control. The clash
question is the same dialog state as Copy Picks. Modal, keyboard-contained
(issue #42 rules), never marks, never moves the cursor.

**The card's height follows its content** (issue #62), between a floor —
260 px, or 380 px while the clash question is up, so the ordinary dialog
keeps its proportions and does not read as an 8-line box holding two
sentences — and the window (`parent.height - 40px`). It used to be a
constant per state, and a constant cannot know how long a sentence is: a
thirteen-frame refusal naming twelve sizes wrapped to nine lines, the
spring above the button row collapsed to nothing, and the row was laid
out BELOW the card — drawn over the scrim (the card does not clip) and
still clickable, because Slint hit-tests children outside their parent.
Measured on the parent tree at 1440×900: the row ended 29 px past the
card's bottom edge. The same rule applies to the Copy Picks card
(floor 480 px), whose report prints one line per failed file — 830 px
past its bottom edge on the same tree.

**Growing is only half of it: past the ceiling the BODY SCROLLS.** A card
that has reached `parent.height - 40px` cannot grow further, and a wrapped
`Text` cannot be made shorter, so the growable text — the plan line, the
skipped line, the hint, the clash question, the error, the progress and
the report — lives in its own `ScrollView` between the fixed header rows
(title, destination, and Copy Picks' rename field) and the button row.
That body is the only row the layout is allowed to shrink: it carries
`vertical-stretch: 1` and no minimum of its own, while the header rows and
the button row cannot give up a pixel. So the deficit always lands in the
body and the content scrolls. Nothing is truncated and nothing becomes
unreachable, which is why the report may still list one line per failed
file.

**The bound on that promise**: the buttons stay inside the card wherever
the card's ceiling can still hold the FIXED rows — about 190 px for Copy
Picks (padding, title, destination row, rename row, button row), i.e.
roughly 300 px of window height once the menu bar, the status bar and the
40 px margin are taken off. Measured 2026-08-30 with 61 failures in the
report: at 640×300 the card is 560×194 and the row sits at 212…244, inside
it; at 640×200 the ceiling is 94 px, the fixed rows do not fit, and the
row is laid out 90 px below the card and 44 px below the window. Below
that height a 560 px-wide card is not a dialog anyone can use anyway, so
the failure is left visible rather than papered over.

- **`ScrollView`, not a bare `Flickable`** (decision 2026-08-30): the
  Flickable version scrolled correctly but showed nothing to say so, and
  its clip is line-aligned, so a report cut off at the ceiling looked
  complete. The std-widgets `ScrollView` is a drop-in with the same
  viewport properties and brings a scrollbar. Adopted only after measuring
  that it changes no geometry in the ordinary case — export card 560×260
  at y=327, Copy Picks 560×480 at y=217, byte for byte — and that the
  issue #49 wheel tests, the wheel routing table and both clash tests
  (Esc/Enter/B/O containment) stay green.

- A wheel over a dialog scrolls THAT DIALOG'S body when it overflows, and
  nothing at all otherwise — never the grid behind the scrim (issue #49's
  contract, which the body inherits by sitting inside the same scrim).
- **And so does the keyboard**, in both dialog scopes: Down/Up move a line
  (40 px), PgDn/PgUp a body height, Home/End the ends — clamped to
  `[height - viewport-height, 0]`, and ONLY while the body overflows, so
  below that these keys keep the meaning they have today (swallowed by the
  clash question, otherwise rejected). B, O, Esc and Enter are not in the
  set and are untouched. This is a keyboard-first app: a report only the
  mouse can reach is a report the user cannot read (QE finding
  2026-08-30 — PgDn used to do nothing at all). Witnessed by a
  `<dialog> body scrolled to Y` mark, since a body holds text and no
  cursor: nothing else in a dump changes when it moves.
- **Below ~500 px of window height the clash question's answer rows need
  a scroll** to come into view (three rows plus the question text exceed
  what the ceiling leaves). They are reachable — wheel, PgDn, or the B/O/
  Esc keys, which answer wherever the body happens to stand. On HEAD they
  were drawn OUTSIDE the card at those sizes, so this is strictly better;
  it is recorded rather than fixed because a dialog that small is not a
  size the app is designed for.
- Bounding the sentence (above) and the layout protect different things:
  the first keeps THIS text short, the second keeps ANY text inside the
  card. Neither alone is the fix — measured 2026-08-30 with each half
  reverted in turn, the layout alone contains a twelve-clause refusal (it
  scrolls) but leaves it unreadable, and the bounded sentence alone still
  puts the row 18 px outside the card as soon as a long file name wraps
  the plan line.
- Nothing moves in the ordinary case: the floor is the height the card
  had, and the content sits under it. Measured 2026-08-30 at 1440×900,
  before and after: plan state 560×260 at y=327, Copy Picks 560×480 at
  y=217 — the coordinates the issue #49 wheel tests are calibrated
  against. The one deliberate change is the #56 hint state, which had a
  286 px special case for exactly this problem and now measures 260 px
  like every other plan state, hint included.
- The witness is a trace, not a screenshot: `clip card laid out at X,Y
  size WxH` and `clip buttons laid out …` (and the `copy` pair), from the
  `changed absolute-position` / `changed height` handlers, the issue #13
  idiom. A screenshot cannot see this — an escaped row is drawn looking
  almost right — and the property is a relation between two rectangles:
  `buttons.y + buttons.h <= card.y + card.h`.
- Two marks make the driven test clock-free (issue #62, harness section of
  ui-grid.md): `clip export finished run N` fires when the report card
  goes up — N counting the exports this process started, so a script that
  exports twice can wait for the second one (issue #70) — and `load
  settled gen N` carries the session generation so a script can wait for
  the SECOND folder it opened. An export CANCELLED by a session swap
  emits no mark: cancelled is not finished.

## Exported badge and hint (issue #56, 2026-08-29)

*"Which of these did I already export?"* — asked at the grid while
scanning a folder, and again at the dialog with the frames already
selected. Two surfaces, one memory.

**The memory is SESSION-ONLY, and that is the contract.** It starts empty
on every folder open, it dies with the process, and it is exactly the ✓
copied badge's promise: *this run only*. Nothing is written to
previews.db and nothing to an XMP sidecar.

- **Why not previews.db** (persona verdict IN-MY-WAY, 2026-08-29): the
  cache is explicitly disposable — it self-heals on a schema-version
  bump, it is evicted, it is invalidated by a folder move, and
  `FASTCULL_NO_CACHE` turns it off. A memory that can vanish for reasons
  the user cannot see is a memory whose ABSENCE cannot be trusted, and a
  badge nobody can trust is worse than no badge. Session-only has a
  one-sentence contract instead.
- **Why not a sidecar property**: ADR 0003 permits sidecar writes, but
  stamping a FastCull-private flag into 30 XMPs per export — files that
  are handed to darktable, Lightroom and Bridge — is disproportionate for
  a hint, and drags other tools' sidecar handling into it. Refused
  outright.
- **The dangerous case is already covered.** Re-exporting the same span
  is caught by the `.mov` name clash question, which asks about what is
  really on disk. This feature never has to prevent anything, which is
  why it may be a hint.
- The user delegated the call (2026-08-29) after the persona gate rated
  the badge USEFUL (low end) and the hint USEFUL (stronger than the
  badge).

**Reads, never decides.** `clip::ExportLedger` is the same shape as
`fileops::SessionCopies` and carries the same rule: it feeds the badge
and the hint and NOTHING else. It never changes a plan, an answer to the
clash question, a mark, or which frames the next export takes.

- **What is recorded**: the plan's KEPT frames (the ids actually in the
  file), against the path the file committed under. Only for a run that
  landed, and CORE decides that: `ClipReport::frames_to_record` gates on
  `earned_the_green_light()` — the same question the report's verified
  line asks, so the badge and the sentence can never disagree — plus the
  stashed ids having the same count as the file's samples. A cancel and a
  failure leave nothing on disk to point at, so they record nothing. The
  app used to re-implement that condition; it does not (validator
  finding, 2026-08-29).
- **A skipped frame is never badged.** It is not in the video; a badge
  saying otherwise is the confident-lie class. The ids handed over are
  the PLAN's frames, which the uniformity rules have already filtered.
- **An Overwrite drops what it replaced.** `record` supersedes the entry
  of the same (canonical) path, so replacing `NAME.mov` with a different
  frame set takes the badge off the frames of the file that is gone.
- **Follow the disk, at two moments only**: when an export finishes, and
  when the export dialog opens. Copy Picks is the precedent but not the
  same placement, and the difference is deliberate: `SessionCopies` is
  re-checked inside `copy_replan`, i.e. on EVERY replan — which for that
  dialog includes a keystroke in its rename field. This dialog has no
  field, so the re-check sits at the dialog's open instead, which is the
  better of the two placements and the one to copy if the ✓ memory is
  ever revisited. NEVER per repaint: the grid asks the ledger once
  per visible cell on every repaint, and a `stat` there would be a storm
  while scrolling. An unplugged drive therefore means no badges — a false
  negative, the safe direction, and it must not be "fixed" by caching a
  last-known-present flag.
  The accepted cost, named so it is not rediscovered: the dialog-open
  re-check is one `stat` per export MADE THIS SESSION, on the UI thread —
  N round trips on a network destination, where N is how many videos this
  session wrote (typically one or two, never per frame). The shape that
  would change it is a session with dozens of exports over a slow mount;
  the answer then is the same one Copy Picks would need, a worker thread.

**The badge.** `▶` on **every frame that went into a clip**, bottom-left:
immediately right of the ✓ when there is one (the ✓ keeps `x: 8px`), in
the ✓'s place when there is not; the `×N` burst pill
keeps the bottom-right. Per FRAME, not per burst: the export's scope is
`Selection::batch`, an arbitrary set — an opener-only badge would lie
about a partially exported burst and would mean nothing at all for a clip
made of two bursts.

- **Not ✓'s green.** Green means "your files are safe at the
  destination", a data-safety signal; this is not one. The badge wears
  the `×N` pill's palette (`#d8d8e0` on `#202028cc`) so a thirty-cell run
  reads quiet, and because exported frames are usually rejects under the
  grid's 40 % dim, where a bare glyph washes out.
- **Visible in the loupe**, like ✓ and ×N and for the same reason
  (ui-grid.md, "the intended loupe badge policy"): the mark has the pill,
  and the cell badges carry everything else.
- **Glyph**: `▶` U+25B6, verified to render MONOCHROME in the app's font
  on Linux (2026-08-29, from the driven test's own screenshot). `▸`
  U+25B8 is the recorded fallback if a platform renders it as a colour
  emoji.
- Residual, accepted: at the 12-column zoom on a window narrower than
  ~1200 px the `▶` pill and the `×N` pill can touch. That is the same
  crowding `✓` and `×N` have always had at that zoom, where the cells are
  thumbnails; the fix, if it is ever wanted, is one badge row that lays
  itself out, not a special case.

**The hint.** ONE line in the plan preview, under the plan line and the
skipped line: *"3 of 30 frames are already in DSC05010-DSC05039.mov"*,
*"all 30 frames are already in …"*, and when the frames are spread over
several videos *"5 of 30 frames are already in 3 videos —
DSC05010-DSC05039.mov and 2 more"*. Never more than one line (this dialog
has one plan line, one skipped line and no other control).

- **The count binds to the VIDEOS, never to the named one** (architect
  finding, 2026-08-29). "5 of 30 … in a-d.mov (+2 more)" claims a-d.mov
  holds five frames when it holds three, and the user cannot check it
  without opening the file. The multi-video sentence therefore says how
  many videos, then names one.
- **One line means ELIDED, not wrapped**, and a two-stem name reaches 45
  characters. The name is therefore LAST in every shape and every count
  comes before it: a long name may cost the user the name, never a
  number. (`and 2 more` is the one thing allowed behind it — it repeats
  the video count already stated.) This used to be forced by a fixed card
  height; since issue #62 the card grows, so the rule stands on its own
  merit — a file name is not worth a second line in a dialog whose job is
  one plan line, and the count that matters is already in front of it.
- Counted over the SCOPE — the frames the user chose — not over the
  plan's kept frames, so the line still stands when the plan itself
  refuses (no destination yet, no room), which is exactly when "you
  already have these" is worth the most.
- Plan state only, like the skipped line: the clash question's card
  stacks three answer rows and has no room for a fourth sentence, and the
  question it is asking is about the file name.
- Grey, not amber: nothing here is wrong, and the skipped line above it
  is a warning that must stay the loudest thing in the card.
- The wording lives in core (`clip::exported_hint`), like every other
  sentence this dialog shares with the report.

**Explicitly NOT built** (see also the panel rule below): no clips
panel or list, no filter or sort by "exported", no auto-reject of
exported frames, no status-bar count, no cache table, no sidecar
property.

## Follow-ups logged, not in M9 (persona 2026-08-27)

- **"Select this burst" / extend the selection to the next burst**
  (Shift+`]` is the natural pair of `]`): the common case — the burst under
  the cursor — needs no selection, but "burst 40 plus burst 41" is
  Shift+arrow over 60 frames today. A burst-grouping/selection change with
  its own gate; tracked as issue #55 — **shipped 2026-08-28** (Shift+`[`/`]`,
  Ctrl+Shift+B, and Esc clearing the selection from the loupe too; the
  contract lives in burst-grouping.md's UI contract).
- **An "exported as video" badge** on the burst, like the Copy Picks
  checkmark: USEFUL, not must-have; a new badge surface, deferred —
  issue #56, **shipped 2026-08-29**. Per FRAME rather than per burst, and
  with a counted dialog hint the persona rated higher than the badge
  itself; the contract is the "Exported badge and hint" section above.

## Explicitly not built (panel rule, one year from release)

No crop, no scale, no rotation of pixels, no fps choice, no speed/loop/
bounce, no format choice, no audio, no montage, no per-frame timing, no
GIF/WebP, no bundled or downloaded ffmpeg, no H.264/AV1 encoder (revisit
only on ≥3 unsolicited requests for a re-encoded output after this ships,
and never without the user's own licence decision). README: one bullet
under the exports, never the headline.

From issue #56 (2026-08-29), on the export MEMORY: no clips panel or
list, no filter or sort by "exported", no auto-reject of exported frames,
no status-bar count of exported frames, no cache table, no sidecar
property. The memory is one session, two surfaces, and nothing else.

## Platform

Linux and Windows, first-class both (user requirement 2026-08-27): the
muxer is pure Rust and the export is file I/O only, so CI verifies it on
both runners with the in-tree reader; ffprobe-based checks run where
ffprobe exists and are skipped (not failed) elsewhere, and the acceptance
list says which claims are CI-verified on Windows and which are
review-only.

## As built (M9, 2026-08-27) — decisions taken during implementation

The user delegated the remaining calls. Each of these is recorded here
because it is a promise to a user, not an implementation detail:

- **The refusal is a status-line message, not a silent grey item.** The
  menu entry is disabled when there is nothing to export, but
  `Ctrl+Shift+E` always works: it appends "Export Frames as Video:
  select frames or stand in a burst" (or "one frame is not a video —
  select more, or stand in a burst") to the status bar for six seconds,
  and the message vanishes the moment it stops being true, so selecting
  the frames it asked for does not leave the complaint on screen.
- **`ui.toml` is read-modify-written.** `clip_dest` lives beside
  `copy_dest` in the same file, and the Copy Picks save path used to
  rebuild that file from its own two keys — which would have erased the
  video destination on every folder change.
- **The report's wording is the plan's wording.** `Cadence::text` and
  `skipped_text` live in core and both surfaces call them, so the
  sentence the user agrees to before Enter is the sentence they read
  afterwards. The "all checksums verified" RULE lives in core too
  (`ClipReport::earned_the_green_light`), as it does for Copy Picks.
- **A cancelled export says "nothing was written"**, not Copy Picks'
  "files that finished remain": this operation produces exactly one
  file and never commits it before it has been verified.
- **Planning opens every frame, on the UI thread, and that is
  affordable here.** The plan has to know each frame's embedded-JPEG
  offset, length and size, which is one file open and a few KB of
  targeted reads per frame (the same walk the grid pipeline does). Copy
  Picks could not afford anything like that — its rename field replans on
  every keystroke, which is exactly how an N² `stat` walk once froze the
  UI for three seconds per character. THIS dialog has no field: the plan
  is built when it opens, when the destination changes, and when the user
  commits — a handful of times, not per keystroke — over a burst of tens
  of frames. Recorded as an accepted cost, with the shape of the thing
  that would change it: a text input in this dialog, or a routine
  thousand-frame selection over a network mount, would move the probe to
  a worker thread.
- **Cancel is a BUTTON as well as `Esc`** in the plan state. The Copy
  Picks dialog has no such button, and its scrim swallows clicks without
  dismissing — so a mouse-only user has no way out of it. The spec's
  dialog minimums name Export *and* Cancel, so this one has both.
- **The dialog is keyboard-contained in every state** (issue #42), and
  this was verified by driving the real app rather than assumed: with the
  dialog up, `Y`/`N` mark nothing, `Ctrl+O` does not open the folder
  picker, and `Ctrl+E` does not raise the Copy Picks dialog underneath.
  The clash question additionally swallows everything that is not `B`,
  `O` or `Esc`, and says out loud that it is still waiting.
- **The clash question's "keep both" row leads with the NUMBER** —
  "Keep both (_1) — the video lands as …" — and carries no byte-cost
  column. The number is the part the answer decides and it must survive a
  long file name eliding inside the row (a name built from two
  descriptive stems reaches 45 characters easily, and before this it
  painted straight through the card and out over the grid behind it). The
  cost column is Copy Picks' own: its answer costs bytes on top of a run
  that was going out anyway, while here there is one file whose size the
  plan line states already.
- **A second export that loses a race for the same name gives up, it
  does not retry.** Two exports aiming at the same "keep both" number
  both plan `_1`; the first commits, and the second fails with "a file
  appeared at the destination during the copy" rather than walking on to
  `_2` (QE, 2026-08-28). This is the copy engine's own no-clobber
  behaviour and the safe direction — the alternative is a write that
  silently lands somewhere the user was never shown — so it stays, and it
  is recorded rather than left to be rediscovered.
- **The frame set is probed twice per export** — once when the dialog
  opens and once when the user commits (the plan built for the preview is
  never the one that runs, the same rule Copy Picks has). A file that
  changes between the two is caught by the write's `read_exact`: a frame
  whose bytes have shrunk fails the export honestly rather than writing a
  sample shorter than the size already in the header.
- **Three Windows name limits this module does NOT handle, named so
  nobody assumes it does** (QE, 2026-08-28; none is reachable on the
  Linux where they were found, so all three are review-only):
  1. `MAX_NAME_BYTES` counts BYTES; NTFS counts 255 UTF-16 code units.
     For ASCII the two coincide and this module is merely conservative —
     never wrongly permissive — but 130 Cyrillic characters are 260 UTF-8
     bytes and are refused here while NTFS would accept them.
  2. Windows' 260-character `MAX_PATH` applies to the whole PATH, not the
     name, so a name this plan accepts can still fail at the commit
     inside a deep destination without long-path support. That is the
     same shape as the bug the length check was moved to prevent, and it
     has no plan-time answer: the destination is the user's.
  3. Windows reserved names (`CON`, `NUL`, `AUX`, a trailing dot or
     space) are not checked. The only way to reach one is the equal-stem
     collapse — two files both named `CON.ARW` would target `CON.mov` —
     and Windows itself forbids creating `CON.ARW`, so it takes a
     network share written from another operating system. Deferred with
     that reason rather than implemented, because the fix cannot be
     tested where it was found.
- **A session swap during a running export reports what LANDED, from a
  flag, not from the destination path.** The swap cancels by dropping the
  handle, which cancels and joins; a worker that had already committed
  leaves a real file whose report dies with the receiver. The worker
  therefore sets a shared flag the moment the file takes its final name,
  and the swap reads that. Looking at the path instead is wrong in one
  specific and very reachable case (validator finding, 2026-08-28): under
  an Overwrite answer the destination is occupied from the moment the
  export starts, so "the file is there" reported YESTERDAY's file as this
  export's.
- **Two limits of "cancel", named rather than papered over** (validator
  findings, 2026-08-28). The cancel flag is polled between frames and
  again per sample of the read-back, but the `fsync` between those two
  phases is one kernel call with no polling point — so a cancel arriving
  during it waits out the whole flush, which on a 4.4 GB export is
  seconds on the UI thread. Splitting the flush buys nothing: the data
  has to reach the disk before it can be read back at all. And a cancel
  that arrives after the commit is a FILE, not a nothing: a session swap
  therefore looks at the destination before reporting, and says "the
  video had already finished: <name>" rather than "nothing was written".
- **A mirrored frame is kept, not skipped.** EXIF orientations 2/4/5/7
  degrade to their unmirrored rotation (1/3/8/6) and the report says how
  many. Skipping them instead would drop frames over a flip the track
  matrix cannot express.

## Acceptance criteria (tests)

Every box below names the test that holds it. Unless a line says
otherwise, the test is hermetic — it needs neither the sample RAWs nor
ffmpeg — and therefore runs on the Windows runner too. `core:` =
`fastcull-core` unit test, `muxer:` = `tests/clip_muxer.rs` (needs the
sample RAWs), `app:` = the driven `tests/screenshot.rs` test.

- [x] Muxer golden file: 3 reference frames → a `.mov` whose atom tree
      matches the pinned golden (ftyp `qt  `, moov-before-mdat, `jpeg`
      sample entry, timescale 1000, `co64`, stts one entry, tkhd identity
      matrix) and which ffprobe reports as `mjpeg, 8640x5760, yuvj422p,
      3 frames` where ffprobe exists — byte-identical samples proven by
      hashing the mdat ranges against the source JPEGs.
      *(The criterion said "30 frames"; the fixture is the three
      reference frames, and the count is corrected here.)*
      → `core: qt::the_container_layout_is_pinned_to_a_golden_file`,
      `core: qt::the_reader_confirms_what_the_writer_promised`,
      `muxer: the_real_reference_frames_produce_the_golden_header`,
      `muxer: every_sample_is_the_camera_jpeg_byte_for_byte`,
      `muxer: ffprobe_agrees_it_is_motion_jpeg` (skipped without ffprobe).
- [x] Cadence: 30 frames with 33 ms gaps → 30 fps; a selection of two
      bursts with a 4 s pause → the median, pause dropped, N frames in
      capture order; gaps at 1 s granularity → 15 fps + the report line;
      no timestamps → 15 fps + the report line; interleaved two bodies →
      capture-sorted merge, clamped cadence, reported.
      → `core: the_median_gap_is_the_frame_duration`,
      `core: a_pause_between_two_bursts_does_not_stretch_every_frame`,
      `core: one_second_granularity_falls_back_to_fifteen_fps`,
      `core: only_millisecond_pairs_measure_a_gap`,
      `core: implausible_gaps_are_clamped_and_said_so`,
      `core: two_interleaved_bodies_merge_in_capture_order_and_clamp`,
      `core: the_cadence_only_explains_itself_when_it_had_to`.
- [x] Order: the file is in capture order whatever the grid sort;
      filename tiebreak for equal timestamps.
      → `core: capture_order_sorts_by_time_then_name_then_the_untimed`,
      `core: the_plan_is_in_capture_order_and_names_the_range`,
      `app: export_frames_as_video_writes_a_real_motion_jpeg` (the three
      fixtures are named so that capture order and name order disagree,
      and the samples are compared in capture order).
- [x] Uniformity: a different-size frame, a different-orientation frame,
      and a no-preview frame are skipped and reported; < 2 frames left
      refuses at plan time; a single-frame selection disables the item.
      → `core: frames_that_cannot_share_the_track_are_skipped_and_reported`,
      `core: the_first_usable_frame_sets_the_track`,
      `core: fewer_than_two_frames_refuses_at_plan_time`,
      `core: the_scope_is_the_selection_or_the_burst_under_the_cursor`,
      `app: export_frames_as_video_writes_a_real_motion_jpeg`
      (`clipavail=false` on a lone frame).
- [x] Orientation: portrait frames produce a rotated track matrix ffprobe
      reports as rotate 90/270; mirrored orientations degrade and report.
      → `core: qt::portrait_frames_turn_the_display_not_the_pixels`,
      `core: a_mirrored_frame_is_kept_degraded_and_counted`,
      `muxer: ffprobe_sees_the_rotation_of_a_portrait_export` (skipped
      without ffprobe; ffprobe 8.x spells it `rotation=-90`, older builds
      `rotate=270`, and both are accepted).
- [x] Files: name from first/last stem; clash question in all three
      answers; temp+commit with no partial under the final name after a
      simulated failure and after cancel; never into the RAW folder
      unless chosen; the RAW files and sidecars untouched (the ADR 0003
      tests extend to this module); `co64` offsets correct in a synthetic
      > 4 GB file (sparse fixture) — Windows CI included.
      → `core: the_name_is_the_frame_range`,
      `core: the_name_names_only_frames_that_are_in_the_file`,
      `core: a_taken_name_raises_the_question_and_each_answer_lands_somewhere_else`,
      `core: each_answer_to_the_clash_question_lands_where_it_says`,
      `core: an_unanswered_clash_question_writes_nothing`,
      `core: a_failure_mid_write_leaves_nothing_at_the_destination`,
      `core: a_cancelled_export_leaves_nothing_behind`,
      `core: the_raw_folder_is_a_legal_destination_when_it_is_chosen`,
      `core: the_raws_and_their_sidecars_come_out_untouched`,
      `core: planning_writes_nothing_at_all`,
      `core: qt::offsets_past_four_gigabytes_are_written_as_64_bit`,
      `app: the_video_export_asks_before_replacing_a_file`.
      *Two notes on this line.* The > 4 GB case is proven on the
      arithmetic AND on the written bytes (a 5.1 GB layout whose `co64`
      table is decoded out of the header) rather than with a sparse
      fixture: no test writes 4 GB to a CI runner's disk, and the check
      is hermetic, so it runs on Windows. And "never into the RAW folder
      unless chosen" differs from Copy Picks on purpose: the copy engine
      REFUSES that destination (it would drop copies of the RAWs beside
      the originals), while this module allows it when the user chooses
      it, because a `.mov` cannot collide with a RAW and "export the
      burst next to the shoot" is a real answer. "Not by default" is
      enforced by there being no destination at all until one is chosen
      or seeded from Copy Picks'.
- [x] Verified: a tampered byte in the written file is detected and the
      verified line withheld; the moov re-parse matches.
      → `core: a_byte_that_changed_on_the_way_to_disk_is_caught`,
      `core: a_moov_that_stopped_describing_the_samples_is_caught`,
      `core: the_green_light_is_only_for_a_run_that_earned_it`,
      `app (unit): pump::the_verified_line_of_a_video_export_is_earned`.
      The re-parse checks more than the sample table: the brand, `co64`,
      `moov`-before-`mdat`, the single `stts` entry, both frame sizes and
      the display matrix all have to describe THIS export.
- [x] Free space: refuses at plan time when the sum does not fit; a
      write failure mid-file removes the temp and reports.
      → `core: a_destination_that_cannot_hold_the_file_refuses_at_plan_time`,
      `core: a_failure_mid_write_leaves_nothing_at_the_destination`,
      `core: a_read_only_destination_fails_honestly_and_leaves_nothing`.
- [x] Hostile inputs: a JPEG with an EXIF orientation but no dimensions,
      a truncated embedded JPEG (the loupe's `no decodable preview` case),
      a 0-byte RAW, a name with spaces/Unicode, a very long name, a
      destination that is a file, a dangling-symlink destination, a
      read-only destination, a selection of 1000 frames (plan time, file
      size line, no memory growth — samples stream, never held in RAM).
      → `core: a_preview_with_no_dimensions_is_skipped`,
      `core: a_truncated_preview_is_copied_as_is_and_a_runaway_one_is_skipped`,
      `core: frames_that_cannot_share_the_track_are_skipped_and_reported`
      (the 0-byte RAW),
      `core: names_with_spaces_and_unicode_survive_into_the_file_name`,
      `core: an_impossible_name_refuses_before_anything_is_written`,
      `core: the_name_check_follows_the_suffix`,
      `core: a_destination_that_is_not_a_folder_refuses_at_plan_time`
      (a file AND a dangling symlink),
      `core: a_read_only_destination_fails_honestly_and_leaves_nothing`,
      `core: a_thousand_frames_plan_without_reading_a_single_sample`,
      `core: the_samples_stream_instead_of_piling_up_in_memory`,
      `core: qt::hostile_bytes_come_back_as_errors_not_panics`.
      *Recorded behaviour for the truncated case*: the export does not
      decode, so a frame whose declared bytes are inside the file is
      copied AS IS and lands in the video looking exactly as broken as it
      does in the loupe; a declared length that runs past the end of the
      RAW is not a frame at all and is skipped. Validating frames by
      decoding them would be the first step towards being an editor.
- [x] App: driven test — stand in a burst, Ctrl+Shift+E, Enter, the
      file lands and ffprobe/in-tree reader confirm it; the item disabled
      with no selection and no burst; marks unchanged after export; the
      dialog owns the keyboard (issue #41/#42 rules).
      → `app: export_frames_as_video_writes_a_real_motion_jpeg`,
      `app: the_video_export_asks_before_replacing_a_file`.
      **One deviation, recorded**: the driven test uses a SELECTION, not
      the burst under the cursor. All three reference RAWs in
      `testdata/raws` declare Sony `SequenceNumber = 0` — "single shot" —
      so no burst can be formed from them, and no fixture in the
      repository carries a burst sequence. The burst-under-cursor scope
      rule is covered by `core: the_scope_is_the_selection_or_the_burst_
      under_the_cursor`, and the app strand that proves the wiring reads
      the same burst index (the disabled-with-a-reason assertion). A
      driven burst strand needs a burst fixture, which is its own piece
      of work (a synthetic RAW with a Sony maker note).
- [x] Exported badge and hint (#56): a landed export badges exactly the
      frames that are IN the file and a skipped frame is never among them;
      a cancel or a failure badges nothing; an Overwrite with a different
      frame set drops the replaced file's frames; the badge and the hint
      follow the disk at the two re-check points and NOT per repaint; a
      session swap forgets every badge; the hint's sentence shapes; and
      the GRID actually paints the badge, in both of its positions.
      → `core: only_a_landed_export_hands_the_ledger_anything`
      (every spoiled variant of a report, and ids that are not the file's
      samples),
      `core: a_skipped_frame_is_never_among_the_ids_an_export_records`,
      `core: the_ledger_badges_only_frames_that_are_in_a_file_that_is_still_there`,
      `core: overwriting_a_video_drops_the_frames_it_no_longer_holds`,
      `core: a_name_written_again_supersedes_even_if_it_was_gone_in_between`,
      `core: the_hint_names_one_video_and_counts_the_others`,
      `core: the_hint_says_how_many_of_how_many`,
      `core: a_fresh_ledger_remembers_nothing_from_the_last_one`,
      `app (unit): state::clip_state_tests::a_session_swap_forgets_every_badge`,
      `app: an_exported_frame_wears_a_badge_until_its_video_is_gone`
      (one copy and two real exports, then the `.mov` deleted by a helper
      thread mid-run: nothing changes until the dialog re-opens, and then
      only the frame that was in the deleted file loses its badge — plus
      the PIXEL assertions below).
      *Two notes on the evidence.* The badge is asserted **in the cells**,
      not only in the ledger: the driven test's final screenshot carries
      all three layouts at once (bare, ✓ + stepped `▶`, `▶` alone in the
      ✓'s slot) and each pill's horizontal extent is MEASURED in the badge
      band, with the ✓'s greenness read against the same rectangle of a
      cell that has no badge and the `▶` glyph's monochrome rendering
      mechanized as "bright and neutral strokes" (a colour-emoji bitmap
      ignores the `color` the UI gives it). What is asserted is the pill's
      LEFT EDGE — x 8's slot, or x 28 when a ✓ is in the way — plus a
      14..=34 px width — "a pill, not the photograph", and not two of them
      run together — because the width belongs to the FONT: the Windows
      runner draws `▶` boxed, so the same pills measure 19 px on the
      ubuntu runner, 21 px on the development seat and 26 px there (PR
      #71's two artifacts, x 9..28 / 28..47 against x 9..35 / 28..54 — the
      same left edges). The fixed rectangle this replaced was that run's
      only red (issue #70); the rule is in ui-grid.md's test ledger.
      Sending `exported: false`, deleting the Slint block and removing the
      badge's 28 px step were each confirmed RED against the criterion's
      ORIGINAL dark-fraction form; against the edge form, the mutants
      re-run are the pixel-equivalent ones — a pill shifted 20 px out of
      its slot, and a pill removed. And "reads, never decides" is
      structural in core
      (`plan` has no ledger parameter, which is what
      `core: the_ledger_never_changes_what_the_next_export_writes` states
      and would catch); its BEHAVIOURAL proof is the driven test's second
      export, which plans all three frames with two of them already
      badged.
- [x] Perf: 30 A1 frames export in < 2 s on the reference laptop (release,
      idle) — an I/O-bound budget, added to perf_budgets.rs.
      → `perf: budget_video_export_30_frames_under_2s`; measured 527 ms
      for 327 MB on the reference laptop, 2026-08-27.
- [x] The skipped sentence is BOUNDED (at most three reasons named, the
      rest folded into a tail whose counts add up), and neither dialog's
      button row can leave its card — at the floor, grown to its content,
      or clamped at the ceiling with the body scrolling (issue #62).
      → `core: a_long_list_of_reasons_is_bounded_and_still_adds_up`,
      `core: the_tail_names_a_kind_only_when_the_whole_tail_is_that_kind`,
      `app: a_long_refusal_keeps_the_export_buttons_inside_the_card`
      (refusal, plan, report and a 640×300 window),
      `app: a_failure_report_longer_than_the_window_keeps_the_copy_buttons_inside_the_card`
      (the Copy Picks half, Unix — a `chmod 555` destination).
      RED on the parent tree 2026-08-30: the row ended 29 px outside the
      export card and 830 px outside the Copy Picks card.
- [ ] USER-VERIFIED (2026-08-27, not automatable): InShot on the phone
      imports and plays a 2880×1920 MJPEG `.mov` and the untouched
      8640×5760 30-frame file. NOT VERIFIED: portrait rotation honoured by
      InShot; playback on iOS; 4:2:0 or progressive JPEGs from other
      bodies. **Still open after M9**: the phone test was run against
      ffmpeg's own file, and the file this module now writes has the same
      layout minus `edts`/`udta`/`wide` and with `co64` in place of
      `stco`. It decodes identically under ffmpeg (verified 2026-08-27:
      byte-identical frames, correct rotation), but nobody has put THIS
      file on the phone.
