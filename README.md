# FastCull

**Fast, open-source photo culling for working photographers.** A Photo Mechanic-class
ingest-and-select tool for Linux and Windows: open a folder of thousands of 100 MB RAW
files, see them instantly, pick the keepers, tag them with IPTC metadata, and hand the
selects to [darktable](https://www.darktable.org/) with every rating and tag intact.

## Why it's fast

FastCull never decodes RAW sensor data on the interactive path. Every modern RAW file
embeds camera-rendered JPEG previews — the Sony A1, for example, embeds a full-resolution
8640×5760 JPEG plus a 1616×1080 preview in every ARW. FastCull reads only those bytes
(~0.5 MB of a 100 MB file for the grid) and decodes them on all cores.

Measured on the reference machine (32-thread Ryzen, real A1 files): ~300 files/sec for
the thumbnail grid pipeline vs. 0.6–1.2 **seconds** per file for a full RAW decode —
the embedded-preview strategy is two orders of magnitude faster.

## Core features (v1)

- Catalog-free: open any folder, cull, done. No import step, no database to manage.
- Zoomable grid: many columns → few → single-image loupe → 1:1, one continuous gesture.
- Keyboard-first pick/reject culling.
- Burst detection: frames from the same burst get a colored border.
- IPTC metadata editing — single image or multi-select — with saved templates and
  variables (`{date}`, `{seq}`, …).
- Filter and sort: picked / rejected / unmarked / bursts, by time or name.
- Copy picks to a destination folder with rename templates; XMP sidecars travel along.
- Darktable-compatible by construction: all state is written to `<name>.<ext>.xmp`
  sidecars using the fields darktable, digiKam, and Lightroom read. RAW files are
  **never** modified.

## Camera support

Any camera supported by [rawler](https://github.com/dnglab/dnglab) works; the
**Sony A1 is the reference camera** and is covered by the test suite with real files
(compressed, lossless-compressed, and uncompressed ARW).

## Stack

Rust workspace: `fastcull-core` (all logic, UI-free, fully tested), `fastcull-app`
([Slint](https://slint.dev/) GPU-rendered UI), `fastcull-cli` (headless driver used by
integration tests). See `specs/` for the full architecture and module specifications —
this project is developed spec-first.

## Building

```sh
cargo build --release
cargo test --workspace
testdata/fetch.sh   # downloads sample RAWs (needed for integration tests)
```

## Testing on Windows

Every CI run whose Windows job passes — on `main` and on pull requests — attaches a
ready-to-run Windows build, so you never have to set up a Rust toolchain on the
Windows machine. (The Linux and Windows jobs are independent, so check that the
whole run is green, not just that an artifact exists.)

1. Open <https://github.com/danilocesar/fastcull/actions/workflows/ci.yml> and click
   the most recent run with a green check mark.
2. Scroll to the **Artifacts** section at the bottom of the run summary page and
   download **`fastcull-windows-x64`**. GitHub always hands it over as a `.zip`.
3. Unzip it anywhere. It contains `fastcull-app.exe`, `fastcull-cli.exe`, `LICENSE`,
   `THIRD-PARTY-LICENSES.md`, this README, and a `BUILD-INFO.txt` naming the exact
   commit it was built from.
4. Double-click `fastcull-app.exe`.

The first launch shows a blue **"Windows protected your PC"** dialog. That is
SmartScreen reacting to an executable nobody has paid a certificate authority to
sign — not a virus warning. Click **More info**, then **Run anyway**. Windows
remembers the choice for that copy of the file; a newly downloaded build asks again.

Notes:

- Artifacts expire **14 days** after the run; grab a fresh one after that.
- Downloading artifacts requires being signed in to GitHub.
- The binaries are linked against a static C runtime, so no "Visual C++
  Redistributable" install is needed.

Tagged releases (`v*`) additionally publish Linux and Windows archives on the
[Releases page](https://github.com/danilocesar/fastcull/releases) — those are the
builds to hand to other people; CI artifacts are for your own testing.

## Cutting a release

Releases are produced by [dist](https://opensource.axo.dev/cargo-dist/) (formerly
cargo-dist). `dist-workspace.toml` is the configuration;
`.github/workflows/release.yml` is **generated from it** and should never be edited
by hand.

**A `v0.1.0` tag already exists** (it marks the end of milestone M4, a commit that
predates this pipeline). The first tag that actually builds a release must be a new
version — bump `[workspace.package] version` in `Cargo.toml` first. To release
`0.1.1`:

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

Pushing the tag starts `release.yml`, which builds on a Linux and a Windows runner,
uploads `fastcull-app-<target>` and `fastcull-cli-<target>` archives plus SHA-256
checksums, and creates the GitHub Release. It also attaches a `source.tar.gz` — the
complete corresponding source for the binaries, as the GPL requires.

Two things worth knowing before you rely on it:

- The trigger is not literally `v*`. dist generates the glob
  `**[0-9]+.[0-9]+.[0-9]+*`, so *any* tag containing a `X.Y.Z` triplet starts the
  workflow. A tag whose version does not match `Cargo.toml` fails in the first job
  and creates nothing.
- On pull requests the workflow only runs `dist plan`; it never builds. That catches
  a malformed `dist-workspace.toml`, but **not** a missing `include` file or a
  platform build break. Those first appear when a real tag is pushed — which is why
  the first run of this pipeline should use a throwaway prerelease tag such as
  `v0.1.1-rc.1` (dist marks those `--prerelease`, and they can be deleted after).

If a release run fails halfway, the tag is already public. Recover by deleting both
the release and the tag, then fixing and re-tagging:

```sh
gh release delete v0.1.1 --yes    # only if a release/draft was created
git push --delete origin v0.1.1
git tag -d v0.1.1
```

After changing `dist-workspace.toml`, regenerate the workflow and commit both files
together:

```sh
cargo install cargo-dist --locked --version 0.32.0   # or the prebuilt installer
dist generate          # rewrites .github/workflows/release.yml
dist generate --check  # fails if release.yml was hand-edited
```

`dist generate --check` only compares the generated workflow against what dist
would write now; it does **not** validate the rest of `dist-workspace.toml`. Keys
like `targets`, `installers` and `[dist.dependencies.apt]` are read at release time
by the workflow itself, so changing them takes effect without regeneration — and
without any check catching a typo. Use `dist plan` to see what they actually do.

Use `dist generate`, **not** `dist init` — `init` rewrites `dist-workspace.toml`
and replaces its explanatory comments with boilerplate.

## License

FastCull's own source code is GPL-3.0-or-later. See [LICENSE](LICENSE).

The **binaries** we publish link Slint, which is offered under GPL-3.0-**only** (or
a paid commercial licence), so a distributed FastCull executable as a whole is
conveyed under GPL version 3 — "or later" applies to our source, not to the linked
result. The complete corresponding source code for any binary we publish is the git
commit it was built from, in this repository:
<https://github.com/danilocesar/fastcull>. CI artifacts record that commit in
`BUILD-INFO.txt`. Release archives do not carry the commit inside them — the
release tag on GitHub identifies it, and every release also ships a
`source.tar.gz` of exactly that source. Run `fastcull-cli --version` if you need
to tell two unpacked archives apart.

Those executables also contain hundreds of Rust crates under MIT, Apache-2.0,
BSD, Unicode-3.0, MPL-2.0 and other licences, most of which require their notice
to travel with the binary. [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md)
collects them and ships inside every artifact and release archive; regenerate it
with `cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md` after changing
dependencies.

This software is based in part on the work of the Independent JPEG Group.
