//! THE APPLICATION ICON IS THE SPEC'S SET, METADATA-FREE, MADE BY ITS SCRIPT,
//! AND WORN WHERE THE SPEC SAYS — the repository half of
//! specs/modules/app-icon.md.
//!
//! `assets/icon/` is derived output committed beside its sources: two SVG
//! drawings, `render-icon.sh` (the only producer), nine PNGs and the Windows
//! `.ico`. Neither CI runner can render them (the Linux image has no
//! ImageMagick, and the script does not run on Windows), so the renders are
//! committed, and these tests are what stops a hand-edited, mis-sized or
//! manifest-carrying file from shipping unnoticed:
//!
//! - the set is exactly the spec's, each PNG N×N 8-bit RGBA and the `.ico`
//!   its seven 32-bit DIB members in order (AC1);
//! - every file is metadata-free: PNG chunks `IHDR`/`IDAT`/`IEND` only, and
//!   no C2PA, JUMBF, XMP or EXIF marker anywhere (AC2 — "Make sure it's
//!   c2pa free", the user, 2026-10-06);
//! - the script, run into a temp dir, reproduces the committed bytes: a
//!   match is green on any tool versions, a mismatch red on the recorded
//!   ones and passed with a printed reason on any other, where the tool is
//!   as likely as the drawing to be the cause (AC1); and its librsvg probe
//!   never refuses a seat that has the delegate (AC1);
//! - the window binds the 48 px PNG (AC4) and the README's title row wears
//!   the 64 px one (AC5).
//!
//! The Windows executable's resource icon (AC3) is not tested here: it
//! exists only in a Windows build, and CI's "Verify Windows artifact" step
//! asserts it on the built exe.
//!
//! The files are read from the repository through `CARGO_MANIFEST_DIR`, as
//! `tests/shortcuts_map.rs` reads the spec and the `.slint`. The PNG and ICO
//! walkers below are TEST code: nothing in FastCull parses a PNG or an ICO at
//! run time (the window icon is embedded and decoded by Slint), so there is
//! no product rule here for `fastcull-core` to own (hard rule 5).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The PNG sizes the spec keeps — the hicolor sizes, so the packaging unit
/// finds its files. The small drawing renders up to 32, the master from 48.
const PNG_SIZES: [u32; 9] = [16, 22, 24, 32, 48, 64, 128, 256, 512];

/// The `.ico`'s members, in the order the spec fixes. 20 is rendered for the
/// `.ico` only and is not kept as a PNG.
const ICO_SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 256];

/// Everything directly under `assets/icon/`, in the byte order
/// `sorted_names` returns (`-` sorts before `.`).
const ICON_DIR: [&str; 5] = [
    "fastcull-small.svg",
    "fastcull.ico",
    "fastcull.svg",
    "png",
    "render-icon.sh",
];

/// The byte strings no rendered file may contain, matched CASE-SENSITIVELY
/// (app-icon.md, "Metadata-free"): the canonical spellings every manifest
/// writer emits. A case-insensitive scan would false-positive on compressed
/// pixel data — a four-letter marker turns up by chance at roughly 1e-4 per
/// file when case is ignored.
const MARKERS: &[&[u8]] = &[
    b"c2pa",
    b"jumb",
    b"jumd",
    b"urn:uuid",
    b"<x:xmpmeta",
    b"adobe:ns:meta",
    b"Exif",
];

/// The development seat's tool versions — ImageMagick's version and
/// quantum, and its librsvg delegate (app-icon.md, "Contracts") — on which
/// the committed renders reproduce byte for byte. The reproduction test
/// compares on any versions; these decide only what a mismatch means: red
/// on them, a printed reason on any other. They follow the development
/// seat's tools whenever those move — with the re-rendered files when the
/// bytes changed, alone when they did not (7.1.2-27 → 7.1.2-32 on
/// 2026-10-06 changed none of the ten files) — so that the one seat that
/// can compare keeps comparing.
const RECORDED_TOOLS: (&str, &str, &str) = ("7.1.2-32", "Q16-HDRI", "RSVG 2.62.3");

/// The eight bytes every PNG file starts with.
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn repo_root() -> PathBuf {
    // crates/fastcull-app -> crates -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/fastcull-app")
        .to_path_buf()
}

fn icon_dir() -> PathBuf {
    repo_root().join("assets").join("icon")
}

fn png_name(size: u32) -> String {
    format!("fastcull-{size}.png")
}

/// The ten rendered files, as paths relative to an output directory of
/// `render-icon.sh`: the nine PNGs, then the `.ico`.
fn rendered_files() -> Vec<String> {
    let mut files: Vec<String> = PNG_SIZES
        .iter()
        .map(|&n| format!("png/{}", png_name(n)))
        .collect();
    files.push("fastcull.ico".to_string());
    files
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The names in a directory, sorted byte-wise.
fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("listing {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("listing {}: {e}", dir.display()))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The chunk types of a PNG, in file order. A chunk is its data length (4
/// bytes, big-endian), its type (4 ASCII letters), the data, and a 4-byte
/// CRC. The walk runs to the end of the buffer, so anything appended after
/// `IEND` is walked too; a truncated chunk panics with its offset instead of
/// reading past the end.
fn png_chunks(bytes: &[u8], what: &str) -> Vec<[u8; 4]> {
    assert!(bytes.starts_with(PNG_SIGNATURE), "{what}: no PNG signature");
    let mut chunks = Vec::new();
    let mut pos = PNG_SIGNATURE.len();
    while pos < bytes.len() {
        let header = bytes
            .get(pos..pos + 8)
            .unwrap_or_else(|| panic!("{what}: truncated chunk header at offset {pos}"));
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = [header[4], header[5], header[6], header[7]];
        let end = pos.saturating_add(12).saturating_add(len);
        assert!(
            end <= bytes.len(),
            "{what}: chunk {} at offset {pos} declares {len} data bytes, past the end of the file",
            String::from_utf8_lossy(&kind)
        );
        chunks.push(kind);
        pos = end;
    }
    chunks
}

/// The metadata-free chunk rule (app-icon.md, "Metadata-free"): exactly
/// `IHDR`, one or more `IDAT`, `IEND`, in that order — so no text chunk, no
/// `eXIf`, no colour or time chunk, and no `caBX`, where a C2PA manifest
/// lives.
fn assert_bare_chunks(bytes: &[u8], what: &str) {
    let chunks = png_chunks(bytes, what);
    let bare = chunks.len() >= 3
        && chunks[0] == *b"IHDR"
        && chunks[chunks.len() - 1] == *b"IEND"
        && chunks[1..chunks.len() - 1].iter().all(|c| c == b"IDAT");
    let names: Vec<String> = chunks
        .iter()
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    assert!(
        bare,
        "{what}: chunks {names:?}, but a metadata-free PNG holds exactly IHDR, IDAT…, IEND \
         (app-icon.md, \"Metadata-free\")"
    );
}

/// Width, height, bit depth and colour type from the `IHDR` chunk, which
/// the PNG format puts first (colour type 6 is RGBA).
fn png_ihdr(bytes: &[u8], what: &str) -> (u32, u32, u8, u8) {
    assert!(bytes.starts_with(PNG_SIGNATURE), "{what}: no PNG signature");
    assert!(
        bytes.len() >= 8 + 8 + 13 && &bytes[12..16] == b"IHDR",
        "{what}: the first chunk is not a whole IHDR"
    );
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    (be(16), be(20), bytes[24], bytes[25])
}

/// One entry of an `.ico`'s directory, and the member bytes it points at.
struct IcoMember<'a> {
    width: u32,
    height: u32,
    bits_per_pixel: u16,
    data: &'a [u8],
}

/// The members of an `.ico`. The file opens with a 6-byte header (reserved
/// 0, type 1 = icon, the member count), then one 16-byte directory entry per
/// member: width and height in one byte each (0 means 256), colour count,
/// reserved, planes, bits per pixel, the member's byte size and its offset
/// in the file. A member that runs past the end panics, naming it.
fn ico_members<'a>(bytes: &'a [u8], what: &str) -> Vec<IcoMember<'a>> {
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    assert!(bytes.len() >= 6, "{what}: shorter than an icon header");
    assert_eq!(
        (u16_at(0), u16_at(2)),
        (0, 1),
        "{what}: not an icon file (header reserved, type)"
    );
    let count = usize::from(u16_at(4));
    assert!(
        bytes.len() >= 6 + 16 * count,
        "{what}: a directory of {count} entries runs past the end"
    );
    let side = |b: u8| if b == 0 { 256 } else { u32::from(b) };
    (0..count)
        .map(|i| {
            let entry = 6 + 16 * i;
            let size = u32_at(entry + 8) as usize;
            let offset = u32_at(entry + 12) as usize;
            let data = bytes
                .get(offset..offset.saturating_add(size))
                .unwrap_or_else(|| {
                    panic!("{what}: member {i} ({size} bytes at offset {offset}) runs past the end")
                });
            IcoMember {
                width: side(bytes[entry]),
                height: side(bytes[entry + 1]),
                bits_per_pixel: u16_at(entry + 6),
                data,
            }
        })
        .collect()
}

/// The `BITMAPINFOHEADER` fields that make an `.ico` member "a 32-bit DIB of
/// its stated size" (app-icon.md, "The asset set"): the header's own size
/// (40 for a BITMAPINFOHEADER), the width, the height — twice the width in
/// an icon, whose bitmap is the colour image stacked on its 1-bit AND mask —
/// and the bits per pixel. The directory entry repeats the size and depth,
/// but a member is decoded from this header, so the two are checked apart.
fn dib_header(member: &[u8], what: &str) -> (u32, i32, i32, u16) {
    let header = member
        .get(..40)
        .unwrap_or_else(|| panic!("{what}: shorter than a BITMAPINFOHEADER"));
    let le32 = |at: usize| [header[at], header[at + 1], header[at + 2], header[at + 3]];
    (
        u32::from_le_bytes(le32(0)),
        i32::from_le_bytes(le32(4)),
        i32::from_le_bytes(le32(8)),
        u16::from_le_bytes([header[14], header[15]]),
    )
}

/// The first metadata marker the bytes contain, if any.
fn marker_in(bytes: &[u8]) -> Option<&'static [u8]> {
    MARKERS
        .iter()
        .copied()
        .find(|marker| bytes.windows(marker.len()).any(|w| w == *marker))
}

/// AC1, the set: `assets/icon/` holds exactly the spec's files — an extra
/// file, a missing size, a leftover `_ico-20.png` or a renamed script is red,
/// which also pins R7 (no other concept of the three rounds enters the
/// repository) — each PNG N×N 8-bit RGBA, and the `.ico` its seven members
/// in the spec's order, each a 32-bit DIB of its stated size: the directory
/// entry says N×N at 32 bits, and so does the member's own header (size 40,
/// width N, height 2N, 32 bits per pixel). ImageMagick writes exactly that
/// for all seven, the 256 px member included.
///
/// Mutants (2026-10-06, each in the working tree, then restored with `git
/// checkout`): an empty `png/extra.png` → red at the listing; the 32 px PNG
/// copied over the 48 → red at its IHDR (32 ≠ 48); the 16 px member replaced
/// by `png/fastcull-16.png`'s bytes, the directory's sizes and offsets
/// rewritten to match → red naming member 0 as PNG-encoded; member 4's
/// header bit count edited from 32 to 24 → red at its header.
#[test]
fn the_icon_assets_are_exactly_the_spec_set() {
    let icon = icon_dir();
    assert_eq!(
        sorted_names(&icon),
        ICON_DIR,
        "assets/icon/ holds exactly the spec's files (app-icon.md, \"The asset set\")"
    );
    let mut want_pngs: Vec<String> = PNG_SIZES.iter().map(|&n| png_name(n)).collect();
    want_pngs.sort();
    assert_eq!(
        sorted_names(&icon.join("png")),
        want_pngs,
        "assets/icon/png/ holds exactly the nine hicolor sizes (app-icon.md, \"The asset set\")"
    );
    for n in PNG_SIZES {
        let name = format!("png/{}", png_name(n));
        assert_eq!(
            png_ihdr(&read(&icon.join(&name)), &name),
            (n, n, 8, 6),
            "{name}: IHDR (width, height, bit depth, colour type) must be {n}×{n}, 8-bit RGBA"
        );
    }
    let ico = read(&icon.join("fastcull.ico"));
    let members = ico_members(&ico, "fastcull.ico");
    let got: Vec<(u32, u32, u16)> = members
        .iter()
        .map(|m| (m.width, m.height, m.bits_per_pixel))
        .collect();
    let want: Vec<(u32, u32, u16)> = ICO_SIZES.iter().map(|&n| (n, n, 32)).collect();
    assert_eq!(
        got, want,
        "fastcull.ico: its members (width, height, bits per pixel), in the spec's order"
    );
    for (i, (member, n)) in members.iter().zip(ICO_SIZES).enumerate() {
        let what = format!("fastcull.ico member {i} ({n} px)");
        assert!(
            !member.data.starts_with(PNG_SIGNATURE),
            "{what} is PNG-encoded, but each member is a 32-bit DIB (app-icon.md, \"The asset set\")"
        );
        let side = i32::try_from(n).expect("an icon's side fits an i32");
        assert_eq!(
            dib_header(member.data, &what),
            (40, side, 2 * side, 32),
            "{what}: its BITMAPINFOHEADER (header size, width, height, bits per pixel) must be a \
             32-bit DIB of its stated size, the height doubled by the AND mask (app-icon.md, \"The \
             asset set\")"
        );
    }
}

/// AC2, metadata-free: every committed PNG walks as `IHDR`/`IDAT`/`IEND`
/// only, every PNG-encoded `.ico` member too (ImageMagick writes DIB members
/// today, and a DIB has no chunk surface, so that branch guards a future
/// ImageMagick that writes PNG members), and no file holds a marker.
///
/// Mutants (2026-10-06, restored with `git checkout -- assets/icon`): a
/// `tEXt` chunk inserted before `IEND` in `png/fastcull-16.png`, or appended
/// after it in `png/fastcull-64.png` → red naming `tEXt`; the bytes `c2pa`
/// appended to `fastcull.ico` → red naming `c2pa`.
#[test]
fn every_rendered_icon_file_is_metadata_free() {
    let icon = icon_dir();
    let pngs: Vec<String> = sorted_names(&icon.join("png"))
        .into_iter()
        .filter(|name| name.ends_with(".png"))
        .map(|name| format!("png/{name}"))
        .collect();
    // Every PNG in the folder is audited, and the spec's nine must be among
    // them, so the audit can never pass on an empty folder.
    for n in PNG_SIZES {
        let name = format!("png/{}", png_name(n));
        assert!(pngs.contains(&name), "{name} is missing from assets/icon/");
    }
    for name in &pngs {
        assert_bare_chunks(&read(&icon.join(name)), name);
    }
    let ico = read(&icon.join("fastcull.ico"));
    for (i, member) in ico_members(&ico, "fastcull.ico").iter().enumerate() {
        if member.data.starts_with(PNG_SIGNATURE) {
            assert_bare_chunks(member.data, &format!("fastcull.ico member {i}"));
        }
    }
    for name in pngs.iter().map(String::as_str).chain(["fastcull.ico"]) {
        if let Some(marker) = marker_in(&read(&icon.join(name))) {
            panic!(
                "{name} contains the metadata marker {:?}: a rendered icon file must carry no \
                 C2PA, JUMBF, XMP or EXIF data (app-icon.md, \"Metadata-free\")",
                String::from_utf8_lossy(marker)
            );
        }
    }
}

/// The seat's ImageMagick version, quantum and librsvg delegate, in
/// `RECORDED_TOOLS`'s shape, or `None` when `magick` cannot be started. A
/// field the output does not carry comes back empty: an empty librsvg field
/// means the seat has no delegate, and the reproduction test skips; an empty
/// version or quantum never equals the recorded one, so a mismatch there
/// passes with its printed reason rather than red.
fn render_tools() -> Option<(String, String, String)> {
    let version = Command::new("magick").arg("-version").output().ok()?;
    let version = String::from_utf8_lossy(&version.stdout);
    // "Version: ImageMagick 7.1.2-27 (Beta) Q16-HDRI x86_64 24344 https://…"
    let tokens: Vec<&str> = version
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let release = tokens
        .iter()
        .position(|&t| t == "ImageMagick")
        .and_then(|i| tokens.get(i + 1))
        .map_or_else(String::new, |t| t.to_string());
    let quantum = tokens
        .iter()
        .find(|t| t.starts_with('Q') && t[1..].starts_with(|c: char| c.is_ascii_digit()))
        .map_or_else(String::new, |t| t.to_string());
    // "…Librsvg SVG renderer (RSVG 2.62.3)"; a build without the delegate
    // lists no such token.
    let formats = Command::new("magick").args(["-list", "format"]).output();
    let formats = formats
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let rsvg = formats
        .match_indices("RSVG ")
        .find_map(|(at, _)| {
            let number: String = formats[at + "RSVG ".len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            (!number.is_empty()).then(|| format!("RSVG {number}"))
        })
        .unwrap_or_default();
    Some((release, quantum, rsvg))
}

/// Removes the reproduction's temp dir on every way out of the test, a
/// failed assertion included: `drop` runs while a panic unwinds.
struct TempDir(PathBuf);

impl TempDir {
    fn new(path: PathBuf) -> Self {
        // A leftover from an earlier run that died with the same pid.
        let _ = fs::remove_dir_all(&path);
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// AC1, reproducible: `render-icon.sh`, run into a temp dir, reproduces
/// every committed PNG and the `.ico` byte for byte. This is the only guard
/// for "the script is the only producer": a drawing changed without a
/// re-render, or a render edited by hand, is red here.
///
/// Compare first, judge second (app-icon.md, "The render script is the only
/// producer"): on any unix seat whose `magick` lists the librsvg delegate,
/// the script runs and its ten files are compared with the committed ones.
/// A match is green on any tool versions, so the guard keeps comparing when
/// the seat's tools move without changing a byte — the old gate compared
/// only on the recorded versions, and compared nowhere once this seat's
/// ImageMagick moved from 7.1.2-27 to 7.1.2-32 on 2026-10-06 with every
/// byte unchanged. A mismatch is red on the recorded versions
/// (`RECORDED_TOOLS`) and passes with a printed reason on any other, where
/// a different ImageMagick or librsvg is as likely as the drawing to be the
/// cause.
///
/// Neither CI runner compares. The Linux image has no ImageMagick. On
/// Windows the test passes with a printed reason before anything runs: the
/// script is the Linux development seat's maintainer tool, and Rust's
/// program search there looks in System32, where WSL puts its `bash.exe`,
/// before PATH (`std`'s `sys/process/windows.rs`, `search_paths`, Rust
/// 1.99.0). The runner's ImageMagick is the official Windows build, which
/// by its dependency list bundles librsvg 2.40.20 (not measured on the
/// runner), so "wherever `magick` lists the delegate" alone could run the
/// script where it cannot succeed. The printed reasons go to stderr, which
/// libtest shows on a pass only under `--nocapture`.
///
/// Mutants (2026-10-06, this seat on 7.1.2-32, each file restored with `git
/// checkout --`): one IDAT byte flipped in `png/fastcull-22.png` → red
/// naming it; `#4da3ff` → `#4da3fe` in `fastcull.svg` without a re-render →
/// red on `png/fastcull-48.png`, the first PNG the master renders. With
/// `RECORDED_TOOLS` edited to another version (not committed), the first
/// mutant passes with the printed reason and the clean tree is green.
#[test]
fn the_render_script_reproduces_the_committed_renders() {
    if !cfg!(unix) {
        eprintln!(
            "icon reproduction skipped: render-icon.sh is the Linux development seat's \
             maintainer tool and is not run on this OS (app-icon.md, \"Contracts\")"
        );
        return;
    }
    let Some(found) = render_tools() else {
        eprintln!(
            "icon reproduction skipped: magick not found on this seat (app-icon.md, \"The render \
             script is the only producer\")"
        );
        return;
    };
    if found.2.is_empty() {
        eprintln!(
            "icon reproduction skipped: this seat's magick ({} {}) lists no librsvg delegate, \
             which render-icon.sh needs (app-icon.md, \"The render script is the only producer\")",
            found.0, found.1
        );
        return;
    }
    let icon = icon_dir();
    let out =
        TempDir::new(std::env::temp_dir().join(format!("fastcull-icon-{}", std::process::id())));
    let run = Command::new("bash")
        .arg(icon.join("render-icon.sh"))
        .arg(&out.0)
        .output()
        .expect("starting bash for assets/icon/render-icon.sh");
    assert!(
        run.status.success(),
        "render-icon.sh exited with {}; its stderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let files = rendered_files();
    // Every file that differs, with the rendered and the committed length.
    let differing: Vec<(&str, usize, usize)> = files
        .iter()
        .filter_map(|name| {
            let committed = read(&icon.join(name));
            let rendered = read(&out.0.join(name));
            if rendered == committed {
                None
            } else {
                Some((name.as_str(), rendered.len(), committed.len()))
            }
        })
        .collect();
    let Some(&(first, rendered_len, committed_len)) = differing.first() else {
        // Every byte reproduced, on whatever tool versions this seat has.
        return;
    };
    let names: Vec<&str> = differing.iter().map(|&(name, _, _)| name).collect();
    let recorded = (
        RECORDED_TOOLS.0.to_string(),
        RECORDED_TOOLS.1.to_string(),
        RECORDED_TOOLS.2.to_string(),
    );
    assert!(
        found != recorded,
        "{first}: the script renders {rendered_len} bytes that differ from the committed \
         {committed_len} — a drawing changed without a re-render, or a render edited by hand \
         (app-icon.md, \"The render script is the only producer\"); {} of {} files differ: \
         {names:?}",
        differing.len(),
        files.len()
    );
    eprintln!(
        "icon reproduction not judged: {} of {} files differ ({names:?}), but this seat renders \
         with {found:?} and the recorded versions are {recorded:?}, so the tool is as likely as \
         the drawing to be the cause (app-icon.md, \"The render script is the only producer\")",
        differing.len(),
        files.len()
    );
}

/// The librsvg probe reads ImageMagick's whole format list before it
/// searches it (render-icon.sh; brief 013's fix commit 3376ce6): piped
/// straight into `grep -q`, grep quit at the first match while magick was
/// still writing, magick died of SIGPIPE and `pipefail` turned a seat WITH
/// librsvg into a refusal (955 of 3,200 runs under 8-way load). This test
/// forces that race without load: a stand-in `magick` whose format list puts
/// the RSVG line first and then writes more than a pipe holds (1 MiB), so a
/// reader that quits early leaves it with unwritable bytes every time. The
/// script must get PAST the probe — the stand-in refuses the render with
/// exit 7, so the script ends there — and must not print the librsvg refusal.
///
/// Unix only: the stand-in is an executable shell script found through
/// PATH, which Git Bash on the Windows runner is not known to resolve from an
/// extension-less file, and the script is the Linux development seat's
/// maintainer tool (app-icon.md, "The render script is the only producer").
///
/// Old red (2026-10-06): with eefd561's script it refuses at the probe 20 of
/// 20; with the fixed script it reaches the render 20 of 20.
#[cfg(unix)]
#[test]
fn the_render_script_reads_the_whole_format_list_before_probing_for_librsvg() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new(
        std::env::temp_dir().join(format!("fastcull-icon-probe-{}", std::process::id())),
    );
    let bin = dir.0.join("bin");
    fs::create_dir_all(&bin).expect("creating the stand-in's directory");
    let standin = bin.join("magick");
    fs::write(
        &standin,
        "#!/usr/bin/env bash\n\
         case \"$1\" in\n\
           -version) echo \"Version: ImageMagick 0.0.0-0 Q16-HDRI stand-in\" ;;\n\
           -list) echo \"      SVG  RSVG      rw+   Scalable Vector Graphics (RSVG 2.62.3)\"; \
                  head -c 1048576 /dev/zero | tr '\\0' x ;;\n\
           *) echo \"stand-in magick: render refused\" >&2; exit 7 ;;\n\
         esac\n",
    )
    .expect("writing the stand-in magick");
    fs::set_permissions(&standin, fs::Permissions::from_mode(0o755)).expect("chmod +x");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new("bash")
        .arg(icon_dir().join("render-icon.sh"))
        .arg(dir.0.join("out"))
        .env("PATH", path)
        .output()
        .expect("starting bash for assets/icon/render-icon.sh");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        !stderr.contains("no librsvg delegate"),
        "render-icon.sh refused a magick that lists librsvg: the probe stopped reading before \
         magick finished writing (app-icon.md, \"The render script is the only producer\"); \
         stderr:\n{stderr}"
    );
    assert_eq!(
        run.status.code(),
        Some(7),
        "render-icon.sh should get past the probe and stop at the stand-in's render refusal; \
         stderr:\n{stderr}"
    );
}

/// The line that binds the window's icon, exactly as `MainWindow` carries it
/// in main.slint. The path is relative to main.slint's own directory:
/// `crates/fastcull-app/ui/`, three levels below the repository root.
const ICON_BINDING: &str = r#"icon: @image-url("../../../assets/icon/png/fastcull-48.png");"#;

/// AC4: `MainWindow`'s block of main.slint carries the exact `icon:` line
/// for the 48 px PNG, and the path in it resolves to the committed file. A
/// wrong path already fails the build (the Slint compiler embeds the file),
/// so what this test adds is the SIZE, which the build cannot see. That the
/// OS receives the bitmap is review-verified (app-icon.md, "Slint and winit
/// facts this module depends on").
///
/// Mutants (2026-10-06, main.slint restored from a copy): `48` → `32` in the
/// line → red while the build stays green; the line deleted → red (the
/// window would silently show no icon, which is what this test exists to
/// catch).
#[test]
fn the_window_binds_the_48_px_icon() {
    let root = repo_root();
    let ui = root.join("crates").join("fastcull-app").join("ui");
    let slint = String::from_utf8(read(&ui.join("main.slint"))).expect("main.slint is UTF-8");
    let start = slint
        .find("export component MainWindow inherits Window {")
        .expect("main.slint declares MainWindow");
    // MainWindow's block runs to the next top-level component, or to the end
    // of the file.
    let main_window: Vec<&str> = slint[start..]
        .lines()
        .skip(1)
        .take_while(|line| {
            !line.starts_with("export component ") && !line.starts_with("component ")
        })
        .collect();
    assert!(
        main_window.iter().any(|line| line.trim() == ICON_BINDING),
        "MainWindow's block of main.slint lacks the line `{ICON_BINDING}` (app-icon.md, \"The \
         running window's icon\")"
    );
    let relative = ICON_BINDING
        .strip_prefix("icon: @image-url(\"")
        .and_then(|rest| rest.strip_suffix("\");"))
        .expect("ICON_BINDING holds one quoted path");
    let bound = ui.join(relative);
    assert!(
        bound.is_file(),
        "main.slint binds {}, which does not exist",
        bound.display()
    );
    let committed = icon_dir().join("png").join(png_name(48));
    assert_eq!(
        bound
            .canonicalize()
            .expect("resolving the bound icon's path"),
        committed
            .canonicalize()
            .expect("resolving assets/icon/png/fastcull-48.png"),
        "main.slint's icon path must resolve to assets/icon/png/fastcull-48.png"
    );
}

/// AC5: the README's first heading line carries the 64 px PNG at 64×64 —
/// left of the title and no bigger (persona 2026-10-06) — and the image it
/// names exists.
///
/// Mutants (2026-10-06, README.md restored from a copy): the `img` removed
/// → red; `width="96"` → red.
#[test]
fn the_readme_title_row_carries_the_64_px_mark() {
    let root = repo_root();
    let readme = String::from_utf8(read(&root.join("README.md"))).expect("README.md is UTF-8");
    // `lines()` drops a `\r\n` line end whole, so a CRLF checkout reads the
    // same.
    let title = readme
        .lines()
        .find(|line| line.starts_with("# "))
        .expect("README.md has a top-level heading");
    for attribute in [
        r#"src="assets/icon/png/fastcull-64.png""#,
        r#"width="64""#,
        r#"height="64""#,
    ] {
        assert!(
            title.contains(attribute),
            "README.md's title row `{title}` lacks {attribute} (app-icon.md, \"The README mark\")"
        );
    }
    assert!(
        icon_dir().join("png").join(png_name(64)).is_file(),
        "README.md's mark assets/icon/png/fastcull-64.png does not exist"
    );
}
