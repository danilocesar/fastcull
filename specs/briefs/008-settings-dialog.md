# Brief 008 — the Settings dialog: a tabbed modal, a settings file, and the settings the specs already promised (issue #39)

Date: 2026-10-01. Issue #39 (its dependents: #15, #24, and the Clear
Cache half of #3 — only the last lands here). Branch `settings-dialog`
from `origin/main` 6c5426c (M12). A feature, so the persona gate ran
(verdicts below). The user is asleep while the unit runs: a question
only the user can answer that arises mid-unit is taken on the most
reversible option, recorded in the decisions log as PROVISIONAL, and put
to the user in the morning report; the PR merges only if no provisional
ruling is outstanding.

## Context

The user's words (2026-09-30): "A modal window within the app (not
mandatory, can be outside, but ideally in the app). That window will
have multiple tabs and each tab will be a category of change: general,
UI, performance, and space for more forms in the future." And
(2026-10-01): "environment variables should take precedent against the
settings; the settings can be an ini file: each group has a name. Each
variable inside the group has a name and a value. When hovering the
name of each field in the screen, a small toolbox should show and a
brief description of what the settings does. That should give us a
simple settings screen to start with."

What exists today. File › Settings… has been a disabled placeholder
since M5 (ui-grid.md "Window chrome": "Settings… (placeholder, disabled
until a settings dialog exists — post-v1)"; `main.slint` draws it as
"Settings… (soon)"). `~/.config/fastcull/ui.toml` remembers the Copy
Picks destination, the rename template and the video destination
(remembered state, not choices; `session.rs`); `templates.toml` holds
the IPTC templates; `~/.cache/fastcull/previews.db` is the thumbnail
cache with a 2 GiB cap enforced at startup and no VACUUM
(catalog-cache.md). Of the shipping environment variables
(test-harness.md) exactly one is a user knob: `FASTCULL_MAX_READERS=N`
replaces the read pool's adaptive cap (raw-pipeline.md).

Issue #39 was parked by the user on 2026-08-03 so that no settings-shaped
request would ship as invisible state. The rule it protects is the
persona's verdict on issue #8 (2026-07-26): a toggle whose state cannot
be seen in the UI is worse than no toggle — the "same folder, different
contents" surprise.

The specs already promise four settings and never delivered them:

- ui-grid.md "Marks and auto-advance": "It becomes a configuration
  option (default on) with the settings dialog; until then it is always
  on." (`filter.rs::cursor_after_mark` already takes `auto_advance:
  bool`; the app passes `true`.)
- ui-grid.md "The selection wash": "a property,
  `selection-wash`/`selection-wash-opacity`, destined to become a
  setting — above ~15 % the tint can shift colour judgement on a final
  scan, accepted knowingly". (`state.rs` clamps the value at its single
  write site "so a future settings path inherits it for free".)
- raw-pipeline.md "Memory": "Full-res decodes: the engine's byte-budget
  LRU, 2 GiB by default" and, in the ladder, "configurable" — nothing
  lets a user change it (`session.rs:154` passes
  `loupe::DEFAULT_BUDGET_BYTES`; issue #3's measurement of 2026-07-31:
  the app's footprint runs ~1.3 GB above the budget because textures
  sit outside it, and the budget holds ~13-14 A1 frames before a
  revisit re-decodes at ~490 ms; `loupe.rs:231` floors the budget at
  200 MB).
- catalog-cache.md "Size cap, default 2 GiB, enforced by LRU eviction …
  when the default database is resolved at startup"; issue #3 asks for a
  user-facing Clear Cache with a size readout.

And one promise found unimplemented at this gate (M10): ui-grid.md
"Visual language" says "Failed: a warning badge and a tooltip with the
reason", while `main.slint` 1336–1352 draws a bare red "!" with no
tooltip. The user's ruling (2026-10-01): build it in this unit.

## Goals

- G1. A Settings dialog inside the app, reached from File › Settings…
  and `Ctrl+,`, with the tabs General | UI | Performance and a shape the
  next unit extends by adding a tab or a row.
- G2. A settings file in the config dir, INI-shaped (a named group, a
  named key, a value), hand-editable, hermetic in every test, with the
  environment winning over the file wherever an environment variable
  governs a knob.
- G3. The settings the specs promised: auto-advance after Y/N, the
  selection wash strength, the loupe memory budget, the thumbnail
  cache cap, the read workers, and Clear cache with a size readout.
- G4. Every setting's state is visible and every field explains itself
  on the screen — nothing hidden, nothing that needs a pointer to read.
- G5. The Failed badge's promised tooltip.

## Non-goals

- "Show paired JPEGs" (#15) and "Lightroom-compatible sidecars" (#24):
  follow-up units on this dialog, each with its own persona gate.
- No new environment variable (the user, 2026-10-01: "let's do it for
  the existing variables that make sense be a settings; in the future,
  new settings can be added and we need to see if would make any sense
  to set them as env variables"). `FASTCULL_NO_CACHE`,
  `FASTCULL_TRACE`, `FASTCULL_DRIVE`, `FASTCULL_NO_CONFIG` and
  `FASTCULL_KITCHEN_COOK_MS` stay what they are — diagnostics and
  harness plumbing, not settings (decision D3).
- No hover tooltip on the settings fields (the user, 2026-10-01, on the
  persona's IN-MY-WAY: inline one-line notes instead).
- No OK / Apply / Cancel, no unsaved-changes state, no global Reset.
- No live preview of the wash behind the dialog (persona: a 67 %-black
  scrim lies about it; apply-on-commit plus Esc is the preview).
- No RAM-share DEFAULT for the loupe memory (the user may type a
  percentage; the default stays the spec's 2 GB) and nothing about the
  held-arrow pace — issue #60 stays parked.
- No periodic VACUUM (the second half of #3 stays open); Clear cache
  vacuums once, by the user's hand.
- No settings for `max_gap`/`min_run` (burst-grouping.md: "no settings
  UI in v1"), no light mode (ui-grid.md: dark-only, pinned), no
  Open Recent (flagged to the user separately — see the decisions log).
- No CLI flag surface: the CLI reads the same file for the knobs it
  shares with the app (the cache cap, the read workers) and keeps
  `--no-cache`.

## Applicable directives

- CLAUDE.md hard rule 5 (the settings model, the file's parsing and
  writing, validation and clamping, the memory-string parser, the
  environment precedence, the readers resolution, the cache size
  measurement and Clear cache are `fastcull-core`'s, with unit tests
  beside them; the app binds Slint properties to them); hard rule 6
  (no budget row moves: the settings are read before any timed region,
  and the dialog never blocks the UI thread on the cache work); rule 1
  is untouched (nothing here writes near a RAW).
- 01-architecture.md: the core modules table gains a row for the new
  module; the threading model ("the UI thread never blocks on I/O") —
  `ui.toml` is already written from the UI thread (`session.rs`,
  ~1 KB, the precedent); the senior developer rules whether the
  settings write follows it or goes through a worker, and the VACUUM
  of Clear cache always does.
- ui-grid.md: "Window chrome" (the File menu order; the placeholder
  sentence becomes false), "Keyboard map" (`Ctrl+,` is added as a
  chord; the shortcuts card lists every binding and the parity test
  locates the table by the `## Keyboard map` H2 — it stays an H2),
  "Modal keyboard containment" (issue #42: Esc closes the topmost
  modal only, the menu bar stays live), "Focus continuity" (the `-1`
  token for a dialog's own scope; a dialog over a focused field
  commits like click-away), "Visual language" (dark-only, the pinned
  palette; the Failed badge's tooltip), "Marks and auto-advance" (the
  sentence that becomes false), "The selection wash", "The selection
  rule" (a mark that does not move the cursor does not collapse the
  selection), the shortcuts card's rules (780 px wide, fits whole at
  1000×700 with ~25 px of room — "the next binding replaces a row or
  moves a section").
- catalog-cache.md "The cache": the cap and its startup enforcement, "no
  VACUUM, pages are reused" (Clear cache becomes the exception), "lock
  contention never triggers deletion … deleting a merely-locked database
  under a live connection loses data and can SIGBUS the peer process"
  (Clear cache never unlinks), the `FASTCULL_NO_CACHE` contract.
- raw-pipeline.md "The adaptive read pool" (the `FASTCULL_MAX_READERS`
  semantics are unchanged and become the setting's semantics) and
  "Memory" (the 2 GiB default; "configurable" becomes true).
- test-harness.md: environment variables ship in release builds and
  explain themselves on stderr; `FASTCULL_NO_CONFIG=1` makes `ui.toml`
  unreachable for load and save and the screenshot harness sets it
  unconditionally — the settings file follows the same rule; the drive
  script's modal tokens (`about`/`shortcuts`), `click:<element>` by
  name (issue #70), `key:ctrl+<k>`.
- docs page map (CLAUDE.md, "docs/ follows specs/"): a new
  `docs/settings.md` and a new entry in the map (settings ↔ settings);
  culling ↔ ui-grid (auto-advance, `Ctrl+,`); faq ↔ catalog-cache and
  everything else (the cache's location and Clear cache; the
  environment variables).
- ADR: the senior developer judges whether the storage-and-precedence
  contract is architectural (ADR 0005); the Manager's recommendation is
  yes — a persistent per-user contract both binaries read, with a
  precedence rule, is the kind of decision the next unit must not
  re-litigate.
- M1 (spec first), M2 (UX choices decided by best practice after the
  gate, dated), M7 (never name the user), M8 (what no role can answer
  goes to the user — in the morning, with the provisional ruling), M11
  (the frames hint says "A1 frames": 149 MB per decoded full-res frame
  is an A1 number), M12 (the branch is cut from `origin/main`).
- Rules of the gate: a feature, so old-red-first does not apply; a
  mutant for every new guard; the senior developer's veto on every test
  change; deferring a spec acceptance criterion needs the user's OK —
  and the user is asleep, so no criterion is deferred overnight.

## Requirements

Wording in quotes is the Manager's; the senior developer may tighten
it in the spec change where a sentence reads better, never loosen what
it promises.

- R1. **Opening and closing.** File › Settings… is enabled and opens
  the dialog; so does the chord **`Ctrl+,`** (comma) in the main key
  scope, matched like `Ctrl+E` and inert while a field or another
  dialog holds the keyboard. The shortcuts card's FILE MENU section
  gains the row (`Ctrl+,` — Settings…), and the card still fits whole
  at 1000×700. `Esc` and a **Close** button close it; a click on the
  scrim does NOT (a form, like Copy Picks, not a popup like About).
  Modal and keyboard-contained in every state (issue #42): under it
  `Y`/`N` mark nothing, `Ctrl+O`, `Ctrl+E` and `Ctrl+Shift+E` are
  inert, every grid key is swallowed; About or the shortcuts card over
  it closes topmost-first; the menu bar stays live (File › Quit works).
  Opening it over a focused IPTC or keyword field commits that field
  like a click-away and the dialog owns the keyboard (the `-1` token);
  closing returns the keyboard to the main key scope, deterministically
  (`key:+` zooms afterwards).
- R2. **The card.** The house style (`ModalScrim`'s card: `#202028`,
  1 px `#3a3a44`, 8 px radius, the scrim swallowing the wheel), its
  height following its content (`card-fits-content`), a **tab strip**
  across the top — **General | UI | Performance**, in that order, the
  active tab marked — and under it the active tab's form: one row per
  setting, a label, a control, and a one-line note beneath them in the
  shortcuts card's dim style (the `G` row's grey line) that says what
  the setting does, its default, and when it takes effect. A **Reset
  to defaults** button per tab, resetting that tab's settings only and
  writing the file. Keyboard: `Tab`/`Shift+Tab` walk the active tab's
  controls and the strip; `Left`/`Right` on the strip switch tabs;
  `Ctrl+Tab`/`Ctrl+Shift+Tab` switch tabs from anywhere in the dialog;
  **digits never switch tabs** (`1`–`5` are reserved and, in a dialog,
  are digits for a number field). The tabs are a list the next unit
  extends by adding one entry, not a hand-rolled triplet.
- R3. **Apply on commit.** A checkbox applies on click or `Space`; a
  number field applies on `Enter`, `Tab` or click-away; **`Esc` in a
  field discards its uncommitted text and closes the dialog** (a "2" on
  the way to "20" never lands as 2 %); every commit writes the file at
  once; there is no Apply, no OK, no Cancel, no unsaved state. After a
  commit the field shows the value IN FORCE — parsed and clamped —
  never the raw text.
- R4. **The file.** `settings.toml` in the config dir (the
  `directories` crate's `config_dir()` for `("org", "fastcull",
  "fastcull")`, beside `ui.toml` and `templates.toml`; `ui.toml` is
  untouched). TOML is the INI the user described: a named table per
  tab and a named key per setting —

  ```toml
  [general]
  auto_advance = true          # default true

  [ui]
  selection_wash = 25          # percent, 0–50, default 25

  [performance]
  loupe_memory = "2 GB"        # "<n> GB" or "<n>%" of total RAM; default "2 GB"
  cache_cap = "2 GB"           # "<n> GB"; default "2 GB"
  max_readers = 0              # 0 = adaptive (default); N ≥ 1 = FASTCULL_MAX_READERS=N
  ```

  The file is read by core at startup (both binaries) and again each
  time the dialog opens, and what it says is APPLIED at that open, so a
  hand edit made mid-session takes effect when the dialog is next
  opened (the note on the Performance knobs still says "at the next
  folder open"). It is written on every commit and on Reset, never
  otherwise: a missing file stays missing until the first change. A
  write is read-modify-write and preserves unknown keys and tables; it
  emits every known key, each under a `#` comment line carrying the
  field's note, so the file documents itself for hand editing
  (`toml_edit`, already in the tree under `toml` 0.8, keeps the user's
  own comments and unknown entries; no new crate). A value out of range
  clamps on read and the dialog shows the clamped value; an unknown key
  is left alone. **A file that fails to parse is never overwritten in
  place**: the defaults are in force, one stderr line and one
  status-line line at startup name the file and the parse error, and
  the FIRST write moves the broken file aside under a name the status
  line reports (`settings.toml.broken`, numbered if that exists) before
  writing a fresh one. **Hermetic**: `FASTCULL_NO_CONFIG=1` makes
  `settings.toml` unreachable for load and save exactly as it does
  `ui.toml`, so the screenshot harness and every driven run never touch
  the real file; core tests pass an explicit path. The whole model —
  the struct, its defaults, the parser, the writer, the clamps — is
  `fastcull-core`'s, pure and unit-tested; the app binds it.
- R5. **Environment precedence.** Where an environment variable governs
  a setting's knob, the environment wins over the file, and the dialog
  shows that field **read-only with the environment's value** and a
  note: "set by `FASTCULL_MAX_READERS` in your environment — unset it
  to change this here". Today exactly one pair exists:
  `FASTCULL_MAX_READERS` ↔ `performance.max_readers`, with the
  semantics raw-pipeline.md records, unchanged. An unparsable
  environment value is ignored and the field stays editable (what
  `pipeline.rs` does today with `parse().ok()`). No new environment
  variable is introduced; the rule is written once, in settings.md, and
  a future setting gets a variable only by its own decision.
- R6. **General › Auto-advance after Y/N** (`general.auto_advance`,
  default on; applies instantly). On: as today — a `Y`/`N` moves the
  cursor to the next frame at every zoom and collapses the selection
  like an arrow. Off: `Y`/`N` mark and the cursor STAYS; the selection
  is left alone, exactly as `U` behaves (no cursor move, nothing
  collapsed — persona 2026-10-01, Manager-recorded: a mark that does
  not move the cursor is not navigation under the file-manager rule).
  The one exception is the one `U` already has: when the mark removes
  the frame from the active filtered view, the live-removal cursor rule
  moves the cursor to the survivor as today. The note states the
  exception in the user's words ("Off keeps the cursor on the frame you
  marked — unless the filter hides it, in which case the cursor moves
  to the next one"). The core rule is `filter.rs::cursor_after_mark`'s
  existing `auto_advance` parameter, now fed from the setting.
- R7. **UI › Selection highlight** (`ui.selection_wash`, integer
  percent 0–50, default 25; applies instantly). Drives
  `selection-wash-opacity` through `state.rs`'s single clamped write
  site; 0 is legal (the accent outline still shows a selection). The
  note says it is grid-only and that above ~15 % the tint can shift
  colour judgement (the spec's own caveat). A number field with a `%`
  suffix; no slider, no live preview.
- R8. **Performance › Loupe memory** (`performance.loupe_memory`,
  default `"2 GB"`; applies at the next folder open). The field accepts
  a number in GB (`2`, `2 GB`, `2GB`, `0.5 GB`; decimals allowed) or a
  percentage of total RAM (`40%`) — the user's rule (2026-10-01):
  "numbers are always expressed in GB, and percentage always total
  RAM". The file stores the string, normalised (`"2 GB"`, `"40%"`).
  The in-force budget is the parsed bytes clamped to [the engine's
  200 MB floor, total RAM when known]; beside the field the dialog
  shows the in-force value and the hint — `= 12.4 GB of 31.1 GB ≈ 83
  A1 frames` (149 MB per decoded A1 full-res frame; M11: the hint names
  the body) — and the note says the app's footprint runs 1–2 GB above
  the number and that it takes effect when a folder is next opened
  ("File › Open Folder…, the same folder is fine"). Total RAM comes from
  `/proc/meminfo`'s `MemTotal` on Linux and `GlobalMemoryStatusEx` on
  Windows — the in-tree precedent is `crates/fastcull-core/src/budget.rs`
  on the archived `screen-rung` branch (`total_ram()`; `sysinfo` was
  refused on 2026-09-26, no new crate); when total RAM is unknown a
  percentage falls back to the default and the hint says so.
  `LoupeEngine::start` receives the in-force bytes (`session.rs:154`).
- R9. **Performance › Thumbnail cache cap** (`performance.cache_cap`,
  `"<n> GB"`, default `"2 GB"`, a sensible floor the senior developer
  names; enforced at the next start). `cache::default_cache_path`
  enforces the file's cap instead of the constant, so the app and the
  CLI honour one number (the "never drift onto different caches" rule
  in `cache.rs`). The note says "enforced when FastCull next starts".
- R10. **Performance › Read workers** (`performance.max_readers`,
  default 0 = adaptive; applies at the next folder open). An
  **Adaptive (recommended)** checkbox and a **Limit** number field,
  enabled when the checkbox is off; a limit N has exactly
  `FASTCULL_MAX_READERS=N`'s meaning (N ≤ 4 pins exactly N readers, N
  > 4 is the ceiling above the floor of 4), and the note carries that
  rule. The environment override shows read-only per R5. Core resolves
  `(env, setting) → the pool's configuration` in one pure, tested
  function that `pipeline.rs` calls where it reads the variable today.
- R11. **Performance › Clear cache.** A row reading "Thumbnail cache:
  22.3 MB in `~/.cache/fastcull/previews.db`" — the size `du` would
  show (the database plus its `-wal` and `-shm` files; the human-bytes
  tiers of brief 006) and the real path — and a **Clear** button. No
  confirmation (the cache is regenerable). Clearing deletes every row
  and VACUUMs through the LIVE connection — it never unlinks the file
  (catalog-cache.md's SIGBUS rule) — on a worker thread, never the UI
  thread; when done the readout is re-measured from disk and shows the
  true size (an empty database's few tens of KB, never a claimed
  "0 B"); the open session keeps its already-painted thumbs. The note
  says "the next open of any folder re-reads its files once". With
  `FASTCULL_NO_CACHE` the row says the cache is off and the button is
  disabled.
- R12. **The Failed badge's tooltip** (ui-grid.md's promise; the user,
  2026-10-01). Hovering the Failed badge shows a small tooltip with the
  failure reason (`Failed(reason)`'s string), in the house style; it is
  the app's first tooltip and is built as a reusable component, not a
  one-off. The keyboard path: when the cursor stands on a failed frame
  the status line carries the reason (`presenter.rs` already tracks
  `cursor_failed`; the senior developer checks what the status line
  says today and the plan adds the reason if it is absent).
- R13. **The harness.** A `settings` drive token (the modal toggle, like
  `about`/`shortcuts`), layout marks for the card, the tab strip and
  each field so driven tests click by NAME (issue #70), `key:ctrl+,`
  (the senior developer verifies the comma survives `key:ctrl+<k>`'s
  literal-text path), and whatever `QEDUMP` fields the plan needs
  (the settings in force). test-harness.md records them.
- R14. **Spec, first.** A new `specs/modules/settings.md` in the
  brief-007 shape (Purpose, Behaviour, Contracts, Acceptance criteria,
  History) owns: the file and its keys, parsing and clamping, the
  read and write moments, the broken-file rule, hermeticity, the
  environment precedence rule, the dialog's rules (R1–R3) and each
  setting's row (R6–R11) — each rule stated once, every other spec
  pointing at it in one sentence. Sentences that become false and
  change in the same commit: ui-grid.md (the Settings… placeholder; the
  keyboard map and the card gain `Ctrl+,`; "until then it is always
  on"; "destined to become a setting"; the Failed badge's tooltip box);
  catalog-cache.md ("no VACUUM" gains the Clear cache exception; the
  cap reads the setting); raw-pipeline.md ("configurable" now points at
  settings.md; the override paragraph points at the setting);
  test-harness.md (`FASTCULL_NO_CONFIG` covers `settings.toml`; the new
  tokens and marks); 01-architecture.md (the modules table; the data
  flow's startup read); ADR 0005 if the senior developer so judges.
  Docs in the same commit as the behaviour: `docs/settings.md` (new),
  the page map line in CLAUDE.md, `docs/culling.md` (auto-advance,
  `Ctrl+,`), `docs/faq.md` (the cache's location and Clear cache; the
  environment variables beside their settings). `CHANGELOG.md` is
  written at release time and is untouched.
- R15. **Tests.** Core, beside the code: defaults; round-trip; unknown
  keys and the user's comments preserved across a write; malformed →
  defaults plus the error, and the file byte-identical after a read;
  the first write moves a broken file aside and the new file parses;
  out-of-range clamps (wash 51 → 50, −1 → 0; memory below the floor →
  the floor; a percentage above 100); the memory-string parser (GB with
  and without the unit, decimals, `%`, garbage → the default, a
  percentage with unknown RAM → the default); the environment
  precedence (variable set → wins; unparsable → ignored; unset → the
  file); the readers resolution function; the cache size measurement
  counts `-wal` and `-shm`; Clear cache leaves the file present, the
  table empty, the file shrunk and the connection usable; the
  auto-advance-off cursor rule (filter.rs already pins it, fed from the
  setting now); `FASTCULL_NO_CONFIG` → no path. App, driven:
  `key:ctrl+,` opens the dialog and `Esc` closes it with the keyboard
  back on the grid (`key:+` zooms); containment (`Y` under it marks
  nothing; the dump proves it); the strip switches by key; a committed
  change is written (the plan says how a driven run gets a sandboxed
  config dir — see open question OQ1); the Failed badge's tooltip
  appears on a driven hover (or is review-verified, if a hover cannot
  be asserted — stated, not faked); the shortcuts parity test passes
  with the new row and the card fits at 1000×700. A mutant for every
  guard: the precedence, the clamp, the broken-file no-overwrite, the
  no-unlink. Existing tests at risk (the plan names each): the
  auto-advance and collapse tests in `filter.rs`/`selection.rs`; the
  shortcuts card's row count and fits-whole tests; any test reading the
  "Settings… (soon)" item; the About/shortcuts containment and the
  Esc-over-stacked-modals tests (a third modal of this kind); the
  `state.rs` wash clamp tests; `pipeline.rs`'s `FASTCULL_MAX_READERS`
  tests; `cache.rs`'s `default_cache_path` cap.

## Acceptance criteria (they also land in settings.md and the touched specs)

- AC1. File › Settings… and `Ctrl+,` open the dialog; `Esc` and Close
  close it; the keyboard returns to the grid; a scrim click does not
  close it.
- AC2. Under the dialog every grid key is swallowed (`Y`/`N` mark
  nothing; `Ctrl+O`, `Ctrl+E`, `Ctrl+Shift+E` inert); About and the
  shortcuts card over it close topmost-first; the menu bar stays live.
- AC3. Three tabs in order, switched by `Left`/`Right` on the strip and
  by `Ctrl+Tab`/`Ctrl+Shift+Tab`; digits never switch tabs; every
  field carries its inline note with its default and its effect moment;
  Reset per tab.
- AC4. Every commit writes `settings.toml` at once, preserving unknown
  keys and comments; the field then shows the value in force; `Esc`
  discards an uncommitted field and closes.
- AC5. A malformed file yields the defaults, a status-line and a stderr
  warning naming the file and the error, and is never overwritten in
  place; the first write moves it aside under a reported name.
- AC6. `FASTCULL_NO_CONFIG=1` makes `settings.toml` unreachable for
  load and save; no test touches the real file.
- AC7. With `FASTCULL_MAX_READERS` set the field is read-only with the
  environment's value and its note; unset, the file's value governs the
  pool; an unparsable value is ignored.
- AC8. Auto-advance off: `Y`/`N` keep the cursor (the filter exception
  as `U`) and leave the selection alone; on: as today.
- AC9. The wash applies instantly at the committed percentage, 0–50
  inclusive.
- AC10. Loupe memory accepts GB and a percentage of total RAM, shows
  the in-force value and the A1-frames hint, clamps to the floor, and
  the engine starts with that budget at the next folder open; with
  unknown RAM a percentage falls back to the default and says so.
- AC11. The file's cache cap is enforced at the next start, app and
  CLI alike.
- AC12. Clear cache: the readout is the db + `-wal` + `-shm` size with
  the path; clearing empties the table and shrinks the file through the
  live connection, off the UI thread; the readout is re-measured; the
  open session keeps its thumbs.
- AC13. The Failed badge shows the reason on hover; the status line
  carries it when the cursor stands on the frame.
- AC14. The shortcuts card lists `Ctrl+,` and still fits whole at
  1000×700.
- AC15. `docs/settings.md` exists, the page map names it, and
  `culling.md` and `faq.md` follow the behaviour.
- AC16. No perf-budget row moves.

## Persona verdicts (2026-10-01, `almost-human-user`)

The dialog found from `Ctrl+,` with `Esc`/Close and no OK/Cancel:
USEFUL ("a culling tool's settings are a first-evening-ever thing, then
a once-a-month thing"); apply-on-commit: MUST-HAVE at this size
("OK/Cancel would be in my way"); the tab strip with content-driven
height: USEFUL with three conditions (digits never switch tabs; `Esc`
in a half-typed number discards; focus continuity exactly as Copy
Picks). Auto-advance: USEFUL (off for the second-pass re-judging two
frames; the note must state the filter exception; off → `Y`/`N` treat
the selection as `U` does). Wash strength: SHRUG, set once to ~15 %
(green foliage reads cold at 25 %; 0 is legal). Loupe memory: USEFUL,
"the one Performance knob I'd touch the first evening" (8 GB on a 32 GB
machine; next-folder-open is fine because that is when it would be
set). Cache cap: SHRUG, "I will never change it". Read workers: USEFUL
on a NAS day; the ≤ 4 / > 4 dual meaning is a comprehension trap, hence
the Adaptive checkbox plus Limit field. Clear cache: USEFUL as a
troubleshooting button, no confirmation, with five conditions (the
readout is what `du` shows; Clear really frees disk; never unlink under
the live session; the note says the next open re-reads once; the path
beside the size). Hover tooltip on the fields: IN-MY-WAY as hover-only
(keyboard-first app; the shortcuts card already explains a row with a
grey line under it) — put to the user, who chose inline notes. A
uniform `FASTCULL_<KEY>` override for every setting: IN-MY-WAY for
General and UI ("every env var that exists is a hidden switch waiting
in someone's `.bashrc`") — put to the user, who chose the existing
variable only. A live wash preview: no. A "Reopen now" button: SHRUG.
Its first-evening gap outside this unit: reopening yesterday's folder
means the native picker again (no Open Recent) — flagged, not a
setting.

## Open questions (answered before step 4)

- OQ-U1 (the persona's question 1, inline notes vs. a hover tooltip):
  the user, 2026-10-01 — inline one-line notes.
- OQ-U2 (the persona's question 2, the scope of the precedence rule):
  the user — "let's do it for the existing variables that make sense be
  a settings; in the future, new settings can be added and we need to
  see if would make any sense to set them as env variables" → only
  `FASTCULL_MAX_READERS` governs a setting today; no new variables.
- OQ-U3 (the persona's question 3, the loupe memory's default and
  unit): the user — "user should be able to specify memory as a
  number: 2GB or as a percentage 40%. Numbers are always expressed in
  GB, and percentage always total RAM." The default stays 2 GB (the
  user did not move it; Manager).
- OQ-U4 (the Failed badge's tooltip, M10): the user — build it in this
  unit.
- OQ1 (for the senior developer's plan): how a DRIVEN test proves that a
  commit writes the file, given the harness sets `FASTCULL_NO_CONFIG=1`
  unconditionally. Options: (a) a harness-only override of the config
  directory for that one test (test plumbing in test-harness.md's
  family, announced on stderr like `FASTCULL_KITCHEN_COOK_MS` — NOT a
  setting and NOT in conflict with "no new environment variable", which
  is about settings), or (b) the write proven in core only and the
  driven test proving the in-memory apply. The Manager's
  recommendation: (a), because AC4 is about the file.

## Decisions log

- D1 (2026-10-01, the user): scope — the dialog, its storage and the
  four spec-promised settings with all three Performance knobs; #15
  and #24 are the next units.
- D2 (2026-10-01, Manager): TOML is the INI — `[group]` and `key =
  value` — because the project already parses TOML (`ui.toml`,
  `templates.toml`) and `toml_edit` is already in the tree; a literal
  `.ini` would add a parser for the same shape.
- D3 (2026-10-01, Manager): `FASTCULL_NO_CACHE` does not become a
  setting — it is a diagnostic switch (app-only; the CLI has
  `--no-cache`), and Clear cache plus the cap cover the user need.
- D4 (2026-10-01, Manager, M2): `Ctrl+,` is the chord (GNOME, VS Code,
  macOS; free in the map); a scrim click does not close a form; `Esc`
  discards an uncommitted field; Reset per tab, each note naming its
  default; digits never switch tabs; `Ctrl+Tab`/`Ctrl+Shift+Tab` and
  `Left`/`Right` on the strip switch tabs (no `Ctrl+PgUp`/`PgDn`:
  fewer bindings, nothing reserved spent).
- D5 (2026-10-01, Manager, on the persona's trust rules): a file that
  fails to parse is never overwritten in place and is moved aside only
  at the first write, under a reported name — a hand-edited config is
  the user's data; Clear cache vacuums through the live connection and
  never unlinks; the readout shows the measured size, never "0 B".
- D6 (2026-10-01, Manager, M2): memory in GB with one decimal, the
  machine's RAM and the "≈ N A1 frames" hint beside it; the cache cap
  in GB only (a percentage of a disk is not what the user described);
  `max_readers = 0` means adaptive.
- D7 (2026-10-01, Manager, M2): with auto-advance off, `Y`/`N` leave
  the selection alone exactly as `U` does — the persona's answer,
  consistent with the file-manager rule (no cursor move, no collapse).
- D8 (2026-10-01, Manager): the Failed badge's tooltip is the app's
  first tooltip and a reusable component; its keyboard path is the
  status line.
- D9 (2026-10-01, Manager): Open Recent / reopen the last folder — the
  persona's first-evening gap — is out of scope and is put to the user
  in the morning report as a candidate for its own unit.
- D10 (2026-10-01, Manager, overnight rule): the user is asleep; a
  question only the user can answer is taken on the most reversible
  option, recorded here as PROVISIONAL with its date, and relayed in
  the morning; the PR merges only when no provisional ruling is
  outstanding.
- D11 (2026-10-01, Manager, on the senior developer's plan OQ-A):
  ONE config-dir resolver, `settings::config_dir()`, names the directory
  for `settings.toml`, `ui.toml` and `templates.toml`; `FASTCULL_NO_CONFIG`
  hides all three and `FASTCULL_CONFIG_DIR=<dir>` redirects all three.
  The resolver closes a measured hermeticity hole — every driven run had
  read the user's real `templates.toml` — and `FASTCULL_CONFIG_DIR` is
  harness plumbing in test-harness.md's family (announced on stderr), not
  a setting, so it does not touch the user's "no new environment
  variable" ruling, which is about settings. Reported to the user in the
  morning as a decision taken.
- D12 (2026-10-01, Manager, M2, plan OQ-B): the Settings dialog and the
  two export dialogs never stack — File › Settings… is greyed while Copy
  Picks or Export Frames as Video is up, and those two while Settings is
  up; About and the shortcuts card still open over Settings. One fewer
  stacking order to get wrong; nothing a user can do in Settings bears on
  a copy in progress.
- D13 (2026-10-01, Manager, plan OQ-C — a recorded deferral, not a
  deferred criterion): no `FASTCULL_CACHE_DIR` in this unit. AC12's
  mechanism — delete, VACUUM, the WAL truncated, the file never unlinked,
  the size counting `-wal` and `-shm` — is pinned in core; the worker
  thread, the `Clearing…` state and the re-measured readout are
  review-verified and settings.md's AC12 box says so. If QE's run or a
  later unit shows the bridge half needs a driven proof, the variable is
  added then.
- D14 (2026-10-01, Manager, M2, the senior developer's user question 2):
  the shortcuts card swaps the FILE MENU and MOUSE columns so the `Ctrl+,`
  row lands in the shorter column — measured on the development seat:
  the right column was 19 px taller, a fifth row there would have grown
  the card by 23 px and left 1.5 px of slack on this font (a coin flip on
  DejaVu/Segoe), the swap grows it by 4 px with 31 px of slack. Every row
  and every section is kept.
- D15 (2026-10-01, Manager, the senior developer's user question 3): the
  thumbnail cache cap's floor is 256 MB (one large shoot's thumbnails),
  and the cap is enforced at every folder open rather than "at the next
  start" — which is what the code path has done since M5 and sooner than
  the brief's R9 said; R9 and AC11 read "next folder open" from here on.
- D16 (2026-10-01, Manager, M3, plan OQ-D and OQ-E): two pre-existing
  gaps the senior developer measured become issues after the unit, not
  work in it — `default_cache_path` evicts on the UI thread at folder
  open, so lowering the cap by gigabytes stalls the next open once; and
  the copy and export dialogs' key scopes let Slint's window-level Tab
  navigation walk into surfaces hidden behind the scrim. The Settings
  dialog's scope handles Tab itself (settings.md) and is not affected.
- D17 (2026-10-01, Manager, plan OQ-F): confirmed — the settings write
  runs on the UI thread like `ui.toml`'s (ADR 0005; 01-architecture.md's
  "never blocks on I/O" corrected to name the two ~1 KB exceptions), the
  hint example is `≈ 89 A1 frames` for 12.4 GB (binary GB, the formatter's
  unit), and "next folder open" replaces "next start" for the cache cap.
- D18 (2026-10-01, Manager, M2, on the senior developer's review
  question): while a number is half-typed in Settings and About or the
  shortcuts card is opened from the menu over it, the keyboard leaving
  the field is a click-away and the number is applied — one rule for
  every cover, the rule every dialog over a field already follows; `Esc`
  before opening Help is the discard. No second rule for the Settings
  fields. Relayed to the user in the morning as a decision taken.
- D19 (2026-10-01, Manager, the circuit breaker on review finding F3):
  the reviewer called the dialog scope's `settings dialog` focus handler
  dormant and offered dropping it; the developer measured `focus:
  settings dialog gained` on a scrim click (a Slint `FocusScope` takes
  focus on click by default), kept the handler, and corrected the
  test-harness.md sentence to name when each mark fires. Ruled for the
  developer on the measurement; the re-review repeats the probe and
  re-raises with the trace if it does not reproduce. Not a question for
  the user: the spec answers it (a mark names when it fires).
- D20 (2026-10-01, QE round 1 — defect D1 and spec correction D20; the
  senior developer's test-integrity review, TP1): a loupe memory below the
  ±PREFETCH window — five decoded A1 frames, ~746 MB — made the engine
  re-decode window members for as long as the cursor rested at 1:1: each
  landing evicted a ring member, and the app's re-focus on every landing
  (`presenter::refresh`) queued it again. QE's measurement on 4856ce8,
  release, 0.5 GB, 60 frames, idle on frame 10 at 1:1: 101 full-res
  decodes in 15 idle seconds, two neighbours alternating every ~0.15 s,
  CPU user 50.9 s over 25.3 s wall; at the 2 GB default 0 decodes and
  6.2 s; 0.75 GB and 1 GB went quiet after one decode each; 0.2 GB on a
  three-file folder gave 42 full-res decodes in 24 s on frame 0. The
  engine logic predates the unit — the fixed 2 GiB hid it. Fixed in the
  engine (raw-pipeline.md, the ring's budget rule: a member evicted under
  a settled focus waits for the next step), not by raising the floor to
  the window, which is an A1 number — a ~100 MP body's window is ~1.5 GB
  (M11). QE's correction D20 (retire the `0.5 GB` example until a fix
  lands) is answered by the fix.
- D21 (2026-10-01, QE round 1 — defect D6 and spec correction D21; TP6):
  settings.md promised that the user's comments and unknown entries
  "survive byte-for-byte", and a CRLF file did not — toml_edit writes LF and
  drops a UTF-8 byte-order mark. QE's 17-line CRLF fixture (a Unicode top
  comment, comments above keys, trailing comments, an unknown key, an
  unknown table, an array) came back 649 bytes with 0 CR after one commit,
  byte-identical to the LF fixture's output; a BOM-prefixed file read
  correctly and lost its mark at the first write. The spec and D5 decide
  it — a hand-edited config is the user's data — so the WRITER changes:
  it restores the file's CRLF line ends and its mark (QE's correction D21,
  to weaken the sentence instead, is answered by the fix). Notepad saves
  CRLF, which made this a Windows hazard.
- D22 (2026-10-01, QE round 1 — defect D5 and spec correction D22; TP5):
  AC3's "every field carries its note" cited
  `settings_tabs_switch_by_keys_and_never_by_digits`, which reads no note,
  and no dump field or mark exposed the notes — deleting the six
  `set_settings_note_*` calls would have left the suite green (QE, by
  inspection). The inline notes were the user's own choice over tooltips
  (OQ-U1). Each note Text now reports what it shows and where it is laid
  out, and `every_settings_note_is_the_core_text` compares every note with
  core's sentence and checks, at the shutter, that the loupe memory note's
  rectangle holds drawn text (its luma variance against the bare card's).
  The dump-field variant QE also proposed was refused in the integrity
  review.
- D23 (2026-10-01, QE round 1 — defect D2 and spec correction D23; TP2):
  AC10's driven test waited on `loupe engine started budget <bytes>`, a
  mark the app built from its own local — mutant M-i (the engine started
  with `DEFAULT_BUDGET_BYTES`, the mark untouched) stayed green. The mark
  now reads the engine's adopted figure, `LoupeEngine::budget()`.
- D24 (2026-10-01, QE round 1 — spec correction D24; the senior
  developer's test-integrity review, TP11, "the Manager's call under D13,
  recommended": built under the Manager's ruling 8, the stage proceeding on
  its best reading): AC11 and AC12 said a driven run cannot have a cache,
  and on Linux it can, with no new variable — `HOME` and `XDG_CACHE_HOME`
  pointed into a scratch dir redirect the `directories` crate's default
  cache. QE measured there: the CLI at cap 0.25 GB evicted a seeded
  300.1 MiB to 236.1 MiB, and with no file the 2 GB default kept 536 MiB;
  the app at a folder open went 300 → 236 MiB; the driven Clear read
  303.5 MB → `Clearing…` → 48.0 KB with the same inode and 0 rows, and the
  session's thumbs stayed painted. "Cannot" holds on Windows only (the
  known-folder lookup). D13 stands — no `FASTCULL_CACHE_DIR` — and the
  app's half of AC11 and AC12 is now driven on Linux by
  `the_cache_cap_and_clear_cache_reach_the_default_cache`.
- D25 (2026-10-01, QE round 1 — defect D10 and spec correction D25; the
  senior developer's test-integrity review, T13): the Thumbnail cache
  cap's note promised "the most the thumbnail cache may keep on disk", and
  the readout beside it said otherwise. QE ran the app sandboxed with
  `cache_cap = "0.1"` (0.25 GB) over a seeded cache: at folder open the
  stored thumbnails were evicted to 236 MiB while the dialog's own readout
  (the `du` figure) said `Thumbnail cache: 303.5 MB` — SQLite reuses the
  pages an eviction frees and gives them back only at a VACUUM, which
  catalog-cache.md allows at Clear alone. Both behaviours are spec'd; the
  note was the false one. Reworded in its one home, `Key::CacheCap::note()`
  — the cap bounds the thumbnails held, and the file shrinks only when you
  Clear it — and settings.md and docs/settings.md follow.
- D26 (2026-10-01, QE round 2 — defect D26; the senior developer's
  test-integrity review, TP-E; the recommended precedence text applied under
  the Manager's ruling 8 "proceed on the best reading", for the Manager to
  confirm under M2): after a settings file had been moved aside, a hand edit
  that broke the fresh file was masked. QE's repro on 6f20679: the open's
  re-read failed (`TOML parse error at line 2, column 16`), the defaults took
  over (wash 12 → 25), and the notice and the status line kept reading
  `settings.toml rewritten — the file that would not read is
  settings.toml.broken`. settings.md stated both "… until the file reads
  again or is moved aside" and "… name where it went … for the rest of the
  session" without saying which wins; the bridge checked the move first. The
  newer read error now wins on both lines and names the earlier aside
  (` — the earlier one is settings.toml.broken`); the next write moves the
  new file aside as `.broken.1` and says `rewritten` again.
- D27 (2026-10-01, QE round 2 — defect D27; the senior developer's
  test-integrity review, TP-A to TP-D): four shipped Settings promises had
  no guard. QE removed each with a mutant in a worktree of 6f20679 and all
  25 settings-related driven tests stayed green: G1, session.rs passing
  `None` to `Pipeline::start` (the app's AC7 test read `readers=` from the
  bridge's own resolution — D23's shape again); G2, the dialog always
  reopening on General; G3, `|| root.shortcuts-visible` taken out of the
  dialog's capture arm, so Esc closed Settings UNDER the shortcuts card
  (settings.md's AC2 had dropped the brief's shortcuts-card half); W, the
  Settings scrim's `scroll-event` arm removed (three wheels scrolled the
  grid to -1800). Each now has a test that its mutant turns red, and the
  pool's bounds are read back from the pool (`Pipeline::read_pool_bounds()`,
  traced at every folder open). One measured adaptation of TP-A's approved
  script: its `wait:` could not be satisfied on either launch — a
  `--synthetic` session starts no pipeline, and a launch folder's mark comes
  before the harness registers its waits (both probed: exit 1 after 30 s) —
  so each run opens an empty folder with `open:` before it waits. TP-F,
  recommended for this round by the integrity review, landed with it: the
  CLI had no test at all, and the cache cap is the one knob both binaries
  share — `settings_cap::the_cli_honours_the_files_cache_cap_and_says_where_it_came_from`
  drives the CLI's cap and the four wordings of its `cache:` line on Linux,
  through the sandboxed default cache; Windows stays review-verified.
- D28 (2026-10-01, QE round 2 — defect D28; PROVISIONAL under D10, put to
  the user): a write emits every known key with its value in force
  (settings.md, "Writing"), so two FastCull instances sharing one config
  dir take back each other's committed settings. QE: instance A committed
  Selection highlight 15; 2.5 s later instance B, whose dialog had been
  opened before A's commit, clicked Auto-advance off and wrote
  `selection_wash = 25` with it; A reopened its dialog, re-read the file,
  and its wash dropped to 25 with no message anywhere. Same root under the
  TP10 ruling: a hand fix made while the dialog is open loses its values
  for the known keys (QE measured `selection_wash = 40 # my choice` written
  back as `25 # my choice`, no `.broken` kept, the notice empty). The code
  follows the spec's text; the spec is silent on two instances and on a
  hand edit made while the dialog is open. Taken on the most reversible
  option — nothing changes in this unit — and put to the user: keep
  writing every key, or write only the keys a commit or a Reset changes
  (creating a missing key with its note), which QE and the developer
  recommend.
- D29 (2026-10-01, QE round 2 — spec correction D29): settings.md's AC6
  said "the two that write set `FASTCULL_CONFIG_DIR`"; seven driven tests
  set it on 6f20679, several of them read-only, and nine tests set it
  after round 2 (eight driven, plus the CLI's). AC6 now reads "every test
  that reads or writes a settings file", the senior developer's open nit
  from the review.
- D30 (2026-10-01, QE round 2 — spec correction D30): raw-pipeline.md's
  ring rule gave the ±PREFETCH window as ~746 MB — 5 × 149,299,200 B =
  746,496,000 B, decimal megabytes — while the app's byte formatter and
  settings.md's GB are binary: 711.9 MB, 0.70 GB. The rule reads "~712 MB —
  0.7 GB in the app's binary units", and docs/settings.md's "below about
  0.75 GB" reads 0.7 GB so the two agree. D20 above keeps its decimal
  figure as the record of round 1.
- D31 (2026-10-01, QE round 2 — spec correction D31, "the Manager's
  call"; taken on the senior developer's recommendation under the Manager's
  ruling 8, for the Manager to confirm): settings.md "Reading" promised one
  stderr line for a file that will not parse, and only startup printed it.
  QE measured a parse failure found at a dialog re-read printing nothing on
  stderr; the D26 strand's log on 6f20679 shows it too — the startup line,
  then two failed re-reads with trace marks and no stderr line. The code
  now matches the agreed sentence rather than the sentence being weakened
  to "at startup" (the alternative QE offered): `Loaded::stderr_line()` is
  the wording's one home, printed by `load_default` and by the bridge's
  re-read at every dialog open.
- D32 (2026-10-01, QE round 2 — spec correction D32; evidence only, never
  Behaviour): both cards at 1000×700 in their tallest states (the Settings
  card with the environment note and a parse error on the notice line),
  Settings card / shortcuts card, in px: Noto Sans 541 (slack 47) / 572
  (slack 31); Liberation Sans 514 / 511; Comfortaa 521 / 536; Adwaita Mono
  582 (slack 26) / clamped 594; Noto Sans Mono clamped 594, slack exactly
  20 / clamped 594. Round 1 measured the Settings card at 587 under Noto
  Sans Mono; D25's cache cap note is one line longer on a mono face, and
  under that face `the_settings_card_fits_its_smallest_window_in_its_tallest_state`
  would be red (it is not a CI face). The risk it carries: the next row
  (#15, #24) will clamp the Settings card on wider faces. The fit test's
  20 px stays as it is — the plan forbids loosening it or pinning a height
  — and the next unit budgets for a taller card, not a wider margin.
- D33 (2026-10-01, QE round 3 — defect D33, major; the senior developer's
  test-integrity review, T1; a behaviour the spec was silent on, ruled under
  M2 as the integrity review recommended and the Manager's hand-off carried
  it): the dialog's Tab ring gave a number field the keyboard with
  `focus()` from code, which Slint does not count as Tab navigation, so
  nothing was selected and a typed number went in beside the value shown.
  QE's repro on 13a904e, keyboard only: Tab to Loupe memory (`2 GB`), 8,
  Enter committed `82 GB`, held at all 31.1 GB of the seat's RAM; Tab to
  Selection highlight (25), 1, 0, Enter committed `1025`, clamped to 50 % —
  each written to settings.toml at once. Every shipped Settings test had
  pressed Ctrl+A before typing. The ring now selects the field it lands on,
  as Slint's own Tab navigation and the IPTC panel do, and
  `a_number_typed_after_tab_replaces_the_value_in_the_field` types the way a
  user does — Tab in, no Ctrl+A.
- Directive candidate (2026-10-01): M9's cleanup command `cargo clean
  -p …` cleans the dev profile only — a `screen-rung` release binary
  from 2026-09-29 survived it and the persona ran it by mistake; the
  command needs `--release` as well (3.0 GiB freed when it ran).
