# Brief 003 — the CI cache key must change when the dev profile changes

Date: 2026-09-06. Branch `ci-cache-key`. Scratch: `.qe-scratch/pipeline-003/`.
A Manager bookkeeping unit (M3): CI plumbing, nothing user-visible, persona
gate skipped. Brief 002 (the selection) is a separate unit awaiting the
user's answers.

## Context

Since #76 landed (PR #80, merged 2026-09-06 as f132a87) every CI run has been
cold: ubuntu-latest 30-31 min and windows-latest 59-62 min, against 17 m 33 s
and 33 m 48 s for the last cached run before the profile line (run
33986518746). The mechanism, from the logs and the cache list:

- `Swatinem/rust-cache@v2` (`.github/workflows/ci.yml` ~182-185,
  `save-if: main`, `cache-on-failure: true`, default `prefix-key: v0-rust`,
  `add-job-id-key: true`) keys the cache on the prefix, the job id, the
  target, a hash of the rustc version and environment, and a hash of the
  lockfiles — the key on both jobs is unchanged since 2026-09-04
  (`v0-rust-test-Linux-x64-91e3cbda-aacf1ed2`,
  `v0-rust-test-Windows_NT-x64-8918a2f9-aacf1ed2`). The workspace
  `Cargo.toml`'s new `[profile.dev.package."*"]` section did not move the
  key.
- The main run of f132a87 (34007321960) therefore restored the stock-profile
  cache under the same key, cargo rebuilt every dependency with the new
  flags, and the post step logged `Cache up-to-date` and saved nothing.
  `gh cache list` shows only the two 2026-09-04 entries (1.77 GiB Linux,
  1.64 GiB Windows) plus two RAW caches; 3.87 GiB of the 10 GB quota.
- So every run pays the cold dependency build the user accepted "once",
  and the spec sentence in `specs/01-architecture.md` ~253-256 ("CI's
  `rust-cache` pays it once per toolchain change … until main repopulates
  the cache") is false as written: main cannot repopulate a cache whose key
  it hits.

## Goals

- G1. A run after the next main run restores the optimised-profile
  artifacts: the cache key changes when the dev profile changes.
- G2. The spec says what is true about the cache key and the cost.
- G3. The cache's size against the 10 GB quota is measured once the new
  entries exist, and a thrash between the two runners' entries is detected
  rather than assumed away.

## Non-goals

- What CI runs, the profile line, the `save-if: main` rule and the RAW
  cache stay as they are.
- No attempt to raise the Actions cache quota (fixed at 10 GB on
  github.com; the CI audit recorded it).

## Requirements

- R1. The rust-cache key changes for this profile: `prefix-key: v1-rust`
  (or an equivalent that makes the profile part of the key — the senior
  developer's plan decides, after reading what rust-cache v2 actually
  hashes and recording it), with a comment in `ci.yml` that says why the
  key had to move and when to move it again (any change to a `[profile]`
  section, since the key does not see it).
- R2. `specs/01-architecture.md` "Build profiles" ~253-256: the sentence
  about rust-cache corrected — the key ignores the profile; the first
  cached run came only after the prefix bump of this unit; the
  once-per-toolchain-change cost holds from then on. The "cached job
  durations" placeholder (~255-256) stays, to be filled by the Manager from
  the first cached run after this merges.
- R3. Proof that the key changed, on this PR's own run: the rust-cache
  restore step logs a miss on the new primary key (a PR never saves), and
  the two jobs are green.
- R4. After the merge (Manager bookkeeping, recorded in the spec's
  placeholder by the next spec-touching commit): the main run's post step
  saves under the new key; `gh cache list` shows the new entries and their
  sizes; the next run restores them and its job durations are recorded. If
  the two runners' entries together exceed the quota and evict each other,
  that is reported to the user with options, not solved here.
- R5. One commit in the project's voice, the attribution trailer, no other
  file touched.

## Acceptance criteria

- AC1. Both CI checks green on the PR; the ubuntu and windows rust-cache
  restore steps log a miss on a `v1-rust-…` key.
- AC2. The spec sentence no longer claims main repopulates the cache under
  an unchanged key; the change is dated and tagged.
- AC3. Post-merge (recorded, not gating this PR): the first main run saves
  two `v1-rust-…` entries; the run after it shows the ubuntu job well
  under 30 min and the windows job well under 59 min.

## Applicable directives

- CLAUDE.md hard rules (none touched: no code, no RAW, no upstream change);
  M1 (spec first — the sentence in `01-architecture.md` moves with the
  change), M3 (this is the Manager's bookkeeping, decided on the evidence
  above), M7, M9 (cleanup ran at the unit's start).
- `specs/01-architecture.md` "Build profiles" (the cost paragraph); the CI
  audit's findings in `.qe-scratch/ci-audit-*` (cache quota, the cold
  Windows cache of 2026-09-04) as inherited evidence.
- The pipeline's test-integrity rule: no test changes are expected; any
  `ci.yml` change beyond the key is out of scope.

## Persona verdicts

Skipped: CI plumbing, nothing user-visible.

## Open questions

- OQ1 (senior developer, in the plan): does rust-cache v2 hash any part of
  `Cargo.toml`, and if so which sections? The observed key did not move for
  a `[profile]` change; the plan records what the action's source says so
  the comment in `ci.yml` is exact.
- None for the user.

## Decisions log

- 2026-09-06, Manager: decided under M3 — the cold runs undo half of #76's
  benefit for the developer's daily workflow; the fix is a key bump, the
  risk (quota thrash) is measured after the merge and taken to the user
  only if it materialises.
- 2026-09-06, senior developer (duty 1): OQ1 settled from rust-cache's
  source — the key hashes the workspace MEMBERS' manifests and the lockfile,
  the toolchain and the environment; the root manifest is virtual and never
  read, so no `[profile]` change moves the key; fix `prefix-key: v1-rust`,
  bumped by hand on any root `[profile]` change. Corrections to this brief's
  context: the key's third component is `os.type()-os.arch()`, not the
  target; the Windows entry is 1.65 GiB; the ubuntu cold range is 26-31 min;
  GitHub's page says the cache limit can be raised by repository
  administrators (unverified here; the CI audit found no such control on
  this repository's settings page on 2026-09-03).
- 2026-09-06, Manager rulings: Q1 keep the checkbox ledger in
  `01-architecture.md`; Q2 delete the orphaned `v0-rust-…` entries after the
  first main run saves the `v1` pair (bookkeeping, M3); Q3 note only,
  verify the cache-size control only if thrash materialises; Q4 the
  developer reads the ubuntu job for AC1, the Manager reads the Windows one.
