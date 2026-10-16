# Brief 015 — Apply re-reads `templates.toml`, by name

Dated 2026-10-16. Branch `templates-reread-on-apply`, cut from
`origin/main` at `52f3a29` (M12). Classification: user-visible change
(a behaviour the spec promised and the app never did), plus the test
plumbing brief 014 deferred to "the next template unit" (014 D14).

## Context

QE's round on brief 014 (014 D15, finding 3) found a promise the app
does not keep: iptc-templates.md says `templates.toml` is "read on
panel open and on Apply — no file watcher (2026-07-25)", but since M5
(`8e67cd6`) `reload_templates` (`crates/fastcull-app/src/session.rs`)
runs only from `load_folder` (every folder open) and from the panel's
open (`crates/fastcull-app/src/iptc_bridge.rs`), never from
`on_iptc_apply_template`, which applies `st.session.templates[index]`
as last loaded. The user guide (docs/metadata.md) describes what the
code does: "edit, save, toggle the panel, apply".

The user was given the two ways to make the spec and the app agree —
Apply re-reads the file first, or the spec is corrected to what the app
does — and chose the first (2026-10-16): **clicking Apply re-reads
`templates.toml` before applying, so a saved edit applies at once.**

The persona measured four facts on the v0.15.1 release binary with
`FASTCULL_CONFIG_DIR` pointed at a file of its own (entries in file
order `zebra`, `alpha`, `Mid`, `broken` with `keywords = 42`, `wipe`
with `city = ""`):

1. The dropdown is alphabetical by byte, not file order: `Mid, alpha,
   wipe, zebra` (uppercase first; `broken` skipped). `toml 0.8.23` is
   built without `preserve_order`, so its `Table` is a `BTreeMap`. A
   template's position is nothing a user can see or steer from an
   editor.
2. The gap, live: panel opened at 2.0 s, the editor rewrote `M title` to
   `M title EDITED` at 3.8 s (atomic rename), Apply clicked at 5.5 s; the
   trace shows `templates loaded` at 0 and 2001 ms only and the sidecar
   holds `M title`.
3. The warning strip sits above the dropdown-and-Apply row and pushes it
   down (~115 px with two warnings).
4. The app never writes `templates.toml` (`save_templates` has no
   caller); the editor is the only writer, so a re-read can clobber
   nothing.

Today's Apply handler has one silent path: `let Some(tpl) =
templates.get(index) else { return }` — unreachable while the list
cannot change between the click and the lookup, reachable once Apply
re-reads.

## Goals

- Apply re-reads `templates.toml` and applies the template the user
  selected, BY NAME, as the file now defines it.
- Every outcome of that re-read is visible in the panel: nothing is ever
  applied silently from the wrong template, and nothing fails silently.
- One rule for every read of the file — panel open, folder open, Apply.
- The dropdown's selection survives every list replacement by name.
- The drive harness reads the template list and the selection, closing
  brief 014 D14's open box with the deferred tests.

## Non-goals

- No file watcher, no timer, no Reload button, no re-read when the
  dropdown opens or the window regains focus: Apply is the one new
  read moment (the user, 2026-10-16, D2). A brand-new template appears
  in the dropdown after the panel is toggled, as today; the guide says
  so.
- No save-template UI; `templates.toml` stays hand-edited.
- The dropdown resetting to the first template when the panel is closed
  (014 D15, the persona's USEFUL follow-up) — not this unit.
- The warning strip's position above the Apply row — a layout follow-up,
  not this unit.
- No change to the tri-state apply rule, the single-level revert, the
  `{seq}` contract or the error semantics of the core loader.

## Requirements

- R1. **Apply re-reads first.** `on_iptc_apply_template` re-reads
  `templates.toml` through the same read as the panel's open and the
  folder open — core's one config-dir resolver, the `templates loaded
  from <path>` mark from the very path it read — before it applies
  anything. Under `FASTCULL_NO_CONFIG` there is no path and no read, as
  for the other two moments.
- R2. **By name.** The template applied is the one whose NAME the
  dropdown showed at the click, looked up in the re-read list by exact,
  case-sensitive match on the TOML key — never by position. The
  persona's MUST-HAVE: position is alphabetical and drifts under every
  edit (insert a name that sorts earlier, rename, delete), and a wrong
  pad stamped on 140 picks announces itself only to someone who looks.
- R3. **A vanished name applies nothing and says so.** If the selected
  name is not in the re-read list (deleted or renamed in the editor),
  nothing is applied, the revert slot is untouched, and the warning
  strip says the template is no longer in `templates.toml` and nothing
  was applied. The dropdown then shows the re-read list on its first
  entry with Apply enabled (disabled, as at open, if the list is empty).
  Never the first template as a fallback, never a silent return — the
  persona's second MUST-HAVE; today's silent `else { return }` becomes
  this warning.
- R4. **One rule for a bad file, at every read.** A corrupt entry is
  skipped with its per-entry warning and the rest load (as today). A
  file that fails to parse as a whole keeps the LAST GOOD list on
  screen and the strip shows the parse error and says the templates
  shown are the last read; at Apply nothing is applied on such a
  failure (the re-read failed, so what is on disk is not what would be
  applied). A missing file is an empty list with no warning (as today).
  The same rule holds for the panel's open and the folder open — one
  rule, stated once; at launch there is no last good list, so a broken
  file at launch gives an empty list with the error, as today.
- R5. **The dropdown follows the name.** After any replacement of the
  template list — Apply's re-read, the folder swap — the dropdown's
  selection is the entry with the previously selected name, at its new
  position; if that name is gone, the first entry (R3). This replaces
  brief 014 D4's "by index, clamped" (014 D4, D15, D14's open box):
  the index drift nobody noticed at a swap of an unchanged file becomes
  visible once the list is replaced at every Apply.
- R6. **Warnings add up.** An apply-time expansion error is appended to
  the strip's read warnings rather than replacing them, so a `""` clear
  warning does not vanish behind an unknown-variable error until the
  next read. (The senior developer may move this to a follow-up with a
  reason in the plan if a test's promise or a contract moves — D7.)
- R7. **The harness reads the list and the selection.** The `QEDUMP`
  line gains a field — the template names the window holds in model
  order and the selected entry (the senior developer names the field
  and its encoding; test-harness.md "new fields are APPENDED", after
  `keywords=`) — so a driven test asserts the dropdown's model and
  selection, never pixels (014 D14, 014 D15).
- R8. **The specs and docs say so.** iptc-templates.md: the read
  moments (panel open, folder open, Apply), R2–R6 as rules, 014 D4's
  sentence rewritten to "by name", 014 D14's open box closed by R7's
  tests, the ledger; test-harness.md: the new dump field; ui-grid.md's
  Slint-facts sentence on the ComboBox if the mechanism changes what it
  relies on; docs/metadata.md: the templates paragraph says the file is
  re-read when the panel opens, when a folder opens and every time Apply
  is clicked — edit, save, Apply — a new template appears after
  toggling the panel, and re-applying an edited template overwrites the
  fields it still sets and leaves the others (an absent field
  preserves; to drop a value the previous version stamped, clear it in
  the panel or set it to `""` in the template).

## Acceptance criteria

Driven through the harness with `FASTCULL_CONFIG_DIR` pointing at a
scratch `templates.toml` (the shape of
`templates_and_ui_prefs_are_read_from_the_one_config_dir`), the file
rewritten from the test between marks (the issue #50 `on_line`
pattern), every assertion on dump fields and marks, nothing on pixels
or coordinates; both runners.

- AC1. **An edited template applies as saved.** Panel open, a template
  selected, the file's field rewritten on disk after the open, Apply:
  the sidecar carries the NEW value and the revert label names the
  template. Red on the pre-change tree (the OLD value lands — the
  persona's fact 2), green after.
- AC2. **A vanished name applies nothing, visibly.** The selected
  template renamed in the file before Apply: no sidecar is written, the
  revert slot stays empty, the strip carries R3's warning, and the
  dropdown shows the re-read list on its first entry. Red on the
  pre-change tree (the stale template is applied).
- AC3. **By name, not by position.** A name that sorts before the
  selected one inserted in the file before Apply: the selected name's
  fields land, not the inserted template's. Green on the pre-change
  tree (no re-read at all); it guards the over-fix — a re-read followed
  by a lookup at the old index — which its mutant must turn red.
- AC4. **A broken file at Apply keeps the list and applies nothing.**
  The file made unparseable before Apply: no sidecar, the strip shows
  the parse error and the last-read note, the dump still lists the
  previous names and Apply stays enabled; after the file is repaired, a
  second Apply applies. Red on the pre-change tree (the stale template
  is applied; or, if the list empties, the dump shows it).
- AC5. **The dropdown follows the name.** After AC3's insertion and
  Apply, the dump's selected entry is still the selected NAME at its new
  position; after a folder swap with the panel open the selection is
  the same name (014 D4 by name). A mutant that keeps the index turns
  the first red.
- AC6. **The list at a swap, as 014 D14 shaped it.** The file deleted
  after the panel read it, then a swap: the dump's template list is
  empty and Apply disabled — red with the swap's forced replacement of
  the names model removed (014 G1's mutant).
- AC7. **Nothing else moves.** `templates_and_ui_prefs_are_read_from_the_one_config_dir`,
  the core template tests, the five focus tests brief 014 named and the
  three swap-chip tests keep their promises, count for count; the
  change-only rule for the names model stands within a session (the
  model is replaced only when the names changed; the selection is set
  without a replacement).
- AC8. **The specs and docs say so** (R8), ticked at the merge.
- AC9. **Warnings add up** (R6; added 2026-10-16 at the spec change, the
  senior developer's Q1): a template with a `""` field and an unknown
  variable — after Apply the strip carries the CLEAR warning AND the
  expansion error. Red on the pre-change tree, where the error replaced
  the read warnings.

## Applicable directives

- Hard rule 1 (sidecars only; the tests' RAWs are copies or symlinks
  under the test's scratch; checksums before and after); hard rule 4
  (every step a commit); hard rule 5 — the name lookup and the
  selection-after-replacement are pure rules in `fastcull-core`
  (`iptc.rs`) with unit tests beside them, the keep-last-good is app
  state around the unchanged loader, the loader's error semantics stay
  in core (this line called the lookup bridging until the spec change,
  2026-10-16 — the senior developer's Q2, D14); hard rule 6 (nothing
  per tick).
- ADR 0003. M1 (the spec moves first), M2 (the UX rulings below),
  M7, M8 (the one persona question went to the user — D2), M12.
- iptc-templates.md (the Templates section, "Applying to a selection",
  "The panel's models", the ledger); test-harness.md ("The dump",
  `FASTCULL_CONFIG_DIR`, the `on_line` rewrite pattern); ui-grid.md
  (*Slint facts*, *Focus continuity*: no reclaim after a template-model
  replacement; the Apply button's focus claim); settings.md (the one
  config-dir resolver, brief 008 D11).
- The gate: old-red first for AC1, AC2, AC4; a mutant for AC3, AC5, AC6
  and every new guard; the senior developer's veto on every test
  change; no loosening.
- Brief 014 D4, D14, D15 (what this unit inherits and rewrites).

## Persona verdicts (2026-10-16, `almost-human-user`)

- **USEFUL** — hit every evening a template is touched; Photo
  Mechanic's in-place stationery never has this staleness; the change
  is what makes hand-edited TOML honest. Two conditions are MUST-HAVE
  on the change itself: apply by name, and never a silent fallback.
  Done by position, or with a silent first-template fallback, the
  change is **IN-MY-WAY** — Apply would become less predictable than
  today.
- Name or position: NAME, exact and case-sensitive (`Osprey` and
  `osprey` are two pads).
- Selected name gone: nothing applied, the strip names it; never the
  first template; the dropdown on the re-read list's first entry.
- Broken file at Apply: "Apply is never stricter or looser than open";
  on a whole-file parse failure keep the last good list and say so —
  one rule for open, swap and Apply; the minimum acceptable is "go
  empty exactly as open does today" with the warning saying to toggle.
- Dropdown after Apply: stay on the NAME; "follow the name at every
  replacement, swap included" is the rule to write once.
- Apply only is enough and is the right moment; focus-regain, timer and
  Reload button rejected. The one limit — a brand-new template needs
  the toggle — is a SHRUG for the persona and the docs should state the
  split.
- Other stumbles: double apply and single revert are fine by the
  existing rule; the tri-state surprise when a line is REMOVED from a
  template and re-applied (absent preserves) wants one docs sentence;
  a failed expansion replaces the read warnings — append instead; the
  strip above the row moves Apply under the pointer — a layout
  follow-up; the dropdown resetting on panel close — USEFUL, outside
  this unit.

## Open questions

None. The persona's one question for the user is answered (D2).

## Decisions log

- D1 (2026-10-16, the user): Apply re-reads `templates.toml` first, so
  a saved edit applies at once — chosen over correcting the spec to
  what the app does (014 D15's question).
- D2 (2026-10-16, the user, on the persona's question relayed
  verbatim): toggling the panel to get a brand-new template into the
  dropdown is acceptable — Apply is the only new read moment; no
  re-read on window focus or dropdown open.
- D3 (2026-10-16, Manager, M2): by NAME — the exact, case-sensitive
  TOML key the dropdown shows (R2).
- D4 (2026-10-16, Manager, M2): a vanished name applies nothing and the
  strip says so; the dropdown lands on the re-read list's first entry;
  never a silent return, never a fallback template (R3).
- D5 (2026-10-16, Manager, M2): keep-last-good on a whole-file parse
  failure, one rule for open, swap and Apply, with the strip saying the
  shown templates are the last read; nothing applied on a failed
  re-read (R4). The alternative — empty as today — leaves Apply
  disabled until a toggle that also resets the dropdown, and makes a
  non-atomic editor save a visible event.
- D6 (2026-10-16, Manager, M2): the dropdown follows the name at every
  list replacement, the swap included; 014 D4's "by index, clamped" is
  rewritten (R5).
- D7 (2026-10-16, Manager): warnings append (R6); deferrable by the
  senior developer with a written reason.
- D8 (2026-10-16, Manager): this unit carries 014 D14's deferred
  plumbing — the dump field and the swap tests (R7, AC5, AC6) — and
  closes 014's open ledger box.
- D9 (2026-10-16, Manager): follow-ups recorded, not opened: the
  dropdown's reset on panel close (014 D15) — superseded by D11 the
  same day; the warning strip's position above the Apply row stays a
  follow-up.
- D10 (2026-10-16, Manager, M9/M12): the branch cut from `origin/main`
  at `52f3a29` (v0.15.1 plus PR #118); the start-of-unit clean freed
  22.4 GiB (`target/` 83 → 63 GB; no `target-qe-*`, no `.qe-scratch`
  left by brief 014); no peer session in this repo.
- D11 (2026-10-16, Manager, M2, on the senior developer's candidate
  question for the user at the spec change): the dropdown's selection
  SURVIVES closing and reopening the panel for as long as the app runs,
  by name — the reopen is a read moment, so the selection follows the
  name as at every replacement; at launch the first entry. The senior
  developer's probe showed that a window property two-way bound to the
  dropdown's index is what a recreated dropdown starts from, so "keep"
  is zero lines and the reset would be one deliberate line; with the
  cost gone, the persona's USEFUL verdict and its own words — the one
  control carrying the user's intent for the evening must not
  re-decide itself — are the best-practice answer. Resolves 014 D15's
  follow-up; supersedes D9's first item; AC5 gains a close-and-reopen
  read with its mutant.
- D12 (2026-10-16, Manager, on Q3): an Apply with an empty batch is a
  `no image — nothing applied` mark only; the strip is unchanged.
- D13 (2026-10-16, Manager, M10, on Q4 and Q5): two corrections outside
  the brief's list ride in the spec commit — 01-architecture.md's
  UI-thread bullet lists the config READS beside the writes (and the
  folder scan, if verified by reading), and docs/metadata.md's field-row
  rebuild triggers lose "when templates reload" (a reload replaces the
  names model only).
- D14 (2026-10-16, Manager, on Q1, Q2 and Q6): R6 gets AC9 and its test;
  the lookup and the selection rule live in core (the directives line
  corrected); the test and core function names are as the spec change
  proposes.
- D15 (2026-10-16, Manager, on the plan's four questions): (1) edit D as
  the senior developer recommends — the names cache survives
  `IptcPanelState::begin_session`, so the names model follows the
  change-only rule at every moment, the swap included, and 014 D14's
  untested forced-replacement line is deleted rather than tested (the
  aliasing 014 fixed cannot touch a cache that is never reset); 014's
  "all three models" sentence and AC6's mutant are rewritten in the spec
  commit; (2) commit 1 carries the window-held dropdown index and so
  D11's keep-across-close, its message saying so, so the dump's field
  order stays the spec's; (3) one full driven suite on the final tree,
  no extra run on commit 1 (014 D10's shape: commit 1 is verified by the
  eleven named tests); (4) AC5's test is renamed
  `the_template_dropdown_follows_the_name_at_apply_at_a_swap_and_at_a_reopen`
  so its name says what it proves; the ledger follows.
