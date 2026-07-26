# Testing builds & cutting releases

Maintainer/developer documentation. Two topics: getting a Windows test build
from CI, and producing a tagged release.

## Testing on Windows

Every CI run whose Windows job passes — on `main` and on pull requests — attaches
a ready-to-run Windows build, so you never have to set up a Rust toolchain on the
Windows machine. (The Linux and Windows jobs are independent, so check that the
whole run is green, not just that an artifact exists.)

1. Open the [**Actions** page](https://github.com/danilocesar/fastcull/actions),
   select the **CI** workflow (`ci.yml`), and click the most recent run with a
   green check mark.
2. Scroll to the **Artifacts** section at the bottom of the run summary page and
   download **`fastcull-windows-x64`**. GitHub always hands it over as a `.zip`.
3. Unzip it anywhere. It contains `fastcull-app.exe`, `fastcull-cli.exe`,
   `LICENSE`, `THIRD-PARTY-LICENSES.md`, the README, and a `BUILD-INFO.txt`
   naming the exact commit it was built from.
4. Double-click `fastcull-app.exe`.

The first launch shows a blue **"Windows protected your PC"** dialog. That is
SmartScreen reacting to an executable nobody has paid a certificate authority to
sign — not a virus warning. Click **More info**, then **Run anyway**. Windows
remembers the choice for that copy of the file; a newly downloaded build asks
again.

Notes:

- Artifacts expire **14 days** after the run; grab a fresh one after that.
- Downloading artifacts requires being signed in to GitHub.
- The binaries are linked against a static C runtime, so no "Visual C++
  Redistributable" install is needed.

Tagged releases (`v*`) additionally publish Linux and Windows archives on the
[**Releases** page](https://github.com/danilocesar/fastcull/releases) — those
are the builds to hand to other people; CI artifacts are for your own testing.

## Cutting a release

Releases are produced by [dist](https://opensource.axo.dev/cargo-dist/) (formerly
cargo-dist). `dist-workspace.toml` is the configuration;
`.github/workflows/release.yml` is **generated from it** and should never be
edited by hand.

**A `v0.1.0` tag already exists** (it marks the end of milestone M4, a commit
that predates this pipeline). The first tag that actually builds a release must
be a new version — bump `[workspace.package] version` in `Cargo.toml` first. To
release `0.1.1`:

```sh
# 1. bump the version in Cargo.toml, then refresh Cargo.lock
cargo check --workspace
# 2. see exactly which archives the tag will produce, before creating it
dist plan
# 3. commit, tag, push
git commit -am "Release 0.1.1"
git tag v0.1.1
git push && git push --tags
```

Pushing the tag starts `release.yml`, which builds on a Linux and a Windows
runner, uploads `fastcull-app-<target>` and `fastcull-cli-<target>` archives plus
SHA-256 checksums, and creates the GitHub Release. It also attaches a
`source.tar.gz` — the complete corresponding source for the binaries, as the GPL
requires.

Two things worth knowing before you rely on it:

- The trigger is not literally `v*`. dist generates the glob
  `**[0-9]+.[0-9]+.[0-9]+*`, so *any* tag containing a `X.Y.Z` triplet starts the
  workflow. A tag whose version does not match `Cargo.toml` fails in the first
  job and creates nothing.
- On pull requests the workflow only runs `dist plan`; it never builds. That
  catches a malformed `dist-workspace.toml`, but **not** a missing `include`
  file or a platform build break. Those first appear when a real tag is pushed —
  which is why the first run of this pipeline should use a throwaway prerelease
  tag such as `v0.1.1-rc.1` (dist marks those `--prerelease`, and they can be
  deleted after).

If a release run fails halfway, the tag is already public. Recover by deleting
both the release and the tag, then fixing and re-tagging:

```sh
gh release delete v0.1.1 --yes    # only if a release/draft was created
git push --delete origin v0.1.1
git tag -d v0.1.1
```

After changing `dist-workspace.toml`, regenerate the workflow and commit both
files together:

```sh
cargo install cargo-dist --locked --version 0.32.0   # or the prebuilt installer
dist generate          # rewrites .github/workflows/release.yml
dist generate --check  # fails if release.yml was hand-edited
```

`dist generate --check` only compares the generated workflow against what dist
would write now; it does **not** validate the rest of `dist-workspace.toml`.
Keys like `targets`, `installers` and `[dist.dependencies.apt]` are read at
release time by the workflow itself, so changing them takes effect without
regeneration — and without any check catching a typo. Use `dist plan` to see
what they actually do.

Use `dist generate`, **not** `dist init` — `init` rewrites `dist-workspace.toml`
and replaces its explanatory comments with boilerplate.
