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
after persona review). Auto-renames are summarized (count) in the plan preview; a per-file
list is v2 (persona: no 148-row table between the user and the Copy
button). **Superseded where they disagree by "Confirm, then suffix"
below (user decision 2026-08-21, NOT YET IMPLEMENTED).**

**Execute** (on a worker thread, progress events per file):
1. Flush pending sidecar writes for all picked images (hard barrier).
   **ORDERING CONTRACT (gate finding 2026-07-26): the flush precedes
   PLANNING, not just execution** — plan() freezes sidecar-existence and
   refresh-mtime answers at plan time, so a plan built before the flush
   ships RAWs without their fresh sidecars while reporting verified. The
   app flushes at dialog open (truthful preview) AND flushes-then-replans
   inside Copy itself; a frozen at-open plan is never executed. Free
   space is likewise re-checked by that final replan; an unreadable
   statvfs reports "free space unknown" and skips the check rather than
   inventing a number.
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

Cancellation: between files only; already-copied files remain (report says
so). Dropping the copy handle (quit / Open Folder mid-copy) CANCELS then
joins: the wait is bounded by the file in flight and the temp-name
contract leaves no partial behind.

The final report's "all checksums verified" sentence appears ONLY when
the run actually copied and verified at least one file — an all-skipped
run verified nothing and must not print the format-the-card green light.

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
  destination default to SKIP, listed as "N already at destination
  (skipped)"; the session-only copied badge is what makes that plan line
  glanceable.
  Cross-session, the plan's "N exist at destination" summary + the skip
  toggle is the safety net. When skipping an existing RAW whose SOURCE
  sidecar changed since, the sidecar alone is re-copied ("N sidecars
  refreshed" in the report) — the card-format caption-after-copy
  recovery is "Ctrl+E again before quitting".
- **"Already copied" means "still there" (bug fix + persona review
  2026-08-21)**: the session remembers, per image and PER DESTINATION,
  the exact RAW path the copy landed at (`fileops::SessionCopies`), and
  the plan re-checks that path every run. A copy the user deleted by
  hand (RAW, or RAW+XMP) is copied again as a normal pick — RAW and
  sidecar together, verified, under the normal collision rules — and
  the plan says so: "N copied earlier but gone from the destination —
  copying again" (counted only for files that actually go out again).
  The skip default keys on the LANDED name, so a `_2` copy is skipped
  and sidecar-refreshed as `_2`, never judged under the natural name
  beside a foreign file (issue #14), and a later template change skips
  the old copy instead of refreshing an orphan sidecar under the new
  name. A gone `_2` copy also tells the plan that the natural name is a
  foreign file: with Skip-existing ON it still goes out as `_2` again,
  and that foreign file's sidecar is never refreshed (gate finding).
  Only the collision suffix is evidence — a landed name that differs
  because the template changed says nothing, and Skip-existing then
  means skip. The suffix is matched by shape (exact `suffixed` parity:
  `_k`, k ≥ 2, no leading zero), not provenance: a `{filename}_{seq}`
  rename template can produce the same name for the session's own copy,
  and the worst outcome there is one extra verified `_2` copy with an
  honest note, never a touched foreign file — accepted (gate round 3);
  carry the CopyRenamed fact with the landed path if cross-session
  memory of copies is ever added. One record per destination: A → B → A in one session still
  skips A's copies. The copied badge follows the disk: the Copy dialog
  re-checks on open, a gone copy loses the badge and regains it when
  the copy lands again. The previous implementation (an id-only set)
  forced the skip over an empty folder — the sidecar came back as a
  refresh, the RAW never; the Skip-existing toggle could not override
  it. Open persona questions (relayed to the user, not decided): a
  "don't re-copy the moved ones" escape on the note for users who
  rearrange the selects folder; cross-session memory of what was
  copied; a same-size guard against refreshing a foreign file's sidecar
  cross-session.
- **Exists-handling UI**: rename default; ONE "Skip existing" toggle shown
  only when collisions exist; overwrite is never exposed in v1 (core
  keeps the mode; it is the one that can destroy a verified prior copy).
- **Ctrl+E commits any in-progress panel field edit** (G7 click-away
  semantics) BEFORE the plan and the flush barrier — a half-typed caption
  must ship.
- **`{seq}` for rename templates follows the SESSION SORT ORDER** (capture
  While a folder is still LOADING that order is deliberately not what the
  grid shows: issue #25 holds the view in filename order until every
  metadata job finishes, but `{seq}` keeps following the true sort, because
  it is baked into permanent filenames and must not encode a transient view
  state. Consequence, recorded: a copy started mid-load numbers files in an
  order matching neither the screen nor the same copy run a few seconds
  later — the capture sort is only partial until the load ends. See
  ui-grid.md, *Provisional order while loading*.
  time default) — same caller contract as IPTC apply; with all-picks
  scope, "view order" would be ambiguous under an active filter.
- Dialog minimums: destination picker (must allow creating a folder) with
  the remembered path displayed PROMINENTLY (yesterday's job is the
  failure mode); template field defaults to EMPTY = keep names; the remembered
  template is OFFERED as a one-click "Use last: …" chip, never silently
  pre-applied (gate-enforced); live preview of the first 3 expanded
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

## The clash question — collision handling v2 (user decisions 2026-08-21,
NOT YET IMPLEMENTED)

The rule as the user stated it: *"if I ask to copy the files to a folder,
you copy the files — maybe add a warning that the files already exist.
Context shouldn't matter more than that."* The session's memory of an
earlier copy must never decide what gets copied (that memory caused the
2026-08-21 bug recorded above); the disk decides, and the user answers one
question.

**1. The clash check.** After the flush barrier and the final replan, and
before any byte moves, every name the plan would write — the RAW **and**
its sidecar, after template expansion — is checked against the
destination. A name is occupied if the filesystem says so
(`symlink_metadata`: a regular file, a directory, a symlink, a broken
symlink, or a case-variant on a case-insensitive volume all count).
Session memory is not consulted.

**2. One question per run; the answer applies to the whole operation**
(user decision: *"one file duplicate triggers the question … then this
option is valid the whole operation"*). No clashes → no question, today's
flow unchanged. Any clash → the dialog asks, in the spirit of *"We
detected clashes on filenames"*, naming the destination and the counts,
with three answers:

- **Overwrite everything** — clashing files are replaced in place;
  clash-free files copy normally. A clashing RAW whose destination copy is
  already byte-identical to the source is NOT re-transferred: only its
  sidecar is rewritten, and only if it differs ("N sidecars refreshed" in
  the report). That is where the v1 sidecar-alone refresh survives (user
  decision 2026-08-21: keep the refresh) — the caption-after-copy recovery
  stays cheap, without the RAW crossing the wire twice. Identity is BLAKE3
  of the destination against the source stream (the hash the copy computes
  anyway; the read is cheaper than the rewrite), never size-or-mtime
  guessing.
- **Create copies** — every clashing image lands under the first free
  numeric suffix, appended to the file-name stem **before** the extension
  (`DSC01234_1.ARW`, never `DSC01234.ARW_1`), **starting at `_1`** (v1
  started at `_2`). A number `k` is free only when BOTH `<stem>_k.<ext>`
  and `<stem>_k.<ext>.xmp` are free — on disk and unclaimed by this plan —
  so a clash on either member moves the pair to `k+1`, and a copy is never
  split across two numbers. Growth is unbounded by design (`_1`, `_2`,
  `_3`, …): each layer costs a deliberate answer.
- **Cancel** — nothing is copied at all, not even the clash-free files
  (user decision; Esc means the same).

**3. The answer is a policy, not a file list.** After the answer the app
flushes and replans with the chosen policy, and only that fresh plan
executes (the ordering contract above: a plan frozen before the question
is never executed). Free space and the "N to copy" summary are computed
for the chosen policy; the pre-answer summary states the worst case, and
the plan errors on space only when even the clash-free total does not fit.

**4. Nothing is replaced unless the user answered Overwrite.** The
executor commits its verified temp file into place without clobbering; a
name that got occupied between the question and the copy fails THAT file
honestly ("a file appeared at the destination during the copy") and the
run continues. Two same-run names differing only in case therefore cannot
eat each other on a case-insensitive destination.

**5. Session memory reads, never decides.** `SessionCopies` survives for
the ✓ copied badge and the "N copied earlier but gone from the destination
— copying again" note. The forced skip, the landed-name judging and
`is_collision_suffix_of` are deleted with this change: issue #14's bug
class (our sidecar written beside a foreign RAW) becomes structurally
impossible, because a sidecar is only ever written beside its own RAW,
under a name that is either free or explicitly overwritten. The v1 "Skip
existing" toggle and the four-way `ExistsMode` are replaced by the three
answers.

**6. Wording and keys.** The question states what each answer does and
what it costs, rather than yes/no (persona: at 9pm "proceed" reads as
"proceed with the copy I asked for"). The destructive answer (Overwrite)
must not sit on `Y`, `N` or Enter — the culling keys and the
do-the-obvious-thing key; Esc = Cancel. Exact labels are settled with the
persona at implementation time.

**Recorded consequence**: exposing Overwrite reverses the v1 decision
"overwrite is never exposed in v1 — it is the one that can destroy a
verified prior copy" (user decision 2026-08-21). It is bounded by the
verified-temp-then-commit contract — a failed or corrupt transfer never
replaces a good file — but it does replace a destination file that differs
from the source: another body's frame under the same name, or a copy the
user edited in place.

**Recorded consequence**: a re-run into a folder that still holds the
session's own copies now ASKS (they are clashes like any others). The
answer that adds the new picks is Overwrite, which re-verifies the
existing ones rather than skipping them; identical RAWs are not
re-transferred (see above), so the cost is a read, not a write.

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
- [x] Re-run after a hand deletion copies again (RAW+XMP, RAW-only), the
      skip default follows the landed `_2` name (issue #14), is kept per
      destination, and a new template leaves no orphan sidecar —
      rerun_recopies_a_destination_the_user_deleted_by_hand,
      rerun_ships_the_pair_when_only_the_raw_was_deleted,
      session_skip_follows_the_landed_name_not_the_natural_one,
      session_copies_are_remembered_per_destination,
      rerun_with_a_new_template_keeps_the_landed_copy_without_orphans,
      record_supersedes_the_entry_of_a_re_spelled_folder,
      collision_suffix_shape_is_recognized,
      skip_existing_is_honored_when_the_only_evidence_is_a_template_change;
      app-level: copy_picks_rerun_recopies_hand_deleted_files
      (screenshot.rs, driven through `copydest:`).
- [x] No partial files after simulated failure (temp-name copy verified) —
      asserted inside execute_copies_verifies_and_isolates_failures.
- [ ] Clash question (v2, not yet implemented): the check sees RAW *and*
      sidecar names, on templated names, from the filesystem (directory,
      symlink, broken symlink count); one question per run whose answer is
      a whole-run policy; Overwrite replaces in place but re-copies only
      the sidecar when the destination RAW is byte-identical; Create
      copies suffixes from `_1` before the extension, RAW/sidecar in
      lockstep, advancing the pair when either member is taken; Cancel
      copies nothing at all; a name occupied after the question fails that
      file alone without clobbering; free space and the summary follow the
      chosen policy.
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      — DEFERRED with the user's explicit OK (2026-07-26, "low priority"),
      tracked as issue #10; spaces/Unicode half already QE-verified
      (`CON`, `NUL`, trailing dots) at plan time.
