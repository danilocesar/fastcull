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
- Destination file already exists (per-file: skip / overwrite / abort — user choice,
  abort default).
- Insufficient free space (sum of sizes vs `statvfs`/`GetDiskFreeSpaceEx`).

**Execute** (on a worker thread, progress events per file):
1. Flush pending sidecar writes for all picked images (hard barrier).
2. Per image: copy RAW → fsync → copy sidecar → fsync. Sidecar is renamed in
   lockstep (`newname.ARW` ⇒ `newname.ARW.xmp`).
3. Verify: destination size == source size for both files (v1; checksums are v2).
4. On per-file failure: record, continue with remaining files (no partial-file left
   behind — copy to temp name, rename on success).
5. Final report: copied / skipped / failed with reasons. Session marks copied
   images with a "copied" badge.

Cancellation: between files only; already-copied files remain (report says so).

## Acceptance criteria (tests)

- [ ] Plan: template expansion, collision detection, dest-inside-source rejection,
      exists-handling in all three modes (tempdir fixtures).
- [ ] Execute: RAW+sidecar pairs land with correct names; sizes verified; a
      read-protected source file fails alone, others complete.
- [ ] Sidecar barrier: a pick made ≤1 s before "copy" is present in the copied
      sidecar (regression for the debounce race).
- [ ] No partial files after simulated failure (temp-name copy verified).
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      (`CON`, `NUL`, trailing dots) at plan time.
