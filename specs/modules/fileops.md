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
- Insufficient free space (the total §3 of "The clash question" requires
  for the policy in play vs `statvfs`/`GetDiskFreeSpaceEx`). **The
  dialog says it in units a person reads** (user decision 2026-09-12,
  brief 006 R2; the persona's IN-MY-WAY on the raw form): `The copy
  needs {needed} and there is {free} free at the destination.`, both
  sizes through the shared formatter ("Sizes on screen", dialog
  minimums) — *"The copy needs 7.3 GB and there is 1.1 GB free at the
  destination."* — the video dialog's sentence shape (video-export.md
  "Free space": *"This video would be 4.5 GB and there is 1.1 GB free at
  the destination."*), on the plan preview and on the drop-back after
  an answer alike. `{needed}` is the total §3 requires to fit for the
  policy in play — the clash-free bytes before the answer and under
  Overwrite and New only, every byte under Keep both — so it can read
  smaller than the summary line's worst case, and the drop-back after
  Keep both can refuse what the preview allowed. Until 2026-09-12 the
  dialog printed core's error text verbatim — `not enough free space:
  need 7834567890 bytes, 123456789 available` — and the user counted
  digits to learn whether the shortfall was 100 MB or 100 GB (persona,
  brief 006). Core's text is unchanged: `PlanError`'s `Display` is the
  developer-facing message, and this sentence is shared with no report,
  so it lives beside the video's in the bridge (brief 006 non-goal: no
  move to core). Every other `PlanError` keeps the text it has.

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
The "New only" answer added 2026-09-12 (§2 of "The clash question",
issue #86) is NOT v1's skip come back: `ExistsMode::Skip` was a forced
skip decided by session memory; New only is the user's explicit per-run
answer about names the plan found occupied on disk.

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
that check is a BLAKE3 comparison of both ends. A New only run that left
every pick (2026-09-12, §2 of "The clash question") is that all-skipped
run made live: it prints what it left and no green light, and never
"Nothing needed copying"; the picks it left are neither copied nor
identical, so the rule needs no change to keep the sentence off them.

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
  re-sending them — or, since 2026-09-12, "New only", which adds the
  picks whose names are free and opens nothing that is there (§2). What
  is GONE: the forced session-skip, the "N already at destination
  (skipped)" plan line, and the skip toggle — New only is none of them:
  the disk decides what clashes and the user answers, per run. What
  SURVIVES: the sidecar-alone refresh, now inside overwrite (the
  caption-after-copy recovery), and the ✓ copied badge as a glanceable,
  non-deciding hint.
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
  replaced by the answers of the clash question — three on 2026-08-21,
  four since 2026-09-12 (which does expose overwrite — see the recorded
  consequence at the end of that section).
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
  A `{seq}` template into a folder that already holds an earlier copy is
  the re-run trap the persona named on 2026-09-12 (brief 005, G3): the
  new picks renumber everything after them, so the names the clash check
  finds occupied may belong to other frames. The plan preview says so
  (§6 of "The clash question", the `{seq}` note — the user's decision on
  brief 005 OQ1: warn on the plan line and in the docs, no refusal).
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
- **Sizes on screen — one formatter, five binary tiers, one decimal**
  (user decision 2026-09-12, brief 006, persona-validated; issue #88).
  Every byte count either dialog prints goes through the ONE formatter
  the two bridges share (`human_bytes` — presentation, not a rule about
  files, so it stays in the app crate: brief 006 non-goal): this
  dialog's summary line (`148 picked · 7.3 GB to copy · 1.2 TB free`),
  the Keep both row's cost (`+590.3 MB`, §6), the free-space refusal in
  the plan-time error list above, and the video dialog's plan line,
  refusal and report line (video-export.md, "Dialog") — every count of
  bytes that is a SIZE. The one other byte count either dialog can print
  is the video's name-length refusal (video-export.md: a name over 255
  bytes is refused at plan time — core's *"the file name would be {len}
  bytes long, which no filesystem accepts: {name}"*, reaching the video
  dialog through its `other` arm), which is a length, not a size, and
  stays as it is: `0.3 KB` for a file name would be worse (QE
  2026-09-12, D4). Five tiers, binary, chosen by threshold: `{n} B`
  below 1,024 bytes (no decimal —
  `0 B`, `1023 B`), then `{:.1} KB` from 2^10, `{:.1} MB` from 2^20,
  `{:.1} GB` from 2^30 and `{:.1} TB` from 2^40, labelled KB/MB/GB/TB,
  one decimal on every tiered value (`1.0 KB`, `12.0 TB`; `1.0 TB` at
  exactly 2^40). The tier is picked first and the value rounded inside
  it, so a count just under a boundary rounds within its own tier —
  1,048,575 B is `1024.0 KB`, never `1.0 MB` — and above the TB tier the
  number runs on (`1024.0 TB`: no PB tier, no volume a photographer
  owns). Binary because that is what `df -h`, Explorer's drive tile and
  a NAS dashboard print — the persona's comparison points; the user,
  brief 006 OQ1: "I don't compare them" — so a brand-new 12 TB volume
  reads `10.9 TB free` here as it does there; GNOME Files and Finder are
  decimal and read ~7 % higher on every GB line, recorded and not a
  bug. Until 2026-09-12 there were three tiers: bytes ran to
  `1048575 B` (the Keep both row read `+1029480 B` for a ~1 MB clash,
  issue #88) and GB ran on past a terabyte — `1228.8 GB free` for a
  1.2 TB NAS, `12288.0 GB free` for a 12 TB volume — four digits and a
  division on the one line meant for a glance (persona 2026-09-12: TB
  tier USEFUL, weekly on the NAS and the 2 TB SSD; KB tier SHRUG — it
  can never reach the cost column, because a clash always costs at
  least its RAW, and shows only through a "free" figure on a
  nearly-full card or a refusal). ONE rule on every line (Manager D2):
  the illustrative sizes in this spec, in video-export.md and in `docs/`
  are the screen's form, one decimal — `328.4 MB` and `358.2 GB free`,
  never `328 MB` and `358 GB free` (the video spec's plan line drifted
  from the screen and is corrected the same day). Not built (D4, the
  persona's own list): a units preference, GiB/TiB labels, a
  decimal/binary switch, per-file sizes, a free-space bar, an ETA.
  Pinned by copy_bridge::a_byte_count_reads_in_its_tier_with_one_decimal
  (the acceptance list, brief 006 AC1); its 2^50 row is what pins the
  run-on rule — a PB arm at 2^50 passed every test in the workspace
  until it was added (QE 2026-09-12, D1) — and its `u64::MAX` row that
  the largest count still prints without a panic (senior-developer
  integrity review 2026-09-12: an integer rewrite of the TB arm
  overflows there and nowhere else).
- **The card's height follows its content and its text region scrolls**,
  the rule video-export.md records for issue #62: a 480 px floor (the
  height it always had, so nothing moves in the ordinary case), the window
  as the ceiling, and the growing text in a `ScrollView` body that takes
  whatever height is left. This card has an unbounded text of its own —
  the report prints one `FAILED name: reason` line per file that failed,
  so a destination that goes read-only mid-run words itself as long as the
  run was. With a fixed height those lines pushed the button row out of
  the card, where Slint still draws and still hit-tests it: measured on
  the parent tree at 1440×900, 61 picks into a read-only folder put the
  row 830 px below the card and 627 px below the window's bottom edge —
  drawn over the desktop, not over the dialog. Nothing is truncated and
  nothing is unreachable — past the ceiling the body scrolls. Its
  rectangles are traced (`copy card laid out …`, `copy buttons laid out
  …`), and asserted by
  `app: a_failure_report_longer_than_the_window_keeps_the_copy_buttons_inside_the_card`
  (Unix: the read-only destination is a `chmod`). **A `#[cfg(unix)]` test
  takes its private helpers with it**: `body_scroll_at` is called only
  from here, so on Windows it is dead code and `cargo clippy
  --all-targets -- -D warnings` — which CI runs on windows-latest —
  refuses to build the test target at all (red on the v0.13.0 commit).
  The helper carries the same gate as its callers; helpers shared with a
  platform-neutral test (`laid_out_at`, `assert_buttons_inside_card`) must
  NOT be gated. A long report scrolls
  with the wheel AND with the keyboard — Down/Up, PgDn/PgUp, Home/End,
  only while it overflows — so the failures past the fold are readable
  without a mouse; the same test drives PgDn and Home and asserts the
  body moved and came back. The bound on that
  promise is video-export.md's: this card's fixed rows want ~190 px, so
  below ~300 px of window height there is no room for them and the row
  goes outside again (measured at 640×200: card 560×94, row 90 px below
  it). Its clash answer rows now sit inside the scrolling body, so one
  round of `copy_picks_asks_once_and_each_answer_does_what_it_says` is
  answered with the mouse — every other answer in the suite is a key.

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
with four answers (three until 2026-09-12; the fourth, New only, is the
user's decision on issue #86, brief 005, and the first row — §6):

- **New only** (labelled *"New only — copy the 4, leave the 144 already
  here untouched"* in the dialog; key `N`; the user, 2026-09-12) — every
  clash-free pick copies exactly as under the other answers (temp name,
  BLAKE3-verified, no-clobber commit; a name claimed by another pick in
  this same run is suffixed and copied, as under every answer — "two
  picks, one name" below), and a clashing pick — RAW name or sidecar name
  occupied, the pair-is-the-unit rule of §1 — is left exactly as the disk
  has it: its RAW and its sidecar are not written, not read and not
  hashed; neither member is opened. A sidecar-only clash (a stray `.xmp`
  beside no RAW) is left like any other clash, because landing a RAW
  beside a sidecar that describes another photograph is the one thing
  this module never produces — and it is counted APART in the report,
  because under this answer it is a photograph that did not land
  (persona G2 2026-09-12: "without that line I believe the photo is
  archived and it is not"). The run's work is the clash-free picks only:
  the executed plan carries no work for a left pick — nothing opens
  either member, no progress event names it, its bytes are not in the
  plan's total and only the clash-free bytes must fit (§3) — and the
  report says what was left, in NAMES not photographs, with the green
  light on the copied line only (§6). The case it exists for (the user,
  issue #86): four more picks into an archive of 144 frames already
  developed in darktable — Overwrite replaces the history stacks, Keep
  both duplicates the 144 as `_1` twins, a fresh folder splits the
  archive; New only adds the four and opens nothing else. Not a
  verification pass in disguise (persona C6: IN-MY-WAY if built — 7 GB
  over the wire to add four files, and every darktable sidecar "fails"
  by design; Manager decision D5): the left picks are not read, so "left
  untouched" means not opened, and Overwrite remains the "is my export
  still bit-perfect?" pass. Why this is not v1's skip: `ExistsMode::Skip`
  was a FORCED skip decided by session memory, which is what caused issue
  #14; New only is the user's explicit answer, once per run, about names
  the plan found occupied on disk tonight — `SessionCopies` still reads
  and never decides (§5), so a hand-emptied folder holds no clash and New
  only copies everything, RAW and sidecar together.
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

*Which total has to FIT* (implementation decision 2026-08-21; New only
added 2026-09-12): before the answer, under **overwrite** and under **New
only**, only the CLASH-FREE total — under overwrite the clashing images
mostly replace bytes that are already there, one verified temp file at a
time, and a destination that really is full then fails those files one
by one with an honest reason without ever destroying the file that was
there; under New only the clashing images are not written at all, so the
clash-free total is also the whole total the plan states. Under **create
copies** the whole total must fit, because every clashing image is a new
file. Blocking an overwrite re-run on a nearly-full destination that it
would barely grow is the failure this avoids; the cost is that a
genuinely full disk is discovered per file rather than up front. A
nearly-full archive plus four new frames must not be refused (persona
2026-09-12, MUST-HAVE), which the clash-free rule gives New only for
free — and the replan after that answer cannot surface a free-space
error the preview did not already show.

**4. Nothing is replaced unless the user answered Overwrite.** Under New
only (2026-09-12) nothing under a clashing name is even OPENED — not the
RAW, not the sidecar, not for a read — so a destination file made
unreadable under such a name cannot fail a New only run. The
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
existing" toggle and the four-way `ExistsMode` are replaced by the
answers of the question — three on 2026-08-21, and New only, the fourth,
on 2026-09-12, which is an explicit per-run answer about names found on
disk, not the session-decided skip that is gone. A pick New only left is
not in `CopyReport::landed` (this run verified nothing about it), so no
record is made for it; a record the session already holds for it
survives, because `SessionCopies::refresh` finds the file still there —
the badge follows the disk (Manager decision D8 2026-09-12; persona:
SHRUG, honest either way). The "copied earlier but gone" note is computed
BEFORE the answer and under every policy, so it is not a promise about
what a given answer will do (added 2026-09-12, brief 005, senior-developer
plan OQ3): a hand-deleted RAW whose sidecar was left behind reads "1
copied earlier but gone — copying again" on the preview, and under New
only that pick is a sidecar-only clash and is LEFT — the report's stray
`.xmp` line, not the note, is then what happened.

**6. Wording and keys** (settled with the persona 2026-08-21; the New
only row, its key and the lines that change with it settled with the
persona 2026-09-12 and decided by the Manager, brief 005 D1-D7; the
question is a STATE of the Copy dialog — `copy-state 3` — not a second
modal, so there is one key scope and no new stacking surface, issue #42).
The question states what each answer does and what it costs, rather than
yes/no (persona: at 9pm "proceed" reads as "proceed with the copy I asked
for"). As specified (the three-row form shipped 2026-08-21; the `N` row
and the warning line's last sentence are brief 005's, 2026-09-12):

```
12 of your 148 picks already have files with these names in
…/2026-08-21-osprey/selects
The other 136 copy normally. Choose once for the whole run:
e.g. DSC01234.ARW, DSC01235.ARW, DSC01240.ARW …

 N    New only — copy the 136, leave the 12 already here untouched
 B    Keep both — the 12 land as DSC01234_1.ARW        +590.3 MB
 O    Overwrite those 12 — identical files are re-checked, not re-sent
 Esc  Cancel — copy nothing at all, not even the 136

Overwriting also replaces those files' .xmp sidecars — edits made at the
destination by another app (darktable) are lost. New only leaves them alone.
```

- The "keep both" row names the file that answer would REALLY make — the
  plan walks the suffix from `_1` and hands the dialog the first free
  pair, so a second keep-both into the same folder says `_2` rather than
  promising a `_1` the copy would not use (gate finding). Rare corner,
  recorded: because a suffix can also be claimed IN-PLAN, the number of
  renames can exceed the clash count when a pick is literally named
  `<other>_1.<ext>`.
- **The New only row** (2026-09-12) reads `New only — copy the {free},
  leave the {clashes} already here untouched`, singular forms following
  the question's habit (`copy the 1`, `leave the 1`). Its first word is
  "New", so `N` reads as New, not No (persona C3; Manager D2). "Already
  here" is allowed on the ROW because the header just qualified it
  ("already have files with these names") and the `e.g.` line is how a
  two-body night is told apart; the REPORT may not say it (C5, below).
  No cost column: New only adds nothing beyond the clash-free copy every
  answer but Cancel makes, so "bytes on keep both only" stands. Its label
  is the ordinary colour. **It is always offered** (Manager decision D6
  2026-09-12, best practice over the persona's SHRUG-leaning-USEFUL to
  hide it): with no clash-free pick the row reads `New only — nothing new
  to copy, leave the {clashes} already here untouched`, and answering it
  runs a copy of nothing whose report is the left line below — a row that
  appears and disappears moves `B` and `O` under the pointer on exactly
  the destructive question, and a stable layout outweighs one row of
  noise.
- Counts are in **picks**, never files (148 picks are 296 files on disk;
  a count the user cannot reconcile is a count they stop trusting), and
  the "other N copy normally" clause is mandatory: once Cancel drops
  everything, the user no longer assumes the other answers behave
  normally.
- **"Overwrite those 12", not "overwrite everything"** — the count in the
  label is what stops the word overstating what happens. Bytes appear on
  "keep both" ONLY: it is the one answer whose cost is knowable up front,
  and a worst-case number on overwrite would state a cost the identity
  check means the user never pays. The cost goes through the shared
  formatter (`+590.3 MB` — "Sizes on screen" in the dialog minimums,
  2026-09-12), and because a clash always costs at least its RAW no KB
  figure can reach this row with real files (persona, brief 006).
- The answers do not fit side by side in the 560px card (three did not,
  measured 2026-08-21; four do not either), so they are stacked rows in
  order of increasing consequence, Cancel set apart: `N`, `B`, `O`, then
  `Esc` (persona C2, Manager D3 2026-09-12 — the safe, common answer is
  the first row read, and a habitual top-row click lands on the least
  consequential answer instead of on Overwrite; "safest next to Cancel"
  was rejected because Cancel does nothing and New only does something,
  and grouping them reads as two flavours of giving up). **No answer
  carries accent/default styling** — a destructive answer that looks
  pre-chosen gets pressed reflexively. The overwrite row's LABEL is
  amber; the New only row's label is the ordinary colour; no row carries
  an accent box.
- Keys: `N`, `B`, `O`, `Esc`. **Enter and Space are inert** (Ctrl+E,
  Enter, Enter must never mass-replace or mass-duplicate; Space is the
  pick key), as is `Y`, and no button takes initial focus. A key that is
  not an answer is swallowed AND flips a visible "Pick one: N, B, O or
  Esc." line — a silently dead Enter reads as a frozen dialog. **`N` was
  inert until 2026-09-12** (this bullet read "as are `Y`/`N`" — the
  2026-08-21 worry was a "No" reading, and `N` is the grid's reject key).
  The user chose `N` for the new answer (issue #86; brief 005 D1), and
  the persona's two reasons hold at the screen (C1): the question is a
  stop, not a rhythm — it appears only after Ctrl+E then Enter, so the
  N-N-N cadence of rejecting never reaches it and a held key cannot
  arrive there — and New only is the least consequential answer that
  still does something: a stray `N` copies what was asked minus the
  clashes and touches nothing that exists. The row's first word kills the
  "No" reading. `Y` stays inert (persona: SHRUG, correct). The video
  export's question is UNTOUCHED (Manager D9): one `.mov` file, for which
  "skip" is Cancel, so `N` there stays a swallowed key with a nudge.
- **The answers are BARE letters only, and the question swallows every
  accelerator** (gate finding 2026-08-21): `Ctrl+O` — Open Folder, a
  reflex — arrives in this scope as a plain `o` plus a modifier, and
  unguarded it ANSWERED the question with the destructive answer. While
  the question is up, `Ctrl+Q`, `Ctrl+E` and the rest are inert too; the
  menu bar remains the way out for the mouse. A destructive answer may
  never be reachable by a key the user presses for something else. `N`
  answers under the same guard — a bare `n`/`N` only; `Ctrl+N`, if ever
  bound, stays out (2026-09-12).
- **Esc returns to the plan preview** with destination and template
  intact (a second Esc closes the dialog): the topmost-first Esc rule, and
  it makes "cancel, then copy somewhere else" one step.
- The plan preview pre-announces the split — `3 new · 148 already exist
  here — Copy will ask what to do` — which cross-session (no ✓ badges
  after a restart) is the only signal that the folder already holds this
  shoot. **The `{seq}` note** (the user, 2026-09-12, on brief 005 OQ1:
  "Warn on the plan line + docs"): when the template in play contains
  `{seq}` (any form, `{seq:3}` included) AND the plan has at least one
  clash, the preview carries, beside that split, `{seq} numbers the whole
  session — the names already here may now belong to other frames`; with
  no `{seq}` in the template, or no clash, it does not appear. The FACT
  is core's (a `CopyPlan` field, `seq_meets_clashes`); the sentence is
  the bridge's. No refusal: every answer stays available. The trap
  (persona G3): the new picks renumber everything after them, so
  "already here" is judged on shifted names and New only would copy the
  wrong frames under a clean report; Overwrite would re-send and replace
  all of them under shifted names, which is worse — so the note sits on
  the preview, where every answer is still ahead.
- The destination is shown TAIL-first (`…/2026-08-21-osprey/selects`)
  wherever it is elided: Slint's elide cuts the end, i.e. exactly the part
  that tells two shoots apart.
- The progress line says WHICH work it is doing (`Checking 12 / 148` for
  an overwrite, which starts by hashing, vs `Copying 2 / 3`) — counting to
  148 while saying "copying" reads as "it is sending my whole export
  again". Under New only the total is the number of picks this run
  copies and nothing else: `Copying 1 / 4`, never `Copying 1 / 148`, and
  no "Skipping" line either — a skip takes no time and deserves no line
  (persona G1 2026-09-12, MUST-HAVE within the feature). The
  `CopyEvent::File` total is that count, and no event is emitted for a
  left pick.
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
- **The New only report** (persona C5, Manager D4 2026-09-12) prints,
  after the lines that count what the run did (`4 copied, all checksums
  verified`; a `landed under new names` line for same-run shared names)
  and before `cancelled`/`FAILED`: `144 already had files with these
  names here — left untouched, not re-checked` (singular: `1 already had
  a file with this name here — left untouched, not re-checked`), then,
  only when it happens, `1 of those is a stray .xmp with no RAW beside it
  — that pick was not copied` (plural: `N of those are stray .xmp files
  with no RAW beside them — those picks were not copied`). The wording
  says the NAMES were taken, never that the photographs are there — on a
  two-body night the 12 clashes are the other camera's frames, and a
  report that says they are "here" is the kind of sentence the user stops
  trusting — and "not re-checked" is said out loud so the green light on
  the copied line can never be read across. "All checksums verified"
  stays on the copied line; `earned_the_green_light` is unchanged (copied
  + identical > 0, all verified, no failure, not cancelled): a run that
  left everything and copied nothing prints the left line, no green
  light, and never "Nothing needed copying". The left counts are decided
  at plan time, from the filesystem, before anything is written — like
  "replaced" — and the report carries them whether or not the run
  finished (a cancel between files changes what was copied, not what was
  left): `CopyReport::left_untouched` and `CopyReport::left_sidecar_only`,
  core's, like every count on the report.

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
there are files already, it should ask."* The answer that added the new
picks was, until 2026-09-12, Overwrite, which re-verifies the existing
ones rather than skipping them; identical RAWs are not re-transferred
(see above), so the cost is a read, not a write. On a network destination
that read is real: ~2× the clashing bytes over the wire to add three
frames (persona; open question 5 to the user). **Corrected 2026-09-12
(brief 005)**: New only now adds the new picks without that read —
nothing under a clashing name is opened — and Overwrite remains the
answer that also re-verifies; the "~2×" is the price of the re-verify,
no longer of adding picks.

**Recorded consequence, and the persona's blocker** (2026-08-21, relayed
to the user, NOT decided here; RESOLVED 2026-09-12, below): a
destination sidecar that DIFFERS is byte-replaced under Overwrite — and
darktable's history stack lives in a file of exactly the name FastCull
uses (`DSC01234.ARW.xmp`, xmp-sidecars.md invariant 2). A user who has
started developing the copies in darktable and then answers Overwrite
loses those history stacks; with "skip" gone there was, until
2026-09-12, no other answer that adds new picks to that folder — New
only is that answer now (corrected in place 2026-09-12, brief 005). The
question therefore says so out loud, and since 2026-09-12 names the way
out in the same breath (persona C4, Manager D7: "(darktable)" stays — it
is the word that makes the user stop): "Overwriting also replaces those
files' .xmp sidecars — edits made at the destination by another app
(darktable) are lost. New only leaves them alone."

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

**RESOLVED 2026-09-12 — "New only" (the user, issue #86; brief 005)**:
the ruling above stands unchanged — overwrite means overwrite, no merge,
and the warning line is Overwrite's whole mitigation — but the escapes
are three now, and the first of them adds picks: New only (§2), which
copies the clash-free picks and opens nothing under a clashing name, so
a folder developed in darktable and a copy re-run can coexist. Keep both
and a fresh folder remain for the case New only does not serve (a
two-body clash, where the "taken" names are the other camera's
photographs and New only would leave the user's frames out — the report
says how many). What the user met that the 2026-08-22 escapes did not
cover: four more picks into an archive of 144 edited frames — Keep both
would have duplicated the 144 as `_1` twins, a fresh folder would have
split the archive.

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
- [x] **Brief 005 AC1 — New only opens nothing under a clashing name**
      (2026-09-12): both members of a clashing pair are byte-for-byte and
      mtime-identical after the run, a destination sidecar that DIFFERS
      from the source included (the darktable case), and a destination
      RAW or sidecar made unreadable under a clashing name (unix:
      `chmod 000`) does not fail the run —
      new_only_leaves_a_clashing_pair_untouched_and_copies_the_rest,
      new_only_never_opens_a_clashing_pair (`#[cfg(unix)]`, which takes
      its private helpers with it); app-level, the differing destination
      sidecar byte-for-byte after `N` —
      copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone.
      Mutants: the New only arm of `plan()` mapped to Replace turns the
      sidecar-bytes assertion red; a read of the left pair's destination
      (a hash, a verification pass) turns the `chmod 000` run red.
- [x] **Brief 005 AC2 — every clash-free pick copies and verifies exactly
      as under the other answers; a same-run shared name is suffixed and
      copied** — asserted in
      new_only_leaves_a_clashing_pair_untouched_and_copies_the_rest; the
      issue #14 shape under the new answer — a hand-emptied folder holds
      no clash, so the gone copies are new and copy again, RAW and
      sidecar together, with the "copied earlier but gone" note —
      a_hand_emptied_folder_holds_no_clash_so_new_only_copies_it_again.
- [x] **Brief 005 AC3 — a sidecar-only clash is left and reported apart**
      — a_sidecar_only_clash_is_left_and_counted_apart_under_new_only.
- [x] **Brief 005 AC4 — the progress total equals the number of picks
      copied; no event for a left pick** —
      new_only_emits_one_event_per_copied_pick_and_none_for_a_left_one
      (the `CopyEvent::File` stream is counted and its names checked);
      app-level, the rendered line itself — `Copying 2 / 2 — c.ARW`
      through `copyprogress=` after `wait:copy finished run 1`, and
      `Starting…` (no Copying, no Skipping) after the all-left run, in
      copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone
      (QE 2026-09-12, D3).
- [x] **Brief 005 AC5 — the report prints the left line (and the
      stray-sidecar line when it applies), the green light attaches to
      copied files only, and an all-left run prints the left line, no
      green light and never "Nothing needed copying"** — pump.rs
      `report_lines` unit test
      a_new_only_run_reports_what_it_left_and_earns_no_green_light_for_it
      (singular and plural of both lines, and their order after the
      copied line); app-level, the left line in the report after `N` —
      copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone;
      the counts carried through a cancel at the executor —
      a_cancelled_new_only_run_still_reports_what_it_left (QE 2026-09-12,
      D2).
- [x] **Brief 005 AC6 — free space: only the clash-free bytes must fit
      under New only** — the_free_space_check_follows_the_answer,
      extended with the fourth policy (an existing test that changes by
      the plan, not a loosening: it gains an arm).
- [x] **Brief 005 AC7 — the dialog**: the N row first with its counts,
      `N` answering as a bare letter only, `Y`/Enter/Space/accelerators
      inert, the nudge `Pick one: N, B, O or Esc.`, the warning line's
      "New only leaves them alone." clause, and the row still offered —
      with its "nothing new to copy" wording — when every pick clashes,
      answering it then reporting the left line and no green light:
      driven through the real dialog like
      copy_picks_asks_once_and_each_answer_does_what_it_says —
      copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone,
      reading `copystate`, `confirm` and `report`, the N row's label and
      the nudge through dump fields the plan names (ui-grid.md's "Debug
      facilities" paragraph follows in the same commit), and the row
      ORDER from the rows' own layout marks (relative y, never a pixel —
      a font metric moves a layout by up to 40 px per seat, 2026-09-04).
      The label colour is review-verified only (no dump carries a
      colour).
- [x] **Brief 005 AC8 — the `{seq}` note appears on the preview when a
      `{seq}` template meets a clash, and only then** — core:
      plan_flags_a_seq_template_that_meets_a_clash (`{seq}` with a clash
      → true; `{seq}` with none → false; a clash with no `{seq}` →
      false); app-level, the note's text in `copynote` on a `{seq}`
      template over a folder that holds the templated name, and its
      absence otherwise — a round of
      copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone.
- [x] **Brief 005 AC9 — docs/copy-picks.md and docs/faq.md say what the
      dialog does, in the same commit** — review-verified at the gate (no
      driven test reads the docs); the pages are part of this spec change
      and ship with the implementation commit.
- [x] **Brief 006 AC1 — the formatter prints `1.0 KB`, `1.0 MB`, `1.0 GB`,
      `1.0 TB` and `12.0 TB` at the boundaries and `1023 B` below the
      first, one decimal on every tiered value and none on bytes, and
      rounds inside the tier the threshold picked (`1024.0 KB` at
      2^20 - 1, `1024.0 GB` at 2^40 - 1)** (2026-09-12, issue #88) — app
      (unit): copy_bridge::a_byte_count_reads_in_its_tier_with_one_decimal,
      the exact string at 0, 1023, 2^10, 2^20 - 1, 2^20, 2^30, 2^40 - 1,
      2^40 and 12 × 2^40, plus the two figures the issue was opened on:
      1,029,480 B reads `1005.4 KB` (was `+1029480 B` on the Keep both
      row) and a 1.2 TB NAS's 1,319,413,953,331 B reads `1.2 TB` (was
      `1228.8 GB`). Mutants, one per tier: each threshold moved by one
      byte, each divisor swapped for its neighbour's, `{:.0}` or `{:.2}`
      on any tier, and the KB or the TB arm deleted — every one must turn
      the test red. Added at QE round 1 (2026-09-12, D1/P1): the rows
      `2^50 → 1024.0 TB` and `u64::MAX → 16777216.0 TB`, red under a PB
      arm at 2^50 (`1.0 PB`) and under an integer rewrite of the TB arm
      (`attempt to multiply with overflow`, seen by the `u64::MAX` row
      alone). Debug on both runners (`cargo test --workspace --locked`,
      ci.yml's Tests step); release on the development seat only — CI's
      release steps run the screenshot target and the perf budgets, not
      the unit tests (corrected 2026-09-12, QE S3: this entry first said
      "debug and release" of both runners). The existing readers of a
      size string stay green with no change:
      pump::the_verified_line_of_a_video_export_is_earned (`344.0 MB`),
      clip_bridge::the_two_untestable_messages_now_have_a_test (`4.5 GB`,
      `1.1 GB`), and the driven guard in
      copy_picks_rerun_recopies_hand_deleted_files, which asserts the
      re-run is NOT an empty plan (`!summary.contains("0 B to copy")` —
      a negative that would also hold if zero printed `0.0 KB`) and
      stays green because 0 still prints `0 B`; the row `(0, "0 B")` of
      the unit test is what pins that (corrected 2026-09-12, QE D2: this
      entry first credited the guard with pinning the form). RED before
      the change on this seat 2026-09-12: the 2^10 row read
      `1024 B`, the 2^40 row `1024.0 GB`, 1,029,480 B `1029480 B` and the
      NAS figure `1228.8 GB` (developer 2026-09-12, the old three-tier
      body run over every row of the test); all ten mutants red, each at
      the row named above.
- [x] **Brief 006 AC2 — a copy plan refused for space shows `The copy
      needs {needed} and there is {free} free at the destination.` with
      both sizes through the formatter, on the plan preview and on the
      drop-back after an answer; every other `PlanError` keeps its text;
      the video dialog's refusal is unchanged** (2026-09-12) — app
      (unit): copy_bridge::the_copy_refusal_reads_in_units_a_person_reads
      — the exact sentence for `needed: 7_834_567_890, free:
      1_234_567_890` (*"The copy needs 7.3 GB and there is 1.1 GB free at
      the destination."*) and `DestNotADirectory` still reading "the
      destination is not a folder", asserted on the bridge's
      error-to-text function, which both replan paths (the preview and
      the drop-back) call; mutants: the space arm mapped back to
      `e.to_string()` turns the first assertion red, the `other` arm
      given a sentence of its own turns the second red. The video's
      sentence stays pinned by
      clip_bridge::the_two_untestable_messages_now_have_a_test. The
      WIRING on the real dialog — both replan paths reaching that
      function — is driven (Manager ruling 2026-09-12, brief 006 D5: the
      defect this half fixes IS a wiring defect, and only a driven round
      goes red if the arm is never rewired): a TIFF-fronted `.ARW` — the
      video suite's `write_synthetic_raw` bytes extended by
      `File::set_len` to 8 TiB, above any runner's or seat's free space
      and below ext4's 16 TiB per-file ceiling; the front stays a real
      TIFF because a file the in-tree walker rejects goes to rawler,
      whose `RawSource::new` maps it with `MAP_POPULATE` and pre-faults
      every page (an 8 TiB zero-filled file stalled the load past the
      30 s wait cap at 22.6 GB RSS, senior-developer plan 2026-09-12);
      the tail is a hole — as the CLASHING pick beside a tiny clash-free
      one, so
      the preview passes, Enter asks, `B` replans under Keep both and the
      drop-back refuses with `copystate=0` and the sentence in a new
      `copyerror=` QEDUMP field (ui-grid.md "Debug facilities" moves in
      the same commit) — the_copy_refusal_reaches_the_dialog_on_the_
      drop_back_after_keep_both, `#[cfg(unix)]` with its reason written
      in the test: NTFS allocates real clusters on `set_len` without the
      sparse attribute, so the fixture cannot exist on the Windows
      runner's disk; the unit test above is what pins the sentence there.
      RED on commit 1 with the field and the test but the arm unchanged,
      2026-09-12: `copyerror="not enough free space: need 8796093026378
      bytes, 10101223424 available"` (developer 2026-09-12); the preview
      line `2 picked · 8.0 TB to copy` read `8192.0 GB to copy` before
      commit 1, which the third driven mutant re-measured. The drop-back
      is that round's; the PREVIEW's own refusal — the one the
      twin's preview cannot show, because its preview is built to pass
      and asserts `copyerror=""` — is driven by
      the_copy_refusal_reaches_the_dialog_on_the_plan_preview (QE
      2026-09-12, D3/P2; senior-developer integrity review 2026-09-12):
      two clash-free picks, the 4,170 B one alone first (its plan fits
      and prints the free figure the refusal is held against; `4.1 KB to
      copy` is the KB tier on screen), Escape, the 8 TiB one joins it
      and the preview refuses with `copystate=0` and the sentence,
      nothing written. RED on commit 1 with the field and the test but
      the arm unchanged: `copyerror="not enough free space: need
      8796093026378 bytes, 10142629888 available"` (developer
      2026-09-12); and — alone among the two rounds — RED when the
      preview path words the refusal differently from the drop-back (a
      policy-conditional arm), which the drop-back round cannot see.
- [x] **Brief 006 AC3 — the illustrative sizes in this spec, in
      video-export.md and in `docs/` are the screen's form, and
      docs/copy-picks.md says what the dialog says when the destination
      lacks the room** (2026-09-12) — review-verified at the gate, like
      brief 005 AC9 (no driven test reads the specs or the docs): the
      screen's form itself is what the two existing unit tests above pin
      (`344.0 MB`, `4.5 GB`), and the docs sentence ships with the
      implementation commit.
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
      overwrite-replaces-sidecars warning, and (2026-09-12) of New only
      leaving a real darktable history stack intact — the core test's
      stand-in is a destination sidecar whose bytes differ from the
      source, which is all the executor could ever see of one.
- [ ] Cross-platform: paths with spaces/Unicode; Windows reserved-name rejection
      — DEFERRED with the user's explicit OK (2026-07-26, "low priority"),
      tracked as issue #10; spaces/Unicode half already QE-verified
      (`CON`, `NUL`, trailing dots) at plan time.
