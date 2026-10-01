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
  an empty string, `abc`, a negative number, a `TB` — is garbage and reads
  as the key's default. The file stores the normalised form, `"2 GB"`,
  `"0.5 GB"`, `"40%"`: the number as typed, one space, the unit (user
  decision 2026-10-01: "numbers are always expressed in GB, and percentage
  always total RAM"). `cache_cap` takes the GB form only; a `%` there is
  garbage.

### Reading

- Core reads the file at startup in both binaries (`settings::load_default`,
  before any timed region) and the app reads it again each time the dialog
  opens; what the file says is applied at that open, so a hand edit made
  mid-session takes effect when the dialog is next opened (the Performance
  knobs still wait for their own moment, below). A missing file is the
  defaults and no error.
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
  single big folder (senior-developer plan 2026-10-01).
- **A file that fails to parse is never overwritten in place** (brief 008
  D5: a hand-edited config is the user's data). The defaults are in
  force; both binaries print one stderr line naming the file and the
  error's first line (`fastcull: <path> could not be read (<error>) —
  defaults in force`), and the app's status line carries `⚠ settings.toml
  could not be read (defaults in force)` until the file reads again or is
  moved aside. The dialog's notice line shows the whole error.

### Writing

- The file is written on every commit of the dialog and on every Reset,
  and never otherwise: a missing file stays missing until the first
  change. The write is a read-modify-write through `toml_edit`: every
  known key is emitted under its table with its in-force value; a key the
  write CREATES is preceded by `#` comment lines carrying the field's
  note (`Key::note()`, wrapped at 78 columns over as many lines as it
  needs), so the file documents itself for hand editing; a key that already
  exists keeps the user's own comment above it and any comment on its
  line; unknown keys, tables and the user's other comments survive
  byte-for-byte (measured on toml_edit 0.22.27, 2026-10-01: `Table::insert`
  on an existing key drops its comment, replacing the value in place does
  not — the plan names the call).
- The FIRST write after a failed read moves the broken file aside before
  writing a fresh one: to `settings.toml.broken`, or `settings.toml.broken.N`
  (N from 1) when that exists, never over an existing file. The status
  line and the dialog's notice then name where it went
  (`settings.toml rewritten — the file that would not read is
  settings.toml.broken`) for the rest of the session.
- A write that fails (a read-only config dir, a full disk) keeps the
  commit in force in memory, prints one stderr line and puts
  `Could not write settings.toml: <error>` on the dialog's notice line;
  nothing is silently lost on screen.
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
  stays editable — exactly what `pipeline.rs` did with `parse().ok()`.
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
  `#3a3a44`, 8 px radius, 560 px wide, its height following its content,
  the scrim swallowing the wheel — with a tab strip across the top:
  **General | UI | Performance**, in that order, the active tab marked
  (brighter label, a 2 px accent underline). The tabs are a LIST — one
  entry per tab on the Slint side, `settings::TABS` on the core side — so
  the next unit adds a tab by adding an entry and a body. Under the strip
  is the active tab's form: one row per setting with a label, a control
  and a one-line note beneath them in the shortcuts card's dim style (the
  `G` row's grey line, `#8a8a96`, 11 px) that says what the setting does,
  its default and when it takes effect. The footer carries `Reset <tab>
  to defaults` — the ACTIVE tab's settings only, written at once — and
  `Close`, and a notice line that is empty unless the file could not be
  read, could not be written, is not saved, or was moved aside.
- **Keyboard.** `Tab`/`Shift+Tab` walk the strip and the active tab's
  controls in order — the strip, the controls top to bottom, Reset, Close
  — and never leave the dialog: the dialog's own key scope handles both,
  so Slint's window-level Tab navigation can never carry the keyboard into
  a field hidden behind the scrim. `Left`/`Right` on the strip switch
  tabs; `Ctrl+Tab`/`Ctrl+Shift+Tab` switch tabs from anywhere in the
  dialog, wrapping; a tab switch puts the keyboard on the strip. **Digits
  never switch tabs** — `1`–`5` are reserved (ui-grid.md) and in a dialog
  they are digits for a number field. Opening the dialog puts the keyboard
  on the strip.
- **Apply on commit.** A checkbox applies on click or `Space`; a number
  field applies on `Enter`, on `Tab` and on click-away (focus leaving it
  while the dialog is up); `Esc` in a field discards its uncommitted text
  AND closes the dialog — a `2` on the way to `20` never lands as 2 %.
  Every commit applies at once, writes the file at once, and then the
  field shows the value IN FORCE — parsed, clamped, normalised — never the
  raw text; `Enter` keeps the keyboard in the field. There is no Apply, no
  OK, no Cancel and no unsaved state (persona 2026-10-01, MUST-HAVE at
  this size).

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
  app and the CLI honour one number. Note: "The most the thumbnail cache
  may keep on disk, in GB (default 2 GB, never below 0.25 GB; enforced
  when a folder is next opened)."
- **Performance › Read workers** (`performance.max_readers`, default 0 =
  adaptive; applies at the next folder open). An **Adaptive (recommended)**
  checkbox and a **Limit** number field, enabled when the checkbox is off;
  a limit N has exactly `FASTCULL_MAX_READERS=N`'s meaning — N ≤ 4 pins
  exactly N readers, N > 4 is the ceiling above the floor of 4 — and no
  ceiling of its own, as the variable has none. Core resolves
  `(environment, setting) → the pool's configuration` in one pure
  function, `settings::resolve_max_readers`, that both binaries call where
  `pipeline.rs` read the variable. The environment override shows
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
  path), `load_default()`, `write(path, &Settings, broken) ->
  Result<Option<PathBuf>, WriteError>` (where a broken file went);
  `Settings::set_from_text(key, text)`, `Settings::reset_tab(tab)`;
  `parse_memory`, `MemorySpec` (`Gb(f64)` | `Percent(u32)`, `Display` is
  the normalised string), `memory_bytes(spec, total_ram) -> (bytes,
  MemorySource)`, `Settings::loupe_memory_bytes(total_ram)`,
  `Settings::cache_cap_bytes()`, `a1_frames(bytes)`,
  `resolve_max_readers(env, setting) -> Readers` (`Adaptive` |
  `Limit(n)` | `Environment(n)`, with `override_for_pool()`),
  `config_dir()` and its pure `config_dir_from(env)`, `total_ram()` and
  `parse_mem_total`.
- Constants: `FILE_NAME`, `WASH_MAX` 50, `WASH_DEFAULT` 25,
  `CACHE_CAP_FLOOR_BYTES` 256 MB, `A1_FRAME_BYTES` 149,299,200,
  `MAX_READERS_VAR`; `loupe::BUDGET_FLOOR_BYTES` 200 MB is the loupe's and
  this module reads it.
- `cache::default_cache_file()` (the path, no open), `cache::
  default_cache_path(cap_bytes)`, `cache::size_on_disk(db)`,
  `PreviewCache::clear()` (catalog-cache.md). `Pipeline::start(jobs,
  cache_path, threads, max_readers: Option<usize>)` (raw-pipeline.md).
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

- [ ] **AC1 — opening and closing.** File › Settings… and `Ctrl+,` open the
      dialog; `Esc` and Close close it; the keyboard returns to the grid
      (`key:+` zooms afterwards); a scrim click does not close it —
      `settings_opens_from_the_chord_and_the_menu_and_closes_with_esc_keeping_the_keyboard`
      (its menu strand Linux-only, like About's).
- [ ] **AC2 — containment.** Under the dialog `Y`/`N` mark nothing and
      `Ctrl+E`/`Ctrl+Shift+E` open nothing; About over it closes
      topmost-first; driven nav tokens are swallowed; the menu bar stays
      live — `settings_contains_every_grid_key_and_stacks_under_about`.
      `Ctrl+O`'s inertness is review-verified: the arm is the same scope
      rule, and a driven Ctrl+O that worked would open the native picker
      and hang the run.
- [ ] **AC3 — the tabs.** Three tabs in order, switched by `Left`/`Right`
      on the strip and by `Ctrl+Tab`/`Ctrl+Shift+Tab` from a field; digits
      never switch; `Tab` walks the controls and never leaves the dialog;
      Reset resets the active tab only; every field carries its note —
      `settings_tabs_switch_by_keys_and_never_by_digits`.
- [ ] **AC4 — the write.** Every commit writes `settings.toml` at once,
      preserving an unknown key and the user's comments; the field then
      shows the value in force; `Esc` discards an uncommitted field and
      closes — `a_settings_commit_writes_the_file_and_esc_discards_a_half_typed_field`
      (under `FASTCULL_CONFIG_DIR`); core
      `a_written_file_round_trips_every_key`,
      `a_write_preserves_unknown_keys_and_the_users_comments`,
      `a_created_key_carries_its_note_and_an_existing_key_keeps_its_comment`,
      `the_normalised_string_is_what_the_file_stores`.
- [ ] **AC5 — a broken file.** A malformed file yields the defaults, a
      status-line and a stderr warning naming the file and the error, and
      is never overwritten in place; the first write moves it aside under
      a reported name — core
      `a_malformed_file_yields_defaults_and_is_left_byte_identical`,
      `the_first_write_moves_a_broken_file_aside_and_writes_a_fresh_one`;
      app `a_malformed_settings_file_yields_defaults_and_is_moved_aside_on_the_first_write`.
- [ ] **AC6 — hermetic.** `FASTCULL_NO_CONFIG=1` makes `settings.toml`
      unreachable for load and save and the dialog says `Not saved`; no
      test touches the real file — core
      `config_dir_honours_the_override_then_no_config`; app
      `settings_under_no_config_applies_in_memory_and_writes_nothing`;
      every driven run carries `FASTCULL_NO_CONFIG=1`, and the two that
      write set `FASTCULL_CONFIG_DIR` to their own scratch dir.
- [ ] **AC7 — precedence.** With `FASTCULL_MAX_READERS` set the field is
      read-only with the environment's value and its note; unset, the
      file's value governs the pool; an unparsable value is ignored —
      core `the_environment_wins_over_the_file_for_max_readers`,
      `the_readers_resolution_feeds_the_pool_exactly_as_the_variable_did`
      (with `pipeline::tests::controller_override_caps_and_pins`); app
      `the_environment_wins_over_the_settings_file_for_read_workers`.
- [ ] **AC8 — auto-advance off.** `Y`/`N` keep the cursor and leave the
      selection alone; the filter exception moves the cursor and ends the
      selection as `U` does; on, as today — core
      `filter::tests::mark_advances_exactly_one_image` (the off row, now
      fed from the setting); app
      `auto_advance_off_keeps_the_cursor_and_the_selection_like_u`.
- [ ] **AC9 — the wash.** The committed percentage reaches the window's
      `selection-wash-opacity` at once, 0–50 inclusive, clamped — in
      `a_settings_commit_writes_the_file_and_esc_discards_a_half_typed_field`
      (`washprop=` reads the window property, not the model); core
      `out_of_range_values_clamp_on_read`.
- [ ] **AC10 — loupe memory.** GB and percentage forms parse, garbage is
      the default, the hint counts A1 frames, the floor and the total
      clamp, unknown RAM falls back and says so, and the engine starts
      with the bytes in force at the next folder open — core
      `the_memory_string_parser`,
      `a_percentage_with_unknown_ram_falls_back_to_the_default_and_says_so`,
      `the_frames_hint_counts_a1_frames`, `parse_mem_total_reads_meminfo`;
      app `loupe_memory_takes_effect_at_the_next_folder_open` (waits on
      `loupe engine started budget <bytes>`).
- [ ] **AC11 — the cache cap.** The file's cap is what
      `default_cache_path` enforces, app and CLI alike — core
      `defaults_are_the_specs_numbers` (the setting's default is
      `cache::DEFAULT_CAP_BYTES`) and
      `cache::tests::eviction_respects_cap_and_lru_order` (the eviction);
      the two call sites are review-verified, because every driven run
      is `FASTCULL_NO_CACHE` and the CLI's default cache is the real one.
- [ ] **AC12 — Clear cache.** The readout is the db + `-wal` + `-shm`
      size with the path; clearing empties the table and shrinks the
      file through a live connection and never unlinks it; the
      connection stays usable — core
      `cache::tests::size_on_disk_counts_the_wal_and_shm_files`,
      `cache::tests::clear_leaves_the_file_present_empty_and_smaller_and_the_connection_usable`
      (asserts the same inode on unix); app
      `clear_cache_is_off_under_no_cache` (the disabled row). The worker
      thread, the `Clearing…` state and the re-measured readout are
      review-verified: a driven run cannot have a cache (OQ in the plan).
- [ ] **AC13 — the Failed badge.** The badge shows the reason on hover and
      the status line carries it when the cursor stands on the frame —
      ui-grid.md's ledger
      (`the_failed_badge_shows_its_reason_on_hover_and_in_the_status_line`).
- [ ] **AC14 — the card.** The shortcuts card lists `Ctrl+,` and still
      fits whole at 1000×700 —
      `the_shortcuts_card_lists_every_binding_in_the_spec`,
      `shortcuts_card_is_a_two_column_sheet_that_fits_its_window`
      (ui-grid.md).
- [ ] **AC15 — docs.** `docs/settings.md` exists, CLAUDE.md's page map
      names it, and `docs/culling.md` and `docs/faq.md` follow the
      behaviour — review-verified.
- [ ] **AC16 — no budget row moves** — `tests/perf_budgets.rs` green in
      release on the idle seat (QE).

## History

- 2026-10-01 — Created (brief 008, issue #39; the Settings dialog, the
  file, the precedence rule and the five settings the specs had promised
  since 2026-07-25). ADR 0005 records the storage-and-precedence contract.
