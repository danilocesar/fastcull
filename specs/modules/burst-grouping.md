# Module spec: burst grouping (`burst.rs`)

## Purpose

Group frames shot in one continuous-drive squeeze so the UI can mark them (colored
border per group). Grouping is display metadata only — it never reorders images and
has no effect on picks, sidecars, or copies.

## Algorithm

Input: session images sorted by capture time (DateTimeOriginal + SubSecTimeOriginal;
filename as tiebreaker).

1. **Sony path**: maker-note `SequenceNumber` (0 = single shot, ≥1 = position in
   burst). A burst = maximal run where `SequenceNumber` ≥ 1 and capture-time gap to
   the previous frame ≤ `max_gap` (default 600 ms).
2. **Generic path** (no usable sequence numbers): maximal run of frames from the
   same camera body (EXIF serial, else model) with consecutive gaps ≤ `max_gap` and
   run length ≥ 3 (avoids labeling two quick singles a burst).
3. Missing SubSec → 1 s timestamp granularity: treat equal timestamps as gap 0.
4. Group identity: `BurstId` = stable hash of (first frame path); color index =
   `BurstId % palette_len` with the constraint that **adjacent** groups in display
   order never share a color (bump index on collision).

`max_gap` and min run length are config values, not constants.

## Acceptance criteria (tests)

- [ ] Synthetic EXIF sets: single shots (Seq=0) never grouped; a 20 fps A1 burst
      (50 ms gaps, Seq 1..N) forms one group; a 700 ms pause splits groups.
- [ ] Generic path: 3+ frames within gaps → group; 2 frames → no group.
- [ ] Mixed bodies interleaved (two cameras shooting simultaneously) group
      independently.
- [ ] Missing SubSec handling per rule 3.
- [ ] Adjacent groups get distinct colors (property test over random group runs).
- [ ] Integration: the real A1 test files (single shots) produce zero groups.
