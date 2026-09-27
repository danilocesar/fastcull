# ADR 0005: libjpeg-turbo as a C dependency, for the loupe's decode and its screen rung

**Status**: accepted (2026-09-26, user decision: "Fine regarding
libjpeg"; issue #60, brief 008) · **Extends**: ADR 0001 (cull on embedded
JPEGs), whose "±2 neighbor prefetch hides it completely" consequence this
ADR narrows — it hid a tap, never a held arrow at fit on a 4K viewport ·
**Touches**: ADR 0002's "contributors need only rustup", which this ADR
narrows

## Context

A held arrow at fit on a 4K viewport went soft after two or three frames
(issue #60, the user's words of 2026-08-29: "the quality gets bad very
quickly, maybe within 2 or 3 frames"). The mechanism: while the key was
held the loupe asked the decoder for the 1616 px mid rung only, and on a
3840×2160 viewport the fit view of a 3:2 frame is 3240×2160, so the mid was
shown upscaled 2×. The A1 embeds exactly two JPEGs — the 1616 preview and
the 8640×5760 full — and the shipped decoder, zune-jpeg 0.4.21 (pure Rust),
decodes a JPEG at one size only: full. A full decode cost 225–269 ms on the
development laptop (`loupe::decode_oriented`, landscape / portrait), ten
times the key repeat, so no cache size fixed it — the frames were never
decoded. What fixes it is a rung between the mid and the full: the same
embedded JPEG decoded at a fraction of its size, which only a decoder with
DCT-domain scaling can do. zune-jpeg cannot; the other pure-Rust decoder,
`jpeg-decoder`, scales at 1/2, 1/4 and 1/8 only and was measured against
the rung below (Alternatives rejected); libjpeg-turbo offers every M/8 from
1/8 to 2/1 — libjpeg v7's IDCT-scaling ladder, which its `README.md` lists
among the fully supported libjpeg v7 features ("only 1/4 and 1/2 are
SIMD-accelerated"); IJG's libjpeg 6b itself scaled at 1/1, 1/2, 1/4 and 1/8
only.

The benchmark (M6, 2026-09-26; its table, the throughput figures and the
two reviewers' notes are in brief 008's Context): the development laptop,
i7-8665U with 4 cores / 8 threads, libjpeg-turbo 3.1.0 built from source
with SIMD, zune-jpeg 0.4.21, the app's own extraction path for the bytes,
11 interleaved samples per cell, two independent reviewers — every latency
cell within 3.4 %, the throughput cells within 6.6 %, the SIMD probe within
1.5 %. On the reference file (`A1_full_lossless_compressed.ARW`, a 9.75 MB
embedded JPEG), single-threaded, thermally saturated:

| per decode | portrait (o8, the shipped shape) | landscape |
|---|---|---|
| zune-jpeg, `decode_oriented` (shipped until 2026-09-26) | 268.8 ms | 225.3 ms |
| libjpeg-turbo, full size, same shape | 226.7 ms | 185.7 ms |
| libjpeg-turbo, 1/2 (4320×2880) + rotate | 146.7 ms | ~137 ms |
| libjpeg-turbo, 3/8 (3240×2160, the 4K fit) | not measured | 127.8 ms |
| libjpeg-turbo, 1/8 (the serial Huffman floor) | — | 90.8 ms |

Throughput on the laptop, frames/s at 1/2 scale: 7.5 / 13.7 / 18.8 / 22.3 /
23.3 / 24.3 at 1 / 2 / 3 / 4 / 6 / 8 workers, against the shipped path's
3.7 / 6.8 / 9.2 / 10.9 / 11.7 / 12.3. The gain is 1.6–1.8× per decode, not
the ~3× the issue estimated — the 1/8 decode still costs 68 % of the
half-scale time, because the A1 JPEG has zero restart markers and its
Huffman decode is strictly serial — and about half of it is the decoder
swap itself. Four workers is the knee on four physical cores; hyperthreads
add 5–9 % at doubled per-frame latency. What the benchmark did not cover:
other bodies' JPEG flavours — every fixture is one A1 body's baseline
4:2:2 stream, so 4:2:0 and progressive streams are unmeasured and the perf
rows that followed bind on that one shape — the Windows build of the
library, and the GPU upload of a rung texture.

The user's culling machines are not the laptop: a 64 GB desktop and a
Ryzen AI Max+ 395 machine, on Linux or Windows by the day. There the
decoders outrun the key once they follow the core count (~130 ms per rung
decode on a 16-core Zen 5 is over 100 frames/s aggregate), and the limit
moves to the display path, where a full-size frame is a 149 MB texture copy
plus a GPU upload per swap and a fit-sized rung is 21 MB. So the rung is
what lets sharp frames keep up with the key on the strong machines, and
what makes them cheaper on the weak one. The user accepted the dependency
and refused pacing the held key ("the application needs to move smooth.
the softness is acceptable, but I need to maximize the situations where the
softness isn't there").

## Decision

FastCull's loupe decodes embedded JPEGs with **libjpeg-turbo ≥ 3.0**
through the `turbojpeg` crate (1.5.x, over `turbojpeg-sys` 1.2.x), and
gains a **screen rung**: the embedded full JPEG decoded with DCT scaling at
the smallest N/8 factor that serves the loupe's fit box under the existing
1.25 `serves` rule (raw-pipeline.md, "The screen rung").

- **From source, static, SIMD required.** The crate's `cmake` and
  `require-simd` features are on: the vendored libjpeg-turbo (3.1.0 in
  `turbojpeg-sys` 1.2.0) is built by cmake and linked statically, and
  `require-simd` passes `-DREQUIRE_SIMD=ON`, under which a seat without
  NASM fails the build (`simd/CMakeLists.txt`, `simd_fail`: a `FATAL_ERROR`
  when `REQUIRE_SIMD` is set, a warning and a SIMD-less library otherwise).
  A SIMD-less build is 1.95× slower at full size and 1.46× at 1/2
  (`JSIMD_FORCENONE=1`, the benchmark's SIMD probe), so it must never ship
  silently.
- **A system libjpeg-turbo ≥ 3.0 may be linked on Linux** through the
  crate's `pkg-config` feature (`TURBOJPEG_SOURCE=pkg-config`; the crate
  requires `atleast_version("3.0")`). Fedora 44 ships 3.1.3; Ubuntu 24.04's
  2.1.x is too old, so CI builds from source, which is the default for
  every build — developer seats, CI, releases.
- **The grid thumbs stay on zune-jpeg 0.4** (brief 008; raw-pipeline.md
  records why). Every loupe rung — mid, screen rung, full — goes through
  libjpeg-turbo, except a stream whose header says CMYK or YCCK:
  libjpeg-turbo refuses those for RGB output ("Unsupported color conversion
  request"), so zune-jpeg decodes them as before, at full scale with no
  rung — no regression for a print-ready bare JPEG, and no new colour path
  in core for files no camera writes (Manager ruling 2026-09-26). The same
  route is the loupe's second opinion for a stream libjpeg-turbo refuses
  over a complaint outside the damage and kept classes (raw-pipeline.md,
  "The decoder's complaints"; added 2026-09-26, the other-cameras pass of
  brief 008).
- **The hostile-input bounds carry over, and the loupe path closes one
  residual with the library's own return contract.** `MAX_DECODED_PIXELS`
  is checked on the header's FULL dimensions right after the header read,
  which allocates no image buffer; `scan_is_terminated` runs on the bytes
  after the header read and the pixel cap, before any buffer is sized or any
  scan byte decoded; and libjpeg-turbo's own truncation behaviour — the
  memory source inserts a fake EOI and warns `JWRN_JPEG_EOF`, the Huffman
  decoder warns `JWRN_HIT_MARKER` and feeds zero bits — ends in a −1 return
  from `tj3Decompress8` whatever the flags, because it returns −1 whenever
  a decode emitted any warning, which the safe crate maps to `Err`. So no
  truncated stream is ever a blank success on the loupe path (measured:
  `Err` over a grey-bottomed buffer; raw-pipeline.md, "Hostile-input
  bounds"). (Narrowed 2026-09-26, the other-cameras pass of brief 008:
  the loupe now reads that `Err`'s text, the decode's first message,
  refuses the damage class and decodes past the rest, so damage behind a
  first message it decodes past can be shown, grey or zero-filled; and an
  arithmetic-coded stream never warned — its decoder meets a marker in the
  data without a word (`jdarith.c`) — so a short arithmetic scan behind a
  valid EOI was a blank success on the loupe path from the start. Both are
  raw-pipeline.md's "Hostile-input bounds" residual. A Huffman-coded stream
  cut short with no such message ahead of the cut is still refused.)
  `TJPARAM_STOPONWARNING` and `TJPARAM_MAXPIXELS` are not set: the safe
  crate keeps its handle private and exposes neither, they would buy an
  abort 13–24 ms sooner on a crafted stream and a second copy of our own
  pixel cap, and taking them means a raw-FFI decompressor in core.
  `TJPARAM_SCANLIMIT` is set, to 100, through the safe crate's
  `set_scan_limit`: its default is no limit, and 100 is the bound zune-jpeg
  0.4's default gave the loupe (raw-pipeline.md). (Added 2026-09-26, the
  step-1 fix round of brief 008: the step-1 code set no limit, on the step-1
  review's premise that zune-jpeg shared the exposure, which was wrong.)

## Consequences

- **Build seats need cmake and nasm** (or a system libjpeg-turbo ≥ 3.0 on
  Linux), **and Windows seats ninja**: CI installs nasm on both jobs, cmake
  is on both runner images and ninja on the Windows images CI and the
  release build on, the release workflow's `dist-workspace.toml` lists nasm
  for apt and chocolatey, and README's build block (the Linux one) and
  `docs/index.md` name the requirement (01-architecture.md, "Native
  dependencies"). ADR 0002's "contributors need only rustup" becomes
  "rustup, cmake and nasm" — and on Windows ninja, for the generator the
  MSVC target names (below; senior-developer review 2026-09-26, F1), beside
  whatever Visual Studio C++ tools the MSVC toolchain already needs.
  (Corrected 2026-09-26, the generator re-ruling of brief 008: this said
  Windows seats need Visual Studio 2022, whose generator the target was to
  name.)
- **The Windows artifact carries the library statically** (the crate links
  `turbojpeg-static` on MSVC): the `crt-static` promise and the "no
  VCRUNTIME140 import" check still apply, and a check that neither exe
  imports `turbojpeg.dll` joins them. The benchmark did not measure the
  Windows build; the unit's first CI run is the proof, and the user tests
  that artifact on the desktop with real folders before any release (user
  decision 2026-09-26).
- **A build without nasm fails, never a silent SIMD-less binary**:
  `require-simd` is the guard, and the landscape full-res row of the perf
  table is the runtime canary, its threshold set under what a SIMD-less
  decode measured (01-architecture.md has the threshold and its reason).
- **Licences.** libjpeg-turbo is IJG and BSD-3-Clause, with zlib on the SIMD
  sources — all in `about.toml`'s accepted list, and README already carries
  the IJG attribution sentence. `cargo about` keys on the crates' own SPDX
  expressions (`Unlicense OR MIT`), not on the C sources a `-sys` crate
  vendors, so `about.toml` carries a clarification naming the three
  vendored texts; and because cargo-about falls back silently, exit 0, when
  a clarification's checksum no longer matches, a test pins that the
  generated `THIRD-PARTY-LICENSES.md` lists the three licences with their
  texts (Manager ruling 2026-09-26).
- **The perf table changes shape**: the full-res row keeps its threshold
  with more headroom, and three rows join it — the landscape full-res row,
  the SIMD canary, and two screen-rung rows, the 4K landscape 3/8 and the 4K
  portrait 2/8 plus its rotate, whose threshold sits under the landscape
  full-res median so that a "rung" that silently became a full decode plus
  a resize is red (01-architecture.md has the thresholds and their rule).
- **The C library follows cargo's opt-level** through the `cmake` crate
  (`Debug` at opt-level 0, `RelWithDebInfo` under the dev profile with the
  #76 line, `Release` in release), so the #76 line — `[profile.dev.package."*"]
  opt-level = 2` — keeps the loupe's debug decode optimised; on the MSVC
  target only because a workspace `.cargo/config.toml` names its CMake
  generator, Ninja, since the crate, when it picks the Visual Studio
  generator itself, overrides the C flags of the configuration it builds
  with every `/O` flag stripped, which leaves MSVC's unoptimised default in
  every profile, release included; any named generator skips that
  override, and Ninja is the one name that holds whatever Visual Studio a
  seat carries (01-architecture.md, "Native dependencies", has the rule and
  its check). (Corrected 2026-09-26, senior-developer review F1: this said
  the #76 line alone kept the decode optimised; the first Windows artifact
  of brief 008 carried the library unoptimised. Named the same day, the
  generator re-ruling of brief 008: Ninja, not the "Visual Studio 17 2022"
  first ruled, which CI's `windows-latest` image — Visual Studio 2026 only
  — cannot configure.)
- **Complaints are read by their text** (the other-cameras pass of brief
  008, 2026-09-26; CLAUDE.md M11). The safe crate returns a complaint as
  text only — the first message of the decode, which libjpeg reports and a
  fatal error replaces — and fails the header read on any warning, so the
  loupe sorts libjpeg-turbo's complaints by the vendored message texts:
  it removes gaps between header segments before the decode as libjpeg
  itself skips them, refuses what only damage raises, keeps libjpeg-turbo's
  image where its decode completed past a message a writer's quirk can
  raise, and asks zune-jpeg for a second opinion where it did not
  (raw-pipeline.md, "The decoder's complaints"). No raw FFI: it would let
  the loupe read a header past a warning, at the cost of core's second
  `unsafe` block, and would sort nothing better, since the TurboJPEG API
  reports every warning as `TJERR_WARNING`. A version that rewords a message moves its stream to
  another class; the tests decode a real stream of each class, and the
  version canary names the texts.
- **`decode_oriented`'s public contract holds** (bytes and orientation in,
  oriented RGB out; the perf budget measures it), the scaled decode is its
  sibling `decode_scaled_oriented`, and `scaled_dims` is the decoder's own
  size arithmetic.
- **The dependency canary grows**: the behaviours the code depends on are
  recorded in `crates/fastcull-core/Cargo.toml` beside the dependency, so
  whoever upgrades it re-reads them (01-architecture.md lists them).
- **No upstream contribution** (hard rule 2): the crates are used as
  published; any patch stays in-tree.

## Alternatives rejected

- **A bigger loupe cache as the fix** (the user's first ask: 24 slots,
  8 GiB). A bigger cache alone never made a held arrow at fit sharp: while
  the key was held only the mid was decoded, and a full decode is ten times
  the key repeat, so the frames a cache would have kept were never decoded;
  a 24-slot ring gave ~0.4 s of sharp travel after a 3 s dwell (issue #60).
  The cache did grow with this decision — a quarter of total RAM, 2 to
  10 GiB, the user's redesign of 2026-09-26 — for another reason: it holds
  the full-res ring at 1:1 and the frames already seen, which a hold at 1:1
  revisits (raw-pipeline.md, "Memory").
- **A pure-Rust decoder with scaling.** zune-jpeg cannot scale, and its
  0.5.15 is a measured regression on this workload (267–279 ms against
  0.4.21's 247–252). `jpeg-decoder` 0.3.2 can, at powers of two only — 1/2
  (4320×2880, 37 MB: 1.8× the pixels of the 3/8 rung a 4K fit needs), 1/4,
  1/8 — and was measured on the same fixture and seat during the spec change
  (2026-09-26, single thread, 2 warm-up and 11 timed interleaved rounds,
  with a zune-jpeg control at 231.2 ms): full 299.6 ms (1.30× zune), 1/2
  184.5 ms, 1/4 153.4 ms, 1/8 148.2 ms. Its serial Huffman floor is 148 ms
  against libjpeg-turbo's 90.8, so its best rung costs what libjpeg-turbo's
  FULL decode costs and 1.44× the 3/8 rung; with its `rayon` feature every
  figure was worse (1/2: 220.2 ms). A pure-Rust rung would buy ~20 % over
  the old decode where libjpeg-turbo buys 43 %, at 1.8× the bytes per rung.
- **The decoder swap alone, without the rung.** Worth ~20 % (226.7 against
  268.8 ms in the shipped portrait shape); it does not make a 4K fit hold
  sharp, because the full-size frame is still ten times the key repeat and
  149 MB per texture swap on the display path.
- **Pacing the held key to the decoders** (issue #60 part 3). Refused by the
  user 2026-09-26: the hold keeps its repeat rate; soft frames are
  acceptable, and the aim is to maximise the sharp ones.
- **`mozjpeg-sys`.** Not needed: the safe `turbojpeg` crate exposes
  `set_scaling_factor`, and its `raw` re-export of `turbojpeg-sys` carries
  the whole `tj3` API should a parameter it does not wrap ever be needed;
  the benchmark's raw-FFI control shows the safe wrapper costs nothing
  measurable.
- **`FASTDCT` / `FASTUPSAMPLE`.** Measured: nothing at 1/2 scale (the
  reduced IDCT ignores `dct_method`), ~3 % at full; neither is worth its
  accuracy cost.
