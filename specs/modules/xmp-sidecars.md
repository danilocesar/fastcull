# Module spec: XMP sidecars (`xmp.rs`, `sidecar_writer.rs`)

## Purpose

Persist every piece of user-authored state — pick/reject and IPTC — as XMP
sidecar files that darktable imports correctly. This is the interoperability
contract of the product: darktable is the reference editor, and digiKam,
Lightroom and Photo Mechanic read the same fields (00-overview.md).

## Behaviour

### Invariants

1. **A RAW file is never opened for writing.** Not to embed metadata, not
   "just this once": Sony ARW rewrites can corrupt embedded previews
   (ADR 0003).
2. The sidecar is `<name>.<ext>.xmp` (`DSC01234.ARW.xmp`), darktable's
   native convention. Never `<name>.xmp` (known darktable import bugs).
3. **Read-modify-write with preservation**: an existing sidecar — Photo
   Mechanic's, Lightroom's, darktable's own — keeps its unknown nodes and
   namespaces byte-faithfully where possible, never silently dropped.
4. Writes are atomic: a temp file in the same directory, fsync, rename over.

### What is written

| FastCull state | XMP property | Notes |
|---|---|---|
| Rejected | `xmp:Rating = -1` | darktable's reject convention |
| Picked | `xmp:Rating = 1` | filterable as ≥1 star in darktable |
| Unmarked | no `xmp:Rating` property | absence, not 0 |
| Keywords | `dc:subject` (rdf:Bag) + `lr:hierarchicalSubject` (rdf:Bag) | both, like digiKam/LR |
| Title | `dc:title` (rdf:Alt, x-default) | |
| Description/caption | `dc:description` (rdf:Alt, x-default) | |
| Creator | `dc:creator` (rdf:Seq) | |
| Copyright | `dc:rights` (rdf:Alt, x-default) | |
| Headline | `photoshop:Headline` | |
| City / Country | `photoshop:City` / `photoshop:Country` | |
| Credit / Source | `photoshop:Credit` / `photoshop:Source` | |
| Job identifier | `photoshop:TransmissionReference` | |
| Location detail | `Iptc4xmpCore:Location` | |

- Serialization: the standard `x:xmpmeta`/`rdf:RDF` envelope, UTF-8,
  namespaces declared once on `rdf:Description`, deterministic property
  order (golden-file tested).
- The rating is written in attribute form; legacy element and `xap:` forms
  are removed or replaced on rewrite.
- `write_keywords` replaces the `dc:subject` and `lr:hierarchicalSubject`
  bags wholesale — the session's keyword list is the full truth for those
  two properties; an empty list removes the bags; everything else, foreign
  keyword stores like `digiKam:TagsList` included, is preserved.
- `write_iptc` serializes the full `IptcData` — fields and both bags — in
  one atomic rewrite. A `None` field REMOVES the property (an empty value is
  never emitted); ownership is matched by XML local name, symmetrically with
  the reader, so a foreign-namespace element whose local name collides is
  replaced (recorded trade-off); foreign nodes and the rating pass through;
  an identical rewrite is byte-stable (removed elements take their
  indentation text nodes with them).
- A write failure is surfaced: a status-bar warning and stderr.

### What is read

`read_sidecar` returns the pick state and the mapped IPTC fields
(`SidecarState.iptc`). Both XMP
forms are accepted — element form (Alt/Seq container text or direct element
text) and the compact attribute form on any `rdf:Description`. Properties
match by XML LOCAL name (alias-prefix tolerant; a foreign attribute whose
local name collides, say `xxx:City`, is accepted — recorded trade-off).
Values are trimmed and whitespace-only values are ignored in both forms; the
first value wins per field, attributes before child elements; a self-closed
or empty property reads as unset and never affects a neighbour. Known
deviation: inside an `rdf:Alt` the first `rdf:li` wins regardless of
`xml:lang` — x-default priority is not implemented (darktable emits
x-default first; revisit if multi-language Lightroom sidecars surface
translated values). At load, `SessionEvent::Sidecar` carries the full
`IptcData`; the app seeds its state from it, guarded like picks so a stale
read racing the debounced writer never reverts a fresh panel edit.

### Write scheduling

A dedicated writer thread (01-architecture.md) owns every sidecar write.
Mutations are debounced per image — `sidecar_writer::DEBOUNCE` is 700 ms,
inside the ≤ 1 s the architecture promises — ordered, and never lost:
flushed on session close and before a copy plan is built (a plan must never
race a pending write; fileops.md), panic-safe through `Drop`.
`SidecarWriter::close` shuts the writer down and returns how many writes its
final drain performed — writes, not marks: re-marks on one image coalesce
into one entry, and a failed write counts, its failure having its own
channel. A session swap traces that number as `sidecar writer closed gen N:
K pending flushed` (test-harness.md), so a driven swap asserts a structural
fact instead of timing a pick against the swap with a stopwatch. Dropping
the writer drains identically and reports nothing: the mark means "a swap
closed it".

## Contracts

- The sidecar name and the never-write-a-RAW invariant are ADR 0003's; the
  copy engine moves sidecars in lockstep with their RAWs (fileops.md).
- The barrier: the writer flushes before any copy plan; keyword-only
  messages merge into a pending full write, so fields are never dropped.
- The darktable round-trip in CI runs `darktable-cli` with a throwaway
  `--configdir`/`--library` in a temp dir — never the user's real config
  (CLAUDE.md hard rule 3) — and is skipped gracefully where darktable-cli is
  absent; neither CI runner installs it, so the round-trip runs on the
  development seat.
- The trace mark `sidecar writer closed gen N: K pending flushed`.

## Acceptance criteria

- [x] Golden files: each pick state and the IPTC set serialize
      byte-identically to `tests/golden/*.xmp`.
- [x] Keyword round-trip over a hostile set — Unicode, quotes, `&`, `<`/`>`,
      CJK, pipe hierarchies, 40-item lists; idempotent rewrite; composes
      with rating writes both ways; the IPTC field strings with the panel
      step.
- [x] Preservation: a sidecar with fake `crs:` and `darktable:history`
      blocks survives our edits with those nodes intact —
      `foreign_nodes_survive_rating_edits` (plus a 50-cycle fuzz at QE).
- [x] darktable round-trip (integration, Linux): an A1 file plus our sidecar
      imported by `darktable-cli`; the rating (since M3) and all three
      keyword shapes — plain, Unicode, pipe hierarchy — land as tags (since
      M5) — `tests/darktable_roundtrip.rs`.
- [x] Atomicity: `kill -9` during a write storm leaves only valid XML files
      — `tests/xmp_crash.rs` (the child storms writes, the parent SIGKILLs,
      15 rounds).
- [x] A self-closed or empty property never affects its neighbours (the
      gate H1 regression test).

## History

- 2026-09-17 — Rewritten (brief 007). The pre-rewrite text is
  `specs/history/xmp-sidecars.md`.
- 2026-09-03 — `SidecarWriter::close` returns its flush count and the swap
  traces it.
- 2026-07-25 — The M3/M5 scope split, approved by the user: M3 ships
  ratings; keyword writing landed with M5 the same day; IPTC field reading
  and writing with the panel step, with the reader's recorded deviations.
- 2026-07-25 — M3: the writer thread and the darktable round-trip
  (`def458c`).
- 2026-07-24 — ADR 0003 and the invariants.
