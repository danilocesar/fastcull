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
- Destination that exists but is NOT a folder — a regular file, or a
  DANGLING symlink, which `metadata()` cannot see (QE + gate findings
  2026-08-21: both used to reach the copy and come back as a pile of
  "File exists (os error 17)" per-file failures). A symlink TO a folder is
  a fine destination.
- Destination inside the source folder, or equal to it.
- A rename template that produces a PATH rather than a file name (a `/`,
  a `\`, `..`, `.`, or the empty string). This enforces the INVARIANT the
  user set on 2026-08-22 — *"we should never write files outside of the
  target"* — which the whole module owes: every byte it writes, including
  suffixed names and the hidden temp files, lands DIRECTLY in the chosen
  destination folder, and `planned_paths_never_leave_the_destination`
  holds every policy and template to it. Both separators are refused on
  every platform so one template means the same thing on Linux and
  Windows.
  *(Two images expanding to the same destination name used to be a
  plan-time error. It is not any more — see "two picks, one name" below.)*
- A rename TEMPLATE whose expansion has no stem — anything starting with
  `.`, which `{camera}.{ext}` produces today because the app never fills
  `{camera}` (QE finding 2026-08-22). It would write `.ARW`: a hidden file
  whose whole name is its extension, which FastCull's own scan skips (no
  extension left to match) and darktable never sees, and the suffix walk
  then yields `.ARW_1`, which has lost the extension as well. Perfect
  copies nobody can see are worse than a refusal. **Templated names only**
  (gate finding 2026-08-22): applied to ORIGINAL names this refused the
  whole copy over a file the user did not name — `catalog.rs` admits by
  extension alone, so a macOS AppleDouble stub `._DSC0001.ARW` is a
  pickable cell, and one such pick blocked every other file with a message
  about a template that was never typed. The user's own file names are
  their business; only names WE invent have to be sane. **Open, separate
  from this rule**: `{camera}` expanding to nothing at all in the app is
  its own bug (`copy_bridge::plan_sources` passes `camera: None`) while
  the template docs offer the variable.
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
lockstep — now from `_1`, as the answer "keep both" and, without asking,
when two picks in one run share a name) and the no-148-row-table rule.

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
   behind — copy to temp name, commit on success). The temp name is
   `<dest>/.fastcull-partial-<pid>-<n>`: short (never the final name plus a
   suffix — that added ~25 bytes and could push a long templated name past
   NAME_MAX, landing a RAW and failing its sidecar), UNIQUE PER FILE, and
   created EXCLUSIVELY (`create_new`).
   **The uniqueness is load-bearing, not tidiness** (gate finding
   2026-08-21): the commit hard-links the temp to its final name and then
   unlinks the temp, and that unlink is allowed to fail — a Windows
   sharing violation from a scanner is the ordinary cause. The name left
   behind is then a SECOND NAME FOR THE FILE JUST COMMITTED, so a shared
   or predictable temp name means the next file truncates a copy already
   reported verified. Unique names plus `create_new` make that
   impossible rather than unlikely; a leftover number is stepped over,
   never opened. The counter is atomic, so the v2 background copy can add
   a second worker on this path without changing the rule.
5. Final report: copied / skipped / failed with reasons. Session marks copied
   images with a "copied" badge.

Cancellation: between files only; already-copied files remain (report says
so). Dropping the copy handle (quit / Open Folder mid-copy) CANCELS then
joins: the wait is bounded by the file in flight and the temp-name
contract leaves no partial behind — with ONE recorded exception (QE
2026-08-21): a hard QUIT does not join anything. `shutdown()` ends in
`process::exit` on purpose (01-architecture.md: 32 readers stuck on a
dying card once made the process unkillable), so `CopyHandle::drop` never
runs and the file in flight leaves its temp behind: one file per hard quit
mid-copy, `<dest>/.fastcull-partial-<pid>-<n>`. Because those names are
unique per file (they have to be — see step 4) they ACCUMULATE rather than
being reused, and the leading dot hides nothing on Windows (gate finding
2026-08-22). Nothing sweeps them: deleting at the destination is not this
module's business, and a name a live process is still writing must never be
removed by another. They can never be mistaken for a photograph, and
joining a copy to a dead card on quit is the worse bargain. An overwrite's identity pass reads as
much as a copy writes, so it polls the cancel flag between blocks and
gives the run back at once (gate finding 2026-08-21) — otherwise the join
on the UI thread would span a whole re-verify of a big RAW on top of the
file in flight. Open Folder mid-copy also ends the dialog honestly:
the run is cancelled by the drop, so the dialog says so instead of
sitting at "running" with a Cancel button that does nothing.

The final report's "all checksums verified" sentence appears ONLY when
the run actually copied and verified at least one file — an all-skipped
run verified nothing and must not print the format-the-card green light.
Since 2026-08-22 that rule is `CopyReport::earned_the_green_light()` in
CORE (it is a fact about the copy, not about the dialog — CLAUDE.md rule
5); a run an overwrite found byte-identical counts as verified, because
that check is a BLAKE3 comparison of both ends.

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
  split across two numbers. Recorded limit (gate finding 2026-08-22): a
  destination name already within a few bytes of the filesystem's maximum
  can still overflow through the `_k` and the `.xmp` — the RAW lands and
  the sidecar fails, honestly reported, and `occupied()` cannot tell "too
  long" from "free" at plan time. Issue #10 territory (name-length and
  reserved-name handling per platform). Growth is unbounded by design (`_1`, `_2`,
  `_3`, …): each layer costs a deliberate answer.
- **Cancel** — nothing is copied at all, not even the clash-free files
  (user decision; Esc means the same).

**Two picks, one name** (user decision 2026-08-22: *"the corner case of
two same filenames from two different folders should always add the
sufix"*). A name can be taken by two different things, and they get
different answers. The DESTINATION holding it is the user's business —
their folder, their earlier files — so the question is asked. Another
pick in THIS run holding it is not a question at all: overwriting would
throw away one of the two photographs the user just asked to copy, and
cancelling would lose both, so the later pick simply takes the first free
suffix under EVERY policy, Ask included, and the run proceeds without
asking. The suffix walk skips names taken on disk as well as in the plan,
so the name it lands on clashes with nothing — which is why it needs no
question even into a crowded folder. It is silent but not invisible: the
plan preview says "N picks share a name with another — those get a
suffix", counted in `CopyPlan::shared_name`, which is kept apart from
`renamed` (a suffix taken because the DESTINATION held the name) so
neither sentence can be said about the other's files.

EXACT-CASE, and that is the one exception to "always" (gate finding
2026-08-22): the in-plan claim set compares names literally, so two picks
named `C.ARW` and `c.arw` are two names to us. On a case-SENSITIVE
destination that is correct — they are two files and both land. On a
folding one they are one name, and the second pick FAILS instead of being
suffixed: refused by the no-clobber commit on the clash-free path, or by
the same-run identity guard under overwrite (§4). Failing is the safe
direction — the alternative is destroying a verified copy — but it is a
failure where this rule promises a suffix, and closing it needs the
destination's folding behaviour probed at plan time, which is on the
carried-forward NOT-VERIFIED list below.

COST, recorded because it is on the interactive path (gate finding
2026-08-22): the suffix walk RESUMES per base name instead of restarting
at `_1` for every pick. Restarting is quadratic, and the input that
triggers it is ordinary typing — a hand-typed template has a literal
prefix before its first `{`, so every pick expands to the same name while
the field is mid-word, and `plan()` runs on the Slint event-loop thread
on every keystroke. Measured for 2,000 picks on one name: 2 M `stat`
calls and 1.7 s (btrfs) to 2.3 s (tmpfs) restarting, versus ≈ 10 k stats
and 4-5 ms resuming — and a network destination multiplies the per-`stat`
cost by three orders of magnitude.
`many_picks_on_one_name_take_consecutive_suffixes` holds the PROBE COUNT,
not the clock: a test-only counter inside `occupied` (compiled out of a
real build) makes the plan's destination probes countable, and the test
asserts the EXACT figure — `4N - 2`, i.e. 7,998 for 2,000 colliding
names. That is the resuming shape written out: 2 probes for the first
pick (the natural name and its sidecar, no walk), then 4 for each of the
others (natural name and sidecar, resumed candidate and its sidecar); the
source sidecar's own `exists()` is a further stat per pick that this
counter does not see. An equality rather than a ceiling, because a walk
that resumes only PARTLY stays under a generous ceiling: a cursor rewound
by 5 on every pick measures 17,978 probes, which a "< 10 per pick" bound
would have passed. The test then plans the same names at 2N and requires
the SAME closed form — 15,998 probes for 4,000 picks — which is what
rules out a cost that merely coincides with `4N - 2` at one N: a
quadratic walk quadruples per doubling, an affine one lands there. The
count is the same number on every machine, which a stopwatch is not: the
earlier 300 ms bound flaked on a shared CI runner at 344 ms while the
walk was resuming correctly (issue #58).

What that fix does NOT do (gate finding 2026-08-22, recorded rather than
implied away): `plan()` is still LINEAR in stat calls — three per pick,
five when the name collides — and still runs synchronously on the
event-loop thread, once per keystroke in the template field, with no
debounce. On a local disk that is milliseconds for a 2,000-pick plan; on
a network or FUSE destination the same three-orders-of-magnitude
multiplier applies to the linear term and it becomes a visible freeze.
The eventual fix is a debounce or planning off-thread; the probe-count
test says nothing about wall-clock time — it
is a debug-build unit test on `temp_dir` guarding the SHAPE of the walk,
not a budget, and `perf_budgets.rs` has no plan-time entry. This also
covers a rename template that collapses several images onto one name
(`same.{ext}` → `same.ARW`, `same_1.ARW`, …), which was a blocking plan
error until this decision.

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
eat each other on a case-insensitive destination — under **keep both** and
the clash-free path because the commit refuses an occupied name, and under
**overwrite** because the executor additionally refuses to write over a
file THIS RUN already landed (gate finding 2026-08-22: overwrite commits
with a rename, which replaces silently, and the plan's in-plan `taken` set
is exact-case, so nothing else stood between two case-twins on an exFAT
card or an SMB share).

That last check is about FILE IDENTITY, not names: on unix, device +
inode, which two names for one file share and two different files never
do; off unix, where no stable file-index API exists, the folded name,
which is the right answer there because Windows filesystems fold case by
default — with the caveat that a Windows directory can be made
case-SENSITIVE (`fsutil file setCaseSensitiveInfo`, which is what every
WSL-created tree is), and there the folded name would refuse a copy the
user asked for, exactly as it did on ext4 before this correction. It FAILS
OPEN in one recorded place: a `symlink_metadata` that errors mid-run
leaves the identity unknown and the overwrite proceeds — refusing on doubt
instead would fail copies the user asked for on the far more common
case-sensitive destination, to protect a folding one. A RAW whose SIDECAR
then failed is not that case: it is on disk, so the commit is reported
through `raw_committed` and its identity is recorded even though the job
returns an error (gate finding 2026-08-22; the fix is unverifiable on this
machine — see the carried-forward list — because reaching it needs a
folding destination). Occupancy proves nothing — the destination name of an overwrite
is occupied by definition — and comparing folded NAMES failed every
overwrite of a case-twin on a case-SENSITIVE destination, where the two
names are two different files and both copies must go out as asked (QE
finding 2026-08-22, a regression this correction removes).

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
at the destination is ever DELETED, only replaced. Two consequences, both
recorded rather than prevented (QE 2026-08-21), and both leaving our RAW
beside an `.xmp` that describes another photograph:

- a pick that has no sidecar OF ITS OWN — its sidecar write failed, or the
  card is read-only, which makes it systematic rather than rare for that
  card — overwrites the RAW and leaves the foreign `.xmp` where it is. The
  report says so ("N destination sidecars left in place — those picks have
  none of their own") so the user hears it here rather than from darktable
  months later, and "keep both" is the answer that walks the pair onto a
  free number instead;
- the sidecar half of a pair can fail AFTER its RAW committed (a directory
  under the sidecar's name, ENOSPC, EACCES). That file is reported failed,
  with the reason spelling out which half landed: "the RAW landed but its
  sidecar did not: …". The same failure on the IDENTITY path — the
  caption-after-copy refresh beside a RAW that was already byte-identical —
  is not a failed file at all: the RAW there is this pick's and verified,
  so the run counts it as identical AND reports the sidecar ("the RAW at
  the destination is this pick's, verified — but its sidecar could not be
  refreshed: …"). Reporting that whole file as failed, with identical = 0,
  described a destination that was in fact correct (gate finding
  2026-08-22).

The opposite direction — OUR sidecar beside a foreign RAW, issue #14 — is
structurally prevented: a sidecar is only ever written after its own RAW
has committed.

**5. Session memory reads, never decides.** `SessionCopies` survives for
the ✓ copied badge and the "N copied earlier but gone from the destination
— copying again" note. The forced skip, the landed-name judging and
`is_collision_suffix_of` are deleted with this change: issue #14's bug
class (our sidecar written beside a foreign RAW) becomes structurally
impossible, because a sidecar is only ever written beside its own RAW,
under a name that is either free or explicitly overwritten. The MIRROR
image is not impossible and is not claimed to be (gate finding): a pick
that has no sidecar of its own — its write failed, or the card is
read-only — can overwrite a RAW and leave a foreign `.xmp` beside it,
because nothing at the destination is ever deleted (§4). "Keep both" is
the answer that walks the pair clear of it. The v1 "Skip
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

- The "keep both" row names the file that answer would REALLY make — the
  plan walks the suffix from `_1` and hands the dialog the first free
  pair, so a second keep-both into the same folder says `_2` rather than
  promising a `_1` the copy would not use (gate finding). Rare corner,
  recorded: because a suffix can also be claimed IN-PLAN, the number of
  renames can exceed the clash count when a pick is literally named
  `<other>_1.<ext>`.
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
  answer that looks pre-chosen gets pressed reflexively. The overwrite
  row's LABEL is amber; no row carries an accent box.
- Keys: `B`, `O`, `Esc`. **Enter and Space are inert** (Ctrl+E, Enter,
  Enter must never mass-replace or mass-duplicate; Space is the pick key),
  as are `Y`/`N`, and no button takes initial focus. A key that is not an
  answer is swallowed AND flips a visible "Pick one: B, O or Esc" line —
  a silently dead Enter reads as a frozen dialog.
- **The answers are BARE letters only, and the question swallows every
  accelerator** (gate finding 2026-08-21): `Ctrl+O` — Open Folder, a
  reflex — arrives in this scope as a plain `o` plus a modifier, and
  unguarded it ANSWERED the question with the destructive answer. While
  the question is up, `Ctrl+Q`, `Ctrl+E` and the rest are inert too; the
  menu bar remains the way out for the mouse. A destructive answer may
  never be reachable by a key the user presses for something else.
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
  identical RAW`. **"Replaced" means "something was under one of the two
  names and this job was allowed to overwrite it"**, decided from the
  filesystem before anything is written (gate finding: deriving it from
  "the destination RAW hashed" silently under-counted an unreadable
  destination file and a sidecar-only clash — both of which really do
  replace something). "All checksums verified" now attaches to copied AND
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
destination by another app (darktable) are lost").

**DECIDED 2026-08-22 — "overwrite means overwrite"** (the user, asked
directly): the sidecar is replaced like any other file, and the persona's
proposed merge (read-modify-write into the destination sidecar, preserving
foreign nodes) is NOT built. The warning in the question is the whole
mitigation, and the other two answers are the escape: Keep both, or a
fresh folder. This also keeps the "Execute" contract intact — a merged
sidecar would differ from the source and could not be checksum-verified
against it, so overwrite-means-overwrite is the only answer that leaves
every copied byte verifiable. Recovery if it happens anyway: delete that
copy at the destination and copy again, or re-import in darktable.

## Acceptance criteria (tests)

- [x] Plan: template expansion, in-plan collision RESOLUTION (two picks on
      one name are suffixed, never refused — including 2,000 of them whose
      suffix-walk probe count is counted rather than timed: exactly
      `4N - 2` probes, and the same closed form again at 2N, and with the
      free-space check requiring their bytes) —
      many_picks_on_one_name_take_consecutive_suffixes,
      free_space_counts_a_batch_suffixed_pick;
      dest-inside-source rejection (tempdir fixtures) —
      plan_templates_seq_and_suffixes_in_batch_collisions,
      two_picks_with_the_same_name_always_get_a_suffix_without_a_question,
      planned_paths_never_leave_the_destination,
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
      appears with its counts, Enter and Ctrl+O are both inert on it, B
      lands `_1` with ITS OWN sidecar beside it, O replaces the differing
      file and re-verifies the identical one, Esc returns to the plan and
      copies nothing at all — proven on a second destination folder that
      stays untouched — and a folder opened under the question drops it).
- [x] Gate round 2 (2026-08-21): the destructive answer is unreachable by
      an accelerator (`Ctrl+O` at the question — app test above); an
      overwrite never hangs on a FIFO under a planned name and never reads
      a non-regular destination —
      overwrite_does_not_hang_on_a_fifo_under_a_planned_name; a pick with
      no sidecar of its own leaves the foreign one and reports the replace
      — overwrite_without_a_sidecar_of_our_own_leaves_the_foreign_one; the
      hard-link-less commit fallback still refuses an occupied name —
      the_no_hard_link_fallback_still_refuses_an_occupied_name; the
      question names the number "keep both" will really use (asserted in
      ask_marks_the_clashes_and_counts_their_bytes_apart); cancellation is
      asserted on the between-files branch rather than racing past it —
      cancel_between_files_keeps_finished_copies.
- [x] Gate round 3 (QE, 2026-08-21): the report never contradicts itself —
      no "nothing needed copying" over failures, no green light without
      verified bytes, and a foreign sidecar left beside our RAW is named —
      pump.rs `report_lines` unit tests (the_headline_says_what_happened,
      the_verified_sentence_follows_what_was_verified,
      a_foreign_sidecar_left_in_place_is_reported); a destination that is a
      file is a plan error — a_destination_that_is_a_file_is_rejected_by_
      the_plan; a sidecar that fails after its RAW landed says which half
      landed — a_sidecar_that_fails_after_its_raw_landed_says_so; a
      228-byte destination name still ships the whole pair —
      a_very_long_destination_name_still_ships_the_whole_pair.
- [x] Gate round 4 (2026-08-21): a temp name is never reused and never
      written through, so an alias left behind by a failed unlink cannot be
      truncated by the next file —
      a_temp_name_is_never_reused_or_written_through; a dangling-symlink
      destination is rejected at plan time while a symlink TO a folder is
      accepted (asserted in
      a_destination_that_is_a_file_is_rejected_by_the_plan).
- [x] Gate round 5 (2026-08-22): a failed caption refresh still counts the
      RAW it verified and says so —
      a_failed_refresh_still_counts_the_raw_it_verified; an overwrite never
      replaces a name this run already landed, on a destination that
      collapses two names —
      an_overwrite_never_replaces_a_file_this_run_just_landed; the green
      light is core's rule — the_green_light_needs_verified_bytes (plus the
      app's report_lines tests) — and the same-run guard is asserted on
      FILE IDENTITY (a hard link drives the collapsing-lookup case on any
      filesystem) with two real case-twins proving no false alarm on a
      case-sensitive destination (that whole-run assertion PROBES the
      filesystem and skips where it folds case, so the windows-latest CI
      job — the one that produces the Windows binary — stays green), and
      the guard is driven through the REAL executor by two hard-linked
      destination names —
      the_executor_refuses_to_overwrite_a_file_this_run_just_landed, which
      is what a deleted `landed.insert` or a deleted guard arm turns red;
      a template that escapes the destination is a plan-time error —
      plan_rejects_a_template_that_escapes_the_destination. Review-verified
      only, no driven test (gate 2026-08-21, deliberate): the "Use last: …"
      template chip's confinement to the plan state — asserting the absence
      of a control by clicking where it would be is a test that passes when
      the click misses (main.slint).
- [ ] NOT VERIFIED ANYWHERE, carried forward (QE 2026-08-21, extended
      2026-08-22 — the same-run guard's folding-destination behaviour and
      the recording of a RAW whose sidecar failed are reachable ONLY on a
      case-folding destination, so both are asserted by their mechanism and
      by hard-link stand-ins rather than by the real lookup): a
      case-insensitive destination (no casefold/FAT mount available on the
      dev box), so "a case-variant counts as occupied" and rule 4's
      "two same-run names differing only in case cannot eat each other"
      are review-verified only; the hard-link-less commit fallback is unit-
      tested but has never run on a filesystem that actually lacks links;
      a Windows drive-prefixed template name (`C:x.ARW`), which
      `dest.join` would turn into a drive-relative path OUTSIDE the
      destination and which the plan now refuses through
      `Path::components` — on unix that same string is an ordinary file
      name, so the rejection cannot be exercised here and
      planned_paths_never_leave_the_destination cannot see it;
      network destinations (the recorded "~2× the clashing bytes over the
      wire" consequence); a real darktable round-trip of the
      overwrite-replaces-sidecars warning.
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      — DEFERRED with the user's explicit OK (2026-07-26, "low priority"),
      tracked as issue #10; spaces/Unicode half already QE-verified
      (`CON`, `NUL`, trailing dots) at plan time.
