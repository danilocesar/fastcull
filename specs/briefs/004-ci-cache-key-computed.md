# Brief 004 — the CI cache key is computed from the profile, not bumped by hand

Date: 2026-09-06. Branch `ci-cache-key-computed` from main d4da1ac. Scratch:
`.qe-scratch/pipeline-004/`. CI plumbing, nothing user-visible; persona gate
skipped. Follows unit 003 (PR #82) and closes it.

## Context

Unit 003 established from the action's source that `Swatinem/rust-cache@v2`
hashes the workspace members' manifests, the lockfile, the toolchain and the
environment, and never reads the root `Cargo.toml` that carries the
`[profile]` tables (it is a virtual manifest). PR #82 moved the key with a
hand-bumped `prefix-key: v1-rust` and a comment saying "bump on any root
`[profile]` change". QE's verdict on #82 (PASS) named the residual: that
obligation is carried by a comment and a human, and its failure is silent —
builds stay green and get 10-25 minutes slower per job, which is how #76's
profile change cost five cold runs across two days before anyone counted
`Compiling` lines. QE asked the user whether to (a) compute the key from the
profile, (b) add a guard test that goes red when the profile changes without
a bump, or (c) keep the comment.

The user (2026-09-06): "for the cache, make a decision based on best
practices regarding ci." The Manager's decision, on that basis: (a). A CI
cache key should be derived from the inputs that shape the build and never
maintained by hand; over-invalidation on a real input change is acceptable,
under-invalidation is the trap this unit removes. (b) was rejected because
it turns one hand edit into two; (c) because its failure is silent by
construction. QE's own first draft of (a) — an `awk` slice of the manifest —
was wrong (it captured the comment block between two profile tables, so a
comment edit would have moved the key), which is why the design here parses
the TOML instead of slicing text.

After the merge of #82 the first main run (34018510289) saved the two
`v1-rust-…` entries (Linux 1.88 GiB, Windows 1.62 GiB), the `v0` entries were
deleted, and usage is 5.61 GiB of the 10 GB quota. Adding a key component
makes this unit's PR run and the next main run cold once more (one pair,
~35 and ~60 min); the run after that merge is then the first cached run
since the profile line, and it fills unit 003's placeholder.

## Goals

- G1. The rust-cache key changes whenever the root manifest's `[profile]`
  tables change, and only then — never for a version bump, a comment or a
  dependency edit (which the action already hashes itself).
- G2. Both runners compute the identical component for the same manifest.
- G3. The spec's "The CI cache key and the profile" paragraph says the rule
  is computed; unit 003's ledger is closed (AC1 ticked, its deferred
  corrections D1-D4 applied) and its placeholder filled by the first cached
  run after this merge.

## Non-goals

- No guard test in the Rust crates (option b); no change to `save-if`,
  action versions, what CI runs, the profile itself, or the RAW cache.
- No text slicing of `Cargo.toml` in shell (`awk`/`sed`/`grep`): the
  component is computed from a parsed document.

## Requirements

- R1. A workflow step before the rust-cache step, `shell: bash` on both
  runners, computes `hash = sha256(canonical JSON of the root manifest's
  parsed `profile` table, keys sorted)[:8]` and exposes it as a step output.
  The senior developer's plan settles the interpreter: Python's `tomllib`
  (3.11+) if the runner's `python3`/`python` under bash is 3.11+ on BOTH
  runners (read the env-facts step's output of a recent run; verify on
  the PR), otherwise `actions/setup-python` pinned, or another parser
  already on both runners. An absent `[profile]` table hashes as `{}`, so
  the step never fails on a manifest without profiles; a parse error FAILS
  the job loudly (never a silent default).
- R2. The rust-cache step receives the component through its `key` input
  (`key: profile-${{ steps.<id>.outputs.hash }}`) — the plan confirms from
  the action's source how `key` enters the cache key and that it is part of
  the primary key, not only the restore key. `prefix-key: v1-rust` stays,
  and its comment is rewritten: it is bumped only for a change to the
  action or the key format, and NOT for profile changes, which the computed
  component covers.
- R3. The comment in `ci.yml` states the rule, the command, and the
  measurement from R5; the stale cold-run ranges (QE 003 D1) and the
  headroom numbers (D2) in `ci.yml` 72-79 / 208-222 are corrected in the
  same commit.
- R4. Spec, same commit or the Manager's spec commit: `01-architecture.md`
  "The CI cache key and the profile" — the bump rule becomes the computed
  rule; the ledger ticks unit 003's AC1 (run 34014978820, both jobs `No
  cache found.` under `v1-rust-…`) and AC3's save half (run 34018510289:
  1.88 GiB and 1.62 GiB saved, v0 deleted, usage 5.61 GiB); the placeholder
  (~270-274) is re-worded to name the run after THIS unit's merge as the
  first cached one; D1 (a prefix bump also discards the release half:
  33 m 28 s / 1 h 09 m 58 s on 34014978820), D2 (`ui-grid.md` 2575-2577:
  the worst cold Windows job is 1 h 09 m 58 s, 20 min of headroom; the
  main run's post step 2 m 15 s), D3 (brief 003's non-goal wording), D4
  (record). New ledger items for this unit's AC1-AC4.
- R5. Proof, by QE on the PR: the step's output is printed in the job log
  on both runners and is identical; the rust-cache restore step's `Cache
  Key:` contains it; locally, the same command against modified copies of
  the manifest — an `opt-level` change moves the hash (the #76 shape), a
  `[workspace.package] version` bump does not, a comment edit does not, a
  reordered table does not, a member-manifest change does not touch this
  component (the action's own hash covers it); the hash is byte-identical
  on this seat and on both runners for the unmodified manifest.
- R6. One developer commit for `ci.yml`, in the project's voice, both
  trailer lines; the spec moves in the Manager's spec commit or with it.

## Acceptance criteria

- AC1. Both CI checks green on the PR; both jobs log the same 8-hex profile
  hash and a `Cache Key:` that contains it, then `No cache found.` (cold by
  design, save-if main).
- AC2. The five local mutants of R5 behave as stated, with the command and
  the hashes recorded in the QE report and the commit.
- AC3. Post-merge (Manager, recorded by the next spec-touching commit): the
  main run saves under the computed key; the following run restores it
  (`full match: true`), and its durations fill the placeholder.
- AC4. `ci.yml` and the spec contain no sentence that still says the
  prefix must be bumped for a profile change.

## Applicable directives

- CLAUDE.md hard rules (none touched); M1 (spec first), M3 (Manager
  bookkeeping, decided as above), M7, M9 (cleanup ran at the unit's start).
- `specs/01-architecture.md` "Build profiles" and "The CI cache key and the
  profile"; unit 003's brief, plan, review and QE report under
  `.qe-scratch/pipeline-003/` (the six mutants in `qe/lockhash.py`, the job
  logs, the action's source under `rust-cache-src/`).
- The test-integrity rule applies to the computing step: a step whose
  failure degrades silently to "the key never moves" is the original bug in
  a new form; the senior developer reviews it as it would a test.

## Persona verdicts

Skipped: CI plumbing, nothing user-visible.

## Open questions

- OQ1 (plan): the interpreter on `windows-latest` under `shell: bash`
  (`python` vs `python3`, version), settled from a recent run's env-facts
  output and the runner image notes.
- OQ2 (plan): whether rust-cache's `key` input is part of the restore key
  or only the primary key (the source, `config.ts`), and whether that
  matters here (it should not: a profile change must miss both).
- None for the user: the user delegated the decision to CI best practice.

## Decisions log

- 2026-09-06, the user: "make a decision based on best practices regarding
  ci."
- 2026-09-06, Manager: option (a), parsed-TOML form, as recorded in the
  context; unit 003's closing items ride in this unit.
