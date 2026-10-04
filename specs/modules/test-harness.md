# Module spec: the test harness (`FASTCULL_*` env vars, the drive script, the marks)

## Purpose

Everything a driven, headless run of `fastcull-app` can be told and can be
asked: the environment variables that ship in release builds, the
`FASTCULL_DRIVE` script and its tokens, the trace marks a script can wait on
and a test can assert, and the `QEDUMP` fields. Wayland offers no external
input automation, so this is how the screenshot suite drives the real
binary. It lives in `fastcull-app` (`harness.rs`, `shutter.rs`,
`presenter.rs`) — presentation plumbing, not business logic.

## Behaviour

### Environment variables

They ship in release builds, so a value leaked into some environment must
explain itself on stderr.

- `FASTCULL_TRACE=1` — eprintln every UI-thread phase over 20 ms, plus the
  loupe's rungs and every mark below. `wait:` does not need it: the switch
  decides what is printed, not what the app observes about itself; every
  test that waits traces anyway, because the failure is a trace line.
- `FASTCULL_DRIVE="6000:one2one;9000:grid;12000:quit"` — the drive script.
- `FASTCULL_NO_CONFIG=1` — the whole config dir unreachable for load and
  save: `ui.toml` (the remembered copy and clip destinations, the
  template), `templates.toml` and `settings.toml`, through the one
  resolver `settings::config_dir()`, in the app and the CLI alike (brief
  008, 2026-10-01; until then it covered `ui.toml` only, and every driven
  run read the user's real `templates.toml`); what `FASTCULL_NO_CACHE=1`
  does for `previews.db` (app-only; the CLI has `--no-cache`). The
  screenshot harness sets both on every run except seven, in four tests —
  the four runs of the test that drives the cache cap and Clear cache
  (settings.md AC11, AC12; the third a clear made to fail, QE 2026-10-02,
  round 5; the fourth the held clear, the kept thumbs and the Tab ring
  reaching Clear, brief 010, 2026-10-03), the cache-on run of the Settings
  card's fit test (settings.md,
  "The card"; QE 2026-10-01, D38), the Clear row of the click-away
  matrix (settings.md AC4; QE 2026-10-02, round 4 D39) and the cache
  strand of the Settings card's never-shrinks test (settings.md AC18; QE
  2026-10-03, D2) — this sentence said "every run but one" until D38,
  "three, in two tests" until round 5, the matrix's row uncounted,
  "five, in three tests" until brief 009's test-integrity review, and
  "six, in four tests" until brief 010 — which
  run without `FASTCULL_NO_CACHE`, through `shoot_with_sandboxed_cache`,
  which refuses to start unless `HOME` and `XDG_CACHE_HOME` both point
  inside the shots dir — so the default cache resolves there, never to
  the user's; Linux only, Windows' known-folder lookup ignoring the
  environment (QE 2026-10-01, D24). The Settings dialog still works under
  `FASTCULL_NO_CONFIG`, in memory, and says `Not saved` (settings.md).
- `FASTCULL_CONFIG_DIR=<dir>` — the config dir redirected to `<dir>`,
  winning over `FASTCULL_NO_CONFIG`, for the tests that read or write a
  config file in a scratch dir (settings.md AC6; corrected 2026-10-02, QE
  D45 and the senior developer's re-review RR-F6: it said "the driven
  tests that must prove a file was written", and the read-workers and the
  templates/ui.toml tests set it to read); announced once on stderr
  (`fastcull: FASTCULL_CONFIG_DIR=<dir> — settings.toml, ui.toml and
  templates.toml are read and written there`). Test plumbing in this
  family, not a setting (brief 008 OQ1).
- `FASTCULL_KITCHEN_COOK_MS=N` — hold every kitchen cook for N ms before the
  pixel work: the pacing knob for the `open:PATH` session-swap test, which
  must catch the queue mid-flight in both profiles; default 0, off. Announced
  once on
  stderr when set (`fastcull: FASTCULL_KITCHEN_COOK_MS=N — every texture
  cook is held`); with tracing, the retarget reports how many queued jobs
  it dropped.
- `FASTCULL_CLEAR_HOLD_MS=N` — hold the Clear cache worker for N ms, on
  the worker itself, before it opens its connection and clears: the pacing
  knob for the "Clear never blocks the UI thread" proof (settings.md AC12)
  — with the worker held, a `dump.` step fires on the UI thread while the
  row still reads `Clearing…`, and its line comes before the worker's own
  `settings cache clear ran on settings-clear` on the one trace, where a
  `join()` after the spawn puts it after; default 0, off. Announced once
  on stderr when set (`fastcull: FASTCULL_CLEAR_HOLD_MS=N — every cache
  clear is held`). Test plumbing in `FASTCULL_KITCHEN_COOK_MS`'s family,
  not a setting (brief 010 D3, 2026-10-03; brief 008 D13 and D42 stand).
- `FASTCULL_COPY_HOLD_MS=N` and `FASTCULL_CLIP_HOLD_MS=N` — hold the Copy
  Picks worker, or the video export's writer, N ms on the worker itself
  before its first file (its first frame), with the run's cancel flag
  polled at least every 10 ms through the hold, so a Cancel pressed during
  it ends the run with nothing copied or written: the pacing knob for the
  running dialog's keyboard ring and the refocus at the worker's finish
  (fileops.md and video-export.md, "The keyboard ring") — with the worker
  held, a `Tab` reaches the running Cancel while the dump still reads
  `copystate=1` (`clipstate=1`), where a 2 KB copy or a three-frame export
  ends in milliseconds and leaves no running state to drive; default 0,
  off. Read once per process, at the first run's start, and announced
  then on stderr (`fastcull: FASTCULL_COPY_HOLD_MS=N — every copy is
  held`, `fastcull: FASTCULL_CLIP_HOLD_MS=N — every video export is
  held`). Test plumbing in `FASTCULL_KITCHEN_COOK_MS`'s family, not a
  setting: core's `fileops::execute_held` and `clip::execute_held` take
  the hold, and `execute` is the same call without one (brief 011,
  2026-10-04: QE's test proposal P3, the senior developer's Shape A).
- `FASTCULL_MAX_READERS=N` — the read pool override (raw-pipeline.md);
  wins over the `performance.max_readers` setting, whose field the dialog
  then shows read-only (settings.md).
- `--screenshot <out>` — forces the software renderer (`take_snapshot`
  yields black frames on the GPU renderer), so the suite does not exercise
  the shipping femtovg renderer; snapshots are JPEG q92 whatever the
  extension; a far-panned 1:1 view snapshots BLACK beyond ~4096 px of pan
  (the software renderer's `Fixed<u16, 4>` offsets) — assert on the trace
  there.

### The drive script

`MS:ACTION;MS:ACTION;…`. Each step is an absolute single-shot timer from
`harness::install`, which runs after the session dispatch and the first
refresh. Only the FIRST colon separates MS from ACTION; `;` splits steps,
so no path or substring may contain one; a malformed entry is skipped
silently; a `wait:` rebases the steps after it. `pick`/`reject` write real
sidecars — scripts target throwaway copies of test data only.

- **Nav actions** — the names `handle_nav` takes (`left`/`right`/`up`/
  `down`, `pgup`/`pgdn`/`home`/`end`, `one2one`, `grid`, `zoom-out`, …, the
  Ctrl variants `ctrl-left`/`right`/`up`/`down`, `ctrl-pgup`/`pgdn`/`home`/
  `end`, `ctrl-burst-prev`/`next`, `select-toggle`). They call `handle_nav`
  directly, bypassing focus, and respect modal containment through the
  harness's own `if` — a MIRROR, not evidence: a containment test presses a
  real key.
- `quit`; `iptc` (the panel toggle); `about` / `shortcuts` (the modal
  toggles: the menu item's `activated` body — the visibility flag plus
  `modal-opened` — and nothing else; they do not force focus and cannot
  exercise the MenuBar's focus restore, which the click-driven tests
  cover); `settings` (the Settings dialog's toggle, brief 008: the menu
  item's body — `settings-open`, which re-reads the file, presents every
  field and claims the keyboard — when it is closed, `settings-close`
  when it is open; the same fidelity caveat as `about`); `resize:WxH` in
  logical px — a REQUEST, gated with `wait:window
  geometry WxH`; `scroll:N` — browse the grid to offset N without claiming
  the cursor, what the wheel does natively; `open:PATH` — the Open Folder
  action minus the native dialog (session swap, kitchen retarget,
  pipeline/loupe restart, marks flush, fresh grid zoom), live under a modal
  like the menu bar; `copydest:PATH` / `clipdest:PATH` — the destination
  pickers minus the native dialog, used BEFORE the `Ctrl+E` /
  `Ctrl+Shift+E` that should see them (neither replans an open dialog); `copytemplate:TEXT` — fills the
  rename field and replans as its `edited` callback would, used AFTER the
  `Ctrl+E` (opening clears the field); `filter:all|picked|rejected|
  unmarked` — the chip's own `set-filter` callback, the whole path; only
  those four names act; live under a modal, so a containment test clicks a
  chip.
- `key:<k>`, `key:ctrl+<k>`, `key:shift+<k>`, `key:ctrl+shift+<k>` — a REAL
  key press and release through `slint::Window::dispatch_event`, the true
  focus system, which the nav tokens bypass (only a dispatched event can
  land on no element). Named keys: `escape`, `return`, `tab`, `left`/
  `right`/`up`/`down`, `pgdn`/`pgup`/`home`/`end`, `f1`, `space`; anything
  else is literal text (`key:k` types k; `key:}` sends the shifted
  character). Actions are trimmed, so `key:ctrl+space` is the only spelling
  of that chord.
- `click.X,Y` — a real pointer move, press and release at window-logical
  coordinates, hit-tested by Slint: the in-window menu bar, panel fields
  and scrims are drivable. `click:<element>` — the same click at the CENTRE
  of the rectangle the app last reported for a self-reporting element
  (`iptc field N`, `copy card`, `copy buttons`, `copy answer N|B|O|Esc`,
  `clip card`, `clip buttons`, and since brief 008 `settings card`,
  `settings tab general|ui|performance`, `settings auto-advance`,
  `settings wash`, `settings loupe-memory`, `settings cache-cap`,
  `settings readers-adaptive`, `settings readers-limit`,
  `settings clear-cache`, `settings reset`, `settings close`,
  `failed badge <id>`, and since brief 011 `copy copy-close` and `clip
  export-close`), resolved at dispatch time from a table the
  layout marks write unconditionally; it echoes `drive ptr click X,Y
  (<element>)`, which a test reads to assert the click landed inside the
  rectangle. A name with no mark yet aborts the run loudly (`drive: click:
  no layout mark for <element> — abandoning the run`, exit non-zero). A
  mark is never retracted, so a name whose element has gone clicks whatever
  is under its last rectangle: a script names only elements it has just put
  on screen, and its outcome assertion catches the stale case. **A traced
  element is clicked by NAME, never by coordinate** (issue #70): on Windows
  the menu bar is the OS's, outside the client area, so every in-window y
  sits 40 px higher than under Linux's in-window bar. Coordinates stay right
  for what reports no rectangle — grid cells, the menu bar, the panel's
  padding strip.
- `press.X,Y` / `move.X,Y` / `release.X,Y` — `click.`'s phases as
  separately schedulable steps, carrying real inter-event timing, which is
  what makes a drag a drag (the issue #46 fling was one). `press.` dispatches
  a move first; scripts pair press and release themselves — an unpaired
  press is a stuck button, by design. `hover:<element>` — a real pointer
  MOVE to the centre of a named rectangle, no press, echoing `drive ptr
  hover X,Y (<element>)`; how a tooltip is raised (brief 008: the Failed
  badge's), with the same loud abort as `click:` for a name with no mark.
- `wheel.X,Y,DY` — a real scroll event, `DY` in logical px (60 = one
  notch-equivalent; positive = up), preceded by a move. `delta_x` is always
  0: horizontal scroll is undrivable, and nothing consumes it.
- `dblclick:X,Y` — replays Slint's real ordering, `clicked` (a re-centre)
  then `double-clicked` on the same release, invoking the callbacks
  directly with no hit-test.
- `wait:<trace substring>` — holds the REST of the script until a mark
  whose LABEL contains the substring has been emitted, marks emitted before
  the wait's own step included ("has this happened yet?", never "next").
  The steps after it keep the GAPS the script wrote, rebased on the moment
  it fires: a satisfied-instantly wait changes the schedule not at all, a
  late one shifts the tail bodily, a step timestamped earlier than the wait
  fires immediately when it is satisfied. Matching is against the label,
  not the `fastcull-trace: [ms]` prefix; the harness's own narration (the
  `drive:` echo, the pointer echoes, the modal-swallow line, the wait
  reports) is never observed; `QEDUMP` lines are. A never-satisfied wait
  prints `drive: wait never satisfied: <substring>` on the trace and on
  stderr after 30 s and exits non-zero. The 30 s runs from the STEP, so
  placement is part of the budget: a late wait's cap can outlast the
  shutter's 60 s readiness cap or the harness's 90 s child watchdog, which
  then kill the run WITHOUT that line; the steps after a wait are rebased,
  so in a multi-wait script the last wait installs later by whatever the
  waits ahead of it burned (the suite's longest carries four waits, 42 s
  of total wait time inside a 48 s tail). An empty substring is dropped as
  malformed. For a mark that is emitted more than once, put the thing that
  differs INTO the mark: `gen N`, `run N`, `row 0 (gen K)`.
- `dump.<label>` — the `QEDUMP` line of app state.

### The marks

- **Loupe rungs**: `loupe ready idx N long L` (the DECODE arrived — L its
  long edge, at or below 2048 a mid rung, above it the full-res); `loupe
  soft idx N factor …` / `loupe thumb idx N factor …` (a transit rung is on
  screen); `loupe idx N factor F extent WxH …` (the SHARP render: the
  full-res texture is on screen and the soft flag is cleared). `wait:loupe
  idx N factor` is the full-res-on-screen gate — every other `loupe …`
  line carries its own word between `loupe` and `idx`; keep the trailing
  ` factor` so `idx 1` cannot match `idx 10`; the sharp line re-fires on
  every pan of the same frame, so it answers "has this frame gone sharp
  yet", never "again". Also `loupe hold …` and `loupe overlay dropped …
  (hold cap)` / `(decode failed)` — the excuse-less `(no rung in hand)`
  form is outlawed (ui-grid.md). The `(decode failed)` drop fires only
  when the overlay was UP and wanted at the moment the failure landed:
  a failure that lands after a `(hold cap)` drop, with no rung re-raised,
  emits no drop line at all, so it is not the gate for "the app knows this
  frame failed" — the badge's layout mark below is (brief 010, 2026-10-03,
  issue #101).
- **Thumbs**: `thumb bytes idx N` (the pipeline read the embedded JPEG, at
  scan time) and `thumb landed idx N` (the kitchen decoded it into a
  texture — only for cells near the view, and nothing evicts it within a
  session). The landing carries no session generation and no index
  terminator (`idx 1` is satisfied by `idx 10`), so only a single-session
  script over a three-file fixture may wait on it, and it says a texture
  EXISTS, not that it was cooked at the current cell size.
- **Layout**: `iptc field N laid out at X,Y size WxH` (window-logical px;
  whenever the layout moves row N, and once at instantiation — rows 0 and
  1 only ever emit the latter, their first position being their last);
  `copy card laid out …`, `copy buttons laid out …`, `clip card …`, `clip
  buttons …` (from `changed absolute-position` and `changed height`; a
  card's mark is also the landing witness for a `resize:` while a dialog
  is up, the card being centred); `copy copy-close laid out …` and `clip
  export-close laid out …` (the Copy/Close and Export/Close buttons, from
  `init`, `changed absolute-position` and `changed height`; created per
  state, so the mark reappears with the button — brief 011, the senior
  developer's review F1); `copy answer N|B|O|Esc laid out …`; `copy
  body scrolled to Y` / `clip body scrolled to Y` (0 at the top, negative
  going down, on change); `shortcuts card laid out …`; `status selected
  laid out …` / `status head laid out …`; since brief 008 `settings card
  laid out …`, `settings tab <name> laid out …`, `settings <control> laid
  out …` for every control named under `click:` above, `settings note
  <name> laid out …` for each row's one-line note (the names below), and
  `failed badge <id> laid out …` (failed cells only, a handful). Since
  brief 009 (settings.md, "The card holds still") `settings card laid out
  …` fires exactly ONCE per open and never on a tab switch — the card has
  one height per open — and again only when the card grows (a text that
  affects its height changed) or the window is resized; a switch that
  commits a typed field carries such a growth when the commit changes a
  height-affecting text (settings.md, "The card holds still"; QE
  2026-10-03, D5); the strip's
  `settings tab <name>` marks likewise fire at the open and not on a
  switch; and three marks join them: `settings body laid out …` (the host
  of the three tab bodies, as tall as the tallest, from `init`, `changed
  height` and `changed absolute-position`), `settings notice laid out …`
  (the reserved notice line — one line tall when blank — the same three
  handlers) and `settings note <name>-env laid out …` (a row's environment
  line, `init` and `changed height` — every row has one, the line being
  `SettingRow`'s, so `auto-advance-env`, `wash-env`, `loupe-memory-env`,
  `cache-cap-env` and `readers-env`; 0 px tall unless a variable governs
  the row, which today only `FASTCULL_MAX_READERS` does, on Read workers;
  corrected 2026-10-03, brief 009's implementation: it named `readers-env`
  alone). `failed badge <id> laid out …` is also the gate for "the app
  knows `<id>` failed": the badge is created in the refresh that sees
  `<id>` enter the failed set, with the cursor's cell laid out — a failure
  on the cursor drops the overlay in that same refresh and the badge's
  mark follows the drop's (QE 2026-10-03, D3: this sentence had given the
  gap between them as a few milliseconds — a seat measurement, which
  belongs in the brief or the commit, and which load runs widened; the
  gate does not depend on the gap)
  — so it fires whatever the overlay's state, where the `(decode failed)`
  drop above does not; the failed-cursor test gates its first dump on it (brief
  010, 2026-10-03, issue #101: on a slow runner the failing decode landed
  2.3 s after the first End, and the second End's dump had read a cursor
  the app did not yet know had failed, 1 of 13 Windows debug runs). A
  `laid out at` mark prints its position rounded to whole px (`{:.0}`,
  harness.rs), and a pixel read at the mark's edge can be one row off what
  was drawn: on windows-latest (2026-10-03) the Settings tab cell's ring
  and underline sat one row below its mark, where on this seat and on
  ubuntu-latest they sit inside it — so a pixel strand reads a window that
  straddles an edge, or counts rows, never a band flush with a mark (brief
  010, the senior developer's review F1: a 3 px band flush with the tab
  cell's bottom edge held one underline row of two there and went red on
  a correct tree; the likeliest mechanism, a fractional layout position
  rounded one way by the mark and the other by the renderer, is
  unconfirmed without a Windows seat).
- **Settings** (brief 008, settings.md): `settings loaded from <path>` /
  `settings: no file (defaults in force)` / `settings: <path> could not
  be read: <error>` at startup and at every open — except an open while a
  write error stands, which reads nothing and traces `settings: not
  re-read (a write failed and none has succeeded since)` (senior-developer
  review F5 of brief 008); `settings opened` /
  `settings closed`; `settings committed <table>.<key> = <value>` then
  `settings written <path>` or `settings not written: <reason>`;
  `settings moved aside <path>`; `settings reset <tab>`; `settings
  wash|loupe-memory|cache-cap|readers-limit shows <text>` whenever a number
  field's text changes, typed or re-shown, and once when the dialog
  creates the field (brief 010, 2026-10-03: the locked Limit's shown value
  — the environment's — is read from its creation mark; until then a
  field showed its first text without a mark) — what the field DISPLAYS,
  where
  the dump's `wash=`, `loupemem=`, `cachecap=` and `readers=` are the
  model's (senior-developer review F1 of brief 008); `settings reset
  shows <text>` from the Reset button's own label, when the dialog creates
  it and whenever it changes — `Reset General to defaults` on General,
  the active tab's title in it (brief 010, 2026-10-03); `settings
  auto-advance|readers-adaptive shows true|false` whenever a checkbox's
  state changes, clicked or re-presented — what the box SHOWS, where the
  dump's `autoadvance=` and `readers=` are the model's (QE 2026-10-02,
  D39); `settings note
  auto-advance|wash|loupe-memory|cache-cap|readers|clear-cache shows
  <text>` from each note Text itself, when the dialog creates it and
  whenever its text changes — what the note says, never what the bridge
  meant it to (QE 2026-10-01, D22) — and `settings note readers-env shows
  <text>` likewise from the read workers row's environment line, which
  reports itself only while `FASTCULL_MAX_READERS` governs the row (QE
  2026-10-02, round 5; corrected 2026-10-03, brief 009: it said "exists
  only while" — the line is a permanent 0 px cell when blank now, no
  longer a conditional element, so the first layout counts it, and its
  `shows` mark is emitted only for a non-empty text — the absence a test
  reads without the variable still holds); `settings cache clearing` when the
  row turns to `Clearing…` and `settings cache cleared <before> ->
  <after>` (bytes) when the worker is done (QE 2026-10-01, D24); between
  the two, from the worker itself, `settings cache clear ran on <thread>`
  — the thread's own name, `settings-clear` (QE 2026-10-02, round 5);
  `settings cache readout shows <text>` from the Thumbnail cache row's
  Text and `settings clear-cache enabled true|false` from the Clear
  button, each when the dialog creates it and whenever it changes — what
  the row SAYS (`Clearing…`, then the re-measured size or the failure)
  and whether Clear is offered, where the dump's `cachereadout=` is the
  bridge's (QE 2026-10-02, round 5); `loupe
  engine started budget <bytes>` at every folder open — the budget the engine ADOPTED
  (`LoupeEngine::budget()`, floored), the proof that the loupe memory
  setting reached the engine (QE 2026-10-01, D23); `read pool started
  floor <F> cap <C>` at every folder open — the bounds the read pool
  ADOPTED (`Pipeline::read_pool_bounds()`), the proof that the read workers
  setting reached the pool (QE 2026-10-01, D27); a launch folder's own
  open emits both before `harness::install`, so a `wait:` on either needs
  an `open:`; `failed tooltip shown: <reason>` when the Failed badge's
  tooltip popup is instantiated.
- **Config reads** (brief 008 D11; QE 2026-10-01, D37): `templates loaded
  from <path>` / `templates: <path> could not be read: <error>` at every
  `templates.toml` read (the IPTC panel's open, every folder open; a missing
  file loads empty, so it is "loaded from" too), and `ui prefs read from
  <path>` at every `ui.toml` read (the copy and export dialogs, and the
  read half of each save) — each built from the very path the read used,
  so a driven run sees that both files go through the one config-dir
  resolver; under `FASTCULL_NO_CONFIG` there is no path, no read and no
  mark.
- **`load settled gen N: cursor pos P, `** — the CONTRACTUAL PREFIX, the
  whole substring a `wait:` registers; the tail differs by zoom and is
  free to (the scroll correction above one column; `scroll X kept (one
  column; the loupe block owns it)` at one). It means `metadata_complete()`
  went false → true: every image's thumb work FINISHED — the `Failed` arm
  counts too, so a folder of malformed RAWs settles with fewer `thumb
  bytes` lines than images — which makes it the right gate for "the load
  is over" and the wrong one for "every thumb exists" or "the textures are
  adopted" (`thumb landed` lands 36-110 ms behind it). Emitted at every
  zoom since issue #73. Only FOLDER sessions may wait on it portably: a
  `--synthetic` session is constructed with its thumbs done and settles
  inside the first laid-out refresh, which can precede `harness::install`
  (on Windows it did). A folder whose scan outlives the 30 s cap, and an
  EMPTY folder (no `view_len`), never settle and the run exits 1. `gen`
  counts from 0 for the launch folder; gen ≥ 1 always settles multi-column
  (`open_folder_at` resets the zoom); for gen ≥ 10 write the trailing
  colon (`wait:load settled gen 1:`). The tail must never contain another
  registered wait substring (it avoids `re-anchor`).
- **`copy finished run N`** / **`clip export finished run N`** — the report
  card went up; N counts the copies (exports) this PROCESS started,
  1-based, carried across a session swap; a bare `wait:copy finished`
  matches as a substring; a run cancelled by a session swap emits none —
  cancelled is not finished — while a run the Cancel button stops puts its
  report card up and emits it like any other (clarified 2026-10-04, brief
  011, QE's P3: "cancelled is not finished" is the swap's case, and the
  ring tests' cancelled strand waits on the mark).
- `sidecar writer closed gen N: K pending flushed` — N is the CLOSED
  session's generation, K the writes still inside their debounce; startup
  and process exit never trace it (xmp-sidecars.md).
- **Focus**: `focus: <what> gained|lost` from the `changed has-focus`
  handlers of the main scope (`keys`), each `iptc field N`, the keyword
  field, `copy dialog`, `clip dialog`, since brief 008 `settings
  dialog` and `settings strip`, and since brief 011 the Copy Picks and
  Export dialogs' ring controls (QE 2026-10-04, D2: this said "the two
  export dialogs'", and Copy Picks is not one) — `copy choose`, `copy
  template`, `copy open-dest`, `copy cancel`, `copy copy-close`, `clip
  choose`, `clip open-folder`, `clip cancel`, `clip export-close` — each
  from its own `changed has-focus`, so a ring's landing is the control's
  `gained` and the dialog scope's own `gained` is the keyboard back at its
  home (the export dialog's two Cancel buttons, the plan state's and the
  running state's, share `clip cancel`: they never coexist; a focused
  button that a state change destroys would emit no `lost` — the
  dangling-weak shape — so the dialog brings the keyboard home first and
  the button's `lost` lands with the scope's `gained`; QE 2026-10-04, D1:
  this said the destroyed button emits no `lost`, which holds only with
  that refocus removed) — a `gained` with no matching `lost` from the
  previous holder is the dangling-weak signature.
  `settings dialog` is
  the Settings dialog's own scope: `gained` when a press on the scrim, or
  on the card outside any control, hands it the keyboard (a FocusScope
  takes focus on a click), `lost` when the keyboard moves on from there.
  `settings strip` is its tab strip: `lost` when a control or the scope
  takes the keyboard and when the dialog closes, `gained` when the
  keyboard comes back to it from elsewhere in the dialog (a tab switch
  from a control, `Tab`/`Shift+Tab` round the ring) — NEVER at the open
  itself, though that is where the keyboard lands: the open's claim is
  made in the dialog's `init`, before the strip's tracker exists, so the
  landing is the tracker's baseline (Cargo.toml's second canary, fact 5),
  and a test proves it by `focusowner=-1` and a `key:right` that switches
  tabs (corrected 2026-10-01, senior-developer review F3: this sentence
  said both marks fired where the keyboard lands on open);
  `focus-keys (<reason>)` — a claim was MADE, tagged at every call site:
  `swap`, `panel-open`, `panel-close`, `modal`, `rebuild`, `deferred` (a
  queued claim has ARRIVED — not the same event as its queuing),
  `copy-dialog`, `clip-dialog`, `settings-dialog`, `settings-close`,
  `cell-click`, `fit-click`, `overlay-click`,
  `template-apply`, `revert`, `field-clear`, `field-accepted`,
  `keyword-removed`, `keyword-accepted`, `keyword-init`, `keyword-watch`,
  the two behind-a-cover bounces, `row N (gen K)` (the rebuild reclaim; K is
  `iptc-rebuild-gen` at the row's birth, so `wait:row 0 (gen K)` holds keys
  until THIS rebuild's claim has landed — issue #69; a script that gains or
  loses a rebuild re-reads K) and `<why> -> row N` (`menu -> row 0`,
  `restore -> row 0`); `iptc rows rebuilt (gen G)` (just before the model is
  replaced); `iptc keyword field created` (the keyword editor's `init`).
  Read together they answer who held the keyboard, what destroyed it, who
  asked for it back and when the claim landed. A DISMISSED menu emits no
  claim mark, so that strand stays on the clock.
- **`window geometry WxH grid GWxGH`** (issue #65) — from
  `presenter::detect_drift` when the geometry it compares has changed, and
  once at the first laid-out refresh. A PANEL TOGGLE emits none (its path
  consumes the change first) — no toggle can satisfy a geometry wait, and a
  script gating on a toggle waits on a panel mark. Both terms LOGICAL px (a
  HiDPI runner divides `Window::size()` by the scale factor). The wait is
  an exact substring match on a `{:.0}`-rounded size: a fractional-scale
  runner can grant `1199x800` for `1200x800` and the wait hangs its 30 s —
  ask for a size that survives the scale, never loosen the match. A
  satisfied wait promises the LAYOUT reached that geometry, not that the
  window stays there (the development seat reverts `1200x800` to 1440x900
  within ~35 ms, while `1440x700`, `1000x700` and `1300x750` stick); a test
  that needs "and it stayed" reads `geometry at shutter`.
- `geometry at shutter …` and `status at shutter: …` — once per run, at
  the end; `about version …` at startup; `grid relayout re-anchor:` /
  `relayout re-anchor: cursor kept at pos …`; `drive swallowed by modal`;
  `pan fold`.

### The dump

`dump.<label>` traces one `QEDUMP` line. `keysfocus` is the main scope's
real `has-focus` — NOT "the keyboard is alive": Slint sends a `FocusOut`
when the WINDOW is deactivated while `focus_item` keeps routing keys, so an
unfocused window reads false with a live keyboard; every focus test asserts
by ACTING or by the token. `focusowner=` is that token: `0` the main key
scope, `1..=N` panel field row i (written `i + 1`), `N+1` the keyword
field, `-1` a dialog's own scope. Then: the loupe and zoom state with the
pan block (`soft`, `vx`/`vy`, the fractional centre, the desired factor,
`one2one`, `zf`, `cursor`, `zoom=`); panel and modal visibility;
`status=` (the whole line); `revert=`; `selected=` (the selection count);
`template=` (the rename template); the copy block — `copy=`,
`copystate=` (0 plan, 1 running, 2 report, 3 the clash question),
`confirm=`, `summary`, `copynote=`, `report=`, `newonly=`, `nudge=`,
`nudged=`, `warning=`, `copyprogress=` (`Starting…`, then each running
line, the last surviving into the report), `copyerror=`; the clip block —
`clip=`, `clipstate=`, `clipavail=`, `clipsummary=`, `clipskipped=`,
`cliperror=`, `clipreport=`, `clipconfirm=`, `clipprogress=` (the export's
running line, the twin of `copyprogress=`), `cliphint=`, `exported=`,
`curexported=`; and `vpy=`, the grid Flickable's offset in Slint's sign (0
at the top, negative going down); then the settings block (brief 008) —
`settings=` (the dialog's visibility), `settingstab=` (0 General, 1 UI, 2
Performance), `settingsfile=` (the path, or `none`), `settingsnote=` (the
dialog's notice line), `autoadvance=`, `wash=` (the model's percent),
`washprop=` (the WINDOW's `selection-wash-opacity`, `{:.3}` — the field
that proves a commit reached the renderer, not the model),
`loupemem=` (bytes in force), `loupehint=` (the hint text), `cachecap=`
(bytes), `readers=` (`adaptive`, `limit:N` or `env:N`), `readersenv=`
(the variable's raw value or empty), `cachereadout=` (the row's text);
then `thumbtex=` (brief 010, 2026-10-03), the number of decoded thumb
textures the session holds (`TextureStore.images`) — what "the open
session keeps its painted thumbs" after Clear is read from (settings.md
AC12).
New fields are APPENDED; `dump_field` finds `name=` by prefix. The
nav-token swallow mirror (`drive swallowed by modal`) covers the Settings
dialog like About and the card.

### The shutter

`--screenshot` arms a readiness predicate per launch mode: at a grid zoom
the 1.5 s floor; `--start-loupe` the mid-or-better texture; `--start-11`
the full-res adopted for the 1:1 frame — a decode-FAILED final cursor above
fit trips the cap and exits 1, so a script that visits a failed image at
1:1 must END on a decodable cursor. A 60 s readiness cap runs from
`shutter::arm` and is not paused while a drive step is pending. The shutter
WAITS for the whole script to have executed before it may fire (a fast
release build otherwise captured a half-driven state). It fires EXACTLY
ONCE (issue #77): the 250 ms poll returns at once when `shot_written` is
set — before that a Wayland seat with a capture longer than the period
photographed twice, and since every test-side reader takes the LAST
`status at shutter` line, a local green could be the second capture's. The
readiness gates do not re-arm for a swapped-in session: a script that
needs the post-swap session settled holds the shutter with a late trailing
action. Exits: the cap refusal and a write failure 1; `finish` before a
shot 2.

### Rules for script authors

- Gate on marks, not clocks. Keep the absolute step as a backstop, insert
  the wait in FRONT of the consumer, and assert the `(satisfied` echo — a
  dropped or misspelled token puts a script back on the clock in silence —
  and, where order matters, the byte-offset ORDERING (`stderr.find(mark) <
  stderr.find("drive: …")`), since the echo proves a wait ran, not that it
  ran first. The mark a wait gates on is the first observable consequence
  of the STATE the assertion reads, and one that fires whatever else the
  view is doing: a mark conditional on a second state (the `(decode
  failed)` drop needs the overlay up) can leave the wait hanging on a run
  that is otherwise correct (brief 010, 2026-10-03, issue #101).
- Positional navigation waits on `load settled gen N`: the view is in
  provisional filename order until the settle re-sorts it. A shot that reads
  RENDERED pixels waits on the textures it reads (`thumb landed idx N`),
  never on the settle alone.
- A geometry change waits on `window geometry WxH`; the `PIN_WINDOW`
  scripts ask for the size the window already has and gate on their own
  layout waits.
- Fixed-time keys after a rows rebuild lose to a slow claim: wait on
  `row 0 (gen K)` (issue #69).
- Never assert an ORDER two independent events may land in (issue #50
  reddened CI in ~15 % of runs); assert what binds after both are known.
- A synthetic session (`--synthetic N`, `--bursts`) may not wait on the
  settle; a folder session may.
- Every test in `tests/screenshot.rs` takes one process-wide mutex as its
  first statement; all four test invocations run `--test-threads=1` under
  `RUST_BACKTRACE=1`; the two suite passes write to separate temp dirs, and
  the `screenshot-evidence-<os>` artifact keeps `*.jpg` and `*.trace.log`
  for 30 days, on green runs too (a Windows red is read against the same
  test's Linux green).
- A `#[cfg(unix)]` test takes its private helpers with it — clippy at `-D
  warnings` on the Windows job refuses dead code — and helpers shared with
  a platform-neutral test are never gated.
- A campaign that reuses a fixture across runs resets it or asserts a
  delta. `keysfocus` counts are seat-sensitive context, never a verdict.
- The menu-click strands are Linux-only (`menu_clicks_are_calibrated()` is
  `!cfg!(windows)`): no dispatched pointer event reaches an OS menu bar.
- The suite drives eleven geometries — 640x300, 900x800, 1000x400 (brief
  009, the Settings body giving before its footer), 1000x700, 1010x520
  (the shortcuts card clamped), 1024x768, 1200x800, 1440x700, 1440x900,
  1500x800, 1600x800 — plus the 1440x900 the app opens at, inside the
  Linux runner's pinned `1920x1200x24` xvfb screen; a test that drives past
  that raises the screen in the same commit (corrected 2026-10-03, brief
  009 commit B: it said ten, and nine before brief 009 — 1010x520, driven
  since 2026-09-04, was never counted; `grep -o 'resize:[0-9]*x[0-9]*'
  crates/fastcull-app/tests/screenshot.rs | sort -u` lists them).
- The suite's size: 122 driven tests after brief 011, 120 after brief
  010 and 118 at its start (`cargo test -p fastcull-app --test screenshot
  -- --list`, re-measured 2026-10-04 at brief 011's commit C). They no
  longer fit one 600 s foreground call in debug on the development seat
  and run there as three `--exact` thirds split from that list: 349 s +
  298 s + 323 s, 970 s, in debug on the idle seat at brief 011's commit C
  (327 s + 280 s + 325 s, 932 s, for the 120 at brief 010's commit E;
  326 s + 271 s + 311 s, 908 s, for the 118); halves would run some
  485 s each, too near the cap (brief 010 R8 and D5; this sentence said
  two halves and left the figures to be measured until brief 010's
  implementation; the "87 tests, 318 s + 288 s" of the agent files dates
  from 2026-09-12, before briefs 008–009 added 31).
- CI facts: a pull request's runs share one concurrency group per ref with
  `cancel-in-progress` (a run that vanishes without a verdict is a cancel,
  not a hang); every other event gets its own group; the job cap is 90
  minutes, set to clear a COLD Windows job (58-72 min measured), so a test
  that adds wall clock to the Windows job spends headroom that is measured;
  both runners are 4 vCPU with ~16 GB, recorded in each run's summary. The
  profile matrix: `has_display()` is `cfg!(windows)`, so on Windows `cargo
  test --workspace` runs the screenshot suite in DEBUG and the release step
  runs it a second time; on Linux the debug step has no display and only the
  xvfb release step runs it; CI's release steps run the screenshot target
  and the perf budgets, not the unit tests.

## Contracts

- The substrings above are registered when the script is parsed and
  `observe()` is `label.contains(needle)`: a mark's text is a contract, and
  changing one changes what every waiting test can see.
- The constants: the 250 ms poll, the 1.5 s floor, the 60 s readiness cap,
  the 30 s wait cap, the 90 s child watchdog; 60 logical px per wheel
  notch; `OVERLAY_HOLD_CAP` 250 ms (ui-grid.md).
- The layout-mark table is written unconditionally, so a resolved click
  never depends on whether the run also asked for a trace.
- The version canary in `crates/fastcull-app/Cargo.toml` records the Slint
  and winit behaviours the shutter and the focus marks depend on.

## Acceptance criteria

- [x] The shutter fires exactly once per run, on every seat and in every
      profile — `shoot_env_stderr_watching` in `tests/screenshot.rs` counts
      `status at shutter` lines (prefix- and label-anchored) in every
      successful traced run and fails on any count but one; the mutant
      (the guard deleted) doubles on a Wayland seat.
- [x] `wait:` mechanics: a satisfied-instantly wait leaves the schedule
      untouched, a late one shifts the tail, a never-satisfied one exits 1
      with its line; the converted scripts carry `(satisfied` and ordering
      assertions that go red when the gated step is hand-shifted ahead of
      the mark, when the wait step is deleted, or when the token is
      misspelled.
- [x] The notch size: 59 logical px fire nothing and the 60th fires exactly
      one stop, residue carried — `overlay_wheel_still_zooms_one_stop_per_notch`.
- [x] `resize:` is gated: the six resize tests are 6/6 red at the wait with
      the token neutered.
- [x] Pointer routing through real hit-testing — the five issue #13 tests
      (ui-grid.md).
- [x] The badge pixel criterion replays over a CI artifact from either
      platform — `assert_badge_pixels`.

## History

- 2026-10-04 — Brief 011, QE's spec corrections D1 and D2 (its defect
  D6): the Focus bullet said a button a state change destroys emits no
  `lost` — on the shipped dialogs it does, beside the scope's `gained`,
  because the keyboard goes home before the button goes (`copy finished
  run 1` → `focus: copy dialog gained` + `focus: copy cancel lost`, driven
  since QE's P3), and the no-`lost` shape is the refocus removed; and it
  called Copy Picks an export dialog.
- 2026-10-04 — Brief 011, QE's test proposal P3 (the senior developer's
  Shape A), spec first: `FASTCULL_COPY_HOLD_MS` and `FASTCULL_CLIP_HOLD_MS`
  hold the copy worker and the export writer before their first file,
  cancellable, so a driven test can reach the running Cancel and wait for
  the finish — a 2 KB copy is over before any key lands, and the other
  shape, 1.2 GB of sparse fakes, would have written 1.2 GB per run into
  the temp directory, and allocated it on Windows; the finished-run
  marks' sentence says a run the Cancel button stops emits its mark too,
  which the cancelled strand waits on.
- 2026-10-04 — Brief 011, the senior developer's review F1: the Copy/Close
  and Export/Close buttons report their layout (`copy copy-close`, `clip
  export-close`), so a driven test clicks them by name.
- 2026-10-04 — Brief 011 commit C: the suite's size re-measured — 122
  tests, three thirds of 349 s + 298 s + 323 s in debug on the idle seat.
- 2026-10-04 — Brief 011 (issue #98): the focus-mark family gains the two
  export dialogs' ring controls, nine names; no new token and no new dump
  field — `key:tab` and `key:shift+tab` existed.
- 2026-10-03 — Brief 010, QE round 1 D3: the known-failed gate's sentence
  keeps the order of the drop's and the badge's marks and loses the
  seat-measured gap between them, which the gate does not depend on.
- 2026-10-03 — Brief 010, the senior developer's review F1: a `laid out
  at` mark's edge can be one row off the drawn pixels (windows-latest), so
  a pixel strand straddles the edge or counts rows.
- 2026-10-03 — Brief 010 commit F: the suite's size measured — 120
  tests, three thirds of 327 s + 280 s + 325 s in debug on the idle seat.
- 2026-10-03 — Brief 010 commit C: the number fields' creation `shows`
  marks, `settings reset shows`, `FASTCULL_CLEAR_HOLD_MS` and the dump's
  `thumbtex=` land as written above.
- 2026-10-03 — Brief 010 agreed, spec first: `FASTCULL_CLEAR_HOLD_MS` (the
  held Clear worker, the never-blocks proof); the number fields' and the
  Reset button's creation marks, `settings reset shows`; the dump's
  `thumbtex=`; `failed badge <id> laid out` named as the known-failed gate
  and the `(decode failed)` drop's condition recorded (issue #101); a
  seventh run without `FASTCULL_NO_CACHE`; the script-author rule on
  conditional marks; the suite's size recorded for re-measurement.
- 2026-10-03 — Brief 009's test-integrity review: a sixth run, in a
  fourth test, drops `FASTCULL_NO_CACHE` through the sandbox — the
  never-shrinks test's Linux cache strand.
- 2026-10-03 — Brief 009 commit B: the suite drives eleven geometries,
  1000x400 joining and 1010x520 (driven since 2026-09-04) counted at last.
- 2026-10-03 — Brief 009's implementation: the environment line's layout
  mark is every row's, `settings note <name>-env laid out`, not
  `readers-env`'s alone (the line is `SettingRow`'s).
- 2026-10-03 — Brief 009 agreed, spec first: `settings card laid out` and
  the `settings tab` marks fire once per open and never on a tab switch;
  `settings body laid out`, `settings notice laid out` and `settings note
  readers-env laid out` added; the environment line's `shows` sentence
  corrected (a permanent cell, reporting only when it speaks); 1000x400
  joins the geometries.
- 2026-10-02 — QE round 5 of brief 008: `settings cache clear ran on
  <thread>`, `settings cache readout shows`, `settings clear-cache
  enabled`; the cache test's third run, and the run count corrected to
  five in three tests (the matrix's Clear row had gone uncounted).
- 2026-10-02 — QE round 5 of brief 008: `settings note readers-env shows`,
  the environment's line on the read workers row.
- 2026-10-02 — QE round 4 of brief 008 (D45; re-review RR-F6):
  `FASTCULL_CONFIG_DIR` is for every test that reads or writes a config
  file, as settings.md AC6 already said.
- 2026-10-02 — QE round 4 of brief 008 (D39): `settings
  auto-advance|readers-adaptive shows`, what a checkbox shows (added with
  the checkboxes' fix, 22cd5cb; this line follows it).
- 2026-10-01 — QE round 3 of brief 008 (D38): a second run with the cache
  on, the Settings card's fit with a long cache path.
- 2026-10-01 — QE round 3 of brief 008 (D37): `templates loaded from`,
  `templates: … could not be read`, `ui prefs read from` — the config reads
  name the path they used.
- 2026-10-01 — QE round 2 of brief 008 (D27): `read pool started floor F
  cap C`, the read pool's adopted bounds, at every folder open.
- 2026-10-01 — Brief 008: `FASTCULL_NO_CONFIG` covers the whole config dir
  (`templates.toml` and `settings.toml` too); `FASTCULL_CONFIG_DIR`; the
  `settings` and `hover:` tokens; the settings and failed-badge marks and
  the settings dump block.
- 2026-09-17 — Moved out of ui-grid.md and reshaped (brief 007). The
  section as moved, with every measurement, is
  `specs/history/test-harness.md`.
- 2026-09-12 — The clash answer rows report themselves and are clicked by
  name; `copyprogress=`, `copyerror=`, `newonly=`, `nudge=`, `nudged=`,
  `warning=` (briefs 005, 006).
- 2026-09-06 — `filter:`, the Ctrl nav tokens, `key:space`, the status
  fragment's layout mark (brief 002).
- 2026-09-05 — The shutter fires once (issue #77); the settle mark at every
  zoom (issue #73).
- 2026-09-04 — `f1`; the CI audit's facts: concurrency groups, the 90-minute
  cap, the pinned xvfb screen, the evidence artifact.
- 2026-09-03 — `load settled gen N`, the `run N` finish marks, `sidecar
  writer closed`; the suite serial under `RUST_BACKTRACE=1`; settle is not
  texture.
- 2026-09-02 — `click:<element>` (issue #70), the Windows menu-bar fact.
- 2026-08-31 — `window geometry` (issue #65).
- 2026-08-30 — The focus marks and `focusowner=` (issues #63, #64).
- 2026-08-29 — `wait:` (issues #13, #61), `vpy=`, the routing tests.
- 2026-08-27 — `clipdest:`, `key:ctrl+shift+<k>`, the clip dump block.
- 2026-08-21/22 — `copydest:`, `copytemplate:`, `copystate=`, `confirm=`.
- 2026-08-09 — `press.`/`move.`/`release.`/`wheel.` (issue #46).
- 2026-08-03 — `key:`, `click.`, `dump.`, `FASTCULL_NO_CONFIG` (issue #41).
- 2026-08-02 — `FASTCULL_KITCHEN_COOK_MS`, `open:PATH` (issue #34).
- 2026-07-31 — `scroll:N`.
- 2026-07-30 — `dblclick:X,Y` (issue #11).
- 2026-07-27 — The shutter waits for the whole script.
- 2026-07-25/26 — `FASTCULL_DRIVE`, `resize:` (issue #16), `about`/
  `shortcuts` (issue #23), `iptc` (issue #12).
