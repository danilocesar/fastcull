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

## Dialog + scope decisions (persona review 2026-07-26; the user CONFIRMED
2026-07-26: "metadata is added before copying. once the copy is done,
it's over" — so no caption-after-copy guard is needed and the
changed-sidecar refresh below is a belt-and-braces detail, not a
workflow pillar; scope v1 = "everything with a star", subset copy
explicitly deferred to a later discussion; modal dialog accepted)

- **Scope: ALL picked images in the session, filter-independent** (the
  inbox-zero loop ends with an EMPTY view — "current view's picks" would
  copy zero files at the exact moment the feature is reached for). The
  dialog's count line ("148 picked images") states the scope; spec text:
  the filter bar does not affect Copy Picks. Subset copy is v2
  (multi-selection exists but is not wired here).
- **Re-run trap (persona IN-MY-WAY on the raw spec)**: rename-suffix is
  the right default for multi-camera DIFFERENT-file collisions, wrong for
  re-running into the same destination (it would duplicate every
  already-copied file). Images copied this session to the same
  destination default to SKIP, listed as "N already copied"; the
  session-only copied badge is what makes that plan line glanceable.
  Cross-session, the plan's "N exist at destination" summary + the skip
  toggle is the safety net. When skipping an existing RAW whose SOURCE
  sidecar changed since, the sidecar alone is re-copied ("N sidecars
  refreshed" in the report) — the card-format caption-after-copy
  recovery is "Ctrl+E again before quitting".
- **Exists-handling UI**: rename default; ONE "Skip existing" toggle shown
  only when collisions exist; overwrite is never exposed in v1 (core
  keeps the mode; it is the one that can destroy a verified prior copy).
- **Ctrl+E commits any in-progress panel field edit** (G7 click-away
  semantics) BEFORE the plan and the flush barrier — a half-typed caption
  must ship.
- **`{seq}` for rename templates follows the SESSION SORT ORDER** (capture
  time default) — same caller contract as IPTC apply; with all-picks
  scope, "view order" would be ambiguous under an active filter.
- Dialog minimums: destination picker (must allow creating a folder) with
  the remembered path displayed PROMINENTLY (yesterday's job is the
  failure mode); template field defaults to EMPTY = keep names (remembered
  but never silently pre-applied); live preview of the first 3 expanded
  names when a template is set; count + total size + free space up front;
  collisions summarized ("3 will be renamed") not tabulated; Enter
  triggers Copy when the plan is clean; per-file N/M progress + cancel;
  final report says "all checksums verified" explicitly (the green light
  to format the card) + failures with reasons + "Open destination folder";
  Ctrl+E with zero picks opens with "No picked images", never a silent
  no-op. Modal in v1. Cut from v1: per-file mode selectors, speed/ETA
  displays, pause, background copy.

**Rejects are not fileops' business (recorded user decision)**: after copy-picks,
rejected and unmarked files stay untouched where they are; the user deletes them
manually later. No move/delete-rejects operation in v1 (revisit only if asked).

Note for verification design (persona observation 2026-07-25): a truncated
ARW can still show a perfect thumbnail — the embedded JPEG sits at the front
of the file — so "looks fine in the grid" proves nothing about integrity;
BLAKE3 verification is the only truth at copy time.

## Acceptance criteria (tests)

- [x] Plan: template expansion, collision detection, dest-inside-source rejection,
      exists-handling in all four modes incl. rename-suffix lockstep (tempdir
      fixtures) — plan_templates_seq_and_rejects_collisions,
      plan_rejects_dest_inside_or_equal_to_source,
      exists_modes_rename_skip_abort_and_session_skip,
      skip_refreshes_changed_sidecar_only.
- [x] Execute: RAW+sidecar pairs land with correct names; checksums verified
      (and a deliberately corrupted destination write is detected); a
      read-protected source file fails alone, others complete —
      execute_copies_verifies_and_isolates_failures,
      copy_verified_detects_corruption_and_cleans_up,
      cancel_between_files_keeps_finished_copies.
- [x] Sidecar barrier: a pick made ≤1 s before "copy" is present in the copied
      sidecar (regression for the debounce race) —
      sidecar_barrier_fresh_pick_lands_in_the_copy.
- [x] No partial files after simulated failure (temp-name copy verified) —
      asserted inside execute_copies_verifies_and_isolates_failures.
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      (`CON`, `NUL`, trailing dots) at plan time.
