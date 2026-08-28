# Export Frames as Video — a burst you can post

`Ctrl+Shift+E` (or **File > Export Frames as Video…**). Select some
frames — usually a burst that made an interesting sequence but no single
good shot — and FastCull writes **one video file** you can drop straight
into a phone editor. **Your RAW files and their `.xmp` sidecars are not
touched**: they are read, never written, and the video goes only into the
folder you point the dialog at. Your picks and rejects are not touched
either — the frames you export are usually the rejects, and exporting
changes no marks.

## What it does, in one sentence

The camera already rendered a full-size JPEG inside every RAW — the same
picture the loupe shows you. The export copies those JPEGs, **byte for
byte**, into a QuickTime `.mov` as a Motion JPEG video. Nothing is
decoded, scaled, cropped, rotated or re-compressed on the way.

That is also the deal: **you get the whole frame, exactly as the camera
made it.** Any crop, any speed change, any effect belongs in the video
editor afterwards. FastCull hands frames to an editor; it is never one.

## What gets exported

- **The frames you selected** (`Shift`+arrows, `Ctrl`+click, `Ctrl+A`).
- **With nothing selected: the burst under the cursor** — all of it,
  including frames the current filter is hiding.
- Neither, or a single frame? There is nothing to export: the menu item
  is greyed out, and pressing `Ctrl+Shift+E` says why in the status bar.

The file is always in **capture order**, whatever the grid is sorted by.
A video that plays backwards because you had sorted by name descending
would be a bug, not a feature.

## The speed comes from the frames themselves

Sony (and most bodies) write the capture time down to the millisecond, so
FastCull measures how fast you were actually shooting and plays the clip
at that speed: a 30 fps burst reads as ~33 ms between frames and comes
back as a real-time clip — one second for thirty frames.

It uses the **middle** gap, not the average, so selecting two bursts
together does not stretch every frame of both to hide the pause between
them. That pause is simply dropped: the two bursts play back to back.

Two things can go differently, and the dialog says so **before** you
press Enter, in the same words the report uses afterwards:

- *"timing not in the files — assumed 15 fps"* — the frames carry no
  millisecond timing (some bodies only record whole seconds), so there is
  no cadence to measure.
- *"gaps of 4.0 s — clamped to 10 fps"* — the frames are not a burst at
  all (single shots minutes apart, or two cameras interleaved), so the
  speed was pulled into a range an editor can actually play.

## Frames that get left out

All frames in one video must share one size and one orientation — that is
what a video is. A frame that does not match the first one is **left out
and named in the dialog**, never scaled or padded to fit:

> skipped — 2 frames: different size (5616×3744)

The usual causes are a crop-mode shot, a second camera body, or a file
whose full-size JPEG is missing. Fewer than two frames left and the
export refuses before writing anything.

Portrait bursts are fine: the pixels stay exactly as shot and the file
carries a rotation flag, the same way a phone records upright video.
(A frame the camera flagged as *mirrored* is exported un-mirrored, and
the report says how many.)

## The file

- **Name**: the first and last frame in the video —
  `DSC05010-DSC05039.mov` — so the name is also the record of which
  frames are in it.
- **Where**: a folder you choose, remembered for next time. The first
  time you use it, it offers your Copy Picks folder, because on an
  ordinary evening that is where today's output goes.
- **Size**: large, and the dialog tells you before you commit. These are
  full-size camera JPEGs, so a Sony A1 frame is about 11 MB — a 30-frame
  burst is ~330 MB, and a 400-frame selection is over 4 GB. That is the
  price of not re-compressing your photographs.

Everything Copy Picks promises about writing files holds here too:

- **Nothing is replaced without your Overwrite answer.** If the name is
  already taken you get one question — **B** keep both (`_1`, `_2`, …),
  **O** overwrite, **Esc** write nothing. `Enter` deliberately does
  nothing on that question. (The one hair-splitting exception, shared
  with Copy Picks: on a filesystem with no hard links — a FAT32 or exFAT
  card, some network mounts — FastCull has to check the name and then
  write it as two steps instead of one, so a file that appears in that
  split second is replaced. On any ordinary disk the two are one
  operation and cannot be raced.)
- **Never a half-written file under the real name.** The video is
  written under a hidden temporary name, read back and checked, and only
  then given its name. A cancel, a full disk or a crash leaves nothing
  you could mistake for a finished video (at worst a hidden
  `.fastcull-partial-…` file, as with Copy Picks).
- **"All checksums verified"** in the report means every frame was
  checksummed on the way in and re-checked from the finished file, and
  the file's own index was re-read and matched. It is not printed
  otherwise.

## On the phone

The file is a standard `.mov`. A file of exactly this shape — thirty
untouched 8640×5760 Sony A1 frames, 328 MB — was imported and played in
**InShot** on Android, which is why the format was chosen. Be precise
about what that proves: the file in that test was muxed by ffmpeg, and
FastCull's own differs from it only by leaving out two optional boxes and
using the 64-bit offset table. ffmpeg decodes the two to identical
frames, and the rotation flag reads back correctly — but **nobody has yet
put a FastCull-made file on a phone.** If you do, the project would like
to hear about it either way.

Other editors and other phones read the same format; none of them have
been tested, and neither has whether InShot turns a portrait burst
upright on import.

If a phone editor stumbles, the likely cause is the sheer frame size
rather than the format — every phone made in the last few years decodes
JPEG in hardware, but 50-megapixel frames are unusual video material.

## What this deliberately does not do

No crop, no scale, no rotation of the pixels, no choice of frame rate, no
speed, loop or bounce, no format choice, no audio, no per-frame timing,
no GIF. There is no ffmpeg bundled or downloaded, and no H.264 or AV1
encoder — the moment FastCull re-encodes your frames it has become an
editor, and there are better ones on your phone.

---

Next: [FAQ & troubleshooting](faq.md)
