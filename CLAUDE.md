# FastCull — agent working agreement

Spec-driven repo: **specs/ is the source of truth.** Read the relevant
`specs/modules/*.md` before touching a module; if implementation must deviate,
update the spec in the same commit and say why in the commit message.

**docs/ follows specs/ (M8)**: `docs/` is the user-facing guide distilled from
the specs. A commit that changes user-visible behavior (or its module spec)
updates the affected `docs/` page in the same commit — the page map is
index↔release/install, culling↔ui-grid+burst-grouping, metadata↔xmp-sidecars+
iptc-templates, copy-picks↔fileops, faq↔catalog-cache+everything else.

## Commands

```sh
cargo build --workspace
cargo test --workspace          # unit + integration
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
testdata/fetch.sh               # sample RAWs (needed by integration tests)
```

Rust lives in `~/.cargo/bin` (rustup, no system packages).

## Hard rules

1. **Never write to a RAW file.** Sidecars only (ADR 0003). Tests enforce this;
   don't "fix" those tests.
2. **No upstream contributions** (rawler or any dependency) without explicit user
   approval — keep patches in-tree.
3. **darktable-cli in tests**: always `--configdir`/`--library` into a temp dir.
   Never touch the user's real darktable config/database.
4. **Git discipline**: every milestone step / spec change / module implementation
   is a proper commit with a descriptive message. No unversioned drops.
5. Business logic lives in `fastcull-core` only; the app crate is a thin Slint
   bridge (01-architecture.md).
6. Performance budgets in `01-architecture.md` are regression-tested — a change
   that breaks a criterion threshold is a failing change.

## Step validation gate (mandatory)

**Before implementation**: any new feature, spec, or milestone-scope decision
gets a usefulness review by **almost-human-user**
(`.claude/agents/almost-human-user.md`) — a persona of a real culling-tool user.
IN-MY-WAY verdicts and workflow gaps are discussed with the user before coding
starts; his questions for the user are relayed verbatim.

**After implementation**: no step is *completed* — no task marked done, no
milestone-step commit declared finished — until BOTH project subagents have
reviewed it:

1. **validator** (`.claude/agents/validator.md`): adversarial review — what is
   missing, remaining, or risky; spec/ADR/CLAUDE.md conformance.
2. **qe-engineer** (`.claude/agents/qe-engineer.md`): executes the change —
   criterion-by-criterion spec verification, regression run, hostile inputs.

Rules of the gate:
- Run both after the step's implementation is believed finished (they can run in
  parallel). FAIL findings must be fixed and the failing agent re-run.
- PASS-WITH-CONCERNS / PASS-WITH-GAPS: address the findings or record the
  explicit decision to defer them (in the task or commit message) — silence is
  not acceptance. Deferring a *spec acceptance criterion* requires the user's OK.
- Findings are fixed by the implementer, never by the reviewing agents.
- The gate applies to implementation steps (code, specs, CI); trivial fixups
  (typos, comment wording) are exempt.

## Conventions

- Rust 2021, `cargo fmt` formatting, clippy clean at `-D warnings`.
- Errors: `thiserror` in core, no `unwrap()` outside tests.
- Tests live next to code (`#[cfg(test)]`) for units; `tests/` per crate for
  integration; golden files under `crates/fastcull-core/tests/golden/`.
- Test data: real RAWs go in `testdata/raws/` (gitignored, fetched by script);
  only tiny synthetic fixtures are committed.
- User context: the user is a professional Sony A1 shooter, ex-Qt developer,
  new to Rust. Explanations in PRs/commits should not assume Rust fluency.
