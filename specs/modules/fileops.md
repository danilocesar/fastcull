# Module spec: copy picks (`fileops.rs`)

## Purpose

Copy Picks is the exit of a cull: every picked RAW and its sidecar go to a
folder the user chooses, optionally renamed by template, verified byte for
byte. Originals are never touched — copy, not move. The engine and every
count and sentence rule live in `fileops.rs`; the dialog is `copy_bridge.rs`
and `main.slint`.

## Behaviour

### Scope

- Copy Picks takes every picked image in the session, whatever the filter
  shows. The inbox-zero loop ends with an empty view, and "the view's picks"
  would copy nothing at exactly that moment. The dialog's count line states
  the scope. Copying a subset is v2 (the multi-selection exists but is not
  wired here).
- Rejected and unmarked files stay where they are. There is no
  move-or-delete-rejects operation (user decision).
- `Ctrl+E` with zero picks opens the dialog saying "No picked images", never
  a silent no-op.
- `Ctrl+E` first commits any half-typed IPTC field, like clicking away, so a
  half-typed caption ships.

### Plan, then execute

**Plan** is pure and unit-testable: from (picked images, destination,
optional rename template) it produces a `CopyPlan` — ordered pairs of source
RAW → destination RAW and source sidecar → destination sidecar — plus the
problems it found. Rename templates use the IPTC variable engine (`{date}`,
`{time}`, `{seq}`, `{seq:N}`, `{filename}`, `{camera}`, `{ext}`;
iptc-templates.md). `{seq}` follows the session sort order (capture time by
default) through `filter::view_true_sort`, never the provisional filename
order a loading folder shows: it is baked into permanent file names. A copy
started mid-load therefore numbers files in an order matching neither the
screen nor the same copy a few seconds later (recorded; ui-grid.md,
*Provisional order while loading*).

Plan-time errors block the copy and are shown in the dialog:

- The destination exists but is not a folder: a file, or a dangling symlink.
  A symlink to a folder is fine.
- The destination is inside the source folder, or is it.
- A template that expands to a path rather than a file name — a `/`, a `\`,
  `..`, `.` or the empty string — refused on every platform, so one template
  means the same thing on Linux and Windows. Every byte this module writes,
  suffixed names and hidden temp files included, lands directly in the
  chosen folder (the user's invariant, 2026-08-22: "we should never write
  files outside of the target").
- A template that expands to a name with no stem (anything starting with
  `.`): it would write `.ARW`, a hidden file that FastCull's own scan skips
  and darktable never sees, and the suffix walk would yield `.ARW_1`.
  Templated names only — the user's own file names are never refused (a
  macOS `._DSC0001.ARW` stub is a pickable cell and must not block the run).
- Not enough free space for the policy in play (see *The clash question*,
  §3). The dialog says it in units a person reads — *"The copy needs 7.3 GB
  and there is 1.1 GB free at the destination."*, both sizes through the
  shared formatter (*Sizes on screen*) — on the plan preview and on the
  drop-back after an answer alike. Core's `PlanError` text stays
  developer-facing; this sentence lives in the bridge, beside the video's.
  An unreadable free-space figure reports "free space unknown" and skips the
  check rather than inventing a number.

**Execute** runs on a worker thread with a progress event per file:

1. Flush the pending sidecar writes of every picked image — and this flush
   precedes PLANNING, not just execution: `plan()` freezes sidecar-existence
   and refresh answers, so a plan built before the flush would ship RAWs
   without their fresh sidecars while reporting verified. The app flushes at
   dialog open (a truthful preview) and flushes-then-replans inside Copy
   itself; a plan frozen at open is never executed. Free space is re-checked
   by that final replan.
2. Per image: copy the RAW → fsync → copy the sidecar → fsync. The sidecar is
   renamed in lockstep (`newname.ARW` ⇒ `newname.ARW.xmp`).
3. Verify: the BLAKE3 of the destination equals the checksum computed while
   the source streamed, for the RAW and the sidecar. Checksums are v1, not
   v2, because the user sometimes culls straight off the card and this copy
   is the working copy before the card is formatted; a truncated ARW can
   still show a perfect thumbnail (the embedded JPEG sits at the front), so
   "looks fine in the grid" proves nothing.
4. On a per-file failure: record it and continue. No partial file is ever
   left under a final name. Each copy goes to `<dest>/.fastcull-partial-
   <pid>-<n>` — short, so a long templated name cannot be pushed past
   `NAME_MAX`; unique per file; created exclusively (`create_new`) — and is
   committed by a no-clobber link (`hard_link` + unlink of the temp; `rename`
   only for an answered Overwrite). The unlink may fail (a Windows sharing
   violation from a scanner is the ordinary cause) and the name left behind
   is then a second name for the file just committed, which is why a shared
   or predictable temp name is forbidden: the next file would truncate a
   copy already reported verified. A leftover number is stepped over, never
   opened. The counter is atomic, so a second worker can join this path
   without changing the rule.
5. The report: copied, identical, replaced, landed under new names, left,
   failed with reasons. The session marks copied images with the ✓ badge.

Cancel is honoured between files; finished copies stay, and the report says
so. Dropping the copy (quit, or Open Folder mid-copy) cancels and joins,
bounded by the file in flight; an Overwrite's identity pass reads as much as
a copy writes, so it polls the cancel flag between blocks. Open Folder
mid-copy ends the dialog honestly — it says the run was cancelled rather than
sitting at "running" with a dead Cancel button. A hard quit joins nothing:
shutdown ends in `process::exit` (01-architecture.md), so the file in flight
leaves its temp behind — one `<dest>/.fastcull-partial-<pid>-<n>` per hard
quit, never reused, never swept (deleting at the destination is not this
module's business, and the leading dot hides nothing on Windows). It can
never be mistaken for a photograph.

**"All checksums verified"** is printed only when the run copied or found
identical, and verified, at least one file, nothing failed and nothing was
cancelled — `CopyReport::earned_the_green_light()`, in core, because it is a
fact about the copy and not about the dialog. An all-skipped or all-left run
prints what it did and no green light, and never "Nothing needed copying".

### The clash question

The disk decides, and the user answers once per run. The rule as the user
stated it (2026-08-21): *"if I ask to copy the files to a folder, you copy
the files — maybe add a warning that the files already exist. Context
shouldn't matter more than that."*

1. **The check.** After the flush and the final replan, before any byte
   moves, every name the plan would write — the RAW and its sidecar, after
   the template — is checked against the destination with
   `symlink_metadata`: a regular file, a directory, a symlink, a broken
   symlink or a case-variant on a folding volume all count as occupied
   (`exists()` would call a broken symlink absent and rename over it).
   Session memory is not consulted. **The pair is the unit**: a pair clashes
   when either member is occupied, a stray `<name>.xmp` beside no RAW
   included, and a pick with no sidecar of its own to write still clashes on
   the sidecar name. Otherwise a RAW could land beside a sidecar describing
   another photograph — the one thing this module must never produce. The
   suffix walk judges occupancy the same way.
2. **One question, four answers, one policy for the whole run.** No clash →
   no question. Any clash → the dialog names the destination and the counts
   and offers, in this order:
   - **New only** (`N`): every clash-free pick copies exactly as under the
     other answers; a clashing pick is left exactly as the disk has it — its
     RAW and its sidecar are not written, not read and not hashed; neither
     member is opened. A sidecar-only clash is left like any other, because
     landing a RAW beside a sidecar that describes another photograph is the
     thing this module never does — and it is counted apart in the report,
     because under this answer it is a photograph that did not land. The
     run's work is the clash-free picks only: no event names a left pick, its
     bytes are not in the total, and only the clash-free bytes must fit. Not a
     verification pass in disguise — the left picks are not read; Overwrite
     remains the "is my export still bit-perfect?" pass. And not v1's skip
     come back: that was a forced skip decided by session memory (issue #14);
     New only is the user's explicit answer about names found on disk
     tonight, so a hand-emptied folder holds no clash and New only copies
     everything.
   - **Keep both** (`B`): every clashing pair lands under the first free
     numeric suffix, appended to the stem before the extension
     (`DSC01234_1.ARW`, never `DSC01234.ARW_1`), starting at `_1`, the sidecar
     sharing the number. A number `k` is free only when both `<stem>_k.<ext>`
     and `<stem>_k.<ext>.xmp` are free on disk and unclaimed by this plan, so
     a copy is never split across two numbers. Growth is unbounded by design
     (`_1`, `_2`, `_3`, …): each layer costs a deliberate answer. Known
     limit: a name already within a few bytes of the filesystem's maximum can
     still overflow through the `_k` and the `.xmp` — the RAW lands and the
     sidecar fails, honestly reported — because `occupied()` cannot tell "too
     long" from "free" at plan time (issue #10 territory).
   - **Overwrite those N** (`O`): clashing files are replaced in place;
     clash-free files copy normally. A clashing RAW whose destination copy is
     already byte-identical (BLAKE3 of the destination against the source
     stream — never size or mtime) is NOT re-sent: it is kept, and only its
     sidecar is rewritten, and only if it differs — the caption-after-copy
     refresh, reported as "N sidecars replaced beside an identical RAW".
     Overwrite means overwrite (user decision 2026-08-22): a destination
     `.xmp` is replaced like any other file, with no merge, which is what
     keeps every copied byte verifiable against its source. The question says
     so out loud, because darktable keeps its history stack in a file of
     exactly that name. The escapes are New only, Keep both, or a fresh folder;
     recovery if it happens anyway: delete that copy and copy again, or
     re-import in darktable. What Overwrite does replace: a
     destination file that differs from the source — another body's frame
     under the same name, or a copy the user edited in place.
   - **Cancel** (`Esc`): nothing is copied, not even the clash-free files. The
     first Esc returns to the plan preview with destination and template
     intact, so "cancel, then copy somewhere else" is one step; a second Esc
     closes the dialog.
3. **The answer is a policy, not a file list.** After the answer the app
   flushes and replans with the chosen policy, and only that fresh plan
   executes; the executor refuses a plan built before the answer — it copies
   nothing and reports "unanswered clash question", which is also what
   Cancel means. What has to fit: before the answer, under Overwrite and
   under New only, only the clash-free bytes (an overwrite mostly replaces
   bytes already there, one verified temp file at a time, and a genuinely
   full disk then fails those files one by one with an honest reason); under
   Keep both, every byte, because every clashing image is a new file. So a
   nearly-full archive plus four new frames is never refused, the summary
   before the answer states the worst case, and the drop-back after Keep
   both can refuse what the preview allowed.
4. **Nothing is replaced unless the user answered Overwrite.** Under New
   only nothing under a clashing name is even opened, so a destination file
   made unreadable under such a name cannot fail the run. The commit is
   no-clobber: a name that got occupied between the question and the copy
   fails that one file honestly ("a file appeared at the destination during
   the copy") and the run continues. Under Overwrite the executor also
   refuses to write over a file THIS RUN already landed — judged by file
   identity (device and inode on unix; the folded name elsewhere, which is
   right where Windows folds case by default and wrong only in a directory
   made case-sensitive by hand) — so two same-run names that a folding
   volume treats as one cannot eat each other. An identity lookup that
   errors mid-run fails open: refusing on doubt would fail copies the user
   asked for on the far more common case-sensitive destination. A RAW whose
   sidecar then fails is still recorded as committed, so the guard sees it.
   The primitive is `hard_link(tmp, dst)` + unlink; on a filesystem without
   hard links (FAT/exFAT cards, some network mounts) the fallback is
   check-then-rename, whose window is recorded rather than hidden. Overwrite
   never removes a directory standing under a planned name (that file fails
   alone) and replaces a symlink as a link, never writing through it.
   Nothing at the destination is ever deleted, only replaced. Two
   consequences, recorded rather than prevented, both leaving our RAW beside
   an `.xmp` that describes another photograph: a pick with no sidecar of its
   own (its write failed, or the card is read-only) overwrites the RAW and
   leaves the foreign `.xmp` in place — the report says "N destination
   sidecars left in place — those picks have none of their own", and Keep
   both is the answer that walks the pair clear; and the sidecar half of a
   pair can fail after its RAW committed (a directory under the sidecar's
   name, `ENOSPC`, `EACCES`) — reported with which half landed: "the RAW
   landed but its sidecar did not: …", or on the identity path "the RAW at
   the destination is this pick's, verified — but its sidecar could not be
   refreshed: …", where the RAW still counts as identical. The opposite
   direction — our sidecar beside a foreign RAW, issue #14 — is
   structurally impossible: a sidecar is only ever written after its own RAW
   has committed, under a name that is free or explicitly overwritten.
5. **Session memory reads, never decides.** `SessionCopies` records, per
   image and per destination, the RAW path a copy landed at (A → B → A in
   one session still knows about both). It feeds the ✓ copied badge and the
   plan note *"N copied earlier but gone from the destination — copying
   again"*, and nothing else: never a plan, an answer, a mark, or which
   frames the next run takes — letting it decide is what caused issue #14.
   The badge follows the disk: the dialog re-checks on open, a gone copy
   loses it and regains it when the copy lands again. A pick New only left
   is not recorded (this run verified nothing about it); a record the
   session already holds for it survives while the file is there. The
   "copied earlier but gone" note is computed before the answer and under
   every policy, so it is not a promise about what an answer will do: a
   hand-deleted RAW whose sidecar was left behind reads "1 copied earlier but
   gone — copying again" on the preview, and under New only that pick is a
   sidecar-only clash and is LEFT — the report's stray-`.xmp` line is then
   what happened. Open since the 2026-08-21 persona review, not decided:
   cross-session memory of what was copied; an escape on the note for users
   who rearrange the selects folder.
6. **Wording and keys.** The question is a state of the Copy dialog
   (`copy-state 3`), not a second modal: one key scope, no new stacking
   surface (issue #42). It states what each answer does and what it costs,
   never yes/no — at 9 pm "proceed" reads as "proceed with the copy I asked
   for":

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

   - Counts are in picks, never files (148 picks are 296 files on disk). The
     "other N copy normally" clause is mandatory: once Cancel drops
     everything, the user no longer assumes the other answers are normal.
     The `e.g.` line is how a two-body night is told apart.
   - The rows are stacked in order of increasing consequence — `N`, `B`, `O`,
     then `Esc` set apart — so a habitual top-row click lands on the least
     consequential answer that still does something. No row carries accent
     or default styling; the Overwrite label is amber, the New only label the
     ordinary colour. New only is always offered: with no clash-free pick it
     reads *"New only — nothing new to copy, leave the 12 already here
     untouched"* and runs a copy of nothing, so `B` and `O` never move under
     the pointer on exactly the destructive question.
   - The New only row reads `New only — copy the {free}, leave the {clashes}
     already here untouched`, singular forms following the question's habit
     (`copy the 1`, `leave the 1`); its first word kills the "No" reading of
     `N`. "Already here" is allowed on the row (the header just qualified it);
     the report may not say it.
   - The Keep both row names the number that answer would really make: the
     plan walks from `_1` and hands the dialog the first free pair, so a
     second Keep both into the same folder says `_2`. Because a suffix can
     also be claimed in-plan, the number of renames can exceed the clash
     count when a pick is literally named `<other>_1.<ext>` (rare, recorded).
   - "Overwrite those 12", never "overwrite everything": the count is what
     stops the word overstating what happens. Bytes appear on Keep both only
     (`+590.3 MB`, through the shared formatter) — the one answer whose cost
     is knowable up front, and a clash always costs at least its RAW, so no
     KB figure can reach the row with real files.
   - Keys: `N`, `B`, `O`, `Esc`, bare letters only. Enter and Space are inert
     (`Ctrl+E, Enter, Enter` must never mass-replace or mass-duplicate; Space
     is the pick key), `Y` is inert, and no button takes initial focus. A key
     that is not an answer is swallowed AND flips a visible *"Pick one: N, B,
     O or Esc."* line — a silently dead Enter reads as a frozen dialog. While
     the question is up every accelerator is swallowed too (`Ctrl+O`, `Ctrl+E`,
     `Ctrl+Q`, and `Ctrl+N` if ever bound): a destructive answer may never be
     reachable by a key the user presses for something else; the menu bar
     remains the way out for the mouse. `N` was inert until 2026-09-12 (the
     "No" worry); it answers now because the question is a stop, not a
     rhythm — it appears only after `Ctrl+E` then Enter, so the N-N-N cadence
     of rejecting cannot reach it — and New only is the least consequential
     answer that still does something.
   - The plan preview pre-announces the split — `3 new · 148 already exist
     here — Copy will ask what to do` — which across sessions (no ✓ badges
     after a restart) is the only sign the folder already holds this shoot.
     When the template in play contains `{seq}` (any form) AND the plan has
     at least one clash, the preview adds *`{seq}` numbers the whole session
     — the names already here may now belong to other frames*: the new picks
     renumber everything after them, so "already here" is judged on shifted
     names, and New only would copy the wrong frames under a clean report.
     A warning, never a refusal (the user, brief 005); the fact is core's
     (`CopyPlan::seq_meets_clashes`), the sentence the bridge's.
   - The destination is shown tail-first (`…/2026-08-21-osprey/selects`)
     wherever it is elided: Slint's elide cuts the end, exactly the part
     that tells two shoots apart.
   - The progress line says which work it is doing: `Checking 12 / 148`
     under Overwrite (which starts by hashing), `Copying 2 / 3` otherwise.
     Under New only the total is the number of picks this run copies —
     `Copying 1 / 4`, never `Copying 1 / 148` — and there is no "Skipping"
     line: a skip takes no time and deserves no line.
   - The report counts what actually happened: `3 copied`, `145 already
     identical — re-verified in place`, `12 landed under new names
     (DSC01234_1.ARW …)`, `12 replaced`, `N sidecars replaced beside an
     identical RAW`, `N destination sidecars left in place — those picks have
     none of their own`. "Replaced" means something was under one of the two
     names and this job was allowed to overwrite it, decided from the
     filesystem before anything is written. "All checksums verified" attaches
     to copied AND re-verified files: an identity check is a BLAKE3
     verification of the destination against the source, so a re-run doubles
     as a free "is my export still bit-perfect?" pass before the card is
     wiped. Under New only, after the lines that count what the run did and
     before `cancelled`/`FAILED`: `144 already had files with these names
     here — left untouched, not re-checked` (singular: `1 already had a file
     with this name here — left untouched, not re-checked`), then, only when
     it happens, `1 of those is a stray .xmp with no RAW beside it — that
     pick was not copied` (plural: `N of those are stray .xmp files with no
     RAW beside them — those picks were not copied`). The wording says the
     NAMES were taken, never that the photographs are there, and "not
     re-checked" is said out loud so the green light on the copied line can
     never be read across. The left counts are decided at plan time, from
     the filesystem, and the report carries them whether or not the run
     finished (`CopyReport::left_untouched`, `left_sidecar_only`).

Exposing Overwrite reverses the v1 decision that it is never exposed; it is
bounded by the verified-temp-then-commit contract (a failed or corrupt
transfer never replaces a good file). A re-run into a folder that still
holds the session's own copies asks like any other clash (the user,
2026-08-21: "it's fine. If you're saving where there are files already, it
should ask"); the answer that adds new picks without re-reading the old ones
is New only, and Overwrite is the answer that also re-verifies — on a network
destination that read is ~2× the clashing bytes over the wire (the
persona's open question 5 of 2026-08-21, answered by New only).

### Two picks, one name

Two picks in one run that expand to the same destination name never ask
(user decision 2026-08-22): overwriting would throw away one of the two
photographs the user just asked for, and cancelling would lose both, so the
later pick takes the first free suffix under every policy and the run
proceeds. The walk skips names taken on disk as well as in the plan, so the
name it lands on clashes with nothing — which is why it needs no question
even into a crowded folder. Silent but not invisible: the preview says
*"N picks share a name with another — those get a suffix"*
(`CopyPlan::shared_name`, kept apart from `renamed`, the suffixes taken
because the DESTINATION held the name, so neither sentence is said about the
other's files). This also covers a template that collapses several images
onto one name (`same.{ext}` → `same.ARW`, `same_1.ARW`, …).

The one exception: the in-plan claim set is exact-case, so `C.ARW` and
`c.arw` are two names to the plan. On a case-sensitive destination both
land; on a folding one the second pick FAILS instead of being suffixed —
refused by the no-clobber commit, or by the same-run identity guard under
Overwrite. Failing is the safe direction, but it is a failure where this
rule promises a suffix; closing it needs the destination's folding
behaviour probed at plan time (the not-verified list).

The suffix walk resumes per base name instead of restarting at `_1` for
every pick. Restarting is quadratic, and ordinary typing triggers it: a
hand-typed template has a literal prefix before its first `{`, so every pick
expands to the same name while the field is mid-word, and `plan()` runs on
the event-loop thread on every keystroke. The resuming shape is 2 probes for
the first pick and 4 for each of the others — exactly `4N − 2` destination
probes for N colliding names — which the test holds as an exact figure at N
and at 2N, because a walk that resumes only partly stays under any generous
ceiling. What that fix does not do: `plan()` is still linear in `stat`
calls (three per pick, five when the name collides) and still synchronous
per keystroke with no debounce — milliseconds on a local disk, a visible
freeze on a network or FUSE destination. A debounce or planning off-thread
is the eventual fix; no perf budget covers plan time.

### The dialog

- A destination picker that can create a folder, with the remembered path
  shown PROMINENTLY (yesterday's job is the failure mode); the template field
  defaults to empty (keep names); the remembered template is offered as a
  one-click *"Use last: …"* chip in the plan state, never silently
  pre-applied; a live preview of the first three expanded names when a
  template is set; count, total size and free space up front — `148 picked ·
  7.3 GB to copy · 1.2 TB free`; collisions summarized, never tabulated (no
  148-row table between the user and the Copy button); Enter copies when the
  plan is clean; per-file progress with Cancel (the plan state has no Cancel
  BUTTON and the scrim swallows clicks without dismissing, so a mouse-only
  user has no way out of it — recorded, unfixed; the video dialog has one);
  the report with its verified
  line, the failures with reasons, and an *Open destination folder* action.
  Modal in v1. Cut from v1: per-file mode selectors, speed and ETA, pause,
  background copy.
- **Sizes on screen** (user decision 2026-09-12, brief 006): every byte
  count either dialog prints goes through the one formatter the two bridges
  share (`human_bytes`, app crate — presentation, not a rule about files):
  this summary line, the Keep both cost, the free-space refusal, and the
  video dialog's plan line, refusal and report. Five binary tiers: `{n} B`
  below 1,024 bytes (`0 B`, `1023 B`), then `{:.1} KB` from 2^10, `MB` from
  2^20, `GB` from 2^30, `TB` from 2^40 — one decimal on every tiered value
  (`1.0 KB`, `12.0 TB`), the tier picked first and the value rounded inside
  it (1,048,575 B is `1024.0 KB`, never `1.0 MB`), and no PB tier (`1024.0
  TB` runs on). Binary because `df -h`, Explorer's drive tile and a NAS
  dashboard are binary; GNOME Files and Finder are decimal and read ~7 %
  higher, recorded and not a bug. The video's name-length refusal is a
  length, not a size, and stays in bytes. Not built: a units preference,
  GiB/TiB labels, a decimal/binary switch, per-file sizes, a free-space bar,
  an ETA.
- **The card is 560 px wide and its height follows its content** (issue
  #62), between a 480 px
  floor — the height it always had, so nothing moves in the ordinary case —
  and the window (`parent.height - 40px`). Past the ceiling the text body
  scrolls in a `ScrollView`: by wheel, and by keyboard — Down/Up a line (40 px),
  PgDn/PgUp a body, Home/End the ends — only while it overflows, so those
  keys keep their meaning otherwise. The header rows and the button row
  never give up a pixel; the body is the only row that shrinks, so the
  buttons stay inside the card wherever the window can still hold the fixed
  rows (~190 px, about 300 px of window height); below that the row leaves
  the card, and that is accepted. The report prints one
  `FAILED name: reason` line per failed file, so a destination that goes
  read-only mid-run words itself as long as the run was — and it is all
  readable without a mouse. The clash answer rows sit inside the scrolling
  body; below ~500 px of window height they need a scroll to come into
  view, and answer by key wherever the body stands.
- The card, its button row and the four answer rows report their
  rectangles, so a driven test clicks them by name (test-harness.md).

## Contracts

- Core owns every count and every sentence rule: `CopyPlan` (the pairs,
  `renamed`, `shared_name`, `seq_meets_clashes`, the clash counts and their
  bytes), `CopyReport` (`landed`, `left_untouched`, `left_sidecar_only`,
  `earned_the_green_light()`), `SessionCopies` (reads, never decides — the
  shape `clip::ExportLedger` copies for the video export), `PlanError`
  (developer-facing `Display`; the free-space sentence is the bridge's).
- The sidecar barrier: no copy plan is built before pending sidecar writes
  are flushed (xmp-sidecars.md, *Write scheduling*).
- The temp-name contract: `<dest>/.fastcull-partial-<pid>-<n>`, unique per
  file, `create_new`, committed by a no-clobber link.
- `{seq}` reads `filter::view_true_sort`, never the provisional order
  (ui-grid.md, *Provisional order while loading*).
- The video export shares the clash policy enum and the formatter; it maps
  New only to the refused `Clash` marker and offers three answers
  (video-export.md, *Files*).
- ADR 0003 and 0004: no RAW is ever opened for writing; nothing at the
  destination is deleted; nothing is replaced without the Overwrite answer.
- For the driven suite (test-harness.md): the marks `copy finished run N`,
  `copy card laid out …`, `copy buttons laid out …`, `copy body scrolled to
  Y`, `copy answer N|B|O|Esc laid out …`; the dump fields `copystate=` (0
  plan, 1 running, 2 report, 3 the question), `confirm=`, `newonly=`,
  `nudge=`, `nudged=`, `warning=`, `copyprogress=` (`Starting…` before the
  first file; the last line survives into the report), `copyerror=`,
  `copynote=` (the preview's notes, the `{seq}` note among them), `report=`;
  the tokens `copydest:PATH` (before the
  `Ctrl+E` that should see it), `copytemplate:TEXT` (after it — opening
  clears the field), `click:copy answer B`.
- A `#[cfg(unix)]` test takes its private helpers with it — `cargo clippy
  --all-targets -- -D warnings` on the Windows job refuses dead code —
  while helpers shared with a platform-neutral test are never gated (red on
  the v0.13.0 commit).

## Acceptance criteria

Core tests are in `fileops.rs` unless named otherwise; `pump.rs` and
`copy_bridge.rs` are app unit tests; `screenshot.rs` tests drive the real
dialog with real key events.

- [x] Plan: template expansion; two picks on one name are suffixed, never
      refused — 2,000 of them with exactly `4N − 2` probes and the same
      closed form at 2N, their bytes counted by the free-space check;
      dest-inside-source and escaping templates rejected —
      `many_picks_on_one_name_take_consecutive_suffixes`,
      `free_space_counts_a_batch_suffixed_pick`,
      `plan_templates_seq_and_suffixes_in_batch_collisions`,
      `two_picks_with_the_same_name_always_get_a_suffix_without_a_question`,
      `planned_paths_never_leave_the_destination`,
      `plan_rejects_dest_inside_or_equal_to_source`,
      `plan_rejects_a_template_that_escapes_the_destination`.
- [x] Execute: pairs land with the right names and verify; a corrupted
      destination write is detected under both commit modes and a corrupt
      replace never destroys the file it would have replaced; a
      read-protected source fails alone; no partial file after a simulated
      failure; cancel keeps the finished copies and is asserted on the
      between-files branch —
      `execute_copies_verifies_and_isolates_failures`,
      `copy_verified_detects_corruption_and_cleans_up`,
      `cancel_between_files_keeps_finished_copies`.
- [x] The sidecar barrier: a pick made ≤ 1 s before Copy is in the copied
      sidecar — `sidecar_barrier_fresh_pick_lands_in_the_copy`.
- [x] A re-run after a hand deletion copies again, RAW and sidecar together,
      with the note and no question when the destination is genuinely empty
      (issue #14); the memory is per destination and a re-spelled folder
      supersedes its own entry —
      `a_hand_deleted_copy_goes_out_again_with_no_question`,
      `session_copies_are_remembered_per_destination_for_the_badge`,
      `record_supersedes_the_entry_of_a_re_spelled_folder`; app:
      `copy_picks_rerun_recopies_hand_deleted_files` (real A1 files, driven
      through `copydest:`).
- [x] The clash check sees RAW and sidecar names, on templated names, from
      the filesystem (directory, symlink, broken symlink) —
      `a_directory_or_a_broken_symlink_under_a_planned_name_is_a_clash`,
      `the_clash_check_sees_templated_names_and_never_reflows_seq`,
      `a_sidecar_left_behind_is_a_clash_the_answers_resolve_both_ways`.
- [x] One question per run, its clashes and bytes counted apart, and the
      Keep both row naming the number it will really use —
      `ask_marks_the_clashes_and_counts_their_bytes_apart`.
- [x] Overwrite replaces a differing file, only refreshes the sidecar of an
      identical one, never removes a directory or writes through a symlink,
      never hangs on a FIFO under a planned name and never reads a
      non-regular destination, and leaves a foreign sidecar in place when
      the pick has none of its own —
      `overwrite_replaces_a_differing_file_and_only_refreshes_an_identical_one`,
      `overwrite_never_removes_a_directory_and_replaces_a_symlink_not_its_target`,
      `overwrite_does_not_hang_on_a_fifo_under_a_planned_name`,
      `overwrite_without_a_sidecar_of_our_own_leaves_the_foreign_one`.
- [x] Keep both suffixes from `_1` before the extension, RAW and sidecar in
      lockstep, advancing the pair when either member is taken on disk or
      in-plan — `create_copies_suffixes_from_1_and_moves_the_whole_pair`.
- [x] Cancel copies nothing; a plan built before the answer is refused
      wholesale; a name occupied after the question fails that file alone
      without clobbering; the hard-link-less fallback still refuses an
      occupied name; free space follows the chosen policy —
      `execute_refuses_a_plan_built_before_the_answer`,
      `a_name_taken_after_the_plan_fails_that_file_alone`,
      `the_no_hard_link_fallback_still_refuses_an_occupied_name`,
      `the_free_space_check_follows_the_answer`.
- [x] The same-run identity guard: an overwrite never replaces a file this
      run just landed, on a destination that collapses two names (a hard
      link drives the collapsing case on any filesystem; two real
      case-twins prove no false alarm on a case-sensitive one, skipping
      where the filesystem folds so the Windows job stays green); driven
      through the real executor; a failed caption refresh still counts the
      RAW it verified —
      `an_overwrite_never_replaces_a_file_this_run_just_landed`,
      `the_executor_refuses_to_overwrite_a_file_this_run_just_landed`,
      `a_failed_refresh_still_counts_the_raw_it_verified`.
- [x] The temp name is never reused and never written through, so an alias
      left by a failed unlink cannot be truncated by the next file; a
      228-byte destination name still ships the whole pair —
      `a_temp_name_is_never_reused_or_written_through`,
      `a_very_long_destination_name_still_ships_the_whole_pair`.
- [x] A destination that is a file, or a dangling symlink, is a plan error
      while a symlink to a folder is accepted —
      `a_destination_that_is_a_file_is_rejected_by_the_plan`.
- [x] The report never contradicts itself: no "nothing needed copying" over
      failures, no green light without verified bytes, a foreign sidecar
      left beside our RAW is named, a sidecar that fails after its RAW
      landed says which half landed; the green light is core's rule —
      `pump.rs` `report_lines`: `the_headline_says_what_happened`,
      `the_verified_sentence_follows_what_was_verified`,
      `a_foreign_sidecar_left_in_place_is_reported`; core:
      `a_sidecar_that_fails_after_its_raw_landed_says_so`,
      `the_green_light_needs_verified_bytes`.
- [x] The question through the real dialog: it appears with its counts,
      Enter and `Ctrl+O` are inert on it, `B` lands `_1` with its own
      sidecar, `O` replaces the differing file and re-verifies the identical
      one, Esc returns to the plan and copies nothing (proven on a second
      destination that stays untouched), a folder opened under the question
      drops it, and one round answers with the mouse by name —
      `copy_picks_asks_once_and_each_answer_does_what_it_says`. Review-
      verified only, deliberately: the *Use last* chip's confinement to the
      plan state (asserting an absent control by clicking where it would be
      is a test that passes when the click misses).
- [x] New only (brief 005, issue #86): a clashing pair is byte-for-byte and
      mtime-identical after the run, a differing destination sidecar
      included, and a pair made unreadable under a clashing name does not
      fail the run; every clash-free pick copies and verifies as under the
      other answers and a same-run shared name is suffixed; a hand-emptied
      folder holds no clash; a sidecar-only clash is left and counted apart;
      the progress total equals the picks copied and no event names a left
      pick; the report prints the left line and the stray-sidecar line, the
      green light attaches to copied files only, an all-left run earns none
      and never "Nothing needed copying", the counts survive a cancel; only
      the clash-free bytes must fit; the `{seq}` note appears exactly when a
      `{seq}` template meets a clash; the dialog: N row first with its
      counts, `N` a bare letter, `Y`/Enter/Space/accelerators inert, the
      nudge line, the warning's "New only leaves them alone." clause, the
      row still offered when every pick clashes, the row order from the
      rows' own layout marks; docs/copy-picks.md and docs/faq.md say what
      the dialog does (review-verified; the label colours — amber Overwrite,
      ordinary New only — are review-verified only, no dump carries a
      colour) —
      `new_only_leaves_a_clashing_pair_untouched_and_copies_the_rest`,
      `new_only_never_opens_a_clashing_pair` (`#[cfg(unix)]`),
      `a_hand_emptied_folder_holds_no_clash_so_new_only_copies_it_again`,
      `a_sidecar_only_clash_is_left_and_counted_apart_under_new_only`,
      `new_only_emits_one_event_per_copied_pick_and_none_for_a_left_one`,
      `a_cancelled_new_only_run_still_reports_what_it_left`,
      `plan_flags_a_seq_template_that_meets_a_clash`,
      `the_free_space_check_follows_the_answer` (its fourth arm); `pump.rs`
      `a_new_only_run_reports_what_it_left_and_earns_no_green_light_for_it`;
      app: `copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone`.
      Mutants: the New only arm mapped to Replace, and any read of the left
      pair's destination, each turn a test red.
- [x] Sizes on screen (brief 006, issue #88): the formatter prints `1.0 KB`,
      `1.0 MB`, `1.0 GB`, `1.0 TB` and `12.0 TB` at the boundaries and
      `1023 B` below the first, rounds inside the tier the threshold picked
      (`1024.0 KB` at 2^20 − 1, `1024.0 GB` at 2^40 − 1), runs on above TB
      (`1024.0 TB` at 2^50) and prints `u64::MAX` without a panic; the two
      figures the issue was opened on read `1005.4 KB` and `1.2 TB` —
      `copy_bridge.rs` `a_byte_count_reads_in_its_tier_with_one_decimal`
      (ten mutants, one per tier and format, each red). The existing readers
      of a size string stay green unchanged:
      `the_verified_line_of_a_video_export_is_earned` (`344.0 MB`),
      `the_two_untestable_messages_now_have_a_test` (`4.5 GB`, `1.1 GB`),
      and the re-run test's `!summary.contains("0 B to copy")` guard.
- [x] The free-space refusal reads *"The copy needs 7.3 GB and there is
      1.1 GB free at the destination."* on the plan preview and on the
      drop-back after an answer, every other `PlanError` keeps its text, the
      video's refusal is unchanged —
      `copy_bridge.rs` `the_copy_refusal_reads_in_units_a_person_reads`;
      driven on both paths with an 8 TiB sparse TIFF-fronted fixture
      (`#[cfg(unix)]`: NTFS allocates real clusters on `set_len`):
      `the_copy_refusal_reaches_the_dialog_on_the_drop_back_after_keep_both`,
      `the_copy_refusal_reaches_the_dialog_on_the_plan_preview`.
- [x] Brief 006 AC3: the illustrative sizes in this spec, in video-export.md
      and in `docs/` are the screen's form, one decimal (`328.4 MB`, never
      `328 MB`), and docs/copy-picks.md says what the dialog says when the
      destination lacks the room — review-verified at the gate (no driven
      test reads the specs or the docs).
- [x] The button row never leaves the card: at the floor, grown to its
      content, or clamped at the ceiling with the body scrolling by wheel
      and by PgDn/Home —
      `a_failure_report_longer_than_the_window_keeps_the_copy_buttons_inside_the_card`
      (`#[cfg(unix)]`, a `chmod 555` destination; red on the parent tree:
      the row ended 830 px outside the card).
- [ ] Not verified anywhere, carried forward: a case-insensitive destination
      (no casefold or FAT mount on the development seat), so "a case-variant
      counts as occupied" and the same-run guard's folding behaviour are
      asserted by their mechanism and by hard-link stand-ins; the
      hard-link-less commit fallback has never run on a filesystem that
      lacks links; a Windows drive-prefixed template name (`C:x.ARW`), which
      the plan refuses through `Path::components` and unix cannot exercise;
      network destinations; a real darktable round-trip of the
      overwrite-replaces-sidecars warning and of New only leaving a real
      history stack intact (the core stand-in is a differing destination
      sidecar).
- [ ] Windows reserved names (`CON`, `NUL`, trailing dots) in templated
      names — deferred with the user's OK (2026-07-26, "low priority"),
      issue #10; spaces and Unicode in paths are covered.

## History

- 2026-09-17 — Rewritten in the brief 007 shape. The pre-rewrite text, with
  every gate finding and measurement, is `specs/history/fileops.md`.
- 2026-09-12 — New only, the fourth answer to the clash question (issue #86,
  brief 005, PR #87); sizes on screen in five tiers and the free-space
  sentence in units a person reads (issue #88, brief 006, PR #90).
- 2026-08-29 — The suffix walk's probe count replaces a stopwatch (issue
  #58, `c8f73f1`; v0.13.0).
- 2026-08-30 — The card's height follows its content and its body scrolls
  (issue #62, v0.13.0).
- 2026-08-22 — Two picks with one name always get a suffix (`be9e76d`);
  overwrite means overwrite, no sidecar merge (the user); `{camera}` filled
  from the session's EXIF model; the suffix walk resumes; the same-run
  identity guard; templates confined to the destination (the user's
  invariant).
- 2026-08-21 — The clash question replaced the v1 rename default, the "Skip
  existing" toggle and the four-way `ExistsMode` (`558f57d`, `c96b777`,
  v0.10.0); "already copied" means "still there" — session memory reads,
  never decides (issue #14, `89146fd`).
- 2026-07-25/26 — M6 shipped (`f79352e`); persona review: all picks, a modal
  dialog, checksums in v1, rejects untouched, the user's confirmation that
  metadata is added before copying.
