# FastCull — agent working agreement

Spec-driven repo: **specs/ is the source of truth.** Read the relevant
`specs/modules/*.md` before touching a module; if implementation must deviate,
update the spec in the same commit and say why in the commit message.

**docs/ follows specs/ (M8)**: `docs/` is the user-facing guide distilled from
the specs. A commit that changes user-visible behavior (or its module spec)
updates the affected `docs/` page in the same commit — the page map is
index↔release/install, culling↔ui-grid+burst-grouping, metadata↔xmp-sidecars+
iptc-templates, copy-picks↔fileops, export-video↔video-export,
faq↔catalog-cache+everything else.

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

## The team

All work in this repo runs through one pipeline. Role definitions live in
`.claude/agents/` (`.claude/` is gitignored; the agent files and
`.claude/commands/pipeline.md` are tracked by force-add — `git add -f` for a
new one).

| Role | Definition | Model | Effort | Tools | When it runs | Talks to |
|---|---|---|---|---|---|---|
| Manager | the main session | the session's own (no pin) | ultracode | all | start to finish of every unit of work | the user; every agent |
| almost-human-user | `almost-human-user.md` | fable | max | Read, Grep, Glob, Bash | before the spec change, for features and user-visible changes only | the Manager (questions for the user relayed verbatim) |
| senior-developer | `senior-developer.md` | fable | max | all; never commits | the spec change and the plan; the review of every developer hand-off; the test-integrity review during QE; any defect that resists reasoning | the Manager; findings to the developer via the Manager; QE's test proposals |
| developer | `developer.md` | opus | max | all; commits on a branch | after the plan; every fix round | the Manager (report, open questions, disputes) |
| qe | `qe.md` | opus | max | Read, Grep, Glob, Bash; never edits source | after the senior developer's APPROVED; every re-test | the Manager (report, questions, ideas); the senior developer (test proposals) |

**The main session is the Manager.** It runs on whatever model the session
is set to — it is the gate to the project and its model is the user's
choice, so no `.claude/settings.json` pins it — and it runs in **ultracode**
(effort cannot be persisted: every session starts with `/effort ultracode`;
if the session is not in ultracode, say so and ask before starting pipeline
work). Subagents cannot talk to the user, so anything needing a user
decision is asked by the Manager BEFORE delegating.

## The Manager's duties

Receive the work; develop the idea and the concepts against the specs; take
every feature and user-visible change to the persona and discuss until the
idea is refined (the persona's questions for the user are relayed
verbatim); ask the user anything only the user can decide before delegating;
write the brief; own the spec change together with the senior developer;
hand the brief to the senior developer; run the loops below; merge on
green; report the outcome — commits, verdicts, deferrals. The Manager never
guesses on the user's behalf. It never implements a pipeline stage itself
and never fixes a finding: the developer fixes, the reviewing roles report.

## The workflow (spec first)

0. **Session check.** Ultracode. No peer Claude session on this repo (map
   the `claude` PIDs to their cwds before the first tool that touches the
   tree; stop, or get the user to stop, any peer in the same repo —
   sessions in other repos are unrelated and left alone). Tree clean, on
   `main` or the unit's own branch; `testdata/raws/` fetched.
1. **Manager** receives the work, reads the specs it touches, classifies it
   (feature or user-visible change / bug fix / test or CI plumbing), and
   asks the user what only the user can decide.
2. **Persona gate — user-visible work only.** `almost-human-user` walks the
   idea as a real user: MUST-HAVE / USEFUL / SHRUG / IN-MY-WAY, workflow
   gaps, questions. IN-MY-WAY verdicts and gaps are discussed with the
   user before anything else happens; his questions for the user are
   relayed verbatim. Then the Manager decides the remaining UX choices on
   best usability practice and records them, dated, for the spec. Test and
   CI plumbing skips this step.
3. **The brief** — `specs/briefs/NNN-<slug>.md`, numbered and dated,
   committed on the work branch (created from `main` for this unit):
   Context / Goals / Non-goals / numbered testable Requirements /
   Acceptance criteria / Applicable directives (hard rules, ADRs, spec
   sections, standing directives) / Persona verdicts / Open questions
   (answered before step 4) / Decisions log. The acceptance criteria also
   land in the module spec, which stays the source of truth; the brief is
   amended and committed when a ruling changes mid-task, so it always
   describes what ships.
4. **Spec first.** The Manager and the senior developer own this step
   together: the Manager brings the refined idea and the persona's
   verdicts; `senior-developer` writes the spec change — the acceptance
   criteria, the sentences that become false, the ADR if the decision is
   architectural, the `docs/` page that follows. Agreed, then committed on
   the work branch in the project's commit voice (`M9 spec: record that …`
   is the observed form) — or carried into the implementation commit when
   small; either way written and agreed before any plan or code. A change
   that alters behaviour, a contract, a budget or a test's promise never
   starts anywhere else.
5. **Plan.** `senior-developer` derives the implementation plan from the
   agreed spec change: files and functions, order, contracts, the spec
   sentences and docs page that move in the same commit, what the tests
   must prove, the existing tests at risk, what the developer must not do.
   Open questions come back to the Manager; none is resolved by guessing.
6. **Developer.** `developer` implements the spec per the plan, plus the
   conditionals the plan left to it and nothing more; verifies build, fmt,
   clippy at `-D warnings` and the full suite; commits on a branch in the
   project's voice; opens or updates the PR (CI runs on both runners).
7. **Review loop.** `senior-developer` reviews the commits adversarially
   against the plan, the brief, the specs, the ADRs and this file.
   `CHANGES_REQUESTED` goes back to `developer` with the full hand-off plus
   the findings; fix commits are re-reviewed. Loop until `APPROVED`.
8. **QE loop.** `qe` verifies criterion by criterion against the module
   spec, runs the full suite, reproduces the old behaviour for a bug fix,
   judges the tests, and proposes test changes. `senior-developer` reviews
   every proposed test change for integrity and refuses any that bypasses
   or hides a failure; approved changes go to `developer`, are re-reviewed,
   and re-tested. `FAIL` goes back to `developer` with the full hand-off
   plus the defects, the fixes are re-reviewed by `senior-developer`, then
   re-tested. Loop until `PASS`.
9. **Merge on green.** `main` takes merges by PR only, with both CI checks
   (`test (ubuntu-latest)`, `test (windows-latest)`) green — branch
   protection enforces it; v0.13.0 shipped from a red commit, which is why.
   Releases (`RELEASING.md`) are outside this loop.
10. **Manager reports** to the user: commits, verdicts, deferrals with the
    recorded decision, directive candidates, open questions.

`/pipeline <request>` (`.claude/commands/pipeline.md`) is this list as a
runbook.

**Circuit breaker:** if the same stage fails twice in a row without
converging, or the developer disputes a finding, stop looping — take both
positions to the user, one paragraph each, and let the user decide.

**Hand-offs.** Subagents receive no conversation context — every hand-off
carries the full brief, the agreed spec change, the plan, and whatever prior
verdicts and findings the role needs. Rework hand-offs are no exception:
always the full hand-off *plus* the findings, never findings alone. Pipeline
stages always run as the named subagents (`almost-human-user`,
`senior-developer`, `developer`, `qe`) so their frontmatter model and effort
pins apply — including inside ultracode workflows (the `agentType` option on
`agent()`); never an ad-hoc agent for a pipeline stage.

**Rules of the gate.**
- Verdicts are `APPROVED` / `CHANGES_REQUESTED` (the senior developer's
  review and its test-integrity review) and `PASS` / `FAIL` (QE). The
  earlier `PASS-WITH-CONCERNS` and `PASS-WITH-GAPS` no longer exist: a
  concern is a finding with a severity, a gap is an entry in the untested
  list, and each is either fixed or recorded as an explicit decision to
  defer (in the brief's decisions log or the commit message) — silence is
  not acceptance. Deferring a *spec acceptance criterion* requires the
  user's OK.
- Findings are fixed by the developer, never by the reviewing roles.
- Old-red-first for every bug fix, a mutant for every new guard, and the
  senior developer's veto on every test change — no test is loosened,
  skipped, retried, gated off a platform or deleted to reach green unless
  the plan or the spec justifies it in writing.
- The gate applies to implementation steps (code, specs, CI); trivial
  fixups (typos, comment wording) are exempt.
- Budget (the user, 2026-09-05): ultracode and max effort go where
  judgement is — the persona discussion, the spec change and plan, the
  review, the test-integrity review, QE's verdict — not to mechanical
  steps. The Manager's practice for a mechanical step (fetching sample
  RAWs, a `cargo fmt`, polling CI, a checksum) is a foreground Bash call
  of its own rather than a max-effort subagent launch. One implementer per
  tree at a time: never two developers, or a developer and a patching
  senior developer, in the same checkout — the second works in a
  `.qe-scratch/` worktree or waits.
- Long commands are single foreground Bash calls (up to 600000 ms); CI is
  polled with an until-loop inside one call; no role ends a run to wait
  for a notification (2026-08-02: 4 of 6 relay stages died that way, with
  the brief forbidding it). The Manager watches every completion: a result
  that is a status line ("I'll wait for the monitor") is a dead agent, not
  a report — SendMessage it at once to resume with foreground polling, or
  relaunch the stage with the full hand-off.

## Directive curation (how this file and the agent files evolve)

Standing instructions live in the repo, not in any assistant's memory. When
the user gives an instruction the Manager judges to be standing policy
rather than a one-off, add it as a numbered, dated directive here
(project-wide) or to the "Standing directives" section of the relevant
agent file (role-specific; every agent file ends with one), then commit with
the message `directive: <summary>` and tell the user it was promoted. A
reviewing role that finds a claim in an agent file or a spec refuted by a
measurement reports it as a directive candidate the same way; the senior
developer owns re-verifying such claims against reality.

### Manager directives

- **M1 — Any doubt? Check the spec.** (the user, 2026-09-05) `specs/` is
  the source of truth and it moves first. A change that alters behaviour, a
  contract, a budget or a test's promise goes into `specs/` before any plan
  or code; a role that cannot find its answer in `specs/` asks the Manager,
  never guesses from the code; code never contradicts a spec sentence
  without the sentence changing in the same commit; a spec sentence found
  wrong is corrected as part of the work, recorded in the commit.
- **M2 — Decide UX choices by best practice after the persona gate.**
  (the user, 2026-08-28, issue #55: "make the decision based on best
  usability practices, then move to the implementation") After the gate,
  pick the best-practice answer for each remaining choice, record it with
  the date in the module spec, and state the decisions in one short block;
  ask the user only about a genuine workflow gap or a semantics change to
  an existing key. A list of relayed persona questions stalls the work.
- **M3 — Own bookkeeping issues are explained and decided, not
  adjudicated by the user.** (the user, 2026-09-05, issue #73: "I don't
  even know what 73 is about. you opened it") An issue that exists only as
  the Manager's deferral record (test plumbing, harness gaps, CI internals)
  gets two sentences on what it is and why it exists, then a decision on
  the evidence with the parties' consensus, then the work and the report.
  The user is asked only what is genuinely the user's: a trade-off in daily
  workflow (build time, what CI runs) or anything user-visible.
- **M4 — Check for peer sessions before touching the tree.** (2026-09-01:
  a background session was still alive on this repo while a new one
  launched gate agents on the same diff, and the user had to stop
  everything) Before the first tool that touches the tree, map every
  running `claude` process to its cwd (`readlink /proc/<pid>/cwd`) and
  stop, or have the user stop, any peer in this repo. Sessions in other
  repos are unrelated. A stopped peer's handover and scratch
  (`.qe-scratch/…`) are inherited evidence, not verified work; the gate
  still runs on anything it left.
- **M5 — The model split is written in the agent files.** (the user,
  2026-09-02 and 2026-09-05) Discovery, planning and review on Fable at
  max effort (persona, senior developer); execution and QE on Opus at max
  effort (developer, QE). The files are the rule: never carry a task-scoped
  model instruction into later tasks, and if a Fable agent dies on a spend
  limit mid-workflow, say so and ask before substituting Opus for a review
  role. An ad-hoc agent outside the pipeline (a discovery or survey reader
  in a workflow script) follows the same split at launch: `'fable'` for
  anything that reads, measures or judges, `'opus'` for anything that
  edits the tree or runs the suite.
- **M6 — Data before architectural decisions.** (the user's standing
  preference, from the assistant's profile note of the user: benchmarks
  before architectural decisions; migrated 2026-09-05) An architectural
  choice is put to the user with a benchmark
  or a measurement, not an argument; a throwaway benchmark under
  `.qe-scratch/` is cheaper than a wrong decision.
- **M7 — Never name the user.** (2026-07-26) Specs, code, commits, issues,
  briefs and relayed persona questions say "the user"; the About dialog's
  contributor credit (issue #23) is the user-directed exception.

### Open decisions the Manager tracks (do not re-ask unless relevant)

- **Held-arrow softness on 4K, issue #60** (parked by the user 2026-08-29:
  "let's discuss this in the future"). A bigger full-res cache was analysed
  and rejected — it is a decode-rate problem; the refined proposal (a
  screen-sized rung via half-scale decode, paced advance, one
  `MemoryBudget`) and six questions for the user are in the ticket. Not to
  be implemented without the user reopening it; the agreed first step is a
  throwaway benchmark of turbojpeg half-scale decode against zune-jpeg
  (baseline 305 ms) on the laptop.
- **Export frames as video (M9, v0.11.0):** still open is a FastCull-made
  `.mov` on the user's phone (only an ffmpeg-muxed file of the same shape
  was tested), InShot honouring rotation on a portrait burst, other bodies'
  JPEG flavours. The panel's rules stand if scope creeps: no editing
  surface (no crop, speed, loop, bounce, montage), README bullet never
  headline.

## Calling for help on a hard defect

**senior-developer** is available to any implementer, and to QE when a
finding needs a mechanism rather than a hypothesis. Call it when a defect
will not reproduce, a fix keeps coming back, a test is red only on CI, or a
dependency behaves in a way its documentation does not explain — and BEFORE
a second speculative fix attempt, not after the third. It diagnoses by
measurement (reproduce, instrument, bisect, mutate), names the mechanism,
and hands back the smallest provable fix; it never commits, so the developer
still owns the change and the gate above still runs.

## Conventions

- Rust 2021, `cargo fmt` formatting, clippy clean at `-D warnings`.
- Errors: `thiserror` in core, no `unwrap()` outside tests.
- Tests live next to code (`#[cfg(test)]`) for units; `tests/` per crate for
  integration; golden files under `crates/fastcull-core/tests/golden/`.
- Test data: real RAWs go in `testdata/raws/` (gitignored, fetched by script);
  only tiny synthetic fixtures are committed.
- User context: the user is a professional Sony A1 shooter, ex-Qt developer,
  new to Rust. Explanations in PRs/commits should not assume Rust fluency.
