# Module spec: settings (`settings.rs`, the Settings dialog)

## Purpose

One settings file per user, `settings.toml`, read by the app and the CLI
alike, and the dialog that edits it: File › Settings… or `Ctrl+,`, three
tabs, every setting visible on the screen with a one-line note, applied on
commit, written at once. The environment wins over the file wherever a
variable already governs a knob. Everything about the file, its values and
their precedence is `fastcull-core`'s; the app binds (brief 008, 2026-10-01).

## Behaviour

### The file

- `settings.toml` lives in the config dir — the `directories` crate's
  `config_dir()` for `("org", "fastcull", "fastcull")`: `~/.config/fastcull/`
  on Linux, `%APPDATA%\fastcull\fastcull\config\` on Windows — beside
  `ui.toml` (the remembered copy and video destinations, fileops.md) and
  `templates.toml` (iptc-templates.md), which are untouched. One resolver,
  `settings::config_dir()`, names that directory for all three files
  (user decision 2026-10-01: the INI the user described, kept as TOML
  because the project already parses TOML — brief 008 D2).
- TOML is the INI: a named table per tab and a named key per setting. Every
  key the file can hold, with its type, range and default:

  ```toml
  [general]
  auto_advance = true          # bool; default true

  [ui]
  selection_wash = 25          # integer percent, 0–50; default 25

  [performance]
  loupe_memory = "2 GB"        # "<n> GB" or "<n>%" of total RAM; default "2 GB"
  cache_cap = "2 GB"           # "<n> GB", never below 0.25 GB; default "2 GB"
  max_readers = 0              # integer; 0 = adaptive (default), N ≥ 1 = FASTCULL_MAX_READERS=N
  ```

  GB here is the app's GB — 1,073,741,824 bytes, the unit the byte
  formatter of fileops.md prints — so `"2 GB"` is exactly the engine's
  2 GiB default (`loupe::DEFAULT_BUDGET_BYTES`) and the cache's
  (`cache::DEFAULT_CAP_BYTES`); a test pins the three to one number.
- A memory string is a decimal number, optional spaces, then `GB` (any
  case) or nothing for GB, or `%` for a share of the machine's total RAM:
  `2`, `2 GB`, `2GB`, `0.5 GB`, `40%`, `40 %` all parse; anything else —
  an empty string, `abc`, a negative number, a `TB`, a figure beyond what
  a 64-bit float holds (309 digits or more; QE 2026-10-01, D7) — is
  garbage and reads as the key's default. The file stores the normalised form, `"2 GB"`,
  `"0.5 GB"`, `"40%"`: the number as typed, one space, the unit (user
  decision 2026-10-01: "numbers are always expressed in GB, and percentage
  always total RAM"). `cache_cap` takes the GB form only; a `%` there is
  garbage. A percentage is a whole number, as the contract's
  `Percent(u32)` says: `40.5%` is garbage, not rounded (developer
  2026-10-01, brief 008 commit A — the spec was silent; Manager-accepted
  under M2, senior-developer review 2026-10-01).

### Reading

- Core reads the file at startup in both binaries (`settings::load_default`,
  before any timed region) and the app reads it again each time the dialog
  opens; what the file says is applied at that open, so a hand edit made
  mid-session takes effect when the dialog is next opened (the Performance
  knobs still wait for their own moment, below). A missing file is the
  defaults and no error.
- While a write has failed and none has succeeded since, an open does NOT
  re-read the file: the commits since the failure live only in memory and
  a re-read would silently take them back, so that open keeps what is in
  force and the notice keeps naming the write error (developer 2026-10-01,
  brief 008 commit B; Manager-accepted, senior-developer review F2).
- A value of the wrong type (`auto_advance = "yes"`, `selection_wash =
  "25"`) reads as THAT key's default; an out-of-range value clamps —
  `selection_wash` 51 → 50 and −1 → 0, `max_readers` below 0 → 0, a
  memory share above 100 % → 100 % — and the dialog shows the clamped
  value. An unknown key or table is left alone, in memory and in the file.
- The values in force are derived from the file at read time, so a clamp
  is never written back by the read: `loupe_memory` bytes are the parsed
  bytes clamped to [`loupe::BUDGET_FLOOR_BYTES` (200 MB), total RAM when it
  is known]; a percentage with unknown total RAM is the default 2 GB and
  the dialog's hint says so; `cache_cap` bytes clamp to [256 MB, ∞) — 256 MB
  holds one large shoot's thumbnails (30–60 KB each), and below it the
  "second open is instant" promise of catalog-cache.md could not survive a
  single big folder (senior-developer plan 2026-10-01). The cap clamps IN
  THE MODEL — below the floor it reads as 0.25 GB — so the field, and the
  file at its next write, carry the value in force, an absolute figure
  with no hint beside it; the loupe memory instead keeps the figure as
  typed, because it is relative to the machine (a share, or a GB figure
  above this machine's RAM), and its hint says how it was clamped
  (developer 2026-10-01, brief 008 commit A — the spec was silent;
  Manager-accepted under M2, senior-developer review 2026-10-01).
- A loupe budget below the prefetch window is legal: the engine holds what
  fits and re-decodes the rest on a step, never while the user is idle
  (raw-pipeline.md, the ring's budget rule; QE 2026-10-01, D1).
- **A file that fails to parse is never overwritten in place** (brief 008
  D5: a hand-edited config is the user's data). The defaults are in
  force; every read that finds the file so — at startup in both binaries,
  and in the app at every dialog open that re-reads it — prints one stderr
  line naming the file and the error's first line (`fastcull: <path> could
  not be read (<error>) — defaults in force`; QE 2026-10-01, D31: the
  dialog's re-read had printed nothing), and the app's status line carries
  `⚠ settings.toml could not be read (defaults in force)` until the file
  reads again or is moved aside, or a write fails (below: a write error is
  newer, and wins). The dialog's notice line shows the whole error.

### Writing

- The file is written on every commit of the dialog and on every Reset,
  and never otherwise, and a missing file stays missing until the first
  CHANGE: a commit or a Reset that changes nothing writes only a file that
  already exists (developer 2026-10-01, brief 008 commit B;
  Manager-accepted, senior-developer review F2). The write is a
  read-modify-write through `toml_edit`: every known key is emitted under
  its table with the setting's own value (below; corrected 2026-10-02 from
  "its in-force value", which for a key the environment governs is the
  environment's); a key the
  write CREATES is preceded by `#` comment lines carrying the field's
  note (`Key::note()`, wrapped at 78 columns over as many lines as it
  needs), so the file documents itself for hand editing — except a key
  created inside an INLINE table the user wrote by hand (`performance = {
  cache_cap = "2 GB" }`), which TOML cannot comment: the key is added
  inside the braces with no note, and the braces stay the user's shape
  (brief 010, 2026-10-03, on QE 2026-10-02 D47 — this sentence had
  promised a note on every created key; measured on toml_edit 0.22.27 the
  same day: expanding the inline table into a `[performance]` table would
  move the group to the end of the file, and `into_table()` wrote an
  unparsable header, so under D5 the shape stays and the note goes); a key
  created under a dotted key (`performance.cache_cap = "2 GB"`) carries
  its note, a dotted table being a table (measured the same day); a key
  that already
  exists keeps the user's own comment above it and any comment on its
  line; unknown keys, tables and the user's other comments survive
  byte-for-byte (measured on toml_edit 0.22.27, 2026-10-01: `Table::insert`
  on an existing key drops its comment, replacing the value in place does
  not — the plan names the call), and so do the file's line ends and a
  UTF-8 byte-order mark: a CRLF file (Notepad's) is written back CRLF on
  every line, an LF file LF, a mark is kept; a file written from nothing
  is LF without a mark (QE 2026-10-01, D6; brief 008 D21: toml_edit writes
  LF and drops the mark, so the writer restores both). The comments of a
  file that holds no entries, and a comment that follows the last entry,
  stay above the first table the write appends, one blank line above its
  header — after any key the write adds to the table they followed —
  rather than below the tables; blank lines alone after the last entry
  stay at the end of the file, as they were (QE 2026-10-01, D35: toml_edit
  keeps them as the document's trailing decor and would print them last;
  corrected the same day from "creates": a table that replaces an entry
  keeps that entry's place, which may be the top of the file; corrected
  2026-10-02, the senior developer's re-review RR-F4: it said "stay where
  they were", and a key the write adds to their table comes before them;
  the blank lines, QE 2026-10-02, D44; the one blank line, RR-F3: a comment
  with no final newline had none, one ending in a blank line had two). A
  table where one of the five keys belongs, or an array of tables or a
  plain value where a tab's table belongs (`[performance.loupe_memory]`,
  `[[general]]`, `general = 5`), is replaced by the key or the table, in
  its place — the shape is not one the file can hold beside ours — and the
  write says nothing about it: its contents go; the comment above its
  header stays above the key or the table that replaces it, and a comment
  on its header's line (on its own line, for a plain value) stays on the
  replacing line (QE 2026-10-01, D35; brief 008 D40, QE 2026-10-02, D42: the
  comment above a replaced sub-table was deleted with it, though the one
  above a replaced `[[general]]` was kept). An array of tables with
  SEVERAL elements keeps every element's comments: the comments above
  each element, in the elements' order, and a later element's header-line
  comment as a line of its own after that element's comments, all above
  the key or the table that replaces the array, whose own header line
  keeps the first element's header-line comment (brief 010, 2026-10-03;
  corrected from a first-element-only carry — QE 2026-10-02, D48: a
  two-element `[[general]]` kept only its first header's comments, the
  second's went with the table).
- Every save writes EVERY known key with the value the saving window
  holds, so a second FastCull window, or a hand edit made while the
  dialog is open, loses to the last save (the user, 2026-10-02, brief 008
  D42 — option A of QE's D28, where a window opened before another's
  commit wrote its own older wash back, and a hand fix made with the
  dialog open was replaced). A key the environment governs
  (`performance.max_readers` under `FASTCULL_MAX_READERS`) is written with
  the setting's own value — the file's, or what a Reset of its tab made
  it — never the environment's: the environment reaches what is in force
  and the read-only field, never the file (the user, 2026-10-02, brief
  008 D42: "make sure that environment variables don't rewrite
  settings").
- A write that finds the file unparsable AT THAT MOMENT moves it aside
  before writing a fresh one, whatever the last read said — the first
  write after a failed read, or a hand edit that broke the file
  mid-session: to `settings.toml.broken`, or `settings.toml.broken.N` (N
  from 1) when that exists, never over an existing file; no path
  overwrites a broken file in place (brief 008 D5; developer 2026-10-01,
  brief 008 commit A, Manager-accepted, senior-developer review F2). The
  status line and the dialog's notice then name where it went
  (`settings.toml rewritten — the file that would not read is
  settings.toml.broken`) for the rest of the session.
- A file the last read could not parse but that has been fixed by hand
  since, and parses by the time of the write, is merged into like any
  other — the user's comments and unknown keys kept, the values in force
  written — and the read error is answered: moving a file that reads aside
  would set the user's fix aside and name it as one that would not read
  (QE 2026-10-01, test proposal TP10; senior-developer test-integrity
  review, recommendation B).
- A write that fails (a read-only config dir, a full disk) keeps the
  commit in force in memory, prints one stderr line and puts
  `Could not write settings.toml: <error>` on the dialog's notice line and
  `⚠ settings.toml could not be written` on the status line, until a write
  succeeds; nothing is silently lost on screen. A write error is the newest
  event — no open re-reads the file while one stands — so on both lines it
  wins over a read error that is still standing, and the status line says
  `(defaults in force)` only while the read error is the newest event,
  which is exactly when it is true (brief 008 D41; QE 2026-10-02, D43: a
  commit whose move-aside had failed left the status line claiming the
  defaults were in force beside the user's value).
- Once the broken file has been moved aside, a write that fails — the
  very write that moved it, or any later one — still names where the file
  went: the notice reads `Could not write settings.toml: <error> — the file
  that would not read is settings.toml.broken` and the status line `⚠
  settings.toml could not be written — the file that would not read is
  settings.toml.broken`, until a write succeeds and it says `rewritten`
  again; after a move the next write starts a fresh file
  (senior-developer review F4, 2026-10-01). The one exception is a read
  error NEWER than the move that still stands — the fresh file broken by
  hand and re-read, and this write unable to move it aside: settings.toml
  itself is then the file that would not read, and both lines name the
  aside as the earlier one, ` — the earlier one is settings.toml.broken`,
  as the read error's own lines do (QE 2026-10-02, round 5, D46: both
  lines called the earlier aside "the file that would not read", the
  notice beside its own "the file that would not read could not be moved
  aside"). The same holds when the write fails at the move-aside with NO
  read error standing — the fresh file broken by hand while the dialog was
  open, so no open re-read it, and the config dir unwritable when the user
  commits: settings.toml is the file that would not read there too, the
  notice reads `Could not write settings.toml: the file that would not
  read could not be moved aside: <error> — the earlier one is
  settings.toml.broken` and the status line ` — ⚠ settings.toml could not
  be written — the earlier one is settings.toml.broken`; the bridge decides
  by the write error's KIND (`WriteError::MoveAside`), never by its text
  (brief 010, 2026-10-03; corrected — the developer, 2026-10-02, in issue
  #100: both lines named the earlier aside as "the file that would not
  read" beside the notice's own words that that file could not be moved
  aside).
- A read error NEWER than the move wins over `rewritten`: when the open's
  re-read fails after a file has been moved aside (a second hand edit broke
  the fresh file), the status line reads `⚠ settings.toml could not be read
  (defaults in force) — the earlier one is settings.toml.broken` and the
  dialog's notice shows the whole error followed by ` — the earlier one is
  settings.toml.broken`; the next write moves the new file aside too
  (`settings.toml.broken.1`) and both lines say `rewritten — the file that
  would not read is settings.toml.broken.1` again (QE 2026-10-01, D26: until
  then `rewritten` stayed on screen while the defaults silently took over).
- The write runs on the UI thread, like `ui.toml`'s (ADR 0005): ~1 KB on
  an explicit user commit inside a modal, never on the culling path.
- **Hermetic**: under `FASTCULL_NO_CONFIG=1` the config dir resolves to
  nothing, so `settings.toml` is neither read nor written — the dialog
  still works, in memory, and its notice line says `Not saved:
  FASTCULL_NO_CONFIG is set`. `FASTCULL_CONFIG_DIR=<dir>` redirects the
  whole config dir for a driven test and wins over `FASTCULL_NO_CONFIG`;
  it is test plumbing, owned by test-harness.md, not a setting. Core tests
  pass an explicit path and never resolve the default.

### Environment precedence

- Where an environment variable governs a setting's knob, the environment
  wins over the file; the dialog shows that field read-only with the
  environment's value and the note `Set by FASTCULL_MAX_READERS in your
  environment — unset it to change this here`. Today exactly one pair
  exists: `FASTCULL_MAX_READERS` ↔ `performance.max_readers`, with the
  semantics raw-pipeline.md records, unchanged. A variable whose value does
  not parse as an integer ≥ 1 is ignored — the file governs and the field
  stays editable — exactly what `pipeline.rs` did with `parse().ok()`. The
  variable never reaches the file ("Writing").
- No new environment variable is introduced by a setting, and a future
  setting gets a variable only by its own recorded decision (user decision
  2026-10-01: "let's do it for the existing variables that make sense";
  the persona's verdict that "every env var that exists is a hidden switch
  waiting in someone's `.bashrc`"). `FASTCULL_NO_CACHE`, `FASTCULL_TRACE`,
  `FASTCULL_DRIVE`, `FASTCULL_NO_CONFIG` and `FASTCULL_KITCHEN_COOK_MS`
  are diagnostics and harness plumbing, not settings (brief 008 D3).

### The dialog

- **Opening and closing.** File › Settings… and the chord `Ctrl+,` (the
  comma) open it; the chord lives in the main key scope beside `Ctrl+E`
  and is inert while a field or another dialog holds the keyboard. `Esc`
  and the Close button close it; a click on the scrim does not (a form,
  like Copy Picks, not a popup like About). Opening it over a focused IPTC
  or keyword field commits that field like a click-away and the dialog
  owns the keyboard (the `-1` token of ui-grid.md's focus continuity);
  closing returns the keyboard to the main key scope deterministically.
  It opens on General at launch and afterwards on the tab it was last
  closed on (senior-developer plan 2026-10-01, Manager-agreed under M2).
- **Stacking.** The dialog is modal and keyboard-contained in every state
  (issue #42's rule): under it `Y`/`N` mark nothing, `Ctrl+O`, `Ctrl+E` and
  `Ctrl+Shift+E` are inert, every grid key is swallowed; About or the
  shortcuts card over it closes topmost-first; the menu bar stays live
  (File › Quit and Open Folder… work). The dialog and the two export
  dialogs never stack: File › Settings… is greyed while Copy Picks or
  Export Frames as Video is up, and those two items are greyed while the
  dialog is up (senior-developer plan 2026-10-01: one fewer stacking order
  to get wrong; the menu bar stays live for everything else).
- **The card** is the house style — `ModalScrim`'s card: `#202028`, 1 px
  `#3a3a44`, 8 px radius, 560 px wide, its height following its TALLEST
  tab — one height per open, the same on every tab, the rules below
  (corrected 2026-10-03, brief 009: it read "its height following its
  content", the active tab's, and the card jumped by the difference
  between UI and Performance at every switch, Close under the mouse with
  it) — the scrim swallowing the wheel as every modal's does (issue #49's
  rule, ui-grid.md, whose ledger names this dialog's test) — with a tab
  strip across the top:
  **General | UI | Performance**, in that order, the active tab marked
  (brighter label, a 2 px accent underline — never a weight change: a bold
  label is a few pixels wider or narrower than its regular self, by face
  and by word, and a strip whose cells follow their labels would shift at
  every switch; brief 009, 2026-10-03, the persona indifferent to which
  mark goes as long as the labels hold still). The tabs are a LIST — one
  entry per tab on the Slint side, `settings::TABS` on the core side — so
  the next unit adds a tab by adding an entry and a body, and the body's
  controls to the dialog's keyboard ring, whose lists are kept by hand
  (`slot-count`, `slot-ok`, `focus-slot`, and `flush` for a number field):
  a control missing there is one `Tab` never reaches (senior-developer
  review F6, 2026-10-01). Under the strip is the active tab's form: one
  row per setting with a label, a control
  and a one-line note beneath them in the shortcuts card's dim style (the
  `G` row's grey line, `#8a8a96`, 11 px) that says what the setting does,
  its default and when it takes effect. The footer carries `Reset <tab>
  to defaults` — the ACTIVE tab's settings only, written at once — and
  `Close`, and a notice line that is empty unless the file could not be
  read, could not be written, is not saved, or was moved aside. The
  Performance tab in its tallest state — the environment's note on the read
  workers row, a read error's whole text on the notice line, and, with the
  cache on, the Thumbnail cache row showing a long path in full (one of
  about 90 characters, which wraps) — fits whole at 1000×700, the smallest
  supported window (ui-grid.md), measured as slack under the card, never
  pinned as a height (QE 2026-10-01, D9; the cache row, QE 2026-10-01,
  D38) — and since the card has one height, so does every tab (brief 009,
  2026-10-03).
- **The card holds still** (the user, 2026-10-03: "get its size fixed";
  brief 009, persona-validated — D1: fixed on a given machine, sized to
  its tallest tab, never a literal pixel height, which on another face is
  dead space or a scrolling settings page). At each open the card takes
  ONE height: the title row, the strip, the rule, the TALLEST of the three
  bodies' preferred heights — hidden bodies count, wrapped texts count
  (the environment's line on Read workers, a long cache path on the
  readout row, a two-line hint) — the reserved notice line and the
  footer, clamped to `window − 40 px` as before; the same height on every
  tab, so a tab switch never changes it and never moves an edge, a button
  or a label. While the dialog is open the height is a high-water mark:
  it never shrinks (a `Clearing…` row replacing a wrapped path, a hint
  that un-wraps after a second commit) and grows only when a text that
  affects it changes — a write error arriving, a notice that wraps —
  never because of a tab switch: a switch that commits a typed field
  (Apply on commit) changes the card exactly as that commit would,
  inside the switch's own event (QE 2026-10-03, D5: a `100` typed into
  Loupe memory and committed by Ctrl+Tab grew the card 506 → 507 px on
  Noto Sans, and a write error so committed 506 → 522, its top 204 →
  196 and Close 660 → 668; this sentence said "never on a tab switch"
  until then); below the 600 px window width the 560 px card
  needs, far under the supported 1000, a narrowed window re-wraps the
  texts and the card keeps that height until the next open (brief 009
  R1; the narrowed window, QE 2026-10-03, D4). The active body is laid
  out from the top under the rule; the footer — the notice line, then
  Reset and Close — sits at the card's bottom on every tab, and the slack
  between the body and the footer is empty card; Close and Reset keep one
  position across every switch, Reset's width following its label as
  before (brief 009 R2, D3: a footer floating under the rows was what the
  persona would read as broken). The notice line is RESERVED: one line
  tall, blank when there is nothing to say, so a notice appearing or
  clearing moves nothing; a notice that wraps grows the card by its extra
  lines, once, under the high-water rule (brief 009 R3). The
  environment's line on the Read workers row is reserved the same way — a
  0 px cell unless `FASTCULL_MAX_READERS` governs the row, its own gap
  above the note when it speaks — so the first layout counts it (brief
  009 R5, senior-developer plan 2026-10-03). Every text that affects the
  height is in place before the card's first frame — the bridge presents
  every field before the dialog becomes visible — so an open lays the
  card out ONCE: exactly one `settings card laid out … size` mark per
  open and none on a switch that commits nothing (QE 2026-10-03, D5;
  test-harness.md; brief 009 R5). The two
  reserved lines are permanent elements, never conditional ones, because
  Slint creates a conditional element after its parent's `init` has run:
  the height that `init` reads — the high-water mark's start and the one
  layout mark — would not count it, and the open would lay the card out
  three times (ui-grid.md, "Slint facts"; corrected 2026-10-03 at the
  test-integrity review, QE's question: it said "one frame after", but
  the child and the change handlers that report the grown height run in
  the same pass as the `init`, before any frame is painted — the growth
  was never on screen). Below the supported minimum window the body gives
  before the footer: the three bodies live in a `Flickable { interactive:
  false }` like the shortcuts card's, so the footer stays inside the
  clamped card, the body clips and scrolls on the wheel, and a tab switch
  puts the body back at its top (brief 009 D4, the senior developer's
  call, 2026-10-03).
- **Keyboard.** `Tab`/`Shift+Tab` walk the strip and the active tab's
  controls in order — the strip, the controls top to bottom, Reset, Close
  — and never leave the dialog: the dialog's own key scope handles both,
  so Slint's window-level Tab navigation can never carry the keyboard into
  a field hidden behind the scrim. `Left`/`Right` on the strip switch
  tabs; `Ctrl+Tab`/`Ctrl+Shift+Tab` switch tabs from anywhere in the
  dialog, wrapping; a tab switch puts the keyboard on the strip. **Digits
  never switch tabs** — `1`–`5` are reserved (ui-grid.md) and in a dialog
  they are digits for a number field. Opening the dialog puts the keyboard
  on the strip. `Tab` or `Shift+Tab` into a number field selects its text,
  so what is typed replaces the value shown — what Slint's own Tab
  navigation does on arrival (`TextInput` selects all only on a
  `FocusReason::TabNavigation` focus, and not on Apple targets, i-slint-core
  1.17.1 `items/text.rs:1180`; macOS is not a supported seat — corrected
  2026-10-02, re-review RR-F5: it read as universal), which the dialog's
  ring must do itself because it focuses by `focus()`; a click into a field
  places the caret, as a click does anywhere (QE 2026-10-01, D33;
  Manager-accepted under M2).
- **Apply on commit.** A checkbox applies on click or `Space`; a number
  field applies on `Enter`, on `Tab` and on click-away (focus leaving it
  while the dialog is up); `Esc` in a field discards its uncommitted text
  AND closes the dialog — a `2` on the way to `20` never lands as 2 %.
  Every commit applies at once, writes the file at once, and then the
  field shows the value IN FORCE — parsed, clamped, normalised — never the
  raw text; `Enter` keeps the keyboard in the field. There is no Apply, no
  OK, no Cancel and no unsaved state (persona 2026-10-01, MUST-HAVE at
  this size). A click on Close, Reset, Clear or a checkbox is a click-away
  like any other: the field's text commits first, then the click does its
  own work — a Reset resets the very field it has just committed — so
  only `Esc` discards (developer 2026-10-01, brief 008 commit B — the spec
  was silent on Close; Manager-accepted under M2, senior-developer review
  2026-10-01). A field commits only what the user typed into it, never a
  text it is merely showing, and it shows the value in force whenever the
  keyboard leaves it (senior-developer review F1, 2026-10-01). A control
  bound to its setting both ways — the two checkboxes — reads its own new
  state before anything that can present the settings anew, the flush of a
  half-typed field included, so the click's own work is the state the click
  left, never the one a commit put back (QE 2026-10-02, D39: a checkbox
  clicked over a half-typed field committed the field and undid the click).

### The settings

- **General › Auto-advance after Y/N** (`general.auto_advance`, default
  on; applies at once). On: `Y`/`N` moves the cursor to the next frame at
  every zoom and collapses the selection like an arrow — ui-grid.md's
  "Marks and auto-advance" and selection rule 1. Off: `Y`/`N` mark and the
  cursor STAYS, and the selection is left alone, exactly as `U` behaves —
  with `U`'s one exception: when the mark removes the frame from the
  active filtered view, the live-removal cursor rule moves the cursor to
  the survivor and that move ends the selection like any other (brief 008
  D7: a mark that does not move the cursor is not navigation under the
  file-manager rule). The rule is `filter::cursor_after_mark`'s
  `auto_advance` parameter, fed from the setting. Note: "Y or N moves to
  the next frame and ends a selection, like an arrow (default on; applies
  at once). Off keeps the cursor on the frame you marked — unless the
  filter hides it, in which case the cursor moves to the next one."
- **UI › Selection highlight** (`ui.selection_wash`, integer percent
  0–50, default 25; applies at once). A number field with a `%` suffix,
  no slider, no live preview (persona: a 67 %-black scrim lies about it;
  apply-on-commit plus `Esc` is the preview). Drives
  `selection-wash-opacity` through the app's single clamped write site
  (`state::clamp_wash_opacity`); 0 is legal — the accent outline still
  shows a selection. Note: "How strongly selected frames are tinted in
  the grid, 0–50 % (default 25; applies at once). Grid only — the loupe
  never tints. Above about 15 % the tint can shift your colour judgement
  on a final scan."
- **Performance › Loupe memory** (`performance.loupe_memory`, default
  `"2 GB"`; applies at the next folder open). The field accepts the memory
  grammar above. Beside it the dialog shows the value in force and the
  A1-frames hint, `= 12.4 GB of 31.1 GB ≈ 89 A1 frames`: bytes in force
  and total RAM through the byte formatter (binary GB, one decimal),
  frames = ⌊bytes ÷ 149,299,200⌋, the bytes of one decoded 8640×5760
  frame — an A1 number, and the hint names the body (M11). When the
  value was clamped the hint says which way (`= 200.0 MB (the floor) …`,
  `= 31.1 GB (all of this machine's RAM) …`); when total RAM is unknown a
  percentage reads `= 2.0 GB (total RAM unknown — the default) ≈ 14 A1
  frames`. Total RAM comes from `/proc/meminfo`'s `MemTotal` on Linux and
  `GlobalMemoryStatusEx` on Windows (core's one `unsafe` block; `sysinfo`
  refused 2026-09-26, no new crate), read once at startup. `LoupeEngine::
  start` receives the bytes in force at every folder open. Note: "Memory
  for decoded full-size frames: a number in GB (2, 0.5 GB) or a share of
  this machine's RAM (40 %) (default 2 GB; applies at the next folder
  open — File › Open Folder…, the same folder is fine). The app's
  footprint runs 1–2 GB above this number."
- **Performance › Thumbnail cache cap** (`performance.cache_cap`,
  `"<n> GB"`, default `"2 GB"`, floor 256 MB; enforced at the next folder
  open in the app and the next run of the CLI). `cache::default_cache_path`
  takes the cap and enforces it where it enforced the constant, so the
  app and the CLI honour one number. The cap bounds the thumbnails stored,
  not the file: SQLite reuses the pages an eviction frees and the file
  shrinks only at Clear's VACUUM (catalog-cache.md), so the Thumbnail
  cache row can read above the cap until then (QE 2026-10-01, D10; brief
  008 D25). Note: "The most the thumbnail cache may hold in thumbnails, in
  GB (default 2 GB, never below 0.25 GB; enforced when a folder is next
  opened). The file itself shrinks only when you Clear it."
- **Performance › Read workers** (`performance.max_readers`, default 0 =
  adaptive; applies at the next folder open). An **Adaptive (recommended)**
  checkbox and a **Limit** number field, enabled when the checkbox is off;
  a limit N has exactly `FASTCULL_MAX_READERS=N`'s meaning — N ≤ 4 pins
  exactly N readers, N > 4 is the ceiling above the floor of 4 — and no
  ceiling of its own, as the variable has none. Clearing Adaptive starts
  a limit of 4 — the pool's floor and the fixed gate
  `FASTCULL_MAX_READERS=4` restores; the setting is one integer, so the
  box has to name some limit (developer 2026-10-01, brief 008 commit A —
  the spec was silent; Manager-accepted under M2, senior-developer review
  2026-10-01). Core resolves `(environment, setting) → the pool's
  configuration` in one pure function, `settings::resolve_max_readers`,
  and both binaries call it through `settings::resolve_max_readers_from_env`,
  which hands it the process's own environment, where `pipeline.rs` read
  the variable (corrected 2026-10-02, QE round 5, SC-7: it said both
  binaries call the pure function itself). The CLI prints `readers: floor F cap C
  (<source>)` after its `cache:` line, the bounds read back from the pool
  (`Pipeline::read_pool_bounds`), the source worded as the `cache:` line's
  (QE 2026-10-01, D34). The environment override shows
  read-only as above. Note: "Adaptive (recommended): 4 readers, growing
  while the storage keeps up. Limit N: exactly N readers when N is 4 or
  less; above 4, at most N (default adaptive; applies at the next folder
  open)."
- **Performance › Thumbnail cache.** A row reading `Thumbnail cache:
  22.3 MB in ~/.cache/fastcull/previews.db` — the size `du` would show, the
  database plus its `-wal` and `-shm` files through the byte formatter,
  and the real path — and a **Clear** button. No confirmation: the cache
  is regenerable. Clearing deletes every row, VACUUMs and truncates the
  WAL through a connection of its own, on a worker thread, never the UI
  thread; it never unlinks the file (catalog-cache.md's lock rule: a
  database deleted under a live connection loses data and can SIGBUS the
  peer); the button is disabled and the row reads `Clearing…` until it
  is done; then the readout is re-measured from disk and shows the true
  size — an empty database's few tens of KB, never a claimed `0 B` — and
  the open session keeps its already-painted thumbs. A clear that fails
  says so in the row. With `FASTCULL_NO_CACHE` set the row reads
  `Thumbnail cache: off (FASTCULL_NO_CACHE is set)` and the button is
  disabled. Note: "Clear removes every cached thumbnail; the next open of
  any folder re-reads its files once."

## Contracts

- `fastcull_core::settings`: `Settings` (the model, `Default` is the
  spec's numbers), `Key` (one per setting, with `tab()`, `table()`,
  `name()` and `note()` — the notes above have ONE home, here, and the
  file's comment lines and the dialog both read them), `Tab` and
  `TABS`; `load(path) -> Loaded` (settings, the read error if any, the
  path; `Loaded::report_on_stderr()` prints a failed read's one stderr
  line, worded by `Loaded::stderr_line()`), `load_default()`, `write(path,
  &Settings) ->
  Result<Option<PathBuf>, WriteError>` (where a broken file went — and
  `WriteError::moved_aside()` says it too when the move succeeded and the
  write after it failed);
  `Settings::set_from_text(key, text)`, `Settings::reset_tab(tab)`;
  `parse_memory`, `MemorySpec` (`Gb(f64)` | `Percent(u32)`, `Display` is
  the normalised string), `memory_bytes(spec, total_ram) -> (bytes,
  MemorySource)`, `Settings::loupe_memory_bytes(total_ram)`,
  `Settings::cache_cap_bytes()`, `a1_frames(bytes)`,
  `resolve_max_readers(env, setting) -> Readers` (`Adaptive` |
  `Limit(n)` | `Environment(n)`, with `override_for_pool()`) and
  `resolve_max_readers_from_env(setting)`, the same against this process's
  environment — the call both binaries make (QE 2026-10-02, round 5,
  SC-7),
  `config_dir()` and its pure `config_dir_from(env)`, `total_ram()` and
  `parse_mem_total`.
- Constants: `FILE_NAME`, `WASH_MAX` 50, `WASH_DEFAULT` 25,
  `CACHE_CAP_FLOOR_BYTES` 256 MB, `A1_FRAME_BYTES` 149,299,200,
  `MAX_READERS_VAR`; `loupe::BUDGET_FLOOR_BYTES` 200 MB is the loupe's and
  this module reads it.
- `cache::default_cache_file()` (the path, no open), `cache::
  default_cache_path(cap_bytes)`, `cache::size_on_disk(db)`,
  `PreviewCache::clear()` (catalog-cache.md). `Pipeline::start(jobs,
  cache_path, threads, max_readers: Option<usize>)`, and
  `Pipeline::read_pool_bounds()` for the bounds the pool adopted from it
  (raw-pipeline.md).
- The window: `settings-visible`, `settings-tab`, one property per field,
  `settings-notice`; callbacks `settings-open`, `settings-close`,
  `settings-commit(key, text)`, `settings-reset(tab)`,
  `settings-clear-cache`; `focus-keys()` routes to the dialog's scope
  while it is up.
- The marks, the `settings` and `hover:` drive tokens, the layout marks
  (`settings card`, `settings tab <name>`, `settings <control>`) and the
  dump fields are test-harness.md's.

## Acceptance criteria

`core:` a `fastcull-core` unit test beside the code; `app:` a driven
`tests/screenshot.rs` test (real dispatched events, dumps and traces).

- [x] **AC1 — opening and closing.** File › Settings… and `Ctrl+,` open the
      dialog; `Esc` and Close close it; the keyboard returns to the grid
      (`key:+` zooms afterwards); a scrim click does not close it —
      `settings_opens_from_the_chord_and_the_menu_and_closes_with_esc_keeping_the_keyboard`
      (its menu strand Linux-only, like About's); opened over a focused
      keyword field holding typed text, it commits the field like a
      click-away and owns the keyboard —
      `settings_over_a_focused_keyword_field_commits_it_and_owns_the_keyboard`
      (the real File menu on Linux, the `settings` token elsewhere; QE
      2026-10-01, D8).
- [x] **AC2 — containment.** Under the dialog `Y`/`N` mark nothing and
      `Ctrl+E`/`Ctrl+Shift+E` open nothing; driven nav tokens are
      swallowed; the menu bar stays live; About and the shortcuts card over
      it close topmost-first —
      `settings_contains_every_grid_key_and_stacks_under_about`,
      `settings_stacks_under_the_shortcuts_card_and_closes_topmost_first`
      (the real Help menu on Linux, the `shortcuts` token elsewhere; a real
      `?` cannot open the card over Settings because the dialog swallows
      it, so the menu is the only real path; QE 2026-10-01, D27: until then
      the box had dropped the brief's shortcuts-card half and mutant G3
      stayed green).
      `Ctrl+O`'s inertness is review-verified: the arm is the same scope
      rule, and a driven Ctrl+O that worked would open the native picker
      and hang the run. The dialog and the export dialogs never stack —
      `settings_and_the_export_dialogs_never_stack`: `Ctrl+,` under Copy
      Picks on every runner; the File menu's greying driven on Linux for
      Copy Picks and for Export Frames as Video, both ways — each export
      item greyed under Settings, Settings… greyed under each — the export
      half on a real two-frame folder whose export is available, each
      greyed click checked against a control click, and review-verified on
      Windows, whose menu bar is the OS's (QE 2026-10-01, D3; the export
      half, QE 2026-10-02, round 5: until then only Copy Picks' greying was
      driven, and taking either export term out left the suite green).
- [x] **AC3 — the tabs.** Three tabs in order, switched by `Left`/`Right`
      on the strip and by `Ctrl+Tab`/`Ctrl+Shift+Tab` from a field; digits
      never switch; `Tab` walks the controls and never leaves the dialog,
      and selects a number field's text on arrival —
      `a_number_typed_after_tab_replaces_the_value_in_the_field` (QE
      2026-10-01, D33: every other test pressed Ctrl+A before typing, and a
      typed number went in beside the value shown);
      Reset resets the active tab only, the field being typed in included
      (its text commits once, first, and the field then shows the
      default) — `settings_tabs_switch_by_keys_and_never_by_digits`,
      `reset_with_a_half_typed_field_commits_it_then_resets`, over each of
      the four number fields (the cap and the Limit since QE 2026-10-02,
      round 5); every field
      carries its note, core's sentence byte for byte, read from the marks
      the notes emit themselves, and the notes are drawn (the loupe memory
      note's rectangle holds text at the shutter, against the bare card) —
      `every_settings_note_is_the_core_text` (QE 2026-10-01, D22: until then
      the box cited a test that read no note); it opens on General at
      launch and reopens on the tab it was closed on — the tabs test's
      `open` and `reopened` dumps (QE 2026-10-01, D27).
- [x] **AC4 — the write.** Every commit writes `settings.toml` at once,
      preserving an unknown key and the user's comments; the field then
      shows the value in force; `Esc` discards an uncommitted field and
      closes — `a_settings_commit_writes_the_file_and_esc_discards_a_half_typed_field`
      (under `FASTCULL_CONFIG_DIR`); the field shows the value in force
      after a commit that leaves it unchanged — a clamp (`60` over 50 %),
      another spelling (`2gb` over `2 GB`, `04` over a limit of 4) — and
      after a value it refuses (`abc`), never the raw text, in every number
      field — `a_commit_that_leaves_the_value_in_force_unchanged_still_reshows_it`
      (QE 2026-10-02, round 5: the field's own `accepted` is the only
      re-show there, and taking it out of any field left the suite green);
      a hand edit made while the dialog is closed is applied at its next
      open — the window's wash takes it, not only the dialog's model, and
      the reopen's own re-read is traced — app
      `a_hand_edit_is_applied_when_the_dialog_next_opens` ("Reading"; QE
      2026-10-02, round 5: with the open's apply taken out the grid kept its
      old tint beside the new value, and the suite stayed green); core
      `a_written_file_round_trips_every_key`,
      `a_write_preserves_unknown_keys_and_the_users_comments`,
      `a_comment_only_files_comments_stay_at_the_top` (blank lines alone
      stay at the end, QE 2026-10-02, D44),
      `a_moved_comment_sits_one_blank_line_above_the_appended_table`
      (re-review RR-F3) and
      `a_table_or_an_array_at_a_known_name_is_replaced_and_the_file_still_parses`
      (QE 2026-10-01, D35; its comment above kept, brief 008 D40),
      `a_replaced_entrys_own_comments_stay_with_what_replaces_it` (brief 008
      D40),
      `a_created_key_carries_its_note_and_an_existing_key_keeps_its_comment`,
      `the_normalised_string_is_what_the_file_stores`; a write that fails
      keeps the commit in force and the next open does not re-read over it
      — app `a_failed_settings_write_keeps_the_commit_and_the_next_open_does_not_reread`;
      a commit or a Reset that changes nothing creates no file — app
      `a_no_change_commit_or_reset_never_creates_the_file` (QE 2026-10-01,
      D4); every control that takes the keyboard from a half-typed field —
      Close, Clear, a checkbox, another field, a tab, `Tab`, `Shift+Tab`,
      `Ctrl+Tab`, `Ctrl+Shift+Tab`, the scrim, About or the shortcuts card
      over the dialog, a menu item, a folder opened under it — commits the
      field first, exactly once, then does its own work (the click-away
      rule of "Apply on commit") — and `Esc` discards it — app
      `every_control_that_leaves_a_dirty_settings_field_commits_it_first`,
      one launch per control, every control over Loupe memory, and Close,
      the scrim and `Esc` over each of the four number fields — Selection
      highlight, Loupe memory, the Thumbnail cache cap and the read
      workers' Limit — the rules each field carries its own copy of; an
      `Esc` row first checks that the field showed the typed text (Reset
      and Enter are the tests named in AC3 and above; Clear and the menu
      rows run on Linux only, the default cache sandboxed, the menu bar in
      the window; the auto-advance checkbox is review-verified until
      General has a number field to leave half-typed) (QE 2026-10-02, D39,
      D40: the Adaptive checkbox committed the field and undid its own
      click, and Close's commit had no guard; QE 2026-10-02, round 5: the
      matrix drove Loupe memory alone, and taking out the other three
      fields' copies of Close's commit, the scrim's and Esc's discard left
      the suite green).
- [x] **AC5 — a broken file.** A malformed file yields the defaults, a
      status-line and a stderr warning naming the file and the error, and
      is never overwritten in place; the first write moves it aside under
      a reported name — core
      `a_malformed_file_yields_defaults_and_is_left_byte_identical`,
      `a_failed_read_says_so_in_one_stderr_line_naming_the_file` (the
      stderr line's one wording; the dialog's re-read prints it too — app
      `a_hand_edit_that_breaks_the_fresh_file_is_shown_not_masked_by_rewritten`,
      QE 2026-10-01, D31),
      `the_first_write_moves_a_broken_file_aside_and_writes_a_fresh_one`,
      and a file fixed by hand before the write is merged, not moved —
      `a_file_fixed_by_hand_after_a_failed_read_is_merged_not_moved_aside`;
      app `a_malformed_settings_file_yields_defaults_and_is_moved_aside_on_the_first_write`;
      where it went is still named when the write after the move fails —
      app unit
      `settings_bridge::tests::a_failed_write_after_the_move_aside_still_names_where_the_file_went`
      (core's half, the error carrying the path, review-verified: a rename
      that succeeds and a write that then fails in the same directory
      cannot be provoked deterministically); a read error after the move is
      shown, never masked by `rewritten`, the earlier aside still named —
      app unit
      `settings_bridge::tests::a_read_error_after_the_move_aside_is_shown_not_masked_by_rewritten`
      and app
      `a_hand_edit_that_breaks_the_fresh_file_is_shown_not_masked_by_rewritten`
      (QE 2026-10-01, D26); a write that fails after a failed read says so
      on the status line, never `(defaults in force)` beside a commit in
      force — app unit
      `settings_bridge::tests::a_write_error_wins_over_a_read_error_on_the_status_line`
      (brief 008 D41; QE 2026-10-02, D43); a write that cannot move a NEWER
      broken file aside names the earlier aside as the earlier one on both
      lines — app unit
      `settings_bridge::tests::a_failed_write_after_a_newer_read_error_names_the_earlier_aside_as_the_earlier_one`
      (QE 2026-10-02, round 5, D46).
- [x] **AC6 — hermetic.** `FASTCULL_NO_CONFIG=1` makes `settings.toml`
      unreachable for load and save and the dialog says `Not saved`; no
      test touches the real file — core
      `config_dir_honours_the_override_then_no_config`, which also pins
      the per-user dir "The file" names, with neither variable set — a
      path ending `fastcull` on Linux and `fastcull\fastcull\config` on
      Windows, computed, the filesystem untouched (QE 2026-10-02, round 5:
      a resolver naming another dir would orphan every user's three files
      at an upgrade, and nothing saw it); app
      `settings_under_no_config_applies_in_memory_and_writes_nothing`;
      every driven run carries `FASTCULL_NO_CONFIG=1`, and every test that
      reads or writes a settings file sets `FASTCULL_CONFIG_DIR` to its own
      scratch dir (corrected 2026-10-01, QE D29: it said "the two that
      write", and seven tests set it then, several of them read-only);
      `templates.toml` and `ui.toml` go through the same resolver, moved
      and hidden with it — app
      `templates_and_ui_prefs_are_read_from_the_one_config_dir`, from the
      marks each read emits with the path it used (QE 2026-10-01, D37: the
      real config dir is empty on every seat, so a revert of either to the
      per-user dir had stayed green).
- [x] **AC7 — precedence.** With `FASTCULL_MAX_READERS` set the field is
      read-only with the environment's value and its note; unset, the
      file's value governs the pool; an unparsable value is ignored —
      core `the_environment_wins_over_the_file_for_max_readers`,
      `the_readers_resolution_feeds_the_pool_exactly_as_the_variable_did`
      (with `pipeline::tests::controller_override_caps_and_pins`) and
      `pipeline::tests::the_pipeline_reports_the_bounds_its_pool_adopted`;
      app `the_environment_wins_over_the_settings_file_for_read_workers`,
      whose dump reads the bridge's resolution and whose `read pool started
      floor F cap C` marks — waited on in the first two runs, read off the
      third run's trace — are the pool's own bounds: env 3 pins (3, 3), an
      ignored `abc` over the file's 7 gives (4, 7), no file gives floor 4
      (QE 2026-10-01, D27: until then the app's call site was unproven —
      mutant G1, `None` passed to `Pipeline::start`, stayed green); the
      CLI's call site is pinned on both runners by
      `settings_cap::the_cli_honours_the_files_read_workers_under_the_environment`,
      which reads the `readers:` line the CLI builds from the pool's own
      bounds: the file's 2 pins (2, 2), the environment's 3 wins over the
      file's 6, an ignored `abc` leaves the file's 6 (4, 6), no file is
      adaptive (QE 2026-10-01, D34: the same mutant on the CLI's call site
      had left the suite green); the Limit field takes nothing while
      Adaptive is ticked and takes a limit once it is cleared — the same
      app test's third run, a click and `6`, Enter, before and after
      clearing Adaptive (QE 2026-10-02, round 4 D41: with the Adaptive term
      dropped from the field's `enabled` the suite had stayed green); the
      environment's note is on screen while the variable governs, core's
      sentence byte for byte — the same app test reads the mark the note's
      own Text emits, `settings note readers-env shows …`, in the first run
      and its absence in the second, and core
      `the_environment_note_is_the_specs_sentence` pins the wording — and the
      Adaptive box is locked with the field: in the first run a click on it
      commits nothing and writes nothing (QE 2026-10-02, round 5: with the
      note presented empty, or the box's lock taken out — a click then
      rewrote the file's own `max_readers = 7` to 0 — the suite had stayed
      green); the
      environment never reaches the file — under `FASTCULL_MAX_READERS=3` a
      commit of another setting saves the file's own `max_readers = 7` —
      core `the_environment_never_reaches_the_settings_file` (a test binary
      of its own, because it sets the variable in its own process) and the
      same app test's fourth run (the user, 2026-10-02, brief 008 D42).
      The field's SHOWN value under the variable is the environment's —
      the same app test's first run reads the Limit field's own `settings
      readers-limit shows 3` mark, emitted when the dialog creates the
      field, before `dump.perf`, where the file says 7 (brief 010,
      2026-10-03; a deferred guard until then, issue #100, QE 2026-10-02
      SC-3 — the box re-opened for it and ticks with the strand).
- [x] **AC8 — auto-advance off.** `Y`/`N` keep the cursor and leave the
      selection alone; the filter exception moves the cursor and ends the
      selection as `U` does; on, as today — core
      `filter::tests::mark_advances_exactly_one_image` (the off row, now
      fed from the setting); app
      `auto_advance_off_keeps_the_cursor_and_the_selection_like_u`.
- [x] **AC9 — the wash.** The committed percentage reaches the window's
      `selection-wash-opacity` at once, 0–50 inclusive, clamped — in
      `a_settings_commit_writes_the_file_and_esc_discards_a_half_typed_field`
      (`washprop=` reads the window property, not the model); core
      `out_of_range_values_clamp_on_read`.
- [x] **AC10 — loupe memory.** GB and percentage forms parse, garbage is
      the default, the hint counts A1 frames, the floor and the total
      clamp, unknown RAM falls back and says so, and the engine starts
      with the bytes in force at the next folder open — core
      `the_memory_string_parser`,
      `a_percentage_with_unknown_ram_falls_back_to_the_default_and_says_so`,
      `the_frames_hint_counts_a1_frames`, `parse_mem_total_reads_meminfo`,
      `loupe::tests::the_engine_reports_the_budget_it_adopted`; app
      `loupe_memory_takes_effect_at_the_next_folder_open` (waits on
      `loupe engine started budget <bytes>`, the engine's own figure,
      `LoupeEngine::budget()` — QE 2026-10-01, D23: until then the mark was
      the caller's local).
- [x] **AC11 — the cache cap.** The file's cap is what
      `default_cache_path` enforces, app and CLI alike — core
      `defaults_are_the_specs_numbers` (the setting's default is
      `cache::DEFAULT_CAP_BYTES`) and
      `cache::tests::eviction_respects_cap_and_lru_order` (the eviction);
      the app's call site is driven on Linux, where `HOME` and
      `XDG_CACHE_HOME` redirect the default cache into a sandbox —
      `the_cache_cap_and_clear_cache_reach_the_default_cache` (300 seeded
      thumbnails of 1 MiB, at most 256 left after a folder open under the
      0.25 GB floor) — and review-verified on Windows, whose known-folder
      lookup ignores the environment (QE 2026-10-01, D24: "every driven run
      is `FASTCULL_NO_CACHE`" had made both call sites review-verified); the
      CLI's call site is driven on Linux by
      `settings_cap::the_cli_honours_the_files_cache_cap_and_says_where_it_came_from`
      (the same sandbox and seed, and the four wordings of where the cap
      came from) and review-verified on Windows (QE 2026-10-01, D27).
- [x] **AC12 — Clear cache.** The readout is the db + `-wal` + `-shm`
      size with the path; clearing empties the table and shrinks the
      file through a live connection and never unlinks it; the
      connection stays usable — core
      `cache::tests::size_on_disk_counts_the_wal_and_shm_files`,
      `cache::tests::clear_leaves_the_file_present_empty_and_smaller_and_the_connection_usable`
      (asserts the same inode on unix); app
      `clear_cache_is_off_under_no_cache` (the disabled row). Driven on
      Linux through the sandboxed default cache —
      `the_cache_cap_and_clear_cache_reach_the_default_cache`: the table
      emptied, the same inode; the clear on its own worker thread, which
      names itself on the trace (`settings cache clear ran on
      settings-clear`); the row reading `Clearing…` and Clear disabled
      until it is done, and after it Clear offered again and the row
      showing the re-measured size in KB, never `0 B` — each read from the
      mark the row's Text and the button emit themselves; and a clear that
      fails, the database made read-only for the clear alone, saying so in
      the row (`Thumbnail cache: could not be cleared (…) — … in …`) — and
      review-verified on Windows, whose known-folder lookup ignores the
      environment. That the clear never BLOCKS the UI thread is driven on
      Linux by the same test's fourth run: with the worker held by the
      harness knob `FASTCULL_CLEAR_HOLD_MS` (test-harness.md), the UI
      thread answers a `dump.` WHILE the hold stands — the dump's line
      before the worker's own `settings cache clear ran on settings-clear`
      line on the one trace — and the row reads `Clearing…` in that dump;
      a `join()` after the spawn puts the dump after the worker's line
      (brief 010, 2026-10-03; review-verified until then: the thread's name
      proves the worker, and a `join()` right after the spawn would still
      report it). The open session keeps its painted thumbs — the dump's
      `thumbtex=` count of decoded thumb textures is the same before and
      after the clear, and at least 1 — in the same run (brief 010;
      review-verified until then, no dump field reading textures); and
      the Tab ring reaches Clear when the cache is on — four Tabs from the
      strip on Performance land on it, and Return starts the clear — in
      the same run (brief 010); all three review-verified on Windows (QE
      2026-10-01, D24: "a driven run cannot have a cache" holds on Windows
      only; brief 008 D13 stands — no new variable for a setting; QE
      2026-10-02, round 5: the worker, the `Clearing…` row, the disabled
      button and a failed clear's wording had stood on the trace's order
      alone, which proved none of them).
      Under `FASTCULL_NO_CACHE` the button is DISABLED, read from its own
      `settings clear-cache enabled false` mark at the open and none
      reading `true` after it — `clear_cache_is_off_under_no_cache` (brief
      010, 2026-10-03; a deferred guard until then, issue #100, QE
      2026-10-02 SC-4 — the box re-opened for it and ticks with the
      strands).
- [x] **AC13 — the Failed badge.** The badge shows the reason on hover and
      the status line carries it when the cursor stands on the frame —
      ui-grid.md's ledger
      (`the_failed_badge_shows_its_reason_on_hover_and_in_the_status_line`).
- [x] **AC14 — the card.** The shortcuts card lists `Ctrl+,` and still
      fits whole at 1000×700 —
      `the_shortcuts_card_lists_every_binding_in_the_spec`,
      `shortcuts_card_is_a_two_column_sheet_that_fits_its_window`
      (ui-grid.md).
- [x] **The Settings card fits its smallest window.** The Performance tab
      in its tallest state fits whole at 1000×700, Close and Reset inside
      the card — `the_settings_card_fits_its_smallest_window_in_its_tallest_state`
      (slack measured, height never pinned; QE 2026-10-01, D9); its cache-on
      run, the row showing a 91-character path over two lines, is driven on
      Linux through the sandboxed default cache and review-verified on
      Windows, whose known-folder lookup ignores the environment — Segoe UI
      leaves 80 px of slack without the cache, and a real Windows cache path
      of about 65 characters is two lines at most (QE 2026-10-01, D38).
- [x] **AC15 — docs.** `docs/settings.md` exists, CLAUDE.md's page map
      names it, and `docs/culling.md` and `docs/faq.md` follow the
      behaviour — review-verified by the senior developer's review of
      brief 008, 2026-10-01 (no test pins prose).
- [x] **AC16 — no budget row moves** — `tests/perf_budgets.rs` green in
      release on the idle seat, every row in three runs (QE 2026-10-02,
      round 5; the figures are brief 008 D45).
- [x] **AC17 — one height per open, every tab** (brief 009 AC1). Exactly
      one `settings card laid out` mark per open, none on any of the
      switches of a General → UI → Performance → General walk, and the
      height of an open on General equals the height of a reopen on
      Performance — `the_settings_card_holds_still_across_its_tabs`, each
      of its three launches (brief 009, 2026-10-03).
- [x] **AC18 — the footer pinned, the notice reserved** (brief 009 AC2).
      Close and Reset keep their x and y at every dump of the walk, and
      neither they, the notice line, the body host nor a tab reports a new
      layout from the first switch to the close; the active tab's first
      control starts at the body's top, within its own height, at every
      dump (QE 2026-10-03, D3); the
      card under `FASTCULL_NO_CONFIG` (a notice at open) is as tall as
      the card with no notice (`FASTCULL_CONFIG_DIR` into an empty scratch
      dir), and the notice line is one line tall in both — the same test's
      second launch; with a two-line notice and the environment's line both
      present at open (a broken file, `FASTCULL_MAX_READERS=3`) the card
      still lays out once — its third launch; and the card never shrinks
      while open — Loupe memory committed as `100` (the wrapped hint is
      taller than its 32 px field on Noto Sans, this seat; on DejaVu Sans
      and on Segoe UI's metrics the two lines fit the field's row and the
      strand cannot go red — corrected, QE 2026-10-03, D2: it read "the
      hint wraps to a second line on the measured seats", and the hint
      wraps on DejaVu Sans too, inside its row) and then `2` leaves the
      height where the wrap put it, no card mark after the second commit,
      and a card that opened with the hint already wrapped (a file saying
      `100 GB`) keeps its opening height after `2`, the body host and the
      footer reporting no new layout after either commit, and the commit
      of `100` grows the card to the height an open with the hint already
      wrapped takes (QE 2026-10-03, D1); with the cache on — Linux only,
      the default cache sandboxed through `HOME` and `XDG_CACHE_HOME`,
      which Windows ignores — a Thumbnail cache row printing a path of
      about 220 characters, four lines, that Clear replaces with
      `Clearing…` leaves the card, the body host, the notice line, Reset
      and Close without a new layout until the clear completes: the strand
      with power on the ubuntu runner's face (QE 2026-10-03, D2) —
      `the_settings_card_never_shrinks_while_it_is_open` (brief 009,
      2026-10-03; its second launch and the no-new-layout checks,
      developer 2026-10-03; the growth check and the cache strand, the
      test-integrity review 2026-10-03); and a write error that wraps the
      notice, arriving while the dialog is open, grows the card by exactly
      the notice's extra line on every runner's face —
      `the_settings_card_grows_by_a_write_error_that_wraps_while_it_is_open`
      (QE 2026-10-03, D1) — and the card so grown KEEPS that height when a
      later write succeeds and the notice un-wraps to the one-line
      `rewritten`: the same test's third commit, the file writable again,
      the card as tall as with the write error, Close where it was, and no
      new layout from the card, Reset or Close after it — the notice line,
      which shrinks, and the body host, which takes the slack, report
      theirs by design — the never-shrinks rule with power on every face,
      Windows' included, where the Loupe memory strand has none (brief 010,
      2026-10-03, brief 009's TP-1 in issue #100; corrected at brief 010's
      implementation the same day: it named the body host and the notice
      line among the silent, which the correct card contradicts — measured,
      the notice 33 → 17 px and the host 306 → 322 px at the third commit).
- [x] **AC19 — the strip holds still** (brief 009 AC3). Every tab's x and
      width are the same at every dump of the walk —
      `the_settings_card_holds_still_across_its_tabs` (brief 009,
      2026-10-03).
- [x] **AC20 — fits at 1000×700 on every tab** (brief 009 AC4). The
      existing fit test, `the_settings_card_fits_its_smallest_window_in_its_tallest_state`,
      unchanged in its assertions — its "tallest state" is every state now,
      and it still opens on Performance where the long cache row lives
      (brief 009, 2026-10-03: green with the rule in place).
- [x] **AC21 — spec and docs** (brief 009 AC5). This section and
      `docs/settings.md` say the card is the same size on every tab —
      review-verified (senior developer 2026-10-03, the review and the
      test-integrity review).
- [x] **AC22 — below the minimum window the body gives before the footer**
      (brief 009 D4, the senior developer's call). At 1000×400 Close and
      Reset lie inside the clamped card, a wheel over the body scrolls it
      (a control's mark rises by the wheel's distance or less, never by
      nothing — the body's end may stop it short; corrected 2026-10-03,
      brief 009 commit B: it read "moves by the wheel's distance", which
      the plan's test does not pin, the stop being a sum of text heights)
      while the grid behind holds `vpy=0.0`, and a tab switch puts the
      body back at its top —
      `below_the_minimum_window_the_settings_body_gives_before_the_footer`
      (brief 009, 2026-10-03); its premise names a reverted window as
      such: between the geometry wait's echo and the last dump no `window
      geometry WxH` other than `1000x400` is traced, the WxH prefix
      compared only — diagnostic quality, a revert is red either way
      (brief 009's TP-3, landed with brief 010, 2026-10-03).
- [x] **AC23 — a plain failed write's status line** (brief 010 R2). With no
      read error standing and no file moved aside, a write that fails puts
      ` — ⚠ settings.toml could not be written` on the status line — no
      `rewritten`, no `(defaults in force)` — app unit
      `settings_bridge::tests::a_plain_failed_write_says_so_on_the_status_line`
      and the driven
      `a_failed_settings_write_keeps_the_commit_and_the_next_open_does_not_reread`,
      whose `committed` and `reopened` dumps read the status line (brief
      010, 2026-10-03; issue #100's first NOW guard).
- [x] **AC24 — `Space` commits a checkbox** ("Apply on commit"; brief 010
      R2). `Space` on the Auto-advance box reached by `Tab` from the strip,
      and on the Adaptive box reached by three `Tab`s on Performance, each
      commits its setting once and the box shows the new state —
      `a_number_typed_after_tab_replaces_the_value_in_the_field`'s Space
      strand, read from `settings committed …` and the boxes' own `shows`
      marks (brief 010, 2026-10-03).
- [x] **AC25 — a hand edit made while the dialog is open loses to the next
      save** ("Writing", brief 008 D42 option A; brief 010 R2). The file
      says `selection_wash = 40` at launch and the open reads it
      (`wash=40`); a hand edit anchored on the app's own `settings opened`
      line rewrites the file to `selection_wash = 10` and adds `hand_edit =
      1`; a commit of ANOTHER setting then saves every key as the dialog
      holds it — the written file reads `selection_wash = 40` beside the
      surviving `hand_edit = 1`, and the model never saw 10 — app
      `a_hand_edit_made_while_the_dialog_is_open_loses_to_the_next_save`
      (the `edited` premise: `hand_edit = 1` in the written file proves the
      edit preceded the write, `wash=40` at the open that it followed the
      re-read) (brief 010, 2026-10-03).
- [x] **AC26 — the Limit has no ceiling of its own** ("Read workers"; brief
      010 R2). `set_from_text(MaxReaders, "64")` is 64 and `"4294967295"`
      the type's own maximum, a file's `max_readers = 1000` reads 1000, and
      the pool adopts `(4, 4, 1000)` — core
      `the_readers_limit_has_no_ceiling_of_its_own` and a `(None, 1000,
      (4, 4, 1000))` row of
      `pipeline::tests::the_readers_resolution_feeds_the_pool_exactly_as_the_variable_did`
      (brief 010, 2026-10-03).
- [x] **AC27 — `Ctrl+,` inert while a keyword field holds the keyboard**
      ("Opening and closing"; brief 010 R3). With the keyword field focused
      and typed into, `Ctrl+,` opens nothing and the field keeps the
      keyboard (`focusowner=` its token) and its text —
      `settings_over_a_focused_keyword_field_commits_it_and_owns_the_keyboard`'s
      strand before its open (brief 010, 2026-10-03; the condition the
      integrity review set — a measured red mutant — is the plan's
      measurement; without one this line records the deferral instead).
- [x] **AC28 — the active tab's accent underline** ("The card"; brief 010
      R3). At the notes test's shutter on Performance, a window of six
      pixel rows straddling the active tab's bottom edge holds at least 2
      accent rows — the 2 px underline — where the same window at its top
      edge holds at most 1 (the 1 px focus ring the strip draws on every
      edge while it holds the keyboard, whose bottom edge shares the
      underline's lower row) and the inactive cells' bottom edges none; a
      row is the accent when its mean blue bias across the cell, inset 3 px
      from its sides, is above 100 — the underline only, never a label's
      weight or brightness — `every_settings_note_is_the_core_text`'s
      pixel strand (brief 010, 2026-10-03; measured the same day on
      windows-latest, ubuntu-latest and this seat: the accent rows 176–179,
      every other row in the windows 5–14, the windows-latest cell drawn one
      row below its mark — test-harness.md, "Layout"; corrected the same day
      by the senior developer's review F1 — the strand averaged 3 px bands
      flush with the mark's edges, which on windows-latest held one
      underline row of two, read no bluer than the top band and went red on
      a correct tree; and before that at the implementation — it read "blue
      bias 92", which its means do not give, and named the inactive cells
      alone, against which a band with the underline transparent still read
      65 from the ring's bottom edge and stayed green).
- [x] **AC29 — Reset names the active tab** ("The card"; brief 010 R3).
      `Reset General to defaults` on General, `Reset UI to defaults` on UI,
      `Reset Performance to defaults` on Performance, read from the
      button's own `settings reset shows` mark at each tab's dump —
      `settings_tabs_switch_by_keys_and_never_by_digits` (brief 010,
      2026-10-03).
- [x] **AC30 — auto-advance off holds in the loupe at 1:1** ("General ›
      Auto-advance"; brief 010 R3). At 1:1 on a real folder with
      auto-advance off, `Y` marks and the cursor stays, `one2one` stays; on,
      `Y` advances at 1:1 too — app
      `auto_advance_off_holds_the_cursor_in_the_loupe_at_one_to_one`, a
      launch of its own (the integrity review's shape; brief 010,
      2026-10-03). The mark path has no zoom branch today, so its only red
      mutant is the grid test's: this pins "at every zoom" against a future
      one.
- [x] **AC31 — core's test scratch dirs go on `Drop`, a red test's kept**
      (brief 010 R3). `testutil::scratch_dir` returns a guard that removes
      the directory when the test ends and keeps it when the test is
      panicking — core
      `testutil::tests::a_scratch_dir_goes_on_drop_and_stays_for_a_panicking_test`
      (brief 010, 2026-10-03; measured on this seat: 1,086 `fastcull-*`
      dirs, 36 MB, left in `/tmp`, a 16 GB tmpfs).
- [x] **AC32 — the two hand-edited writer shapes** ("Writing"; brief 010
      R4). A key created inside an inline table carries no note and the
      braces stay — core
      `a_key_created_inside_an_inline_table_carries_no_note_and_the_braces_stay`
      (QE 2026-10-02, D47); every element's comments of a multi-element
      array stay above what replaces it, in order, a later header-line
      comment as a line of its own — core
      `every_elements_comments_of_a_replaced_array_stay_above_what_replaces_it`,
      over `[[general]]` and `[[performance.loupe_memory]]` (QE 2026-10-02,
      D48).
- [x] **AC33 — a failed move-aside with no read error standing** ("Writing";
      brief 010 R6). The notice reads `Could not write settings.toml: the
      file that would not read could not be moved aside: … — the earlier
      one is settings.toml.broken` and the status line ` — ⚠ settings.toml
      could not be written — the earlier one is settings.toml.broken` — app
      unit
      `settings_bridge::tests::a_failed_move_aside_with_no_read_error_standing_names_the_earlier_aside_as_the_earlier_one`
      (brief 010, 2026-10-03; the developer's 2026-10-02 finding in issue
      #100).

## History

- 2026-10-03 — Brief 010, the senior developer's review F1: AC28 counts
  the active tab's accent rows in windows straddling its edges — the 3 px
  bands flush with its mark held one underline row of two on
  windows-latest, where the cell is drawn one row below its mark, and went
  red on a correct tree.
- 2026-10-03 — Brief 010, the scratch-dir guard: core's unit tests hold a
  guard that removes their scratch dir when they end and keeps it for a
  red test; AC31 ticked beside its test.
- 2026-10-03 — Brief 010 commit D: the driven guards of issue #100 —
  Clear disabled under FASTCULL_NO_CACHE, the locked Limit's shown value,
  Space on both checkboxes, a hand edit made while the dialog is open,
  `Ctrl+,` inert over the keyword field, the underline, Reset's label, the
  held Clear (never blocking, the thumbs kept, the Tab ring to Clear),
  brief 009's TP-1 and TP-3, auto-advance off at 1:1; AC7, AC12, AC18,
  AC24, AC25, AC27, AC28, AC29 and AC30 ticked beside their tests, AC22's
  TP-3 clause landed; AC18's TP-1 sentence (it called the body host and
  the notice line silent at the third commit, where both report by
  design) and AC28's measurement corrected.
- 2026-10-03 — Brief 010 commit B: the bridge keeps a failed write's error
  whole and names an earlier aside as the earlier one when the write
  failed at the move-aside with no read error standing (red on f771f6f);
  a plain failed write's status line pinned, unit and driven; AC23 and
  AC33 ticked beside their tests.
- 2026-10-03 — Brief 010 commit A: the writer carries every element's
  comments of a replaced multi-element array (QE D48, red on f771f6f); the
  inline-table shape (QE D47) and the Limit's missing ceiling of its own
  pinned, no code change for either; AC26 and AC32 ticked beside their
  tests.
- 2026-10-03 — Brief 010 agreed, spec first: "Writing" states the
  inline-table exception (QE D47), the every-element carry for a
  multi-element array (QE D48) and the failed move-aside with no read error
  standing (the developer's finding, issue #100); AC7, AC12 and AC18
  re-opened for the shown Limit, the Clear guards and brief 009's TP-1;
  AC22 names TP-3; AC23–AC33 opened for the guards of issue #100.

- 2026-10-03 — Brief 009's test-integrity review: AC18 names the
  never-shrinks test's growth check and Linux cache strand, the
  body-from-the-top check and the write-error growth test, and its power
  condition is corrected (QE D2); the permanence sentence corrected — a
  conditional element is created after its parent's `init`, in the same
  pass, never painted short; a window narrowed below 600 px recorded (QE
  D4); AC21 ticked, review-verified.
- 2026-10-03 — Brief 009 commit B: the bodies' host is a `Flickable {
  interactive: false }` that gives below the minimum window and goes back
  to its top on a tab switch; AC22 ticked beside its test, its wheel
  distance worded as the test proves it.
- 2026-10-03 — Brief 009, the rule landed: the card's height a high-water
  mark over its layout, the body host as tall as the tallest body, the
  notice line and every row's environment line permanent, the strip at one
  weight; AC17–AC20 ticked beside their tests, AC18 naming the
  never-shrinks test's second launch.
- 2026-10-03 — Brief 009 agreed, spec first (the user: "The settings
  screen is bumping depending on its size … get its size fixed"): the card
  takes one height per open sized to its tallest tab, the footer is pinned
  to the bottom, the notice line and the environment's line are reserved,
  the strip keeps one weight; "The card" corrected, "The card holds still"
  added, AC17–AC22 opened for the plan's tests.

- 2026-10-02 — Merged (PR #96). AC7 and AC12 narrowed to what their tests
  prove; the guards QE's closing sweep listed and two hand-edited writer
  shapes (D47, D48) deferred to issue #100 (brief 008 D46).
- 2026-10-02 — QE round 5 of brief 008, a hand edit applied at the reopen
  ("Reading"): AC4 names the test that reads the window's wash after a
  hand edit and a reopen.
- 2026-10-02 — QE round 5 of brief 008, the value in force re-shown: a
  commit that changes nothing in force, or a refused value, leaves the
  field showing the value in force; AC4 names the test.
- 2026-10-02 — QE round 5 of brief 008, the click-away matrix (SC-5): the
  matrix drives Close, the scrim and Esc over all four number fields, and
  the Reset test the cap and the Limit; AC3 and AC4 name what each covers.
- 2026-10-02 — QE round 5 of brief 008, Clear cache (SC-2): the worker
  names its thread, the row and the Clear button report what they show,
  and a clear made to fail is driven; AC12 names what each proves and what
  stays review-verified.
- 2026-10-02 — QE round 5 of brief 008, stacking (SC-4): the Export Frames
  as Video item's greying under Settings, and Settings… greyed under the
  export dialog, are driven; AC2 names both halves.
- 2026-10-02 — QE round 5 of brief 008, the environment's row (SC-3): AC7's
  note and the Adaptive box's lock under the variable get their guards —
  the note reports itself, and AC7 names what reads it and the click that
  proves the lock.
- 2026-10-02 — QE round 5 of brief 008, bookkeeping (SC-1, SC-7) and the
  config dir's tail: AC16 ticked on QE's three idle release runs; "Read
  workers" and the Contracts name the wrapper both binaries call beside the
  pure function; AC6's test now pins the per-user dir's tail on Linux and
  Windows.
- 2026-10-02 — QE round 5 of brief 008, the earlier aside (D46): a write
  that could not move a newer broken file aside called the earlier aside
  "the file that would not read" on both lines; the F4 sentence of
  "Writing" names that one exception, both lines call it the earlier one,
  and AC5 names the test.
- 2026-10-02 — Bookkeeping (the senior developer's re-review RR-F5):
  "Keyboard" says Slint's own select-on-Tab holds off Apple targets only,
  as its source guards it; macOS is not a supported seat.
- 2026-10-02 — The user's answer to QE's D28 (brief 008 D42): every save
  writes every setting as the saving window holds it, the last save wins,
  and a setting the environment governs is saved with its own value,
  never the variable's; "Writing" states both, and AC7 names the tests of
  the second.
- 2026-10-02 — QE round 4 of brief 008, the status line after a failed
  write (brief 008 D41; QE D43): a standing write error wins on the status
  line as on the notice, so `(defaults in force)` is said only while it is
  true; "Reading" and "Writing" say so, and AC5 names the test.
- 2026-10-02 — QE round 4 of brief 008, a replaced entry's comments (brief
  008 D40; QE D42, re-review RR-F2): the comment above a table the writer
  replaces, and the one on its header's line, stay with the key or table
  that takes its place, for each of the three replaced shapes; the
  exception sentence of "Writing" says its contents go and its comments
  stay; AC4 names the tests.
- 2026-10-02 — QE round 4 of brief 008, comments after the last entry
  (D44; re-review RR-F1, RR-F3, RR-F4): "Writing" says where the moved
  comments go exactly — after any key the write adds to their table, one
  blank line above the first appended header — and that blank lines alone
  stay at the end; the writer keeps that one blank line in LF and CRLF
  files alike.
- 2026-10-02 — QE round 4 of brief 008, the Limit field's gate (D41): a
  Limit field enabled under Adaptive had no guard; AC7 names the strand
  that clicks it before and after Adaptive is cleared.
- 2026-10-02 — QE round 4 of brief 008, the click-away family (D39, D40;
  brief 008 D39): a checkbox clicked over a half-typed field committed the
  field and undid the click; "Apply on commit" says a control bound both
  ways reads its own state before anything can present, and AC4 names the
  one test that drives every control which leaves a half-typed field.
- 2026-10-01 — QE round 3 of brief 008, the card with the cache on (D38):
  the tallest state counts the Thumbnail cache row showing a long path, the
  fit test runs it on Linux, and the box says what stays review-verified.
- 2026-10-01 — QE round 3 of brief 008, shapes the file cannot keep (D35):
  a table where a key belongs, or an array of tables or a value where a
  tab's table belongs, is replaced in its place, and "Writing" names the
  exception instead of promising every byte; the comment rule says
  "appends".
- 2026-10-01 — QE round 3 of brief 008, comments at the end of the file
  (D35): a comment-only file's comments, and a comment after the last
  entry, came back below the tables a write created; they stay where they
  were now, and "Writing" says so.
- 2026-10-01 — QE round 3 of brief 008, the one resolver (D37): the
  templates.toml and ui.toml reads name the path they used, and AC6 names
  the test that pins both to the config-dir resolver.
- 2026-10-01 — QE round 3 of brief 008, the CLI's read workers (D34): the
  CLI prints the bounds its read pool adopted, and a second CLI test pins
  its call site; AC7 names it.
- 2026-10-01 — QE round 3 of brief 008, select on Tab (D33): the dialog's
  ring focused a number field without selecting it, so a typed number went
  in beside the value shown (Tab, 8, Enter on Loupe memory committed 82 GB);
  "Keyboard" now says a field reached by `Tab` or `Shift+Tab` selects its
  text, and AC3 names the test.
- 2026-10-01 — QE round 2 of brief 008, bookkeeping (D29): AC6 says every
  test that reads or writes a settings file sets `FASTCULL_CONFIG_DIR`,
  where it said "the two that write".
- 2026-10-01 — QE round 2 of brief 008, the CLI's cap (D27, TP-F): the
  CLI's call site and the wording of its `cache:` line get the CLI's first
  test, driven on Linux through the sandboxed default cache.
- 2026-10-01 — QE round 2 of brief 008, four promises with no guard (D27):
  the read workers setting is read back from the pool itself, and the
  reopen tab, the shortcuts card over the dialog and the wheel over its
  scrim each gain a test; AC2 names the shortcuts card again.
- 2026-10-01 — QE round 2 of brief 008, stderr at a re-read (D31): a file
  found unreadable by the dialog's re-read printed nothing on stderr,
  though "Reading" promised the line; the re-read prints core's one
  wording now, and the sentence says which reads print it.
- 2026-10-01 — QE round 2 of brief 008, a read error after the move-aside
  (D26): the spec said both "until … moved aside" and "name where it went
  … for the rest of the session" without saying which wins, and the code
  let `rewritten` mask a newer read error; the newer read error now wins,
  naming the earlier aside, and AC5 gains its tests.
- 2026-10-01 — QE round 1 of brief 008, the cache (D24): the app's cap
  enforcement and Clear cache are driven on Linux through a sandboxed
  default cache (`HOME`, `XDG_CACHE_HOME`), no new variable; AC11 and AC12
  say what stays review-verified, and where (brief 008 D24).
- 2026-10-01 — QE round 1 of brief 008, four promises with no test (D3,
  D4, D8, D9): the never-stack greying, the no-change write, Settings over
  a focused keyword field, and the card's fit in its tallest state each
  gain a driven test; the card's fit is stated under "The card".
- 2026-10-01 — QE round 1 of brief 008, the notes (D5): every note reports
  what it shows and where it is laid out, and AC3's note clause has a test
  that reads them (brief 008 D22).
- 2026-10-01 — QE round 1 of brief 008, the cache cap's words (D10, D11):
  the note says the cap bounds the thumbnails held, and that the file
  shrinks only at Clear (brief 008 D25); the CLI names where the cap in
  force came from.
- 2026-10-01 — QE round 1 of brief 008, test proposal TP10 (the senior
  developer's recommendation B): a write decides from the file as it is
  NOW — one fixed by hand after a failed read is merged into, not moved
  aside as a file "that would not read"; `write` loses its `broken`
  argument.
- 2026-10-01 — QE round 1 of brief 008, the file (D6, D7): a write keeps
  the file's CRLF line ends and its byte-order mark, which the "survive
  byte-for-byte" sentence had promised without the writer doing it (brief
  008 D21); a figure past a 64-bit float is garbage instead of being stored
  as `inf GB`.
- 2026-10-01 — QE round 1 of brief 008, the loupe memory (D1, D2): a
  budget below the prefetch window is legal and goes quiet when the user is
  idle (raw-pipeline.md's ring budget rule, brief 008 D20); AC10's driven
  proof reads the budget the engine adopted (D23).
- 2026-10-01 — Review round 1 closed (senior-developer review of brief
  008, F3 and F6, and the Manager's rulings): the four details recorded
  "for review" are Manager-accepted under M2 and read as rules, the cache
  cap's as the ruling words it; the card paragraph names the keyboard
  ring's hand lists a new tab must join; AC15 ticked, review-verified.
- 2026-10-01 — A write that fails (senior-developer review of brief 008,
  F2, F4, F5): the two behaviours the developer had added without stating
  them are stated — an open does not re-read while a write error stands,
  and any write moves an unparsable file aside — with the no-change write
  rule beside them; a write that fails after the move-aside still names
  where the file went; AC4 and AC5 gain their tests.
- 2026-10-01 — Reset over a half-typed field (senior-developer review of
  brief 008, F1): a Reset clicked while a field held typed text committed
  the text, reset, and then let the field commit its stale text again; a
  field now commits only what the user typed and re-shows the value in
  force when the keyboard leaves it. AC3 gains its test.
- 2026-10-01 — The Failed badge's tooltip (brief 008 commit C): AC13
  ticked beside its test.
- 2026-10-01 — Implemented (brief 008 commits A and B): the file, the
  precedence and the dialog; AC1–AC12 and AC14 ticked beside their tests.
  Four details the spec had left open are recorded where they apply as
  the developer's, for review: a whole-number percentage, the cache cap's
  floor shown in the field, Close committing like a click-away, and a
  cleared Adaptive starting at 4.
- 2026-10-01 — Created (brief 008, issue #39; the Settings dialog, the
  file, the precedence rule and the five settings the specs had promised
  since 2026-07-25). ADR 0005 records the storage-and-precedence contract.
