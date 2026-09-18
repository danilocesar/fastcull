# test-harness.md before the 2026-09-17 reshape (brief 007)

Verbatim: the "Debug facilities" section as moved out of ui-grid.md that day. The live spec is `specs/modules/test-harness.md`.

---

# Module spec: the test harness (`FASTCULL_*` env vars, the drive script, the marks)

## Purpose

Everything a driven, headless run of `fastcull-app` can be told and can be
asked: the environment variables that ship in release builds, the
`FASTCULL_DRIVE` script and its tokens, the trace marks a script can wait on
and a test can assert, and the QEDUMP fields. Wayland offers no external
input automation, so this is how the screenshot suite drives the real
binary. Moved out of `ui-grid.md` on 2026-09-17 (brief 007), verbatim from
its "Debug facilities" section; the reshape into the brief's headings
follows in its own commit.

## Debug facilities (env vars, app-level)

Documented because they ship in release builds (validator finding):

- `FASTCULL_TRACE=1`: eprintln any UI-thread phase (`handle_nav`, `refresh`
  stages, texture adoption) exceeding 20 ms, plus the loupe's own rungs — the
  evidence channel for hang reports. The rungs are three sentences a `wait:`
  can tell apart: `loupe ready idx N long L` (the DECODE arrived — L its long
  edge, at or below `MID_RUNG_MAX_LONG` (2048) a mid rung, above it the
  full-res), `loupe soft idx N factor …` / `loupe thumb idx N factor …` (a
  TRANSIT rung is what is on screen) and `loupe idx N factor F extent WxH …`
  (the SHARP render: the full-res texture is the one on screen and the soft
  flag is cleared). `wait:loupe idx N factor` is therefore the
  full-res-on-screen gate: every other `loupe …` line carries its own word
  between `loupe` and `idx` (`ready`, `soft`, `thumb`, `hold`, `overlay
  dropped`), so not one of them contains that substring — where the bare
  `idx N factor` that `one_to_one_click_claims_the_keyboard` uses on purpose
  matches any rung (its own comment says why: the claim under test is the
  overlay's,
  which every rung has). Keep the trailing ` factor` so `idx 1` cannot match
  `idx 10`; the sharp line re-fires on every pan of the same frame, so it
  answers "has this frame gone sharp yet", never "again".
  The thumb path is traced at BOTH of
  its stages, because they are seconds apart and only the first touches
  the file: `thumb bytes idx N` (the pipeline read the embedded JPEG, at
  scan time) and `thumb landed idx N` (the kitchen decoded it into a
  texture — only for cells near the view, and nothing evicts it within a
  session, so the line is also "the loupe's thumb rescue is armed for N"
  for the rest of that session). A test that manufactures a mid-session decode failure
  needs both: the first says the corruption is safe to apply, the second
  that the rescue rung had a texture to skip (issue #50). Since 2026-09-03
  the landing is also what a shot GATES on when its assertion reads
  rendered content, which puts two of that mark's properties on the
  critical path: it carries no session generation, so an old session's
  landing satisfies a new session's wait and only a single-session script
  may wait on it; and it has no index terminator, so `idx 1` is satisfied
  by `idx 10`. Both are why the three shots gated this way run over
  three-file fixtures — view indices 0-2, one session each.
  The IPTC panel's field rows report their own geometry the same way:
  `iptc field N laid out at X,Y size WxH`, in window-logical px, emitted
  whenever the layout moves row N — and once per row when the conditional
  panel's items are instantiated, which is the moment the row becomes
  hit-testable at all (issues #13/#61). A driven click on a panel field is
  a point chosen before the app existed, and whether the field is THERE
  yet is a layout outcome a loaded machine can be seconds late with — so a
  script clicks it BY NAME (`click:iptc field 0`) and the test asserts
  afterwards that the point the harness resolved was inside the rectangle
  — the calibration guard, which since issue #70 reads that resolution
  instead of a coordinate the test repeats. Both hooks are
  needed: the instantiation report is the only one rows 0 and 1 ever emit
  (their first computed position is already their last), and the
  move report is what tells a script that a `resize:` has landed.
  The two dialog cards report the same way (issue #62): `clip card laid
  out at X,Y size WxH`, `clip buttons laid out …`, and the `copy` pair,
  from `changed absolute-position` and `changed height`. Their heights
  follow their content now, so a card's rectangle is a layout outcome
  rather than a number in the .slint file, and the property that matters
  — the button row is inside the card — is a relation between the two:
  `buttons.y + buttons.h <= card.y + card.h`. No screenshot can stand in
  for it: neither card clips, so a row laid out below its card is drawn
  over the scrim looking almost right and stays clickable. A card's mark
  is also the landing witness for a `resize:` while a dialog is up — the
  card is centred, so its x moves with the window's width.
  A dialog body that scrolls reports its offset the same way —
  `clip body scrolled to Y` / `copy body scrolled to Y`, 0 at the top and
  negative going down, emitted on change — because a body holds text and
  no cursor, so nothing else in a dump moves when PgDn does. `key:` also
  understands `pgdn`, `pgup`, `home` and `end` now; before issue #62 the
  grid's own PgUp/PgDn/Home/End were reachable only through the `nav`
  tokens, which bypass the key path. `f1` joined them with the shortcuts
  card's opener (2026-09-04; this sentence lacked it until 2026-09-06),
  and `space` with Ctrl+Space (brief 002): the step parser trims each
  action, so a literal trailing blank cannot spell the key, and
  `key:ctrl+space` is the only way to send the chord. The Ctrl chords
  have `nav` tokens as well (`ctrl-left`/`right`/`up`/`down`,
  `ctrl-pgup`/`pgdn`/`home`/`end`, `ctrl-burst-prev`/`next`,
  `select-toggle`) for a script that needs to bypass focus, and the
  status bar's selection fragment reports its rectangle as `status
  selected laid out at X,Y size WxH` (the grey head as `status head laid
  out …`), which is how a test reads its colour.
  `filter:all|picked|rejected|unmarked` switches the filter chip
  (2026-09-06, senior-developer review F1): it invokes the window's own
  `set-filter` callback with the string the chip passes, so a script gets
  the chip's whole path — the name-to-enum mapping, the view recompute
  and the cursor rules that follow it. It exists because the rules that
  only apply while a filter is ON had no driven proof: a mark that takes
  its frame OUT of the view (the `U` half of the collapse rule) and a
  cursor the filter has hidden (the guard on `select-toggle`) are
  unreachable without one, and the chips are not self-reporting
  elements, so `click:` cannot name one. Only those four names act; an
  unknown one does nothing rather than silently meaning `all`. Like
  `open:` it is harness plumbing, not a grid key, so **it stays live while
  a modal is up** (QE 2026-09-06, D3, measured: the view switched under an
  open shortcuts card, which no chip can do — they sit behind the scrim).
  A test about modal containment must therefore click a chip, never send
  this token.
  Two more marks let a driven run gate on the app instead of the clock
  (issue #62): `clip export finished run N` and `copy finished run N` fire
  when the respective report card goes up, and `load settled gen N`
  carries the session generation — `session-gen` counts from 0 for the folder the app
  opened with, so the second folder a script opens settles as `gen 1`.
  That generation is what makes the #13 "next occurrence" limitation
  survivable for a session swap: every session used to settle with the
  same sentence, so `wait:load settled` could only ever match the first.
  The `run N` on the two finish marks is the same idiom against the same
  limitation (issue #70): N counts the copies (respectively exports) this
  PROCESS has started, 1-based, incremented where the worker is launched
  and carried across a session swap like the remembered destination — so
  a script's second copy waits on `copy finished run 2` instead of being
  satisfied by the first one's mark, which is what the two clash tests
  replaced with an 800 ms and a 1.3 s guess (the copy one was a Windows
  red at v0.13.0: `copystate` read 1, the copy was still running). A bare
  `wait:copy finished` still matches, being a substring. A run CANCELLED
  by a session swap emits no mark at all — cancelled is not finished, and
  the dialog's report says which — so a wait for that run's number ends
  the script, correctly: nothing it waits for will happen.
  What the SETTLE means is the metadata predicate itself, not a moment in
  the render: `metadata_complete()` is `self.thumbs_done >= self.labels.len()`
  (state.rs), so `load settled gen N` says by DEFINITION that every image's
  thumb work FINISHED — a dump taken behind `wait:load settled gen N` reads
  `N thumbs loaded` by construction, not by a same-tick coincidence a
  pipeline change could break in silence. Finished is not the same as
  arrived, and the counter says which (QE 2026-09-03): `thumbs_done` is
  incremented at TWO sites in the pump — the `ThumbReady` arm, which also
  traces `thumb bytes idx N`, and the `Failed` arm, which traces nothing.
  A folder of malformed RAWs therefore settles with fewer `thumb bytes`
  lines than images, and the dump still reads `N thumbs loaded` because the
  presenter clamps the count to the image count. So the settle is the right
  gate for "the load is over" and the wrong one for "every thumb exists". Three limits an author has to know
  (2026-09-03). It is the bytes, not the pixels: the thumb TEXTURES are
  adopted afterwards, `thumb landed idx N` is that mark, and the Windows
  debug runner's clip-badge trace puts its three landings 36, 75 and 110 ms
  behind the settle — so a shot whose claim is RENDERED content gates on the
  landings, never on the settle alone. Whether it is observable AT ALL in a
  `--synthetic` session is PLATFORM-DEPENDENT and a script must not rely on it
  either way: a synthetic state is constructed with `thumbs_done: n`, so it
  settles inside the first laid-out refresh, and that refresh can fall on
  either side of `harness::install`. Measured on Linux (QE 2026-09-05),
  `--synthetic 4 --start-11` emits the settle at `[28]` and a wait on it IS
  satisfied, install having run first; the Windows artifacts show the first
  laid-out refresh possibly preceding install, and there the wait is never
  satisfied, burns the full 30 s cap and ends the run. Only FOLDER sessions
  may wait on the settle portably, and they may for a structural reason —
  their `thumbs_done` starts at 0 and is incremented only in the pump, which
  runs on the event loop, i.e. after install. And the mark
  is ZOOM-INDEPENDENT but its sentence is not (issue #73): the edge it
  reports — `metadata_complete()` false→true — has no layout term, so since
  #73 it fires at ONE column too, on the same edge, in the same phase of the
  same refresh pass, and a `--start-11`/`--start-loupe` script gates on it
  like any other. That newly reachable gate has two EDGES a `--start-11`
  author has to know, and both fail loudly rather than silently (QE
  2026-09-05): a folder whose scan outlives WAIT_CAP never settles inside the
  wait — 1000 files gave `wait never satisfied … (after 30 s)` and exit 1 —
  and an EMPTY folder never settles at ANY zoom, because the settle needs
  `can_anchor` and `can_anchor` needs `view_len > 0`, so that run burns the
  same 30 s cap and exits 1 too.
  What is CONTRACTUAL is the PREFIX `load settled gen {gen}: cursor pos
  {pos}, ` — that is the whole substring a `wait:` registers, `observe()`
  being a plain `label.contains(needle)`. The tail
  after it differs by zoom and is free to: above one column it reports the
  scroll correction (`scroll X -> Y  (cursor was …)`), at one column it
  reports `scroll X kept (one column; the loupe block owns it)`, because
  there NO correction is applied — the strip re-anchors through
  `claim_cursor_at_loupe` instead — and the `last_cursor_visible` a
  correction would be computed from is one pass stale there, so reporting
  one would be false twice over. Two rules bind whoever next touches the
  tail: it must not contain any other registered wait substring (it
  deliberately avoids `re-anchor`, which is a substring of both `grid
  relayout re-anchor:` and `relayout re-anchor: cursor kept at pos`), and
  the mark carries the same no-terminator hazard as `thumb landed idx 1` /
  `idx 10` — `wait:load settled gen 1` is a substring of `load settled gen
  10:`, so an author who ever needs gen ≥ 10 writes the trailing colon into
  the wait (`wait:load settled gen 1:`), which the parser permits because
  only the FIRST colon separates MS from ACTION. Since `open_folder_at`
  resets the zoom, gen ≥ 1 always settles multi-column; only gen 0 of a
  loupe launch can settle at one column. Counted over the 579 CI traces on
  disk (Windows debug, Windows release, Linux release), no generation is
  ever emitted twice in one run: 362 (run, generation) pairs, every one of
  them exactly once.
  `sidecar writer closed gen N: K pending flushed` (2026-09-03) is traced by
  a session SWAP once the old session's writer has drained: N is the CLOSED
  session's generation (read before the bump) and K how many writes were
  still inside their debounce window and were flushed by the close
  (`SidecarWriter::close`, xmp-sidecars.md). It is the observable behind
  "flushed on session close": since 2026-09-03 the swap-flush test asserts
  the structural fact — `K == 1` for a mark still inside its debounce —
  where it used to measure the pick-to-swap gap between two harness echoes
  against the 700 ms debounce with a stopwatch (the issue #58 shape). A
  `wait:` could not serve there in any case: the claim is that the debounce had NOT fired, and
  a wait only ever answers "has this happened yet". The startup path closes
  no writer and process exit goes through the drop, so neither traces it: the
  mark means "a swap closed it".
  **Focus, as it moves (issues #63/#64)**: `keysfocus` at a dump is one
  sample of a value that changes several times inside a single input
  dispatch, which is how a stranded keyboard shipped twice — a run could
  say the keyboard was lost, never by what. Four marks make the whole
  path readable, all through `trace_mark_with` so they cost nothing when
  tracing is off:
  - `focus: <what> gained|lost` — from the `changed has-focus` handler of
    the main key scope (`keys`), each panel field row (`iptc field N`),
    the keyword field, and each dialog scope (`copy dialog`, `clip
    dialog`). A `gained` with no matching `lost` from the previous holder
    is the dangling-weak signature, printed.
  - `focus-keys (<reason>)` — a claim was MADE, tagged at every call
    site: `swap`, `panel-open`, `panel-close`, `modal`, `rebuild`,
    `deferred`, `copy-dialog`, `clip-dialog`, `cell-click`, `fit-click`,
    `overlay-click`, `template-apply`, `revert`, `field-clear`,
    `field-accepted`, `keyword-removed`, `keyword-accepted`,
    `keyword-init`, `keyword-watch`, and the two behind-a-cover bounces.
    `deferred` is the one that says a queued claim has ARRIVED, which is
    not the same event as the caller queuing it — the gap between those
    two lines is exactly what issue #63 turned out to be.
  - `iptc rows rebuilt (gen G)` — the item-tree mutation itself, emitted
    just before `set_iptc_fields` replaces the field-rows model.
  - `iptc keyword field created` — the keyword editor's `init`, the other
    moment an editor can take focus without any click.
  Read together they answer "who held the keyboard, what destroyed it,
  who asked for it back, and when the claim landed" from one log.
  **`window geometry WxH grid GWxGH` (issue #65)** — emitted from
  `presenter::detect_drift` when the geometry it compares has changed
  (and once at the first laid-out refresh), i.e. at the instant a new
  geometry reaches the layout. Narrower than "every relayout", and
  deliberately: a PANEL TOGGLE relayouts the grid but emits no mark,
  because that path consumes the geometry change before `detect_drift`
  gets to compare. The upshot is a feature — no panel toggle can satisfy
  a geometry wait, so a script waiting for a window size cannot be fooled
  by a dock opening — but it is a carve-out, not a general rule, and a
  script that wants to gate on a toggle must wait on a panel mark
  instead. It is the acknowledgement `resize:` never had: `geometry at
  shutter` is the only other geometry witness and it fires once, at the
  end — exactly once on every seat since issue #77 (2026-09-05; the
  harness section says what fired it twice before, and the test harness
  now counts). **Both terms are LOGICAL pixels** — `Window::size()` is physical,
  so a HiDPI runner at scale 2 would report `2400x1600` and never match
  the `1200x800` a script asked for; the window size is divided by the
  scale factor and the grid terms are logical already.
  The wait is an EXACT substring match on a `{:.0}`-rounded logical size,
  which bounds where that holds: a fractional-scale runner (1.25, 1.5)
  can grant a non-integer logical size that rounds to a neighbour —
  `1200x800` requested, `1199x800` announced — and the wait then hangs
  its full 30 s and ends the run. The signature is a `never satisfied`
  line on an otherwise healthy machine, with a `window geometry` mark in
  the log one pixel away from the one asked for; the fix is to ask for a
  size that survives the runner's scale, not to loosen the match.
  **What a satisfied `wait:window geometry WxH` promises, exactly:** the
  app's LAYOUT reached that geometry — the relayout path ran, columns and
  cell sizes were recomputed for it, and the re-anchor logic saw it. It
  does NOT promise the window is still that size when the run ends.
  Whether the window STAYS is seat- and size-dependent, which is exactly
  why no test may assume it. On the development seat `resize:1200x800`
  measured 10 runs of 10 where the layout reached it and the compositor
  reverted the window to 1440x900 some 31-38 ms later, so `geometry at
  shutter` read `grid 1440x800` — while `1440x700`, `1000x700` and
  `1300x750` stuck on that same seat, and the validator's seat did not
  revert `1200x800` at all. The practical consequence, worth knowing
  when reading these tests: on a reverting seat the three tests that ask
  for `1200x800` (`grid_resize_shrink_keeps_content_anchored`, whose
  control run makes four scripts, `grid_resize_grow_at_bottom_stays_at_
  bottom` and `grid_resize_at_top_stays_at_top`) run their post-resize
  steps at 1440x900, so the grow case is really 1440 -> 1500. The
  relayout path they exercise is genuine either way; the geometry they
  exercise it AT is the compositor's choice. That is the honest limit of a request-with-no-reply,
  and it is enough for what the resize tests assert (the app's REACTION
  to a geometry change); a test that needs "and it stayed" must read
  `geometry at shutter` instead. See also the issue #61 paragraph in the
  test ledger, which is the same fact seen from the other side.
  **`keysfocus` IS NOT "the keyboard is alive" (issue #63, 2026-08-30) —
  every focus test must assert by ACTING or by the token.** Slint sends a
  `FocusOut` when the WINDOW is deactivated
  (`WindowInner::set_active(false)`), but `WindowInner::focus_item` is
  untouched and keeps routing key events to the same scope. So an
  unfocused window reads `keysfocus=false` with a perfectly live
  keyboard: proven in a driven run with no clicks at all — `keysfocus`
  went false on its own, and the next `key:+` zoomed the grid. Both of
  the reds originally reported for issues #63 and #64 read
  `keysfocus=false` at a dump whose run went on to zoom with a `+`, so
  the SYMPTOM those issues reported is this artifact; the ownerless
  window they led to is separately real and separately measured. A
  keystroke that acts cannot be faked by a deactivation, so
  `session_swap_mid_field_edit_discards_and_keeps_the_keyboard` now sends
  `key:+` 50 ms after the swap and requires the zoom, and the other focus
  tests assert `focusowner=` instead.
  That field is the fourth thing a dump carries about focus (appended
  2026-08-30): the owner token itself — `0` the main key scope, `1..=N` a
  panel field row, `N+1` the keyword field, `-1` a dialog's own scope. It
  answers "WHICH element does the app believe holds the keyboard", which
  is a different and stronger question than `keysfocus`'s "is the main
  scope's `has-focus` set". **Every `keysfocus` assertion in the
  screenshot suite was converted to it** (20 of them, 2026-08-30): the
  `=true` ones were false-REDS waiting to happen — two fired in release
  suites the same afternoon, one with `zoom` proving the menu action and
  the following `+` both worked, one in a run whose EVERY dump read
  `keysfocus=false` including those taken with no dialog up at all — and
  the `=false` ones were weak besides, since "not the main scope" is
  equally true of a stranded keyboard. `keysfocus` stays in the dump: it is still the only
  reading of the real `has-focus`, and comparing it against the token is
  how a deactivation is recognised.
  `winactive=` was considered and NOT added: Slint 1.17 exposes window
  activation only through `i-slint-core`'s internal `WindowInner::active`
  (the `.slint` language has no `Window.active`), and `fastcull-app` does
  not depend on that crate. The artifact is already readable in a trace
  as a `focus: … lost` that no `gained` follows and no `focus-keys (…)`
  precedes.
- `FASTCULL_DRIVE="6000:one2one;9000:grid;12000:quit"`: timed injection of
  nav actions (same names `handle_nav` takes, plus `quit`, `iptc` — the
  panel toggle, issue #12 — `about`/`shortcuts` — the modal toggles,
  issue #23 — and `resize:WxH` in logical pixels, issue #16: the
  wrong-photo-after-resize bug class needs real window resizes
  drivable or it ships regression-blind) for headless reproduction and
  QE runs — Wayland offers no external input automation.
  **`resize:` is a REQUEST, and a script must assert its landing with
  `wait:window geometry WxH` (issue #65).** The token calls
  `Window::set_size` and returns; the compositor is free to answer late,
  to answer with a different size, or never to answer at all — the issue
  #61 investigation measured 9 loaded runs in 10 where a `resize:1200x800`
  went unanswered for the life of the run. A test that does not wait is
  testing the DEFAULT geometry, and three of the six resize tests passed
  with the token neutered because their invariants hold at 1440x900 too.
  All six gate on the mark now; with the token neutered they are 6/6 red
  at the wait, naming the geometry that never arrived.
  Note which failure each half catches. An UNSATISFIED wait ends the run
  through the app's own `exit(1)` after the 30 s cap, so a compositor
  that never answers is loud on its own and needs no assertion. The
  `stderr.contains("wait:… (satisfied")` guards in the three tests whose
  invariants hold at any geometry catch the other failure: a wait step
  that was never REGISTERED — a dropped or misspelled token, a `;` eaten
  by an edit — where the app exits 0 and the run is back on the clock
  with nothing complaining. Driven NAV
  keys respect the modal containment exactly like real keypresses
  ("drive swallowed by modal" trace); `quit`/`iptc`/`resize` and the
  modal toggles themselves remain live harness plumbing, like the menu
  bar. That mirror is CONVENIENCE, not evidence: it is the harness's own
  `if`, not the FocusScope's, so a test that asserts containment must
  press a real key (`key:n`), not a nav token — the two containment tests
  did the latter for months and would have stayed green with the shipped
  guard deleted (issue #13's fidelity note). The `about`/`shortcuts`
  toggles are the menu item's own `activated` body — the visibility flag
  plus `modal-opened` — and nothing else: they do not force focus (that
  bare `focus-keys()` went away with issue #41), and what they cannot
  exercise is the MenuBar's post-activation focus restore, which is why
  the focus-sensitive tests reach the popups by clicking the real Help
  menu items — a strand gated by `menu_clicks_are_calibrated()`, so on
  the Windows runner those tests fall back to the token path and the
  fidelity fix is, in practice, exercised on Linux only (QE 2026-08-29).
  That gate is NOT about font metrics, which is what this paragraph and
  the helper's own comment used to say (corrected 2026-09-02, issue #70):
  on Windows there is no in-window MenuBar to click at all — the winit
  backend reports `supports_native_menu_bar()` there (its `muda`
  dependency) and the menus are the OS menu bar, outside the client area,
  where no dispatched pointer event can reach them. Within the Linux
  in-window bar the item geometry does follow the platform's font
  metrics, which is what the coordinates are calibrated for.
  `scroll:N` browses the grid to offset N logical px WITHOUT claiming the
  cursor — what the wheel does natively, and the one gesture the harness
  could not express, which is why a re-anchor that hauled a browsing user's
  viewport back reached review unnoticed (2026-07-31).
  `open:PATH` (issue #34) is the Open Folder menu action minus the native
  rfd dialog: it calls the same shared function the menu callback calls
  (session swap, kitchen retarget, pipeline/loupe restart, marks flush,
  fresh grid zoom), so a script can drive an app-level session swap
  mid-operation — the path the texture-kitchen gate found review-verified
  only. Like the menu bar it stays live while a modal is up (harness
  plumbing, not a nav key). The path is everything after the first colon,
  so a folder whose path contains `;` cannot be scripted (recorded
  limitation of the `;`-separated script format).
  `copytemplate:TEXT` (2026-08-22) fills the Copy Picks rename field and
  replans exactly as the field's own `edited` callback does, so a driven
  run gets the preview and the plan a real keystroke would produce
  without focusing a LineEdit and sending one key event per character.
  Use it AFTER the `Ctrl+E` that opens the dialog: opening deliberately
  clears the field (the remembered template is offered, never
  pre-applied), so a template set before it is wiped.
  `copydest:PATH` (2026-08-21) is the Copy Picks destination picker minus
  the native rfd dialog: it sets the destination the dialog shows on its
  next `Ctrl+E` (the open path keeps an already-chosen destination over
  the remembered ui.toml one), so a script can drive a real copy →
  hand-delete → copy run — the exact flow the copied-this-session re-run
  bug shipped through, untestable before (fileops.md, "already copied
  means still there"). Same `;` limitation as `open:`; use it BEFORE the
  `Ctrl+E` that should see it (it does not replan an open dialog).
  `clipdest:PATH` (2026-08-27) is the same thing for the video export's
  destination (video-export.md): the export writes a NEW KIND of file, and
  without this the whole flow — plan line, clash question, the `.mov` on
  disk — is unreachable headlessly. Same `;` limitation and the same
  "use it before the `Ctrl+Shift+E` that should see it" rule.
  `key:ctrl+shift+<k>` (2026-08-27) dispatches a real two-modifier chord,
  which the video export needs: `Ctrl+Shift+E` and `Ctrl+E` are two
  different actions and differ ONLY by the Shift modifier, so a harness
  that could not hold Shift could not tell them apart. `key:shift+<k>`
  alone holds Shift only (first used 2026-08-28 for Shift+`]`; the
  modifier is what separates it from `]`, and the `}` spelling is sent as
  plain `key:}`).
  Caveat for script
  authors (QE G2, 2026-08-02): the `--screenshot` readiness gates do not
  re-arm for the swapped-in session — a shutter that was already
  satisfied can fire while the new folder is still loading — so a script
  that needs the post-swap session settled must hold the shutter with a
  late trailing action (the drives-pending wait), as the issue #34 tests
  do.
  Malformed entries are skipped silently. Scripts may include mark actions
  (`pick`/`reject`), which write real sidecars — QE runs target throwaway
  copies of test data only.
  The `--screenshot` shutter WAITS for the whole drive script to have
  executed before it may fire (in addition to its readiness gates): a
  fast release build otherwise reaches readiness before late-scheduled
  actions run and captures a half-driven state — the same script must
  mean the same shot in every profile (found 2026-07-27 when
  settle-then-pin drive schedules moved past the 1.5 s floor).
  **The shutter fires exactly once per run, on every seat and in every
  profile (issue #77, 2026-09-05).** One `status at shutter` mark, one
  `geometry at shutter` mark, one JPEG — and the test harness's spawn
  helper asserts that count on every successful traced run, so a local
  trace and a CI trace mean the same thing. Until 2026-09-05 they did
  not: the development seat photographed TWICE in every run (10 of 10
  stock-debug runs of the loupe-resize script, the second shot following
  by about half a second (468-526 ms across 44 unguarded runs of that
  script on this seat, 2026-09-05: 474-488 and 468-485 plan-time,
  490-507 and 509-526 in the two old-red sets of the #77 guard — the
  #77 commit's and the fix commit's — and 480-504 and 504-515 in QE's
  two; the gap is the capture duration and moves with window size,
  profile and thermal state — always past the 250 ms period, which is
  the fact that matters; corrected 2026-09-05, QE D6, after D1's
  narrower 468-507 was itself overtaken by two later sets); 26 of 26
  and 25 of 29 in the two counts that found it during the #73
  discussion) while CI photographed once (0 of 660
  artifact traces on disk — eight passes of five CI runs, Windows debug
  and release and Linux release). The mechanism, from the sources this build pins
  (senior-developer plan 2026-09-05): the poll is a `TimerMode::Repeated`
  250 ms timer, and Slint re-arms a repeated timer at `now + period`
  BEFORE running its callback (i-slint-core 1.17.1 `timers.rs:283-284,
  389-393`); Slint runs due timers as the FIRST thing in every winit loop
  iteration, inside `new_events` (i-slint-backend-winit 1.17.1
  `event_loop.rs:599-612`), while `slint::quit_event_loop()` is a winit
  USER EVENT (`lib.rs:838-844`) handled later in an iteration
  (`event_loop.rs:546-557`). A capture callback longer than the period
  therefore returns with its own timer already overdue, and whether the
  quit or the overdue poll runs next is the platform's: winit's Wayland
  loop reads a user event posted during `new_events` only at its NEXT
  dispatch, so the overdue poll fires first and photographs again (winit
  0.30.13 `platform_impl/linux/wayland/event_loop/mod.rs`,
  `single_iteration`: `NewEvents` at 345, `pending_user_events` drained
  at 355, filled by the dispatch that precedes the iteration); its X11
  loop pulls user events off a channel in the SAME iteration
  (`platform_impl/linux/x11/mod.rs:512, 549`) and its Windows pump drains
  messages posted during `new_events` before the iteration ends
  (`platform_impl/windows/event_loop.rs:368-420`), so on both the quit
  lands first. Measured on the development seat, stock dev profile: the
  callback took 461-478 ms — the software renderer's `take_snapshot`
  ~110 ms, the JPEG encode and write ~360 ms, both dependencies at
  opt-level 0 — 210-230 ms past the period, and the second shot followed
  6 ms after the first callback returned (3 of 3 probed runs); the SAME
  binary on X11 (XWayland, `WAYLAND_DISPLAY` unset) fired once in 5 of 5
  runs with a callback of 1.8-1.9 s, which is the falsification: it is
  the delivery order, not the duration alone. Both conditions are
  needed — a Wayland seat AND a capture over 250 ms — which is why the
  Linux release runner (X11) and the Windows runners never doubled. The
  capture's cost is mostly WORKSPACE code: with dependencies optimised
  (issue #76) the same seat measured 236-239 ms for the loupe-resize
  script's 1440x700 window (1 of 10 runs still doubled, at 266 ms) and
  332-366 ms for the default 1440x900 window of the center-anchor
  script (2 of 2 doubled without the guard) — the software renderer
  dropped to 11-20 ms but the RGBA→RGB conversion in
  `write_snapshot_jpeg` runs at opt-level 0 — so the two-shot is not a
  stock-profile curiosity and the guard is load-bearing on every Wayland
  seat in debug. The fix is the smallest that is provable: the
  poll returns at once when `shot_written` is already set, so nothing
  after the first capture can photograph, whatever the platform delivers
  next; the readiness predicate, the 1.5 s floor, the 60 s cap and the
  two failure exits (cap refusal and write failure, exit 1; `finish`,
  exit 2) are untouched. Two things a reader of traces must know: every
  test-side reader of `status at shutter` takes the LAST line
  (`.lines().rev().find_map`), not the first — so before the fix a local
  green could be a green of the SECOND capture, taken half a second after
  the state CI photographs (QE, 2026-09-05: 12 of 25 double-shot runs
  described materially different states, one where the intended capture
  read `0/2 loaded` and the accidental one the exact `2 thumbs loaded`
  string an anti-vacuity assertion needed); and the count guard's
  old-red and its mutant are shown on a WAYLAND seat — the stock dev
  profile doubles every script there (10 of 10), the optimised one every
  default-window script (the center-anchor script: 2 of 2 without the
  guard, 0 of 3 with it) — and never on CI, whose seats deliver the quit
  before the overdue poll. With the guard: 10 of 10 single shots in the
  stock profile with a 486-505 ms capture, 3 of 3 in the optimised one
  with 345-366 ms (senior-developer plan 2026-09-05). The dependency
  behaviours are recorded in the version canary of
  `crates/fastcull-app/Cargo.toml`.
  `key:<k>` / `key:ctrl+<k>` (issue #41 sweep, promoted from QE
  instrumentation) dispatches a REAL key press+release through
  `slint::Window::dispatch_event` — through the true focus system, which
  the nav tokens bypass (they call `handle_nav` directly, so they are
  blind to the whole stranded-keyboard class: only a dispatched event can
  land on no element). Named keys: `escape`, `return`, `tab`,
  `left`/`right`/`up`/`down`, later `pgdn`/`pgup`/`home`/`end`, `f1` and `space` (recorded where they were added, above); anything else is sent as literal text
  (`key:k` types k). `ctrl+` synthesizes a held Control around the press.
  `click.X,Y` dispatches a real pointer move+press+release at
  window-logical coordinates, hit-tested by Slint — this makes the
  in-window menu bar drivable headlessly, including the menu's own focus
  save/restore machinery, plus panel fields and modal scrims. (Spelled
  with a dot: the visual break from the step's `MS:ACTION` colon keeps
  scripts readable.)
  `click:<element>` (issue #70) is the same click at the CENTRE of the
  rectangle the app last reported for a self-reporting element — the
  names of the layout marks above: `iptc field N`, `copy card`, `copy
  buttons`, `copy answer N` / `copy answer B` / `copy answer O` / `copy
  answer Esc` (the four rows of the clash question, which report
  themselves the way the cards do — brief 005, 2026-09-12: the mouse round
  of `copy_picks_asks_once_and_each_answer_does_what_it_says` clicked a
  coordinate that became the New only row when that row was added on top,
  so it clicks `copy answer B` by name now), `clip card`, `clip buttons`. The harness keeps those
  rectangles in a table written by the same callbacks that emit the marks,
  UNCONDITIONALLY (a resolved click must not depend on whether the run
  also asked for a trace log), and resolves the name AT DISPATCH TIME, so
  the point is the layout this run produced rather than one measured
  elsewhere. It echoes `drive ptr click X,Y (<element>)`, unobserved like
  the other pointer echoes, and that echo is what a test reads to assert
  the click landed inside the rectangle. A name with no layout mark yet is
  never a click into nowhere: the step traces `drive: click: no layout
  mark for <element> — abandoning the run`, prints the same sentence on
  bare stderr and exits non-zero — the `wait:` cap's shape, and for its
  reason (the step holds the shutter, so the silent alternative is a
  half-driven run photographed anyway).
  What the table cannot know is whether the element is still THERE: a
  mark is never retracted (Slint has no destroy hook to retract it
  from), so a name whose element has since gone — the panel closed, the
  dialog dismissed — resolves to its LAST rectangle and clicks whatever
  is under it now, silently (validator 2026-09-02: `key:i`, `key:i`,
  `click:iptc field 0` → a click into the grid, exit 0). A script names
  only elements it has just put on screen, and its outcome assertion —
  `focus: iptc field 0 gained`, the dialog's answer — is what catches
  the stale case; `assert_click_resolved` alone does not.
  **The rule: a traced element is clicked by NAME, never by coordinate.**
  A literal point is measured on one platform's layout and lands silently
  somewhere else on another. The measurement that made this a rule: on
  Windows the menu bar is the OS menu bar, outside the client area (see
  "Window chrome"), so every in-window y sits 40 px higher than under the
  Linux `fluent` bar's 40 px band — source-verified, and measured exactly
  that between the two CI runners on the first Windows artifact (see
  "Window chrome") — and the
  seven clicks, in five tests, that hit the Title field at `1290,177`
  were landing 43 px below its centre on Windows — three reds at v0.13.0
  (issue #70; the coordinate appeared 12 times in the file, seven script
  steps and five assertion markers). Coordinates remain right for what
  reports no rectangle: grid cells (derived from the column geometry), the
  menu bar, the panel's padding strip, the dialog answer rows.
  `press.X,Y` / `move.X,Y` / `release.X,Y` (issue #46; promoted from QE
  instrumentation like `key:`/`click.` before them, the PR #43
  precedent) are `click.`'s three phases as separately SCHEDULABLE
  steps — real dispatched pointer events that, spread over timed script
  steps, carry real inter-event timing, which is what makes a drag a
  drag: `click.`'s single-tick sequence has zero displacement and zero
  velocity, so no drag gesture (and no drag-derived defect class — the
  issue #46 fling was exactly one) was drivable headlessly before
  these. `press.` dispatches a move first so hover state is coherent,
  like `click.`; a `move.` while pressed extends the drag; scripts are
  responsible for pairing press/release (an unpaired `press.` leaves
  the button down, exactly like a real stuck button — that fidelity is
  the point).
  `wheel.X,Y,DY` (issue #46 gate finding) dispatches a REAL scroll
  event at window-logical coordinates, `DY` in logical px (60 = one
  notch-equivalent per this contract's accumulator; positive = up),
  preceded by a move so hover targeting is coherent. Promoted because
  the overlay's scroll wiring — which #46 rewrote — was reachable by
  no test and no Wayland automation: which surface receives a wheel,
  the two separate accumulators, and the post-Flickable coordinate
  terms were all review-verified only. `delta_x` is dispatched as 0 —
  horizontal scroll is undrivable until the token grows a fourth
  field (recorded limitation; nothing in the app consumes it today).
  `wait:<trace substring>` (issue #13, 2026-08-29) holds the REST of the
  script until a trace mark whose label contains the substring has been
  emitted. Every other step is an absolute single-shot timer, i.e. a
  guess about how long the app will take: three tests hand-rolled the
  missing primitive with observer threads and channels, and two clicked
  at a timestamp a loaded machine did not honour (issue #61 — the panel
  field was not laid out yet; the 1:1 texture was not up yet, so the
  point belonged to a different surface). The steps after a `wait:` keep
  the GAPS the script wrote, measured from the moment it fires (their
  timestamps are rebased on the wait's own), so a wait already satisfied
  when it comes due changes the schedule not at all and a late one shifts
  the tail bodily. Matching is against the mark's LABEL, not the
  `fastcull-trace: [ms]` prefix, and includes marks emitted BEFORE the
  wait's own step: the substrings are registered when the script is
  parsed, so `wait:thumb landed idx 11` is satisfied by a thumb that
  landed ten seconds earlier ("has this happened yet?", never "happen
  next"). Four recorded limitations of that shape: a wait cannot ask for
  the NEXT occurrence of a mark already emitted once — find a substring
  unique to the state you mean, or keep that step on the clock. (Waiting
  for the second session's settle was the example, and it is now
  expressible: the mark carries the session generation,
  `wait:load settled gen 1`, issue #62. That is the pattern for the
  limitation generally — put the thing that DIFFERS into the mark. The
  second instance is the rebuild reclaim, `wait:row 0 (gen K)` from
  `focus-keys (row 0 (gen K))`, where K is `iptc-rebuild-gen` at the
  row's birth, i.e. the number of content-changing rows rebuilds so far:
  it is what lets a script hold its keys until THIS rebuild's claim has
  landed instead of trusting a timestamp against the reclaim gap the
  owner-invariant table prices — issue #69. K is a property of the
  script, so a script that gains or loses a rebuild must re-read it; the
  failure is the loud one, a wait that is never satisfied. Two
  corollaries (validator 2026-09-02): the cursor-move script opens the
  panel only after `wait:load settled gen 0`, so a slow runner whose
  metadata lands after the panel opened cannot add a rebuild and shift
  K; and where the row's `gen` does NOT differ, the re-assert's own mark
  does — `focus-keys (<why> -> row N)`, `menu -> row 0` once a menu item
  has activated, `restore -> row 0` after a rebuild — which is what the
  menu-item strand waits on before its keys. A DISMISSED menu emits no
  claim mark of its own, so that strand stays on the clock, and its test
  says so. The third
  instance is `run N` on the copy/export finish marks); "past"
  starts at `harness::install`, which runs AFTER the session dispatch and
  the first refresh, so a mark from the opening scan or the first layout
  is never observed; only the APP is observed, never the harness narrating
  its own script (the `drive: <action>` echo, the pointer/wheel echoes,
  the modal-swallow line and the wait reports are all emitted unobserved,
  because each quotes the script's own text and would otherwise let a wait
  fire on a later step's echo — `QEDUMP` lines stay observable, being app
  state); and a substring cannot contain `;`, the step separator, which
  splits it first (the same limitation `open:PATH` carries).
  Because it is a plain substring, a wait can pin the GEOMETRY a
  script's coordinates were measured in — `wait:iptc field 0 laid out at
  1150` is satisfied only in a 1440 px-wide window — which matters because
  `resize:` is a REQUEST to the compositor, and under load it can go
  unanswered for the whole run: that is the other half of issue #61 (the
  click was fine, the window was never resized, and the panel was 240 px
  from where the script thought). A script that needs a non-default size
  must therefore wait for evidence of it, and one that only needs a KNOWN
  size should ask for the default it already has. A step whose timestamp
  is EARLIER than the wait's fires immediately when the wait is satisfied
  (the rebase saturates at zero); it does not run before the wait, and
  timestamps below the wait's own carry no meaning beyond their order.
  It does NOT require
  `FASTCULL_TRACE=1` — the switch decides
  what is printed, not what the app may observe about itself — though
  every test that waits also traces, because the failure below is a trace
  line. A wait that is never satisfied never lets the rest of the script
  through silently: its own step holds the shutter, and after 30 s
  (bounded under the screenshot harness's 90 s watchdog, so the app is
  still alive to say why) it prints `drive: wait never satisfied:
  <substring>` on the trace and on bare stderr and exits non-zero. Two
  budgets bound how long a wait may reasonably take: that 30 s buys the
  diagnostic only for a wait whose step comes due before ~60 s (later, the
  harness watchdog's generic timeout wins), and the shutter's own 60 s
  readiness cap runs from `shutter::arm` and is NOT paused while a drive
  step is pending — a wait that takes 25 s leaves ~35 s for the cursor's
  texture to arrive. That used to be a real margin in a debug build over
  a 50 MP frame — the full-res decode took 26-40 s on the Windows debug
  runner and 31 s on the development seat while the decoder compiled at
  opt-level 0 — and since 2026-09-05 it is ample in every profile: with
  dependencies optimised in the dev profile the same decode lands in
  about 2 s in debug (issue #76; the numbers and the decision are in
  `01-architecture.md`, "Build profiles"; corrected 2026-09-05,
  senior-developer plan). The 30 s runs from the STEP, not from install, which is what lets
  a wait target an event slower than the cap itself and makes a wait's
  PLACEMENT part of its budget — but SCRIPT time is not that budget
  (corrected 2026-09-04, validator F3): the steps behind a satisfied wait
  are rebased on the moment it fired, so in a multi-wait script the last
  wait INSTALLS later than its authored timestamp by whatever the waits
  ahead of it burned. The latest AUTHORED wait step in the suite is the
  clip-badge test's `wait:clip export finished run 2` at 33.2 s, in the
  script that also carries the most waits (four — 4.9 s, 16.9 s, 25.0 s,
  33.2 s — with a tail ending at 48.0 s), so its bound is not 33.2 + 30 but
  the harness's 90 s watchdog against that 48.0 s tail: 42 s of TOTAL wait
  time across the four, room enough for one of them to spend its whole
  30 s cap. Measured in release on the development machine 2026-09-04, all
  four are satisfied after 0 ms and the test takes 48.3 s. Past the 42 s
  the watchdog kills the child and the run reports a bare timeout, WITHOUT
  the `wait never satisfied: <substring>` line the 30 s cap exists to buy.
  The issue #46 M3 drag test's `wait:loupe idx 0 factor` is the cap-placed
  one, and since 2026-09-04 it is profile-split like
  `panel_toggle_at_one_to_one_reanchors_the_crop`: 20 s in DEBUG, because
  the sharp render it waits for lands at 26-40 s on the Windows debug
  runner (measured 2026-09-02 across that job's uploaded traces; 30.3 s in
  that test's own run, 18.1 s on the development machine), so its cap
  reaches 50 s where a step at 0 s would have ended those runs at 30 s and
  the fixed 45 s lead it replaced reached only 45; 1.5 s in RELEASE, where
  the same mark lands at 0.38-0.46 s (three runs) and its cap still reaches
  31.5 s. Those debug landing times are the stock dev profile's and are
  historical since 2026-09-05 (issue #76): with dependencies optimised
  the sharp render lands in seconds in debug too, so the 20 s placement is
  satisfied when it comes due and costs a debug run about 18 s of idle
  schedule — harmless, kept as is, and re-timed only on the PR's Windows
  debug artifacts, the first evidence of the new landing time on that
  runner (a schedule is re-timed on a measurement, never on an estimate;
  senior-developer plan 2026-09-05). The issue #46 M1 transit test's
  `wait:loupe idx 8 factor` (2026-09-05) is placed the same way, at 20.2 s
  in BOTH profiles: its End lands on a stone-cold frame at 20.05 s and the
  sharp it waits for landed 1.8-2.8 s later on the development seat idle
  and 9.3-14.4 s later under the #76 load recipe in debug (1.6-2.1 s in
  release under the same load), so its cap reaches 50.2 s against the
  shutter's 60 s; the `dump.landed` behind it keeps its authored 26.5 s as
  its floor — it fires 6.3 s after the wait is satisfied, never earlier
  and never on its own (measured 28.32-28.37 s idle, 39.4-40.6 s under
  the #76 load recipe; a never-satisfied wait aborts the run at 50.2 s
  with no dump at all — QE 2026-09-05, D3). That gate replaced a bare
  clock, which in a debug build under load photographed a legitimately
  dropped overlay before its re-raise (the ledger item above). The gaps after the wait are identical in both forms — a wait's
  tail is written in gaps, not offsets — and the 18.5 s the split takes
  off that one test is visible in the whole step: the release screenshot
  suite, serial, measured 451.5 s against the 468.8 s of the run before
  it (development machine, 2026-09-04). The
  corollary for authors: the steps after a wait keep their gaps from the
  wait's OWN timestamp, so gating a late-scheduled block means moving the
  block UP to the wait, not leaving it where the clock had it — a wait at
  20 s with a tail still written at 45 s lands that tail 25 s AFTER the mark
  it was supposed to follow. An
  empty substring would match the next mark whatever it is, so
  `wait:` with nothing after it is dropped like any other malformed step.
  `dump.<label>` traces the focus/surface state for test assertions:
  `keysfocus` (the main key scope's real `has-focus`, via the
  `dbg-keys-focus` debug property), loupe/zoom state, panel and modal
  visibility, the copy dialog's visibility, plan summary and rename
  template, the revert-slot label, and the status line. Since 2026-08-21
  it also carries `copystate=` (0 plan / 1 running / 2 report / 3 the
  clash question) and `confirm=` (the question's text): the clash question
  is a STATE of the Copy dialog rather than a second modal (fileops.md),
  so `copy=true` alone cannot tell a plan preview from a question about
  replacing files — without those two fields the one irreversible
  operation in the app would be assertable only down to "a dialog exists".
  Since 2026-09-12 (brief 005) it also carries `newonly=` (the New only
  row's label — its zero-new form says whether the run has anything to
  copy), `nudge=` and `nudged=` (the "Pick one: …" line's text, and
  whether an inert key raised it) and `warning=` (the amber line under the
  rows): the fourth answer's row, its key and its inert companions (`Y`,
  `Ctrl+N`) are otherwise assertable only down to `copystate=3`.
  Since 2026-09-12 it also carries `copyprogress=` — the running line
  (`Copying 2 / 2 — c.ARW`; `Checking 1 / 12 — …` under Overwrite), the
  copy's twin of `clipprogress=`. Its last value survives into the report
  card because nothing resets `copy-progress` when a run ends
  (`start_copy` writes "Starting…", the pump's `File` arm writes each
  line, nothing else writes it), so a driven test reads the FINAL line
  after `wait:copy finished run N` instead of sampling a running one (QE
  2026-09-12, D3).
  Since 2026-09-12 (brief 006) it also carries `copyerror=` — the
  plan-time refusal on the copy dialog (the free-space sentence *"The
  copy needs 8.0 TB and there is 358.2 GB free at the destination."*, a
  destination that is a file, a template that makes a path), the copy's
  twin of `cliperror=`: the refusal's wiring is driven (fileops.md, brief
  006 AC2), and without the field a driven run could see `copystate=0`
  after an answer but not whether the drop-back said why in the dialog's
  words or in core's raw byte counts.
  Since 2026-08-27 the same block exists for the video export —
  `clip=`, `clipstate=` (the same four states), `clipavail=` (is there
  anything to export), `clipsummary=` (the plan line), `clipskipped=`,
  `cliperror=`, `clipreport=` and `clipconfirm=` — for the same reason:
  it is the app's second irreversible file operation, and its plan line
  is the only place the user is told the frame rate before pressing
  Enter. Keyboard focus was otherwise INVISIBLE to
  every headless run — a stranded keyboard could not even be asserted.
  It also carries the loupe pan block (`soft`, `vx`/`vy`, the
  fractional pan centre, the desired factor — issue #46): a
  wrong-position frame is precisely a state nothing re-renders, so
  render-time traces (which fire on CHANGE) cannot see it; the dump
  makes the overlay's position observable at a scripted instant.
  Since 2026-08-29 it ends with `vpy=` — the grid Flickable's scroll
  offset, in Slint's own sign (0 at the top, negative going down).
  Whether a wheel moved the GRID was observable only at SHUTTER time
  (the `geometry at shutter` trace carries a `scroll` term), never at a
  scripted instant, which is how two modal scrims that let the wheel
  through to the Flickable behind them went unnoticed (issue #49). New
  fields are APPENDED (`dump_field` finds `name=` by prefix and does not
  care about order — appending is for the reader and for small diffs,
  not correctness).
- `FASTCULL_NO_CONFIG=1`: makes `ui.toml` (the remembered copy
  destination/template and the video export's destination) unreachable
  for both load and save — what
  `FASTCULL_NO_CACHE` does for the cache (issue #13 gap, surfaced by the
  issue #41 sweep: a driven copy dialog displayed the user's real
  remembered destination). The screenshot test harness sets both
  unconditionally.
- `FASTCULL_KITCHEN_COOK_MS=N`: hold every kitchen cook for N ms before the
  pixel work (issue #34; same family as `FASTCULL_MAX_READERS`). Pacing
  knob for the `open:PATH` session-swap test, which must catch the kitchen
  queue provably mid-flight at a scripted swap in BOTH build profiles — a
  release build otherwise drains a screenful of thumbs in tens of
  milliseconds and the test becomes timing roulette. The job still flows
  queue → cook → done → drain unchanged; with FASTCULL_TRACE the retarget
  reports how many queued jobs it dropped, which is the test's proof that
  the swap really happened mid-flight (a dropped count of zero would make
  the no-stale-adoption assertion vacuously green). Default 0 (off). When
  set, the app announces it once on stderr unconditionally
  (`fastcull: FASTCULL_KITCHEN_COOK_MS=N — every texture cook is held`):
  the knob ships in release builds, and a value leaked into some
  environment makes the whole app mysteriously slow — a bug report's
  stderr must say why (validator risk note, 2026-08-02).
