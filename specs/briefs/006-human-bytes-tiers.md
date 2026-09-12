# Brief 006 — the byte sizes the dialogs show get their KB and TB tiers (issue #88)

Date: 2026-09-12. Issue #88 (Manager bookkeeping, M3, from QE's
observation on unit 005). Branch `human-bytes-tiers` from `main` c912f7f.
Scratch: `.qe-scratch/pipeline-006/`. A user-visible formatting fix, so
the persona gate ran (verdicts below).

## Context

`human_bytes` (`crates/fastcull-app/src/copy_bridge.rs`) formats every
byte count the Copy Picks and Export Frames as Video dialogs show: the
copy summary line (`148 picked · 7.3 GB to copy · 1.2 TB free`), the Keep
both row's cost (`+590 MB`), the video summary (`30 frames · 328 MB · 358
GB free`), the video not-enough-space error, and the video report line.
It has three tiers — GB at 2^30, MB at 2^20, otherwise the raw count with
" B" — one decimal, binary units labelled MB/GB, and no test. Two gaps:
below 1 MB the raw count is printed (`+1029480 B`), and above 1 TB the GB
tier runs on (`1228.8 GB free` for a 1.2 TB NAS, `12288.0 GB free` for a
12 TB volume). No spec sentence records the format; fileops.md §6 quotes
`+590 MB` and video-export.md quotes `328 MB` / `358 GB free`.

The persona's gate (below) found one more line in scope: the copy
dialog's not-enough-space refusal prints core's `PlanError::
InsufficientSpace` text verbatim — `not enough free space: need
7834567890 bytes, 123456789 available` — on the plan preview and on the
drop-back after a Keep both answer, while the video dialog translates the
same error into a sentence with the formatter (`no_room_for_it`,
`clip_bridge.rs`: "This video would be 4.5 GB and there is 1.1 GB free at
the destination."). It is the one line where the human form matters most
(the night the destination is nearly full), and it is the issue's subject:
the byte sizes the copy dialog shows.

## Goals

- G1. A KB tier and a TB tier, so every count from a few bytes to tens of
  terabytes reads as a number with one decimal and a unit — `1.2 TB
  free`, never `1228.8 GB free`.
- G2. The copy dialog's refusal reads like the video dialog's: the two
  sizes through the formatter, in a sentence.
- G3. The format is recorded in the spec (one rule, both dialogs; the
  spec's illustrative lines match what the screen prints) and pinned by
  unit tests on the tier boundaries and on the refusal sentence.

## Non-goals

- No change of unit system (binary stays; the labels stay GB/TB — the
  user's answer to OQ1), no change of the decimal count, no unit
  preference or settings row, no change to which lines show a size and
  which do not (§6: bytes on keep both only), no per-file sizes, no
  free-space bar, no ETA, no move of the function to core.

## Requirements

- R1. `human_bytes`: five tiers — `{:.1} TB` at or above 2^40, `{:.1} GB`
  at or above 2^30, `{:.1} MB` at or above 2^20, `{:.1} KB` at or above
  2^10, and `{n} B` below — binary, one decimal on every tier that has a
  unit, labels as today. Every existing caller keeps its string shape
  (the copy summary, the Keep both cost, the video summary, the video
  refusal, the video report line).
- R2. The copy dialog renders `PlanError::InsufficientSpace { needed,
  free }` as `The copy needs {needed} and there is {free} free at the
  destination.` (the two sizes through `human_bytes`), on the plan
  preview and on the drop-back after an answer; every other `PlanError`
  keeps its current text. Core's error text is unchanged: `PlanError`'s
  `Display` is the developer-facing message, and this sentence is shared
  with no report, so it lives beside the video's in the bridge
  (corrected 2026-09-12 — this line first said "for the CLI and the
  logs", but nothing in the CLI or a log reads a `PlanError`).
- R3. Spec: fileops.md's dialog minimums gain the format rule (the five
  tiers, binary, one decimal, both dialogs share the formatter) and the
  refusal sentence; every illustrative size line takes the form the
  screen prints — video-export.md's plan and report lines (`328.4 MB`,
  `358.2 GB free`) and fileops.md §6's `+590 MB` → `+590.3 MB` (amended
  2026-09-12: the Keep both row has printed one decimal since
  2026-08-21); the acceptance criteria below land in fileops.md.
- R4. Docs: `docs/copy-picks.md` says what the dialog says when the
  destination lacks the room (one sentence, plus one parenthetical on
  the binary units — D7), and its clash-question block's `+590 MB` takes
  the screen's form; no other docs sentence quotes a size that changes.
- R5. Tests: a unit test on the tier boundaries (1023 B, 1 KB, 1 MB - 1,
  1 MB, 1 GB, 1 TB - 1, 1 TB, 12 TB → the exact strings) with a mutant
  per boundary that must go red; a unit test on the copy refusal
  sentence (the exact string for a needed/free pair, and that a
  different `PlanError` is untouched); a driven Linux-only round that
  proves the refusal's WIRING on the real dialog (D5); the existing
  readers of a size string stay green unchanged — pump's `344.0 MB` unit
  test, clip_bridge's `4.5 GB` / `1.1 GB` unit test, and the driven
  `0 B to copy` guard in copy_picks_rerun_recopies_hand_deleted_files
  (amended 2026-09-12: this line first named a `copy_summary` test that
  does not exist).

## Acceptance criteria (also land in fileops.md)

- AC1. The formatter prints `1.0 KB`, `1.0 MB`, `1.0 GB`, `1.0 TB`, `12.0
  TB` at the boundaries and `1023 B` below the first; one decimal on
  every tiered value.
- AC2. A copy plan refused for space shows the sentence of R2 with both
  sizes formatted, on the preview and on the drop-back after an answer
  (the wiring driven on the real dialog, D5); the video dialog's refusal
  is unchanged.
- AC3. The spec's illustrative lines match the screen's form; the docs
  sentence of R4 exists.

## Persona verdicts (almost-human-user, 2026-09-12; report in
`.qe-scratch/pipeline-006/PERSONA.md`)

KB tier SHRUG (unreachable on the cost column — a sidecar-only clash
still costs its RAW under Keep both; reachable only through a "free"
figure on a nearly-full card — do it because it costs nothing); TB tier
USEFUL (the NAS and the 2 TB SSD print four-digit GB figures on the one
line meant for a glance); binary units USEFUL to keep (`df -h`, the NAS
dashboard and Explorer are binary; GiB/TiB labels SHRUG, keep GB);
one decimal everywhere SHRUG, with one rule on every line and the spec's
examples matching the screen (`328.4 MB`, `358.2 GB free`); the copy
refusal in raw bytes IN-MY-WAY — fixed here (G2). Explicitly not wanted:
a units preference, a GiB toggle, per-file sizes, a free-space bar, an
ETA.

## Open questions

- OQ1 (the persona's question, relayed verbatim 2026-09-12, non-blocking):
  "When you check a copy afterwards, which number do you hold FastCull's
  'to copy' and 'free' figures against — `df -h`, the NAS dashboard or
  Windows Explorer (all binary, like FastCull), or GNOME Files (decimal)?
  If it is GNOME Files, FastCull will read about 7% under it on every GB
  line forever and decimal would be the honest base; if it is the others,
  binary stays. Nothing else turns on the answer." **The user, 2026-09-12: "I don't compare them"** — binary stays, as
  today (D1).

## Decisions log

- D1 (Manager, 2026-09-12, persona): KB and TB tiers, binary units kept,
  labels KB/MB/GB/TB, one decimal on every tier with a unit.
- D2 (Manager, persona): the spec's illustrative size lines take the
  screen's form; one rule on every line.
- D3 (Manager, persona's IN-MY-WAY): the copy dialog's refusal gets the
  video dialog's sentence shape through the same formatter.
- D4 (Manager, persona): no units preference, no GiB labels, no
  per-file sizes, no bar, no ETA.
- D5 (Manager, 2026-09-12, on the senior developer's OQ2, M3): the copy
  refusal's wiring is proven by a driven round, not by review — a sparse
  `.ARW` past the destination's free space as the clashing pick, `B`, the
  drop-back refuses with the sentence in a new `copyerror=` dump field;
  `#[cfg(unix)]` with the reason written (NTFS allocates on `set_len`).
- D6 (Manager, senior developer's OQ1): at a tier boundary the threshold
  picks the tier and the value rounds inside it — `1024.0 KB` at
  2^20 - 1, never `1.0 MB`; the current behaviour, stated.
- D7 (Manager, senior developer's OQ4): the docs parenthetical on binary
  units stays (a sentence is not a preference; it pre-empts the one
  support question the persona named).
