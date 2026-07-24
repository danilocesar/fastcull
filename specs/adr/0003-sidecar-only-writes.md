# ADR 0003: Sidecar-only metadata writes

**Status**: accepted (2026-07-24)

## Decision

FastCull never writes into a RAW file. All user state (pick/reject as xmp:Rating,
IPTC) is written to `<name>.<ext>.xmp` sidecars only.

## Rationale

- Safety: Sony ARW metadata rewrites are known to corrupt/lose embedded previews;
  the originals are irreplaceable professional assets.
- Interoperability: darktable natively imports `<name>.<ext>.xmp` sidecars
  (ratings, labels, dc:subject/lr:hierarchicalSubject, dc:* IPTC) — the exact
  handoff this product exists for. digiKam/Lightroom/Photo Mechanic read the same
  fields.
- Simplicity: no per-format embedded-write support matrix; one serializer, golden-
  file tested (xmp-sidecars spec).

## Consequences

- Sidecars must travel with files: the copy engine moves them in lockstep and the
  darktable naming convention is mandatory (`<name>.xmp` form is buggy in
  darktable imports — never emit it).
- Consumers that read only embedded metadata won't see our IPTC; out of scope
  (darktable is the contracted consumer; PM/LR/digiKam read sidecars too).
