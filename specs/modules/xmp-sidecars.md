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

## Acceptance criteria (tests)

- [ ] Golden-file tests: each pick state + representative IPTC set serializes
      byte-identically to checked-in `.xmp` fixtures.
- [ ] Round-trip: write → read yields identical state (property-based test over
      arbitrary IPTC strings incl. Unicode, quotes, `&`, CJK, emoji).
- [ ] Preservation: a fixture sidecar containing foreign nodes (fake
      `crs:`/darktable `darktable:history` blocks) survives our edit with those
      nodes intact.
- [ ] **darktable round-trip (integration, Linux)**: `darktable-cli` with throwaway
      `--configdir`/`--library` in a temp dir (NEVER the user's real config)
      imports an A1 file + our sidecar; exported/queried state shows our rating and
      keywords. Skipped gracefully when darktable-cli is absent.
- [ ] Atomicity: kill -9 during a write storm leaves only valid XML files
      (crash-test harness: child process writes in a loop, parent kills it).
