# Module spec: XMP sidecars (`xmp.rs`)

## Purpose

Persist all user-authored state (pick/reject, IPTC) as XMP sidecar files that
darktable imports correctly. This is the interoperability contract of the product.

## Invariants (non-negotiable)

1. **A RAW file is never opened for writing.** Not to embed metadata, not "just this
   once". Sony ARW rewrites can corrupt embedded previews.
2. Sidecar name: `<name>.<ext>.xmp` (e.g. `DSC01234.ARW.xmp`) — darktable's native
   convention. Never `<name>.xmp` (known darktable import bugs).
3. **Read-modify-write with preservation**: if a sidecar exists (Photo Mechanic,
   Lightroom…), unknown XML nodes/namespaces are preserved byte-faithfully where
   possible, and never silently dropped.
4. Writes are atomic: write temp file in same dir, fsync, rename over.

## Field mapping (what we write; what darktable reads)

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

Serialization: standard `x:xmpmeta`/`rdf:RDF` envelope, UTF-8, namespaces declared
once on `rdf:Description`. Property order deterministic (golden-file testable).

## Write scheduling

Owned by the dedicated sidecar-writer thread (`01-architecture.md`): mutations are
debounced ≤1 s per image, flushed on session close and on copy-picks start (a copy
plan must never race a pending sidecar write).

The session-close flush is OBSERVABLE from the app (2026-09-03):
`SidecarWriter::close` shuts the writer down and returns how many writes its
final drain performed — the marks still inside their debounce when the session
went away — and a session swap traces that number as `sidecar writer closed
gen N: K pending flushed` (harness section of ui-grid.md). It exists so a
driven session swap can assert a structural fact about the writer instead of
timing the pick against the swap with a stopwatch.
The count is WRITES, not marks: re-marks on one image coalesced into one
pending entry count once, and a failed write counts too (its failure has its
own channel). Dropping the writer drains identically and reports nothing, so
the mark means "a swap closed it". The debounce constant itself is 700 ms
(`sidecar_writer::DEBOUNCE`), inside the ≤1 s the paragraph above promises.

## M3/M5 scope split (recorded and APPROVED by the user 2026-07-25)

M3 ships pick/reject only: `xmp:Rating` write (attribute form; legacy element
and `xap:` forms are removed/replaced on rewrite), sidecar-at-open, writer
thread, darktable round-trip asserting RATINGS. Keyword WRITING landed with
M5 (2026-07-25): `write_keywords` replaces the `dc:subject` +
`lr:hierarchicalSubject` bags wholesale (the session's keyword list is the
full truth for those two properties; everything else — including foreign
keyword stores like `digiKam:TagsList` — is preserved), an empty list
removes the bags, and the darktable round-trip asserts all three keyword
shapes (plain, Unicode, pipe-hierarchy) land as tags. IPTC FIELD writing
(title/creator/city/…) lands with the IPTC panel step.
Write failures are surfaced to the UI (status-bar warning + stderr).

**IPTC field WRITING (M5 panel step)**: `write_iptc` serializes the full
IptcData (fields + both keyword bags) in one atomic rewrite: `None` fields
REMOVE the property (tri-state clear; an empty value is never emitted),
ownership is matched by XML local name symmetrically with the reader (a
foreign-namespace element whose local name collides is replaced — same
recorded trade-off as reading), foreign nodes/rating pass through, and
identical rewrites are byte-stable (removed elements take their
indentation text nodes with them — QE measured unbounded growth
otherwise).

**IPTC field READING (M5, 2026-07-25)**: `read_sidecar` also returns the
mapped IPTC fields (`SidecarState.iptc`). Contract: both XMP forms are
accepted — element form (Alt/Seq container text or direct element text) and
compact attribute form on any `rdf:Description`; properties match by XML
LOCAL name (alias-prefix tolerant, symmetric with the keyword reader — a
foreign attribute whose local name collides, e.g. a hypothetical
`xxx:City`, is accepted; recorded trade-off); values are trimmed and
whitespace-only values are ignored in both forms; the FIRST value wins per
field (attributes are scanned before child elements). Self-closed or empty
properties read as unset and must never affect neighboring properties
(gate H1 regression test). KNOWN DEVIATION: inside an `rdf:Alt`, the first
`rdf:li` wins regardless of `xml:lang` — x-default priority is not
implemented (darktable emits x-default first; revisit if multi-language
Lightroom sidecars surface translated values). CLOSED with the panel step:
`SessionEvent::Sidecar` carries the full IptcData; the app seeds its
session state from it, guarded (like picks) so a stale sidecar read
racing the debounced writer never reverts a fresh panel edit.

## Acceptance criteria (tests)

- [x] Golden-file tests: each pick state serializes byte-identically to
      checked-in fixtures (`tests/golden/*.xmp`); the IPTC set joins in M5.
- [x] Round-trip (keywords half, M5): write → read yields the identical
      keyword list over a deterministic hostile set (Unicode, quotes, `&`,
      `<`/`>`, CJK, pipe hierarchies, 40-item lists); idempotent rewrite;
      composes with rating writes both ways. IPTC field strings join with
      the panel step.
- [x] Preservation: a fixture sidecar containing foreign nodes (fake
      `crs:`/darktable `darktable:history` blocks) survives our edit with those
      nodes intact (`foreign_nodes_survive_rating_edits` + QE 50-cycle fuzz).
- [x] **darktable round-trip (integration, Linux)**: `darktable-cli` with throwaway
      `--configdir`/`--library` in a temp dir (NEVER the user's real config)
      imports an A1 file + our sidecar; exported/queried state shows our rating and
      keywords (ratings since M3; keywords asserted against data.db/tagged_images
      since M5). Skipped gracefully when darktable-cli is absent.
- [x] Atomicity: kill -9 during a write storm leaves only valid XML files
      (`tests/xmp_crash.rs`: child storms writes, parent SIGKILLs 15 rounds).
