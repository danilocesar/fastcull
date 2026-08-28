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
   frame.
2. **Gaps are not preserved.** A gap larger than the median plays as one
   frame step: two bursts selected together play back to back, the pause
   between them dropped. Recorded alternative, rejected: true per-frame
   durations (QuickTime allows it) would make a 5 s pause a 5 s freeze
   inside the clip and produce variable-frame-rate files that some editors
   mishandle; the phone editor is where pacing is decided.
3. **Fallbacks, reported in the completion line.** Frames lacking SubSec
   (1 s granularity: gaps of 0 or 1000 ms) or lacking a timestamp at all →
   the cadence cannot be measured → **15 fps**, and the report says
   "timing not in the files — assumed 15 fps". A median gap below 10 ms or
   above 1000 ms (two bodies interleaved, or a selection of singles) →
   clamped to [10 fps, 120 fps] and reported likewise. Duration and fps
   are shown in the plan line before the user confirms, so a wrong cadence
   is visible before a byte is written — and the plan line carries the
   SAME "assumed 15 fps" / "clamped" wording as the report (persona: it
   must be impossible to miss before Enter). When the cadence was
   measured, neither line says where it came from; the parenthesis appears
   only in the fallback cases.

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
- A frame whose **EXIF orientation** differs from the first frame's is
  skipped and reported the same way.
- A frame with **no usable embedded JPEG** (the loupe's `no usable embedded
  preview` badge) is skipped and reported.
- The fullres source is `EmbeddedPreviews::fullres()` — the largest
  embedded JPEG, the loupe's own source; for CR3/RAF the rawler fallback
  applies (raw-pipeline.md), whatever it yields.

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
- **Free space**: sum of the JPEG lengths + a header allowance, checked at
  plan time like Copy Picks; a destination whose filesystem cannot hold a
  file of that size (FAT32 above 4 GB) fails honestly at write time with
  the OS error and the temp file removed — recorded, not pre-detected
  (there is no portable way to ask).

## Dialog (minimums)

Menu: **File › Export Frames as Video…** (the wording is the user's;
"video", not "clip", in the menu), keystroke **Ctrl+Shift+E** — beside
Copy Picks' Ctrl+E as "the other exit"; a modifier chord so it cannot
fire from a fat finger mid `]`/`N` (persona 2026-08-27). Disabled with its
reason in the status line, never a silent grey item. One dialog in the
Copy Picks style:
destination row with a Choose… button and the remembered path; one plan
line — *"30 frames · 8640×5760 · 30 fps (from the camera's timestamps) ·
1.0 s · 328 MB → DSC05010-DSC05039.mov · 358 GB free"*; a skipped line
when there is one; **Export** (Enter, when the plan is clean) and Cancel
(Esc); progress "n / N" with Cancel while writing; the report with the
verified line and an Open folder action. No other control. The clash
question is the same dialog state as Copy Picks. Modal, keyboard-contained
(issue #42 rules), never marks, never moves the cursor.

## Follow-ups logged, not in M9 (persona 2026-08-27)

- **"Select this burst" / extend the selection to the next burst**
  (Shift+`]` is the natural pair of `]`): the common case — the burst under
  the cursor — needs no selection, but "burst 40 plus burst 41" is
  Shift+arrow over 60 frames today. A burst-grouping/selection change with
  its own gate; tracked as an issue.
- **An "exported as video" badge** on the burst, like the Copy Picks
  checkmark: USEFUL, not must-have; a new badge surface, deferred.

## Explicitly not built (panel rule, one year from release)

No crop, no scale, no rotation of pixels, no fps choice, no speed/loop/
bounce, no format choice, no audio, no montage, no per-frame timing, no
GIF/WebP, no bundled or downloaded ffmpeg, no H.264/AV1 encoder (revisit
only on ≥3 unsolicited requests for a re-encoded output after this ships,
and never without the user's own licence decision). README: one bullet
under the exports, never the headline.

## Platform

Linux and Windows, first-class both (user requirement 2026-08-27): the
muxer is pure Rust and the export is file I/O only, so CI verifies it on
both runners with the in-tree reader; ffprobe-based checks run where
ffprobe exists and are skipped (not failed) elsewhere, and the acceptance
list says which claims are CI-verified on Windows and which are
review-only.

## Acceptance criteria (tests)

- [ ] Muxer golden file: 3 reference frames → a `.mov` whose atom tree
      matches the pinned golden (ftyp `qt  `, moov-before-mdat, `jpeg`
      sample entry, timescale 1000, `co64`, stts one entry, tkhd identity
      matrix) and which ffprobe reports as `mjpeg, 8640x5760, yuvj422p,
      30 frames` where ffprobe exists — byte-identical samples proven by
      hashing the mdat ranges against the source JPEGs.
- [ ] Cadence: 30 frames with 33 ms gaps → 30 fps; a selection of two
      bursts with a 4 s pause → the median, pause dropped, N frames in
      capture order; gaps at 1 s granularity → 15 fps + the report line;
      no timestamps → 15 fps + the report line; interleaved two bodies →
      capture-sorted merge, clamped cadence, reported.
- [ ] Order: the file is in capture order whatever the grid sort;
      filename tiebreak for equal timestamps.
- [ ] Uniformity: a different-size frame, a different-orientation frame,
      and a no-preview frame are skipped and reported; < 2 frames left
      refuses at plan time; a single-frame selection disables the item.
- [ ] Orientation: portrait frames produce a rotated track matrix ffprobe
      reports as rotate 90/270; mirrored orientations degrade and report.
- [ ] Files: name from first/last stem; clash question in all three
      answers; temp+commit with no partial under the final name after a
      simulated failure and after cancel; never into the RAW folder
      unless chosen; the RAW files and sidecars untouched (the ADR 0003
      tests extend to this module); `co64` offsets correct in a synthetic
      > 4 GB file (sparse fixture) — Windows CI included.
- [ ] Verified: a tampered byte in the written file is detected and the
      verified line withheld; the moov re-parse matches.
- [ ] Free space: refuses at plan time when the sum does not fit; a
      write failure mid-file removes the temp and reports.
- [ ] Hostile inputs: a JPEG with an EXIF orientation but no dimensions,
      a truncated embedded JPEG (the loupe's `no decodable preview` case),
      a 0-byte RAW, a name with spaces/Unicode, a very long name, a
      destination that is a file, a dangling-symlink destination, a
      read-only destination, a selection of 1000 frames (plan time, file
      size line, no memory growth — samples stream, never held in RAM).
- [ ] App: driven test — stand in a burst, Ctrl+Shift+E, Enter, the
      file lands and ffprobe/in-tree reader confirm it; the item disabled
      with no selection and no burst; marks unchanged after export; the
      dialog owns the keyboard (issue #41/#42 rules).
- [ ] Perf: 30 A1 frames export in < 2 s on the reference laptop (release,
      idle) — an I/O-bound budget, added to perf_budgets.rs.
- [ ] USER-VERIFIED (2026-08-27, not automatable): InShot on the phone
      imports and plays a 2880×1920 MJPEG `.mov` and the untouched
      8640×5760 30-frame file. NOT VERIFIED: portrait rotation honoured by
      InShot; playback on iOS; 4:2:0 or progressive JPEGs from other
      bodies.
