# ADR 0005: One settings file, read by both binaries, under the environment

**Status**: accepted (2026-10-01, user decisions of 2026-09-30 and
2026-10-01; brief 008) · **Decides**: where user settings live, who reads
them, and what wins when two sources disagree

## Context

Until brief 008 FastCull had no settings: `ui.toml` remembered two
destinations and a template (state, not choices), `templates.toml` held
the IPTC templates, and the one user knob was an environment variable
(`FASTCULL_MAX_READERS`). Four settings had been promised in the specs
since 2026-07-25 and never delivered, because issue #39 was parked on the
persona's rule that a toggle whose state cannot be seen in the UI is worse
than no toggle. Issue #39 now ships the dialog; the next units (#15 paired
JPEGs, #24 Lightroom sidecars) add rows to it. What they must not
re-litigate is below.

## Decision

- **One file per user, `settings.toml`, in the config dir** — the
  `directories` crate's `config_dir()` for `("org", "fastcull",
  "fastcull")`, beside `ui.toml` and `templates.toml`; one resolver in
  core (`settings::config_dir()`) names the directory for all three, and
  `FASTCULL_NO_CONFIG=1` makes all three unreachable.
- **TOML is the INI the user asked for**: a table per tab, a key per
  setting, a `#` note above every key the app writes. The project already
  parses TOML in two places and `toml_edit` was already in the tree; a
  literal `.ini` would have added a parser for the same shape.
- **Both binaries read it, at startup, through core.** A knob the CLI
  shares with the app (the cache cap, the read workers) comes from the
  same file; the CLI grows no flag surface for it.
- **The environment wins over the file** wherever a variable already
  governs a knob, and the dialog shows that field read-only with the
  variable's value. A variable that does not parse is ignored. No setting
  introduces a new variable; a future setting gets one only by its own
  recorded decision.
- **The file is the user's.** A file that fails to parse is never
  overwritten in place; the defaults are in force, the failure is said on
  stderr and on the status line, and the first write moves the file aside
  under a reported name. A write preserves unknown keys and the user's
  comments.
- **Apply on commit, write at once, on the UI thread.** No Apply/OK/Cancel
  and no unsaved state; every commit is a ~1 KB read-modify-write on an
  explicit user action inside a modal — `ui.toml`'s precedent — and a
  failed write stays in force in memory and says so. Work that can take
  seconds (Clear cache's VACUUM) runs on a worker.
- **Core owns the model**: the struct, the defaults, the parser, the
  writer, the clamps, the precedence and the memory grammar are
  `fastcull-core`'s and unit-tested there; the app binds Slint properties
  to them (hard rule 5).

## Consequences

- Adding a setting is: a `Key` in core with its table, name, default,
  range and note; a row in the dialog; a sentence in `settings.md`; a
  line on `docs/settings.md`. Nothing else learns about the file.
- A setting's effect moment is part of its row ("applies at once", "at
  the next folder open") and the note says it; nothing restarts the app.
- `FASTCULL_CONFIG_DIR` exists so a driven test can prove a write without
  touching the real file; it is harness plumbing (test-harness.md) and
  wins over `FASTCULL_NO_CONFIG`.
- The one-resolver rule closes a hermeticity gap: `templates.toml` was
  read from the real config dir by every driven run until this ADR.
