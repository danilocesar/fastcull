# Module spec: export frames as video (`clip.rs`, `clip/qt.rs`)

## Purpose

A second exit beside Copy Picks: the selected frames — usually a burst that
produced "interesting cinematic, but no good shot" — become ONE video file
made of the camera's own embedded full-res JPEGs, so the burst can be edited
and posted from a phone video editor. The user's words (2026-08-27): *"All
the frames selected, their high resolution JPEGs, exported in video format.
Any edits, crops, effects should be done by an external editor."* The design
principle, signed by a three-persona review: **FastCull may hand frames to
an editor; it is never one.** The export has no options.

## Behaviour

### What is exported

- **The selection**, when there is one — the selected ids in view order,
  deliberately NOT `Selection::batch`, whose cursor fallback would make one
  frame a video (`clip_bridge.rs`); otherwise **the burst under the cursor**; neither → the menu item
  is disabled with its reason ("select frames or stand in a burst"). One
  frame is not a video: the item is disabled for a single frame too.
- A selection means the selected frames that are IN THE VIEW — what you see
  is what you stamp, the number the status bar prints; a burst means the
  whole burst, members the filter hides included (a burst is a fact about
  capture times).
- **Capture order, always**: the selection is a set; the file is ordered by
  the capture-time sort key (filename as tiebreaker) whatever the grid's
  sort. A video that plays backwards because the grid was sorted by name
  descending is a bug.
- Pick state is irrelevant and untouched — the export reads marks like Copy
  Picks does and never writes them; by definition the frames are usually
  rejects. Nothing consumes the selection: a finished export leaves it as
  it was, Cancel changes nothing, and Esc in any dialog state closes the
  dialog with the selection intact — the next plain move, or a second Esc
  on the grid, ends it (the user rejected auto-deselect: "I don't think
  auto deselecting is intuitive"). The per-burst rhythm needs no Esc: a
  plain `]` collapses the selection (ui-grid.md's selection rule), so
  "export this burst, `]`, export the next" takes the burst under the
  cursor the second time.
- **Every frame keeps its whole image.** No crop, no scale, no rotation of
  pixels.

### Motion JPEG, bytes untouched

The file is a QuickTime `.mov` whose video track is Motion JPEG: every
sample is the RAW's embedded full-res JPEG **copied byte for byte** — no
decode, no scale, no crop, no re-encode, no colour conversion. Why:

- No H.264 encoder that a static GPL binary can ship cleanly on Linux and
  Windows exists today (OpenH264 compiled from source is outside Cisco's
  royalty cover until 29 Nov 2027; the pure-Rust AV1 encoder took 110 s for
  30 frames and Meta does not accept AV1). Motion JPEG needs no encoder and
  no licence.
- The user tested a 2880×1920 MJPEG `.mov` and the real artefact — 30
  untouched 8640×5760 camera JPEGs at 15 fps, 328 MB — in InShot on the
  phone: "all worked".
- It is I/O bound: extracting 30 frames is a byte copy (0.21–0.24 s for
  327 MB); a decode → crop → re-encode path measured 3.2 s and 1.7 GB peak.
- The Sony full-res JPEG is 4:2:2 (`yuvj422p`) and the phone decoded it;
  other bodies may embed 4:2:0 or progressive JPEGs — a body whose full-res
  JPEG a phone will not play is a support question, not a design one.

Accepted with it: the file is large (~11 MB per A1 frame; a 400-frame
selection is 4.4 GB), and the frames are exactly what the camera rendered
(ADR 0001's trade-off). A 4K-scaled variant is deliberately NOT offered: it
would be a re-encode, the first step into the editor.

### The cadence: the camera's own (user decision 2026-08-27)

Sony writes `SubSecTimeOriginal` to the millisecond; the app already turns
`DateTimeOriginal` + `SubSecTimeOriginal` into `FrameMeta::time_ms` for
burst grouping. The export uses it:

1. **Constant frame rate from the median gap.** Frames sorted by capture
   time; the sample duration is the MEDIAN gap in milliseconds (timescale
   1000). A 30 fps A1 burst reads ~33 ms gaps → 30.3 fps, 1 s for 30
   frames. The median, not the mean, so one pause (two squeezes selected
   together) does not stretch every frame. Only a pair of CONSECUTIVE
   frames that BOTH carry SubSec precision contributes a gap — a
   whole-second timestamp dropped into a burst would add a spurious 1000 ms
   sample.
2. **Gaps are not preserved.** A gap larger than the median plays as one
   frame step: two bursts selected together play back to back. True
   per-frame durations were rejected (a 5 s pause would freeze the clip and
   some editors mishandle variable frame rate); pacing is decided in the
   phone editor.
3. **The window**: the sample duration is the median gap clamped into
   [9 ms, 100 ms] — 100 ms is exactly the promised 10 fps floor; 9 ms is
   111 fps, the fastest whole millisecond inside the promised 120 fps
   ceiling. **Fallbacks are said in the plan line and the report, in the
   same words**: no pair of frames with SubSec precision → 15 fps and
   "timing not in the files — assumed 15 fps"; a median outside the window
   (two bodies interleaved, a selection of singles) → clamped and "gaps of
   4.0 s — clamped to 10 fps". A measured cadence is just "30.3 fps".
   Duration and fps are shown before the user confirms, so a wrong cadence
   is visible before a byte is written.

### The container

QuickTime, one video track, `jpeg` sample description (the layout ffmpeg
produces for `-c:v copy` from a JPEG sequence, which the phone accepted;
the golden file pins it). Timescale 1000. `moov` BEFORE `mdat` — every
sample size is known from the plan, so the file plays while it is still
being transferred. **64-bit chunk offsets (`co64`) always**: a 4 GB+ file
is routine here. `ftyp` major brand `qt  `. No audio track. The track's
display matrix carries the frames' EXIF orientation. Written by an in-tree,
dependency-free muxer (`clip/qt.rs`) — no crate on crates.io writes a
`jpeg` sample entry, and the atom set is small — golden-file tested like
the XMP serializer, with an in-tree reader that re-parses what it wrote.

### Uniformity: skip, never scale

All samples in a Motion JPEG track share one frame size and one
orientation; the first frame in capture order sets both.

- A frame with **different dimensions** (a crop-mode shot, a different
  body, a file whose only embedded JPEG is a smaller preview) is skipped and
  reported ("2 frames skipped: different size (5616×3744)"). Scaling would
  be a re-encode, padding an edit. Fewer than 2 frames left → the export
  refuses at plan time.
- A frame whose **EXIF orientation** differs from the first's is skipped and
  reported the same way; so is a frame with **no usable embedded JPEG**
  (the loupe's `no usable embedded preview` badge). The source is
  `EmbeddedPreviews::fullres()`, the loupe's own; a container the in-tree
  walker cannot read (CR3, RAF) has no frame here for the same reason it has
  no picture in the loupe — the rawler fallback is a half-size RAW *decode*,
  pixels, and an export needs a byte range. A selection made entirely of
  them refuses at plan time.
- **The sentence is bounded** (issue #62): at most THREE reasons are named,
  the biggest groups first, the rest folded into one tail whose counts add
  up — *"skipped — 4 frames: different size (390×400) · 3 frames: different
  size (380×400) · 2 frames: no usable embedded JPEG · 11 more frames in 9
  other sizes"*. The tail names its kind (`in N other sizes`, `in N other
  orientations`) only when the whole tail is that kind, and says `for N
  other reasons` otherwise — a shorter sentence may never become a wrong
  one. `skipped_text` is the one place this happens, so the plan line, the
  refusal and the report are bounded together.

### Orientation

Pixels are never rotated (a re-encode). Portrait frames (orientation 6/8)
stay in sensor orientation inside the samples and the track matrix in
`tkhd` rotates the display, as phone cameras record portrait video;
orientation 1 is the identity. Mirrored orientations (2/4/5/7) are kept,
degraded to their unmirrored rotation (1/3/8/6), and the report says how
many — skipping them would drop frames over a flip the matrix cannot
express.

### Files: the Copy Picks contract (ADR 0004)

- **Name**: `<first stem>-<last stem>.mov` in capture order
  (`DSC05010-DSC05039.mov`) — the stems are the first and last frame IN THE
  FILE, never a skipped one; two equal stems collapse to `<stem>.mov`.
  Rename templates do not apply. A name over 255 bytes — reachable from two
  long stems — is refused at PLAN time, on the name THIS plan would write,
  `_k` suffix included: a 255-byte name whose destination is occupied still
  gets the clash question, Overwrite still works, and only Keep both is
  refused, by the replan that answer triggers, before a byte is written.
- **Destination**: a folder the user chooses, remembered across sessions as
  `clip_dest` in `ui.toml`, read-modify-written beside `copy_dest`. Seeded
  from the Copy Picks destination until a clip folder is first chosen
  (persona: on an ordinary evening the selects folder is where today's
  output goes). Never the RAW folder by default; allowed when chosen — a
  `.mov` cannot collide with a RAW, and "export the burst next to the
  shoot" is a real answer (Copy Picks refuses that destination for its own
  reason).
- **Clash**: the same question as Copy Picks (fileops.md) minus New only —
  Keep both (`_1`, `_2`, …) / Overwrite / Cancel — because this export
  writes one file and for one file "skip" is Cancel. The policy enum is
  shared, so the planner maps New only to the refused `Clash` marker: a
  wiring mistake that ever sent that policy here writes nothing and
  replaces nothing. Nothing is ever replaced without the Overwrite answer.
- **Write**: the copy engine's shape — one worker thread, a progress event
  per frame, cancel between frames, a unique temp name, no-clobber commit
  (`hard_link` + unlink; `rename` only for an answered Overwrite), never a
  partial file under the final name; a hard quit leaves at most one hidden
  `.fastcull-partial-*`. Every sample's bytes are read with `read_exact`
  against the length in the header: a file that shrank between the plan
  and the write fails the export honestly.
- **Verified**: every sample is BLAKE3-hashed on the way in; the finished
  file is re-read and each sample range re-hashed; the `moov` is re-parsed
  by the in-tree reader and must describe exactly the samples written — the
  brand, `co64`, `moov` before `mdat`, the single `stts` entry, both frame
  sizes and the matrix. "All checksums verified" appears only when all of
  that passed for every frame (`ClipReport::earned_the_green_light`, in
  core, as for Copy Picks).
- **Free space**: the sum of the JPEG lengths plus the header — an exact
  number, since every sample size is known — checked at plan time. The
  refusal reads *"This video would be 4.5 GB and there is 1.1 GB free at the
  destination."*, both sizes through the shared formatter (fileops.md,
  *Sizes on screen*). A filesystem that cannot hold a file of that size
  (FAT32 above 4 GB) fails honestly at write time with the OS error and the
  temp removed — recorded, not pre-detected.

### The dialog

- Menu **File › Export Frames as Video…** (the user's wording — "video",
  never "clip", in the menu), keystroke **Ctrl+Shift+E** —
  beside Copy Picks' Ctrl+E as "the other exit", a chord so it cannot fire
  from a fat finger mid `]`/`N`. The item is disabled when there is nothing
  to export, but the chord always works: it appends *"Export Frames as
  Video: select frames or stand in a burst"* (or *"one frame is not a
  video — select more, or stand in a burst"*) to the status bar for six
  seconds, and the message vanishes the moment it stops being true.
- One dialog in the Copy Picks style: a destination row with Choose… and
  the remembered path; one plan line — *"30 frames · 8640×5760 · 30.3 fps ·
  1.0 s · 328.4 MB → DSC05010-DSC05039.mov · 358.2 GB free"* — whose cadence
  field carries the fallback wording when there is one; a skipped line when
  there is one; the exported hint (below); **Export** (Enter, when the plan
  is clean) and **Cancel** (Esc, and a button — a mouse-only user needs a
  way out); progress "n / N" with Cancel while writing; the report with the
  verified line — *"Exported 30 frames · 1.0 s · 30.3 fps · 328.4 MB →
  DSC05010-DSC05039.mov, all checksums verified"* — and an Open folder
  action. No other control. A cancelled export says "nothing was written":
  it produces one file and never commits it before it is verified. The
  clash question is a state of the same dialog; its Keep both row leads
  with the number (*"Keep both (_1) — the video lands as …"*) and carries no
  cost column.
- **The report's wording is the plan's wording**: `Cadence::text` and
  `skipped_text` live in core and both surfaces call them, so the sentence
  the user agrees to before Enter is the sentence read afterwards.
- Modal and keyboard-contained in every state (issue #42): `Y`/`N` mark
  nothing, `Ctrl+O` does not open the picker, `Ctrl+E` does not raise Copy
  Picks; the clash question swallows everything but `B`, `O` and `Esc` and
  says so. The dialog never marks, never moves the cursor, never touches
  the selection.
- **The card's height follows its content** (issue #62): a floor of 260 px
  (380 px while the clash question is up), the window as the ceiling
  (`parent.height - 40px`), and past the ceiling the text body scrolls in a
  `ScrollView` (not a bare `Flickable`, which scrolled but showed nothing to
  say so and clipped a cut-off report to look complete, 2026-08-30) — by
  wheel, and by Down/Up (a line, 40 px), PgDn/PgUp (a body),
  Home/End, only while it overflows. The header rows and the button row
  never give up a pixel, so the buttons stay inside the card wherever the
  window can hold the fixed rows; below ~300 px of window height they
  cannot, the row leaves the card, and that is accepted — a 560 px-wide card
  is not a dialog anyone can use there. A wheel over the dialog scrolls its body
  or nothing — never the grid behind the scrim (issue #49). Below ~500 px
  of window height the clash answer rows need a scroll to come into view
  and answer by key wherever the body stands (recorded; not a size the app
  is designed for).
- **Planning opens every frame on the UI thread, and that is affordable
  here**: the plan needs each frame's embedded-JPEG offset, length and
  size — one file open and a few KB of targeted reads per frame — and it is
  built when the dialog opens, when the destination changes and when the
  user commits: a handful of times over tens of frames, not per keystroke
  (this dialog has no text field). A text input here, or a routine
  thousand-frame selection over a network mount, would move the probe to a
  worker thread. The plan built for the preview is never the one that
  runs: the frame set is probed twice per export.

### The exported badge and the hint (issue #56)

*"Which of these did I already export?"* — two surfaces, one memory, and
the memory is SESSION-ONLY: it starts empty on every folder open and dies
with the process, exactly the ✓ copied badge's promise. Nothing is written
to `previews.db` (a disposable cache is a memory whose absence cannot be
trusted — persona IN-MY-WAY) nor to a sidecar (a private flag in 30 XMPs
handed to darktable, Lightroom and Bridge is disproportionate for a hint).
The dangerous case — re-exporting the same span — is already caught by the
`.mov` name clash question, which is why this may be a hint.

- **Reads, never decides.** `clip::ExportLedger` has the shape of
  `fileops::SessionCopies` and its rule: it feeds the badge and the hint and
  nothing else — never a plan, an answer, a mark, or which frames the next
  export takes (`plan` has no ledger parameter).
- Recorded: the plan's KEPT frames against the committed path, only for a
  run that landed — `ClipReport::frames_to_record` gates on
  `earned_the_green_light()` plus the ids matching the file's sample count,
  so the badge and the verified line can never disagree. A skipped frame is
  never badged. An Overwrite supersedes the entry of the same canonical
  path, taking the badge off the frames of the file that is gone.
- **Follows the disk at two moments only** — when an export finishes and
  when the export dialog opens (Copy Picks re-checks on every replan; this
  dialog has no field, so its open is the better placement). Never per
  repaint: the grid asks the ledger once per visible cell on every repaint,
  and a `stat` there would be a storm. An unplugged drive means no badges —
  a false negative, the safe direction; not to be "fixed" with a
  last-known-present flag. Accepted cost: one `stat` per export made this
  session, on the UI thread, at dialog open.
- **The badge**: `▶` (U+25B6, monochrome in the app's font on Linux — the
  Windows runner draws it boxed, 26 px against 19, an accepted residual; `▸`
  is the recorded fallback for a colour-emoji face) on every frame that went into a clip, bottom-left,
  immediately right of the ✓ when there is one (the ✓ keeps `x: 8px`), in
  the ✓'s place when there is not; the `×N` burst pill keeps the
  bottom-right. Per FRAME, not per burst — the export's scope is an
  arbitrary set. In the `×N` pill's palette (`#d8d8e0` on `#202028cc`), not
  ✓'s green: green is the data-safety signal and this is not one, and a
  bare glyph washes out under the 40 % reject dim these frames usually
  wear. Visible in the loupe, like ✓ and ×N. Residual, accepted: at the
  12-column zoom on a window narrower than ~1200 px the `▶` and `×N` pills
  can touch, the same crowding ✓ and ×N always had.
- **The hint**: ONE line in the plan preview, under the plan line and the
  skipped line — *"3 of 30 frames are already in DSC05010-DSC05039.mov"*,
  *"all 30 frames are already in …"*, or, spread over several videos, *"5
  of 30 frames are already in 3 videos — DSC05010-DSC05039.mov and 2 more"*.
  The count binds to the VIDEOS, never to the named one (naming one file
  beside a count it does not hold is a claim the user cannot check). One
  line means elided, not wrapped, so the name is LAST and every count comes
  before it (`and N more` is the one thing allowed behind it — it repeats the
  video count already stated). Counted over the SCOPE the user chose, not the plan's kept
  frames, so the line stands when the plan itself refuses. Plan state only;
  grey, not amber — the skipped line above it is the warning and must stay
  the loudest thing in the card. The wording is core's
  (`clip::exported_hint`).

### Limits named, not handled

- Three Windows name limits (review-only; unreachable on the Linux seat):
  `MAX_NAME_BYTES` counts bytes while NTFS counts 255 UTF-16 units (for
  ASCII they coincide; 130 Cyrillic characters are refused here and
  accepted by NTFS — conservative, never wrongly permissive); Windows'
  260-character `MAX_PATH` applies to the whole path, so an accepted name
  can still fail at commit in a deep destination without long-path
  support; reserved names (`CON`, `NUL`, `AUX`, a trailing dot or space) are not
  checked — reachable only through the equal-stem collapse from a share
  written by another OS.
- A second export that loses a race for the same `_k` name gives up ("a
  file appeared at the destination during the copy") rather than walking
  on to `_k+1`: the no-clobber behaviour, and the safe direction.
- Two limits of cancel: the `fsync` between the write and the read-back is
  one kernel call with no polling point, so a cancel during it waits out
  the flush (seconds on a 4.4 GB export); and a cancel after the commit is a
  FILE, not a nothing — a session swap looks at a flag the worker sets the
  moment the file takes its name (not at the destination path, which under
  Overwrite is occupied by yesterday's file from the start) and says "the
  video had already finished: <name>".

### Explicitly not built (panel rule, one year from release)

No crop, no scale, no rotation of pixels, no fps choice, no speed/loop/
bounce, no format choice, no audio, no montage, no per-frame timing, no
GIF/WebP, no bundled or downloaded ffmpeg, no H.264/AV1 encoder (revisit
only on ≥ 3 unsolicited requests for a re-encoded output after this ships,
and never without the user's own licence decision). README: one bullet
under the exports, never the headline. On the export memory (issue #56):
no clips panel or list, no filter or sort by "exported", no auto-reject of
exported frames, no status-bar count, no cache table, no sidecar property.

### Platform

Linux and Windows, first-class both (user requirement): the muxer is pure
Rust and the export is file I/O only, so CI verifies it on both runners
with the in-tree reader; ffprobe-based checks run where ffprobe exists and
are skipped, not failed, elsewhere.

## Contracts

- Core owns the sentences and the rules: `Cadence::text`, `skipped_text`,
  `clip::exported_hint`, `ClipReport::earned_the_green_light`,
  `ClipReport::frames_to_record`, `clip::ExportLedger` (reads, never
  decides), `MAX_NAME_BYTES`; the muxer and reader in `clip/qt.rs`.
- ADR 0004's derived-output contract: only into a folder the user chose; a
  RAW never opened for writing, a sidecar never modified; nothing replaced
  without Overwrite, nothing deleted; temp name + no-clobber commit;
  verified by hashing every byte in and re-reading it out; built from bytes
  the RAW already contains, never a re-encode.
- Shared with Copy Picks (fileops.md): the clash policy enum (New only maps
  to `Clash` here), the byte formatter, the card-height rule, `ui.toml`
  read-modify-written.
- For the driven suite (test-harness.md): the marks `clip export finished
  run N` (fires when the report card goes up; a run cancelled by a session
  swap emits none), `clip card laid out …`, `clip buttons laid out …`,
  `clip body scrolled to Y`; the dump fields `clip=`, `clipstate=`,
  `clipavail=`, `clipsummary=`, `clipskipped=`, `cliperror=`,
  `clipreport=`, `clipconfirm=`, `clipprogress=`, `cliphint=`, `exported=`,
  `curexported=`;
  the tokens `clipdest:PATH` (before the `Ctrl+Shift+E` that should see
  it), `key:ctrl+shift+e`.

## Acceptance criteria

Every box names the test that holds it. Unless a line says otherwise the
test is hermetic — neither the sample RAWs nor ffmpeg — and runs on the
Windows runner too. `core:` = a `fastcull-core` unit test, `muxer:` =
`tests/clip_muxer.rs` (needs the sample RAWs), `app:` = a driven
`tests/screenshot.rs` test.

- [x] Muxer golden file: the 3 reference frames → a `.mov` whose atom tree
      matches the pinned golden (ftyp `qt  `, moov before mdat, `jpeg`
      sample entry, timescale 1000, `co64`, one `stts` entry, identity
      `tkhd`), which ffprobe reports as `mjpeg, 8640x5760, yuvj422p, 3
      frames` where ffprobe exists; samples byte-identical by hashing the
      mdat ranges — `core: qt::the_container_layout_is_pinned_to_a_golden_file`,
      `core: qt::the_reader_confirms_what_the_writer_promised`,
      `muxer: the_real_reference_frames_produce_the_golden_header`,
      `muxer: every_sample_is_the_camera_jpeg_byte_for_byte`,
      `muxer: ffprobe_agrees_it_is_motion_jpeg`.
- [x] Cadence: 33 ms gaps → 30 fps; two bursts with a 4 s pause → the
      median, pause dropped; 1 s granularity → 15 fps and the line; no
      timestamps → 15 fps and the line; two bodies interleaved → the
      capture-sorted merge, clamped and said so; the cadence explains
      itself only when it had to —
      `core: the_median_gap_is_the_frame_duration`,
      `core: a_pause_between_two_bursts_does_not_stretch_every_frame`,
      `core: one_second_granularity_falls_back_to_fifteen_fps`,
      `core: only_millisecond_pairs_measure_a_gap`,
      `core: implausible_gaps_are_clamped_and_said_so`,
      `core: two_interleaved_bodies_merge_in_capture_order_and_clamp`,
      `core: the_cadence_only_explains_itself_when_it_had_to`.
- [x] Order: capture order whatever the grid sort, filename tiebreak —
      `core: capture_order_sorts_by_time_then_name_then_the_untimed`,
      `core: the_plan_is_in_capture_order_and_names_the_range`,
      `app: export_frames_as_video_writes_a_real_motion_jpeg` (fixtures named
      so capture order and name order disagree).
- [x] Uniformity: a different-size frame, a different-orientation frame and
      a no-preview frame are skipped and reported; < 2 frames refuses at
      plan time; a single frame disables the item —
      `core: frames_that_cannot_share_the_track_are_skipped_and_reported`,
      `core: the_first_usable_frame_sets_the_track`,
      `core: fewer_than_two_frames_refuses_at_plan_time`,
      `core: the_scope_is_the_selection_or_the_burst_under_the_cursor`,
      `app: export_frames_as_video_writes_a_real_motion_jpeg` (`clipavail=false`
      on a lone frame).
- [x] Orientation: portrait frames produce a track matrix ffprobe reports
      as rotate 90/270 (`rotation=-90` on ffprobe 8.x, `rotate=270` on
      older builds — both accepted); mirrored frames degrade and report —
      `core: qt::portrait_frames_turn_the_display_not_the_pixels`,
      `core: a_mirrored_frame_is_kept_degraded_and_counted`,
      `muxer: ffprobe_sees_the_rotation_of_a_portrait_export`.
- [x] Files: the name from first/last stem, naming only frames in the file;
      the clash question in all three answers, an unanswered question
      writes nothing; temp + commit with no partial under the final name
      after a simulated failure and after cancel; the RAW folder is a legal
      destination when chosen; RAWs and sidecars untouched (the ADR 0003
      tests extend here); `co64` offsets correct past 4 GB — proven on the
      arithmetic and on the written bytes of a 5.1 GB layout decoded out
      of the header, hermetic, so it runs on Windows —
      `core: the_name_is_the_frame_range`,
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
      `app: the_video_export_asks_before_replacing_a_file` (whose `n` round
      asserts New only leaves the question up; the nudge it raises is
      review-verified only).
- [x] Verified: a tampered byte in the written file is caught and the
      verified line withheld; a `moov` that stopped describing the samples
      is caught; the green light only for a run that earned it —
      `core: a_byte_that_changed_on_the_way_to_disk_is_caught`,
      `core: a_moov_that_stopped_describing_the_samples_is_caught`,
      `core: the_green_light_is_only_for_a_run_that_earned_it`,
      `app (unit): pump::the_verified_line_of_a_video_export_is_earned`
      (`344.0 MB`).
- [x] Free space: refuses at plan time when the sum does not fit; a write
      failure mid-file removes the temp and reports; the refusal sentence —
      `core: a_destination_that_cannot_hold_the_file_refuses_at_plan_time`,
      `core: a_read_only_destination_fails_honestly_and_leaves_nothing`,
      `clip_bridge::the_two_untestable_messages_now_have_a_test` (`4.5 GB`,
      `1.1 GB`).
- [x] Hostile inputs: a JPEG with orientation but no dimensions, a
      truncated embedded JPEG (copied AS IS when its declared bytes are
      inside the file — validating by decoding would be the first step
      towards being an editor; skipped when the declared length runs past
      the end of the RAW), a 0-byte RAW, spaces and Unicode in names, a
      very long name, the name check following the suffix, a destination
      that is a file or a dangling symlink, a read-only destination, a
      1000-frame selection that plans without reading a sample and streams
      instead of piling up in memory, hostile bytes to the reader —
      `core: a_preview_with_no_dimensions_is_skipped`,
      `core: a_truncated_preview_is_copied_as_is_and_a_runaway_one_is_skipped`,
      `core: names_with_spaces_and_unicode_survive_into_the_file_name`,
      `core: an_impossible_name_refuses_before_anything_is_written`,
      `core: the_name_check_follows_the_suffix`,
      `core: a_destination_that_is_not_a_folder_refuses_at_plan_time`,
      `core: a_thousand_frames_plan_without_reading_a_single_sample`,
      `core: the_samples_stream_instead_of_piling_up_in_memory`,
      `core: qt::hostile_bytes_come_back_as_errors_not_panics`.
- [x] App: stand in a burst, Ctrl+Shift+E, Enter — the file lands and the
      reader confirms it; the item disabled with no selection and no burst;
      marks unchanged; the dialog owns the keyboard —
      `app: export_frames_as_video_writes_a_real_motion_jpeg`,
      `app: the_video_export_asks_before_replacing_a_file`. Recorded
      deviation: the driven test uses a SELECTION, not the burst under the
      cursor — all three reference RAWs declare `SequenceNumber = 0`, so no
      repository fixture forms a burst; the burst scope is covered by
      `core: the_scope_is_the_selection_or_the_burst_under_the_cursor` and
      the app strand reads the same burst index for the disabled-with-a-
      reason assertion.
- [x] The second export after a plain move holds only the new frames
      (brief 002): 4 selected → export → Esc → Right, Right → Shift+Right →
      2 frames, no earlier-video hint, 2 samples, no clash question; Esc on
      the report leaves the selection intact, a second Esc clears it —
      `app: the_second_video_holds_only_the_new_span`.
- [x] The badge and the hint (issue #56): a landed export badges exactly the
      frames in the file; a skipped frame never; a cancel or a failure
      badges nothing; an Overwrite drops the replaced file's frames; the
      re-check at the two moments and not per repaint; a session swap
      forgets every badge; the hint's sentence shapes; the GRID paints the
      badge in both positions, measured in the cells by the pill's LEFT
      EDGE (x 6..=12 in the ✓'s slot, x 26..=32 beside a ✓, 14..=34 px wide
      — the width is the font's: 19 px on ubuntu, 26 px on Windows, 21 px on
      the development seat) —
      `core: only_a_landed_export_hands_the_ledger_anything`,
      `core: a_skipped_frame_is_never_among_the_ids_an_export_records`,
      `core: the_ledger_badges_only_frames_that_are_in_a_file_that_is_still_there`,
      `core: overwriting_a_video_drops_the_frames_it_no_longer_holds`,
      `core: a_name_written_again_supersedes_even_if_it_was_gone_in_between`,
      `core: the_hint_names_one_video_and_counts_the_others`,
      `core: the_hint_says_how_many_of_how_many`,
      `core: a_fresh_ledger_remembers_nothing_from_the_last_one`,
      `core: the_ledger_never_changes_what_the_next_export_writes`,
      `app (unit): state::clip_state_tests::a_session_swap_forgets_every_badge`,
      `app: an_exported_frame_wears_a_badge_until_its_video_is_gone`
      (`assert_badge_pixels`, replayable over a CI artifact from either
      platform).
- [x] Perf: 30 A1 frames export in < 2 s on the reference laptop, release,
      idle — `perf: budget_video_export_30_frames_under_2s` (527 ms for
      327 MB, 2026-08-27).
- [x] The skipped sentence is bounded and neither dialog's button row can
      leave its card — at the floor, grown, or clamped with the body
      scrolling (issue #62) —
      `core: a_long_list_of_reasons_is_bounded_and_still_adds_up`,
      `core: the_tail_names_a_kind_only_when_the_whole_tail_is_that_kind`,
      `app: a_long_refusal_keeps_the_export_buttons_inside_the_card`,
      `app: a_failure_report_longer_than_the_window_keeps_the_copy_buttons_inside_the_card`
      (the Copy Picks half; Unix only — a `chmod 555` destination;
      review-only on Windows).
- [ ] USER-VERIFIED (2026-08-27, not automatable): InShot on the phone
      imports and plays a 2880×1920 MJPEG `.mov` and the untouched
      8640×5760 30-frame file. NOT VERIFIED: portrait rotation honoured by
      InShot; playback on iOS; 4:2:0 or progressive JPEGs from other
      bodies; and the file THIS module writes on the phone — the test used
      ffmpeg's file of the same layout (minus `edts`/`udta`/`wide`, `co64`
      for `stco`), which decodes identically under ffmpeg. Tracked among
      CLAUDE.md's open decisions.
- [ ] Review-verified only: the export dialog's `?`/F1 close arm (the copy
      dialog's was driven; the export one cannot be reached on synthetic
      data, which has nothing to export — the two arms are
      character-identical).

## History

- 2026-09-17 — Rewritten (brief 007). The pre-rewrite text, with every
  validator and QE finding of the M9 rounds, is
  `specs/history/video-export.md`.
- 2026-09-12 — The plan line's sizes in the screen's form (brief 006); New
  only mapped to the refused `Clash` marker (brief 005).
- 2026-09-06 — The per-burst rhythm needs no Esc; the dialog never touches
  the selection (brief 002).
- 2026-08-30 — The card's height follows its content and the body scrolls;
  the skipped sentence bounded (issue #62, v0.13.0).
- 2026-08-29 — The exported badge and the hint (issue #56); the wheel over
  the scrim (issue #49).
- 2026-08-28 — The as-built decisions of the M9 gate (validator and QE
  findings): the name length checked on the planned name, the CR3/RAF
  correction, the session-swap flag, the two limits of cancel, the mirrored
  frames kept, the Keep both row's wording (`fad52ea`); v0.11.0.
- 2026-08-27 — M9: Motion JPEG of the untouched camera JPEGs (three-persona
  review, the phone test), the cadence from the median gap, the seeded
  destination and the chord (persona gate), ADR 0004; at implementation time
  the cadence window settled at [9 ms, 100 ms], replacing a draft that named
  two disagreeing windows (a 10–1000 ms trigger and a [10, 120] fps target),
  and "filter state is irrelevant" gave way to the two halves — a selection
  is the frames in view, a burst is the whole burst.
