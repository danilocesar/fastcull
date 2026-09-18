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
- `FASTCULL_NO_CONFIG=1` — `ui.toml` (the remembered copy and clip
  destinations, the template) unreachable for load and save; what
  `FASTCULL_NO_CACHE=1` does for `previews.db` (app-only; the CLI has
  `--no-cache`). The screenshot harness sets both unconditionally.
- `FASTCULL_KITCHEN_COOK_MS=N` — hold every kitchen cook for N ms before the
  pixel work: the pacing knob for the `open:PATH` session-swap test, which
  must catch the queue mid-flight in both profiles; default 0, off. Announced
  once on
  stderr when set (`fastcull: FASTCULL_KITCHEN_COOK_MS=N — every texture
  cook is held`); with tracing, the retarget reports how many queued jobs
  it dropped.
- `FASTCULL_MAX_READERS=N` — the read pool override (raw-pipeline.md).
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
  cover); `resize:WxH` in logical px — a REQUEST, gated with `wait:window
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
  `clip card`, `clip buttons`), resolved at dispatch time from a table the
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
  press is a stuck button, by design.
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
  form is outlawed (ui-grid.md).
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
  is up, the card being centred); `copy answer N|B|O|Esc laid out …`; `copy
  body scrolled to Y` / `clip body scrolled to Y` (0 at the top, negative
  going down, on change); `shortcuts card laid out …`; `status selected
  laid out …` / `status head laid out …`.
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
  cancelled is not finished.
- `sidecar writer closed gen N: K pending flushed` — N is the CLOSED
  session's generation, K the writes still inside their debounce; startup
  and process exit never trace it (xmp-sidecars.md).
- **Focus**: `focus: <what> gained|lost` from the `changed has-focus`
  handlers of the main scope (`keys`), each `iptc field N`, the keyword
  field, `copy dialog` and `clip dialog` — a `gained` with no matching
  `lost` from the previous holder is the dangling-weak signature;
  `focus-keys (<reason>)` — a claim was MADE, tagged at every call site:
  `swap`, `panel-open`, `panel-close`, `modal`, `rebuild`, `deferred` (a
  queued claim has ARRIVED — not the same event as its queuing),
  `copy-dialog`, `clip-dialog`, `cell-click`, `fit-click`, `overlay-click`,
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
at the top, negative going down). New fields are APPENDED; `dump_field`
finds `name=` by prefix.

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
  ran first.
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
- The suite drives nine geometries — 640x300, 900x800, 1000x700, 1024x768,
  1200x800, 1440x700, 1440x900, 1500x800, 1600x800 — plus the 1440x900 the
  app opens at, inside the Linux runner's pinned `1920x1200x24` xvfb
  screen; a test that drives past that raises the screen in the same
  commit.
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
