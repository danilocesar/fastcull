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
button). **SUPERSEDED AND REMOVED 2026-08-21 by "The clash question"
below**: the four-way `ExistsMode`, the auto-suffix that never asked, and
the `DestExists`/abort error are gone from the code. What survives is the
SHAPE of the rename (a numeric suffix before the extension, sidecar in
lockstep — now from `_1`, and only as the answer "keep both") and the
no-148-row-table rule.

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
- **Re-run trap (persona IN-MY-WAY on the raw spec)** — **SUPERSEDED
  2026-08-21 by "The clash question"**. The problem it named is real (a
  re-run must not duplicate every already-copied file) and the answer is
  now a question rather than a silent skip: the user answers "overwrite
  everything", which re-verifies the copies that are there instead of
  re-sending them. What is GONE: the forced session-skip, the "N already
  at destination (skipped)" plan line, and the skip toggle. What SURVIVES:
  the sidecar-alone refresh, now inside overwrite (the caption-after-copy
  recovery), and the ✓ copied badge as a glanceable, non-deciding hint.
- **"Already copied" means "still there" (bug fix 2026-08-21, issue #14;
  its PLANNING half superseded the same day by "The clash question")**:
  the session records, per image and PER DESTINATION, the exact RAW path a
  copy landed at (`fileops::SessionCopies`), and re-checks that path every
  run. That record is now **read-only** — the ✓ copied badge and the
  plan's "N copied earlier but gone from the destination — copying again"
  note — because letting it DECIDE is what caused the bug: an id-only set
  forced a Skip over a folder the user had emptied by hand, so the sidecar
  came back as a refresh and the RAW never did, and the Skip-existing
  toggle could not override it. Deleted with the clash question: the
  forced skip, the landed-name judging and `is_collision_suffix_of` —
  issue #14's bug class (our sidecar written beside a foreign RAW) is
  structurally impossible now, because a sidecar is only ever written
  beside its own RAW under a name that is free or explicitly overwritten.
  One record per destination: A → B → A in one session still knows about
  both. The badge follows the disk (the Copy dialog re-checks on open, a
  gone copy loses it and regains it when the copy lands again). Open
  persona questions (relayed to the user, not decided): cross-session
  memory of what was copied; an escape on the note for users who rearrange
  the selects folder.
- **Exists-handling UI** — **SUPERSEDED 2026-08-21**: the rename default,
  the "Skip existing" toggle and "overwrite is never exposed" are all
  replaced by the three answers of the clash question (which does expose
  overwrite — see the recorded consequence at the end of that section).
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

## The clash question — collision handling v2 (user decisions 2026-08-21;
IMPLEMENTED 2026-08-21, wording settled with the persona at that point)

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
symlink, or a case-variant on a case-insensitive volume all count —
`exists()` would report a broken symlink as absent and rename straight
over it). Session memory is not consulted.

**The PAIR is the unit** (implementation decision 2026-08-21, recorded
because it is stricter than the sentence above): a destination pair
clashes when EITHER member is occupied — including a stray `<name>.xmp`
beside no RAW, and including the case where the pick has no sidecar of its
own to write. The alternative (check only the names we will actually
write) would let a RAW land next to a sidecar describing some other
photograph, which is the one thing this module must never produce. The
same rule governs the suffix search below, so occupancy is judged
identically in both places.

**2. One question per run; the answer applies to the whole operation**
(user decision: *"one file duplicate triggers the question … then this
option is valid the whole operation"*). No clashes → no question, today's
flow unchanged. Any clash → the dialog asks, in the spirit of *"We
detected clashes on filenames"*, naming the destination and the counts,
with three answers:

- **Overwrite everything** (labelled *"Overwrite those N"* in the dialog
  — the count is what stops the word overstating what happens; §6) —
  clashing files are replaced in place; clash-free files copy normally. A clashing RAW whose destination copy is
  already byte-identical to the source is NOT re-transferred: only its
  sidecar is rewritten, and only if it differs ("N sidecars replaced
  beside an identical RAW" in the report). That is where the v1 sidecar-alone refresh survives (user
  decision 2026-08-21: keep the refresh) — the caption-after-copy recovery
  stays cheap, without the RAW crossing the wire twice. Identity is BLAKE3
  of the destination against the source stream (the hash the copy computes
  anyway; the read is cheaper than the rewrite), never size-or-mtime
  guessing.
- **Create copies** (labelled *"Keep both"* in the dialog — every button
  here creates copies, so that phrase named the operation rather than the
  choice; §6) — every clashing image lands under the first free
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
is never executed) — the plan built to ASK is dropped, and the executor
refuses the whole run if one ever reaches it (it copies nothing and
reports "unanswered clash question", which is also what Cancel means).
Free space and the "N to copy" summary are computed for the chosen
policy; the pre-answer summary states the worst case (clash-free +
clashing bytes).

*Which total has to FIT* (implementation decision 2026-08-21): before the
answer and under **overwrite**, only the CLASH-FREE total — the clashing
images mostly replace bytes that are already there, one verified temp file
at a time, and a destination that really is full then fails those files
one by one with an honest reason without ever destroying the file that was
there. Under **create copies** the whole total must fit, because every
clashing image is a new file. Blocking an overwrite re-run on a
nearly-full destination that it would barely grow is the failure this
avoids; the cost is that a genuinely full disk is discovered per file
rather than up front.

**4. Nothing is replaced unless the user answered Overwrite.** The
executor commits its verified temp file into place without clobbering; a
name that got occupied between the question and the copy fails THAT file
honestly ("a file appeared at the destination during the copy") and the
run continues. Two same-run names differing only in case therefore cannot
eat each other on a case-insensitive destination.

The primitive is `hard_link(tmp, dst)` + unlink of the temp: the portable
"create this name only if it is free", which fails with `AlreadyExists`
instead of clobbering — `rename` (used ONLY for an answered overwrite)
would replace silently. On a filesystem with no hard links (FAT/exFAT
cards, some network mounts) the link fails with something other than
`AlreadyExists` and the fallback is check-then-rename; that check-to-
rename window is unavoidable there and is recorded rather than hidden.
Overwrite itself never removes a DIRECTORY standing under a planned name
(the rename fails, that file fails alone) and replaces a symlink as a
link, never writing through to its target (persona 2026-08-21). Nothing
at the destination is ever DELETED, only replaced: a pick that has no
sidecar of its own leaves a foreign `.xmp` sitting beside the RAW it
overwrote (rare — picking writes a sidecar — and the honest answer for
that debris is "keep both", which walks the pair onto a free number).

**5. Session memory reads, never decides.** `SessionCopies` survives for
the ✓ copied badge and the "N copied earlier but gone from the destination
— copying again" note. The forced skip, the landed-name judging and
`is_collision_suffix_of` are deleted with this change: issue #14's bug
class (our sidecar written beside a foreign RAW) becomes structurally
impossible, because a sidecar is only ever written beside its own RAW,
under a name that is either free or explicitly overwritten. The v1 "Skip
existing" toggle and the four-way `ExistsMode` are replaced by the three
answers.

**6. Wording and keys** (settled with the persona 2026-08-21; the
question is a STATE of the Copy dialog — `copy-state 3` — not a second
modal, so there is one key scope and no new stacking surface, issue #42).
The question states what each answer does and what it costs, rather than
yes/no (persona: at 9pm "proceed" reads as "proceed with the copy I asked
for"). As shipped:

```
12 of your 148 picks already have files with these names in
…/2026-08-21-osprey/selects
The other 136 copy normally. Choose once for the whole run:
e.g. DSC01234.ARW, DSC01235.ARW, DSC01240.ARW …

 B    Keep both — the 12 land as DSC01234_1.ARW        +590 MB
 O    Overwrite those 12 — identical files are re-checked, not re-sent
 Esc  Cancel — copy nothing at all, not even the 136
```

- Counts are in **picks**, never files (148 picks are 296 files on disk;
  a count the user cannot reconcile is a count they stop trusting), and
  the "other N copy normally" clause is mandatory: once Cancel drops
  everything, the user no longer assumes the other answers behave
  normally.
- **"Overwrite those 12", not "overwrite everything"** — the count in the
  label is what stops the word overstating what happens. Bytes appear on
  "keep both" ONLY: it is the one answer whose cost is knowable up front,
  and a worst-case number on overwrite would state a cost the identity
  check means the user never pays.
- Three answers do not fit side by side in the 560px card (measured), so
  they are stacked rows in order of increasing consequence, Cancel set
  apart. **No answer carries accent/default styling** — a destructive
  answer that looks pre-chosen gets pressed reflexively — and only the
  word "Overwrite" is coloured.
- Keys: `B`, `O`, `Esc`. **Enter and Space are inert** (Ctrl+E, Enter,
  Enter must never mass-replace or mass-duplicate; Space is the pick key),
  as are `Y`/`N`, and no button takes initial focus. A key that is not an
  answer is swallowed AND flips a visible "Pick one: B, O or Esc" line —
  a silently dead Enter reads as a frozen dialog.
- **Esc returns to the plan preview** with destination and template
  intact (a second Esc closes the dialog): the topmost-first Esc rule, and
  it makes "cancel, then copy somewhere else" one step.
- The plan preview pre-announces the split — `3 new · 148 already exist
  here — Copy will ask what to do` — which cross-session (no ✓ badges
  after a restart) is the only signal that the folder already holds this
  shoot.
- The destination is shown TAIL-first (`…/2026-08-21-osprey/selects`)
  wherever it is elided: Slint's elide cuts the end, i.e. exactly the part
  that tells two shoots apart.
- The progress line says WHICH work it is doing (`Checking 12 / 148` for
  an overwrite, which starts by hashing, vs `Copying 2 / 3`) — counting to
  148 while saying "copying" reads as "it is sending my whole export
  again".
- The report counts what actually happened: `3 copied`, `145 already
  identical — re-verified in place`, `12 landed under new names
  (DSC01234_1.ARW …)`, `12 replaced`, `N sidecars replaced beside an
  identical RAW`. "All checksums verified" now attaches to copied AND
  re-verified files: an identity check IS a BLAKE3 verification of the
  destination against the source, so a re-run doubles as a free "is my
  export still bit-perfect?" pass before the card is wiped (persona).

**Recorded consequence**: exposing Overwrite reverses the v1 decision
"overwrite is never exposed in v1 — it is the one that can destroy a
verified prior copy" (user decision 2026-08-21). It is bounded by the
verified-temp-then-commit contract — a failed or corrupt transfer never
replaces a good file — but it does replace a destination file that differs
from the source: another body's frame under the same name, or a copy the
user edited in place.

**Recorded consequence**: a re-run into a folder that still holds the
session's own copies now ASKS (they are clashes like any others) —
confirmed by the user 2026-08-21: *"it's fine. If you're saving where
there are files already, it should ask."* The answer that adds the new
picks is Overwrite, which re-verifies the existing ones rather than
skipping them; identical RAWs are not re-transferred (see above), so the
cost is a read, not a write. On a network destination that read is real:
~2× the clashing bytes over the wire to add three frames (persona; open
question 5 to the user).

**Recorded consequence, and the persona's blocker** (2026-08-21, relayed
to the user, NOT decided here): a destination sidecar that DIFFERS is
byte-replaced under Overwrite — and darktable's history stack lives in a
file of exactly the name FastCull uses (`DSC01234.ARW.xmp`,
xmp-sidecars.md invariant 2). A user who has started developing the
copies in darktable and then answers Overwrite loses those history
stacks; with "skip" gone there is no other answer that adds new picks to
that folder. The question therefore says so out loud
("Overwriting also replaces those files' .xmp sidecars — edits made at the
destination by another app (darktable) are lost"), and the persona's
proposed fix — merge into the destination sidecar (read-modify-write,
preserving foreign nodes) instead of copying over it — is an OPEN
QUESTION for the user, because it would make the copied sidecar differ
from the source and so cannot be checksum-verified against it (the
"Execute" contract above). Recovery today: delete that copy at the
destination and copy again, or re-import in darktable.

## Acceptance criteria (tests)

- [x] Plan: template expansion, in-plan collision detection,
      dest-inside-source rejection (tempdir fixtures) —
      plan_templates_seq_and_rejects_collisions,
      plan_rejects_dest_inside_or_equal_to_source.
- [x] Execute: RAW+sidecar pairs land with correct names; checksums verified
      (and a deliberately corrupted destination write is detected, under
      both commit modes — a corrupt REPLACE never destroys the file it
      would have replaced); a read-protected source file fails alone,
      others complete — execute_copies_verifies_and_isolates_failures,
      copy_verified_detects_corruption_and_cleans_up,
      cancel_between_files_keeps_finished_copies.
- [x] Sidecar barrier: a pick made ≤1 s before "copy" is present in the copied
      sidecar (regression for the debounce race) —
      sidecar_barrier_fresh_pick_lands_in_the_copy.
- [x] Re-run after a hand deletion copies again, RAW+XMP together, with the
      "copied earlier but gone" note and no question when the destination
      is genuinely empty (the 2026-08-21 bug, 1:1) —
      a_hand_deleted_copy_goes_out_again_with_no_question; the memory is
      per destination and a re-spelled folder supersedes its own entry —
      session_copies_are_remembered_per_destination_for_the_badge,
      record_supersedes_the_entry_of_a_re_spelled_folder; app-level:
      copy_picks_rerun_recopies_hand_deleted_files (screenshot.rs, driven
      through `copydest:`, real A1 files).
- [x] No partial files after simulated failure (temp-name copy verified) —
      asserted inside execute_copies_verifies_and_isolates_failures.
- [x] Clash question (v2): the check sees RAW *and* sidecar names, on
      templated names, from the filesystem (directory, symlink, broken
      symlink count) — a_directory_or_a_broken_symlink_under_a_planned_
      name_is_a_clash, the_clash_check_sees_templated_names_and_never_
      reflows_seq, a_sidecar_left_behind_is_a_clash_the_answers_resolve_
      both_ways; one question per run whose answer is a whole-run policy,
      with its clashes and their bytes counted apart —
      ask_marks_the_clashes_and_counts_their_bytes_apart; Overwrite
      replaces in place but re-copies only the sidecar when the destination
      RAW is byte-identical, and never removes a directory or writes
      through a symlink —
      overwrite_replaces_a_differing_file_and_only_refreshes_an_identical_
      one, overwrite_never_removes_a_directory_and_replaces_a_symlink_not_
      its_target; Create copies suffixes from `_1` before the extension,
      RAW/sidecar in lockstep, advancing the pair when either member is
      taken (on disk or in-plan) —
      create_copies_suffixes_from_1_and_moves_the_whole_pair; Cancel copies
      nothing at all (and an unanswered plan is refused wholesale) —
      execute_refuses_a_plan_built_before_the_answer; a name occupied after
      the question fails that file alone without clobbering —
      a_name_taken_after_the_plan_fails_that_file_alone; free space follows
      the chosen policy — the_free_space_check_follows_the_answer.
      App-level, driven through the real dialog with real key events:
      copy_picks_asks_once_and_each_answer_does_what_it_says (the question
      appears with its counts, Enter is inert on it, B lands `_1`, O
      replaces the differing file and re-verifies the identical one, Esc
      returns to the plan and copies nothing at all — proven on a second
      destination folder that stays untouched).
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      — DEFERRED with the user's explicit OK (2026-07-26, "low priority"),
      tracked as issue #10; spaces/Unicode half already QE-verified
      (`CON`, `NUL`, trailing dots) at plan time.
