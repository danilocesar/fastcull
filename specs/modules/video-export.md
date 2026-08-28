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
  Three details settled at implementation time (2026-08-27): the stems
  are the FIRST AND LAST FRAME IN THE FILE, never a skipped one, or the
  range would name frames the user cannot find inside it; two equal stems
  (the same name with two extensions) collapse to `<stem>.mov` rather
  than `a-a.mov`; and a composed name over **255 bytes** — the per-name
  limit of every mainstream filesystem, reachable from two long stems —
  is refused at PLAN time, because discovering it at commit time would
  cost the user the whole write first.
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
- **The dialog is keyboard-contained in every state** (issue #42), and
  this was verified by driving the real app rather than assumed: with the
  dialog up, `Y`/`N` mark nothing, `Ctrl+O` does not open the folder
  picker, and `Ctrl+E` does not raise the Copy Picks dialog underneath.
  The clash question additionally swallows everything that is not `B`,
  `O` or `Esc`, and says out loud that it is still waiting.
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
- [x] Perf: 30 A1 frames export in < 2 s on the reference laptop (release,
      idle) — an I/O-bound budget, added to perf_budgets.rs.
      → `perf: budget_video_export_30_frames_under_2s`; measured 527 ms
      for 327 MB on the reference laptop, 2026-08-27.
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
