# FastCull — agent working agreement

Spec-driven repo: **specs/ is the source of truth.** Read the relevant
`specs/modules/*.md` before touching a module; if implementation must deviate,
update the spec in the same commit and say why in the commit message.

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

## Conventions

- Rust 2021, `cargo fmt` formatting, clippy clean at `-D warnings`.
- Errors: `thiserror` in core, no `unwrap()` outside tests.
- Tests live next to code (`#[cfg(test)]`) for units; `tests/` per crate for
  integration; golden files under `crates/fastcull-core/tests/golden/`.
- Test data: real RAWs go in `testdata/raws/` (gitignored, fetched by script);
  only tiny synthetic fixtures are committed.
- User context: a professional Sony A1 shooter, ex-Qt developer, new to
  Rust. Explanations in PRs/commits should not assume Rust fluency.
