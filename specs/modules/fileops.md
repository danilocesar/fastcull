# Module spec: copy picks (`fileops.rs`)

## Purpose

End-of-cull operation: copy picked RAWs and their sidecars to a destination folder,
optionally renamed by template. Originals are never touched (copy, not move — user
decision).

## Two-phase contract: plan, then execute

**Plan** (pure, unit-testable): given (picked images, destination, optional rename
template) produce a `CopyPlan`: ordered list of (source RAW → dest RAW, source
sidecar → dest sidecar) plus detected problems. Rename templates reuse the IPTC
variable engine (`{date}`, `{seq}`, `{filename}`, `{camera}`, `{ext}`).

Plan-time errors (block execution, shown to user):
- Destination inside the source folder, or equal to it.
- Template collision: two images expand to the same destination name.
- Insufficient free space (sum of sizes vs `statvfs`/`GetDiskFreeSpaceEx`).

Destination file already exists (per-file modes): **rename (default)** / skip /
overwrite / abort. Rename appends a numeric suffix before the extension
(`DSC01234_2.ARW`, sidecar in lockstep) — the default because multi-camera days
produce identical filenames landing in one flat selects folder (user decision
after persona review). Auto-renames are listed in the plan preview.

**Execute** (on a worker thread, progress events per file):
1. Flush pending sidecar writes for all picked images (hard barrier).
2. Per image: copy RAW → fsync → copy sidecar → fsync. Sidecar is renamed in
   lockstep (`newname.ARW` ⇒ `newname.ARW.xmp`).
3. Verify: BLAKE3 checksum of destination equals a checksum computed while
   streaming the source copy, for both RAW and sidecar. Checksums were promoted
   from v2 to v1 (user decision after persona review): the user sometimes culls
   directly off the card mount, making this copy the working copy before the
   card is formatted — size-only verification is not enough for that flow.
4. On per-file failure: record, continue with remaining files (no partial-file left
   behind — copy to temp name, rename on success).
5. Final report: copied / skipped / failed with reasons. Session marks copied
   images with a "copied" badge.

Cancellation: between files only; already-copied files remain (report says so).

**Rejects are not fileops' business (recorded user decision)**: after copy-picks,
rejected and unmarked files stay untouched where they are; the user deletes them
manually later. No move/delete-rejects operation in v1 (revisit only if asked).

## Acceptance criteria (tests)

- [ ] Plan: template expansion, collision detection, dest-inside-source rejection,
      exists-handling in all four modes incl. rename-suffix lockstep (tempdir
      fixtures).
- [ ] Execute: RAW+sidecar pairs land with correct names; checksums verified
      (and a deliberately corrupted destination write is detected); a
      read-protected source file fails alone, others complete.
- [ ] Sidecar barrier: a pick made ≤1 s before "copy" is present in the copied
      sidecar (regression for the debounce race).
- [ ] No partial files after simulated failure (temp-name copy verified).
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      (`CON`, `NUL`, trailing dots) at plan time.
