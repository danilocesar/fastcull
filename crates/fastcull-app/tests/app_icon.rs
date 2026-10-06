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
//!   its seven 32-bit DIB members in order, each of its exact length and,
//!   where a PNG of its size exists, holding that PNG's pixels (AC1);
//! - every file is metadata-free: PNG chunks `IHDR`/`IDAT`/`IEND` only, each
//!   chunk's CRC verified, and no C2PA, JUMBF, XMP or EXIF marker anywhere;
//!   the two SVG sources are bare drawings (AC2 — "Make sure it's c2pa
//!   free", the user, 2026-10-06);
//! - the script, run into a temp dir, reproduces the committed bytes: a
//!   match is green on any tool versions, a mismatch red on the recorded
//!   ones — saying whether the pixels or only the encoding moved — and
//!   passed with a printed reason on any other, where the tool is as likely
//!   as the drawing to be the cause (AC1); its librsvg probe never refuses a
//!   seat that has the delegate, it names the librsvg coder so no external
//!   SVG delegate runs, and its own audit refuses a text chunk and a marker
//!   (AC1);
//! - the window binds the 48 px PNG at MainWindow's own depth (AC4), and the
//!   README's title row wears the 64 px one left of the title (AC5).
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
/// quantum, its librsvg delegate and its libpng, as `magick -version` and
/// `magick -list format` report them (app-icon.md, "Contracts") — on which
/// the committed renders reproduce byte for byte. The reproduction test
/// compares on any versions; these decide only what a mismatch means: red
/// on them, a printed reason on any other. They follow the development
/// seat's tools whenever those move — with the re-rendered files when the
/// bytes changed, alone when they did not (7.1.2-27 → 7.1.2-32 on
/// 2026-10-06 changed none of the ten files) — so that the one seat that
/// can compare keeps comparing. zlib, which writes the PNGs' compressed
/// data under libpng, reports itself nowhere `magick` can show; the red
/// names it when only the encoding moved (QE 2026-10-06, D2).
const RECORDED_TOOLS: (&str, &str, &str, &str) =
    ("7.1.2-32", "Q16-HDRI", "RSVG 2.62.3", "libpng 1.6.58");

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

/// The CRC-32 the PNG format stores after every chunk, computed over the
/// chunk's type and data: the reflected polynomial 0xEDB88320, with the
/// register started at and finally XORed with 0xFFFFFFFF (the PNG
/// specification's "CRC algorithm"). Bit by bit, without a table: the files
/// are small and this is test code.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// The chunk types of a PNG, in file order. A chunk is its data length (4
/// bytes, big-endian), its type (4 ASCII letters), the data, and a 4-byte
/// CRC. The walk runs to the end of the buffer, so anything appended after
/// `IEND` is walked too; a truncated chunk panics with its offset instead of
/// reading past the end. Every chunk's stored CRC must equal the one computed
/// over its type and data, so a byte edited by hand inside any chunk is red
/// here on both CI runners, where the reproduction test cannot compare (QE
/// 2026-10-06, TP5).
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
        let stored = u32::from_be_bytes([
            bytes[end - 4],
            bytes[end - 3],
            bytes[end - 2],
            bytes[end - 1],
        ]);
        let computed = crc32(&bytes[pos + 4..end - 4]);
        assert!(
            stored == computed,
            "{what}: chunk {} at offset {pos}: stored CRC {stored:#010x}, computed {computed:#010x}",
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

/// One entry of an `.ico`'s directory, and the member bytes it points at:
/// `data` is the directory's byte size starting at its offset in the file.
struct IcoMember<'a> {
    width: u32,
    height: u32,
    bits_per_pixel: u16,
    offset: usize,
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
                offset,
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

/// A decoded PNG: width, height, colour type, bit depth and the pixel bytes,
/// rows top to bottom.
type DecodedPng = (u32, u32, png::ColorType, png::BitDepth, Vec<u8>);

/// Decodes a PNG with the `png` crate, both checksums verified — every
/// chunk's CRC and the compressed stream's Adler-32, which the crate
/// otherwise skips — so a file damaged by hand does not decode.
fn decode_png(bytes: &[u8]) -> Result<DecodedPng, png::DecodingError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .expect("an icon's pixels fit in memory");
    let mut pixels = vec![0; size];
    let frame = reader.next_frame(&mut pixels)?;
    pixels.truncate(frame.buffer_size());
    reader.finish()?;
    Ok((
        frame.width,
        frame.height,
        frame.color_type,
        frame.bit_depth,
        pixels,
    ))
}

/// A 1×1 transparent RGBA PNG carrying a `tEXt` chunk (keyword `Comment`),
/// written by the `png` crate: the smallest file the chunk rule must refuse.
/// Its text holds none of `MARKERS`, so only the chunk rule can catch it.
/// Unix only, like its one caller, the audit test: on Windows it would be
/// dead code, which clippy's `-D warnings` refuses (CI run 37519581454).
#[cfg(unix)]
fn png_with_a_text_chunk() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk(
            "Comment".to_string(),
            "a chunk the audit must refuse".to_string(),
        )
        .expect("adding a tEXt chunk");
    let mut writer = encoder.write_header().expect("writing the PNG header");
    writer
        .write_image_data(&[0, 0, 0, 0])
        .expect("writing the one pixel");
    writer.finish().expect("finishing the PNG");
    bytes
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
/// Each member is that DIB and nothing more (QE 2026-10-06, TP4): its length
/// is the 40-byte header, N·N four-byte pixels and the AND mask's N rows of
/// one bit per pixel padded to a 32-bit boundary — 40 + 4·N·N + N·⌈N/32⌉·4,
/// so 1128, 1720, 2440, 4264, 9640, 16936 and 270376 bytes — the members
/// follow one another without a gap, and the file ends with the last. The
/// six members with a PNG of their size hold that PNG's pixels exactly: a
/// DIB stores its rows bottom-up and each pixel as blue, green, red, alpha,
/// so DIB row r is PNG row N−1−r with the colour bytes swapped. The 20 px
/// member has no PNG to compare with (it is rendered for the `.ico` only)
/// and is held to its length and header.
///
/// Mutants (2026-10-06, each in the working tree, then restored with `git
/// checkout`): an empty `png/extra.png` → red at the listing; the 32 px PNG
/// copied over the 48 → red at its IHDR (32 ≠ 48); the 16 px member replaced
/// by `png/fastcull-16.png`'s bytes, the directory's sizes and offsets
/// rewritten to match → red naming member 0 as PNG-encoded; member 4's
/// header bit count edited from 32 to 24 → red at its header; the 16 px
/// member's 1024 pixel bytes zeroed → red at its first differing pixel;
/// member 3's directory size understated by 1 → red at its length.
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
        let n = n as usize;
        assert_eq!(
            member.data.len(),
            40 + 4 * n * n + n * (n.div_ceil(32) * 4),
            "{what}: its directory size must be exactly a 32-bit DIB of {n}×{n}: the 40-byte \
             header, {n}·{n} four-byte pixels and the AND mask's {n} rows padded to 32 bits \
             (app-icon.md, \"The asset set\")"
        );
    }
    for i in 1..members.len() {
        let (before, member) = (&members[i - 1], &members[i]);
        assert_eq!(
            member.offset,
            before.offset + before.data.len(),
            "fastcull.ico member {i} must start where member {} ends",
            i - 1
        );
    }
    let last = members.last().expect("fastcull.ico has members");
    assert_eq!(
        last.offset + last.data.len(),
        ico.len(),
        "fastcull.ico must end with its last member"
    );
    for (i, (member, n)) in members.iter().zip(ICO_SIZES).enumerate() {
        if !PNG_SIZES.contains(&n) {
            continue;
        }
        let name = format!("png/{}", png_name(n));
        let (width, height, colour, depth, rgba) = decode_png(&read(&icon.join(&name)))
            .unwrap_or_else(|e| panic!("{name} does not decode: {e}"));
        assert_eq!(
            (width, height, colour, depth),
            (n, n, png::ColorType::Rgba, png::BitDepth::Eight),
            "{name}: decoded size, colour type and bit depth"
        );
        let n = n as usize;
        for r in 0..n {
            let y = n - 1 - r;
            let dib_row = &member.data[40 + r * n * 4..40 + (r + 1) * n * 4];
            let png_row = &rgba[y * n * 4..(y + 1) * n * 4];
            for x in 0..n {
                let bgra = &dib_row[x * 4..x * 4 + 4];
                let want = &png_row[x * 4..x * 4 + 4];
                assert!(
                    [bgra[2], bgra[1], bgra[0], bgra[3]] == [want[0], want[1], want[2], want[3]],
                    "fastcull.ico member {i} ({n} px): pixel ({x}, {y}), counted from the top \
                     left, is BGRA {bgra:?} in the .ico but RGBA {want:?} in {name} — the .ico \
                     must hold the renders' pixels (app-icon.md, \"The asset set\")"
                );
            }
        }
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

/// What neither SVG source may contain (app-icon.md, "Metadata-free"):
/// the places a name or a manifest is typed into an SVG — `<metadata>`,
/// `<title>`, `<desc>`, an XML comment — and the ways a drawing carries an
/// external or hidden payload — `<image>`, any `href=` (`xlink:href=`
/// included), `<script>`, `<foreignObject>`. Matched case-sensitively, as
/// SVG's element names are.
const SVG_FORBIDDEN: [&str; 8] = [
    "<metadata",
    "<title",
    "<desc",
    "<!--",
    "<image",
    "href=",
    "<script",
    "<foreignObject",
];

/// AC2, the sources: the two SVG drawings are bare drawings — none of
/// `SVG_FORBIDDEN`, none of the rendered files' markers, and a root `<svg>`
/// element with the 512×512 `viewBox` both are drawn on (the user,
/// 2026-10-06, "c2pa free", and M7; widened to the sources by the Manager,
/// QE 2026-10-06, Q1). A source carrying any of these would render it into
/// the PNGs, or carry it into the repository, without any rendered file's
/// audit seeing it.
///
/// Mutants (2026-10-06, each in fastcull.svg, restored with `git checkout
/// --`): `<metadata><rdf:RDF/></metadata>` before `</svg>` → red; `<image
/// href="x.png"/>` → red; the viewBox `0 0 500 500` → red; `<!-- -->` → red.
#[test]
fn the_svg_sources_are_bare_drawings() {
    for name in ["fastcull.svg", "fastcull-small.svg"] {
        let svg = String::from_utf8(read(&icon_dir().join(name)))
            .unwrap_or_else(|e| panic!("{name} is not UTF-8: {e}"));
        for forbidden in SVG_FORBIDDEN {
            assert!(
                !svg.contains(forbidden),
                "{name} contains `{forbidden}`: an SVG source is a bare drawing, with no \
                 metadata, title, description, comment, embedded image, link, script or foreign \
                 object (app-icon.md, \"Metadata-free\")"
            );
        }
        if let Some(marker) = marker_in(svg.as_bytes()) {
            panic!(
                "{name} contains the metadata marker {:?} (app-icon.md, \"Metadata-free\")",
                String::from_utf8_lossy(marker)
            );
        }
        let root = svg
            .find("<svg")
            .and_then(|at| svg[at..].find('>').map(|end| &svg[at..=at + end]))
            .unwrap_or_else(|| panic!("{name} has no <svg> root element"));
        assert!(
            root.contains(r#"viewBox="0 0 512 512""#),
            "{name}: its root element `{root}` must carry viewBox=\"0 0 512 512\" (app-icon.md, \
             \"Metadata-free\" and \"The asset set\")"
        );
    }
}

/// The seat's ImageMagick version, quantum, librsvg delegate and libpng, in
/// `RECORDED_TOOLS`'s shape, or `None` when `magick` cannot be started. A
/// field the output does not carry comes back empty: an empty librsvg field
/// means the seat has no delegate, and the reproduction test skips; an empty
/// version, quantum or libpng never equals the recorded one, so a mismatch
/// there passes with its printed reason rather than red.
fn render_tools() -> Option<(String, String, String, String)> {
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
    // "      PNG* PNG       rw-   Portable Network Graphics (libpng 1.6.58)":
    // the line whose format column is PNG (`*` marks native blob support).
    let libpng = formats
        .lines()
        .find(|line| matches!(line.split_whitespace().next(), Some("PNG*" | "PNG")))
        .and_then(|line| {
            let at = line.find("libpng ")?;
            let number: String = line[at + "libpng ".len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            (!number.is_empty()).then(|| format!("libpng {number}"))
        })
        .unwrap_or_default();
    Some((release, quantum, rsvg, libpng))
}

/// Whether a PNG the script rendered and the committed one hold the same
/// image — `None` — or how they differ. Two files with different bytes but
/// identical pixels differ only in how they were encoded.
fn pixel_difference(committed: &[u8], rendered: &[u8]) -> Option<String> {
    match (decode_png(committed), decode_png(rendered)) {
        (Ok(c), Ok(r)) if c == r => None,
        (Ok(c), Ok(r)) if (c.0, c.1, c.2, c.3) != (r.0, r.1, r.2, r.3) => Some(format!(
            "committed {:?}, rendered {:?} (width, height, colour type, bit depth)",
            (c.0, c.1, c.2, c.3),
            (r.0, r.1, r.2, r.3)
        )),
        (Ok(c), Ok(r)) => Some(format!(
            "{} of {} pixel bytes differ",
            c.4.iter().zip(&r.4).filter(|(a, b)| a != b).count(),
            c.4.len()
        )),
        (Err(e), _) => Some(format!("the committed file does not decode: {e}")),
        (_, Err(e)) => Some(format!("the rendered file does not decode: {e}")),
    }
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
/// a different ImageMagick, librsvg or libpng is as likely as the drawing to
/// be the cause.
///
/// The red reads the pixels before it speaks (QE 2026-10-06, D2): every
/// differing PNG pair is decoded, checksums on, and compared. Pixels that
/// differ mean the drawing changed without a re-render, a render was edited
/// by hand, or a rasteriser library under the recorded ImageMagick and
/// librsvg (cairo, pixman) moved. Pixels identical in every differing file
/// mean only the encoding changed — zlib, which ImageMagick does not report,
/// moved under the recorded versions, or a file was re-encoded by another
/// tool. Both are red: the remedy for both is a re-render committed with its
/// reason, and `RECORDED_TOOLS` moves only when `magick`'s own report moved.
/// The `.ico`'s members are uncompressed, so a difference there is never the
/// encoder's.
///
/// Neither CI runner compares. The Linux image has no ImageMagick. On
/// Windows the test passes with a printed reason before anything runs: it
/// gives its child no PATH of its own, and Rust's program search for such a
/// child looks in the executable's directory, then System32 — where WSL puts
/// its `bash.exe` — then the Windows directory, and only then in the
/// parent's PATH, where Git Bash would be (`std`'s `sys/process/windows.rs`,
/// `search_paths`, Rust 1.99.0). The runner's ImageMagick is the official
/// Windows build, which by its dependency list bundles librsvg 2.40.20 (not
/// measured on the runner), so "wherever `magick` lists the delegate" alone
/// would have run the script where it cannot succeed; the script is the
/// Linux development seat's maintainer tool. The printed reasons go to
/// stderr, which libtest shows on a pass only under `--nocapture`.
///
/// Mutants (2026-10-06, this seat on 7.1.2-32, each file restored with `git
/// checkout --`): one IDAT byte flipped in `png/fastcull-22.png` → red
/// naming it, its committed file failing to decode; `#4da3ff` → `#4da3fe` in
/// `fastcull.svg` without a re-render → red on `png/fastcull-48.png`, the
/// first PNG the master renders, its pixels differing; the renders re-encoded
/// by a classic zlib 1.3.1 in place of this seat's zlib-ng → red saying only
/// the encoding changed. With `RECORDED_TOOLS` edited to another version (not
/// committed), the first mutant passes with the printed reason and the clean
/// tree is green.
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
    if differing.is_empty() {
        // Every byte reproduced, on whatever tool versions this seat has.
        return;
    }
    let names: Vec<&str> = differing.iter().map(|&(name, _, _)| name).collect();
    let recorded = (
        RECORDED_TOOLS.0.to_string(),
        RECORDED_TOOLS.1.to_string(),
        RECORDED_TOOLS.2.to_string(),
        RECORDED_TOOLS.3.to_string(),
    );
    if found != recorded {
        eprintln!(
            "icon reproduction not judged: {} of {} files differ ({names:?}), but this seat \
             renders with {found:?} and the recorded versions are {recorded:?}, so the tool is as \
             likely as the drawing to be the cause (app-icon.md, \"The render script is the only \
             producer\")",
            differing.len(),
            files.len()
        );
        return;
    }
    // On the recorded versions every mismatch is red; the pixels decide
    // which red. Each entry is a file whose content, not only its encoding,
    // differs, and how.
    let changed: Vec<String> = differing
        .iter()
        .filter_map(|&(name, rendered_len, committed_len)| {
            if name.ends_with(".png") {
                pixel_difference(&read(&icon.join(name)), &read(&out.0.join(name)))
                    .map(|how| format!("{name} ({how})"))
            } else {
                Some(format!(
                    "{name} ({rendered_len} bytes rendered against {committed_len} committed; its \
                     members are uncompressed, so that difference is never the encoder's)"
                ))
            }
        })
        .collect();
    assert!(
        !changed.is_empty(),
        "{} of {} files differ in their bytes but decode to the same pixels ({names:?}): only \
         the encoding changed — zlib moved under the recorded versions (ImageMagick does not \
         report it; this seat's zlib-ng reports itself as 1.3.1) or a file was re-encoded by \
         another tool — re-render and commit the new bytes saying which; move nothing in \
         RECORDED_TOOLS (fastcull.ico reproduced: its members are uncompressed, so no encoder \
         writes them) (app-icon.md, \"The render script is the only producer\")",
        differing.len(),
        files.len()
    );
    panic!(
        "the pixels differ in {} — the drawing changed without a re-render, a render was edited \
         by hand, or a rasteriser library under the recorded ImageMagick and librsvg (cairo, \
         pixman) moved (app-icon.md, \"The render script is the only producer\"); the pixels \
         differ in {} of the {} files that differ in their bytes: {}; all that differ: {names:?}",
        changed[0],
        changed.len(),
        differing.len(),
        changed.join("; ")
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
/// Unix only because it cannot compile elsewhere: the stand-in is made
/// executable with `std::os::unix::fs::PermissionsExt` and put first on a
/// `:`-joined PATH; the script is the Linux development seat's maintainer
/// tool (app-icon.md, "Contracts").
///
/// The test needs bash and nothing else on the seat: the script refuses to
/// start without python3 (its chunk audit), so a stand-in `python3` sits
/// beside the stand-in `magick` and the script gets past that check on a seat
/// without one — it stops at the stand-in's render refusal before any audit
/// runs (QE 2026-10-06, D3). Without it this test was red on such a seat for
/// the seat's lack of python3, never for the probe.
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
    // Never run: the script only asks `command -v python3`, and stops at the
    // stand-in magick's render refusal before its audit would call it.
    let python = bin.join("python3");
    fs::write(&python, "#!/usr/bin/env bash\nexit 0\n").expect("writing the stand-in python3");
    fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).expect("chmod +x");
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

/// The render names ImageMagick's librsvg coder (`RSVG:<file>`), so no
/// external SVG delegate is ever consulted (app-icon.md, "The render script
/// is the only producer"; QE 2026-10-06, D1). Handed a bare file name,
/// ImageMagick's SVG reader (`coders/svg.c`, `ReadSVGImage`, 7.1.2-32) first
/// runs the `svg:decode` delegate its `delegates.xml` names — `inkscape`, on
/// Fedora's — and falls back to librsvg only when that command is absent or
/// fails. A seat with Inkscape installed therefore rendered other bytes
/// while the script still reported librsvg's version, and the recorded tool
/// versions could not see the rasteriser that made the files.
///
/// A stand-in `inkscape`, first on PATH, logs every call and exits 1, so
/// ImageMagick falls back to librsvg and the bytes come out the same either
/// way: the red is the CALL LOG, never the bytes. `MSVG:` (ImageMagick's own
/// renderer) would leave the log empty too but changes the bytes, which the
/// reproduction test catches — the two tests pin `RSVG:` together.
///
/// Gated exactly like the reproduction test: it returns early, with a
/// printed reason, only where `magick` is absent or lists no librsvg
/// delegate, so it compares on the development seat and on neither CI
/// runner. Unix only because it cannot compile elsewhere: the stand-in is
/// made executable with `std::os::unix::fs::PermissionsExt` and put first on
/// a `:`-joined PATH.
///
/// Old red (2026-10-06, this seat, ImageMagick 7.1.2-32 with RSVG 2.62.3):
/// with b7aad8c's script, which handed ImageMagick the bare file name, the
/// stand-in logged 10 calls — one per render — in each of 10 runs; with
/// `RSVG:$1`, none, 10 runs of 10.
#[cfg(unix)]
#[test]
fn the_render_names_the_librsvg_coder_and_never_runs_an_svg_delegate() {
    use std::os::unix::fs::PermissionsExt;
    let Some(found) = render_tools() else {
        eprintln!(
            "icon delegate check skipped: magick not found on this seat (app-icon.md, \"The \
             render script is the only producer\")"
        );
        return;
    };
    if found.2.is_empty() {
        eprintln!(
            "icon delegate check skipped: this seat's magick ({} {}) lists no librsvg delegate, \
             which render-icon.sh needs (app-icon.md, \"The render script is the only producer\")",
            found.0, found.1
        );
        return;
    }
    let dir = TempDir::new(
        std::env::temp_dir().join(format!("fastcull-icon-delegate-{}", std::process::id())),
    );
    let bin = dir.0.join("bin");
    fs::create_dir_all(&bin).expect("creating the stand-in's directory");
    let standin = bin.join("inkscape");
    fs::write(
        &standin,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$INKSCAPE_LOG\"\nexit 1\n",
    )
    .expect("writing the stand-in inkscape");
    fs::set_permissions(&standin, fs::Permissions::from_mode(0o755)).expect("chmod +x");
    let log = dir.0.join("calls.log");
    fs::write(&log, "").expect("creating the stand-in's call log");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = dir.0.join("out");
    let run = Command::new("bash")
        .arg(icon_dir().join("render-icon.sh"))
        .arg(&out)
        .env("INKSCAPE_LOG", &log)
        .env("PATH", path)
        .output()
        .expect("starting bash for assets/icon/render-icon.sh");
    assert!(
        run.status.success(),
        "render-icon.sh exited with {}; its stderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    for name in ["png/fastcull-16.png", "fastcull.ico"] {
        assert!(
            out.join(name).is_file(),
            "render-icon.sh exited 0 but wrote no {name}"
        );
    }
    let calls = String::from_utf8_lossy(&read(&log)).into_owned();
    let calls: Vec<&str> = calls.lines().collect();
    assert!(
        calls.is_empty(),
        "ImageMagick ran the stand-in inkscape {} times (the first: `{}`): the render must name \
         the librsvg coder (`RSVG:<file>`), because a seat whose delegates.xml routes SVG to \
         Inkscape renders different bytes under the same reported versions (app-icon.md, \"The \
         render script is the only producer\")",
        calls.len(),
        calls.first().copied().unwrap_or_default()
    );
}

/// The script's own audit refuses what the repository test refuses
/// (app-icon.md, "Metadata-free": the script audits both rules and refuses
/// the render; QE 2026-10-06, TP9) — the check a maintainer sees before a
/// re-render is committed. A stand-in `magick` copies known files instead of
/// rendering (its last argument names the output; `$STANDIN_DATA` holds a
/// copy of each committed render, and of the 16 px PNG as the `.ico`'s 20 px
/// one), so the audit runs on bytes the test chose, on any unix seat with
/// python3 — CI's Linux job included. Three runs: the committed set exits 0
/// and says `metadata audit: clean`; a 1×1 PNG with a `tEXt` chunk in place
/// of `fastcull-22.png` exits exactly 1, naming the file's extra chunks and
/// `metadata audit: FAILED`; the `.ico` with `c2pa` appended exits exactly
/// 1, naming the marker's file.
///
/// The stand-in sits alone in its PATH folder: the audit runs the seat's own
/// python3, which the script needs, so a seat without it is red here (Q2).
/// Unix only because it cannot compile elsewhere: the stand-in is made
/// executable with `std::os::unix::fs::PermissionsExt` and put first on a
/// `:`-joined PATH.
///
/// Mutants (2026-10-06, render-icon.sh restored with `git checkout --`):
/// `|| bad=1` deleted from the chunk audit's line → the script dies there
/// under `set -e`, exit 1 without `metadata audit: FAILED`, red; `|| bad=1`
/// turned into `|| true` → the text-chunk run exits 0, red; the marker
/// scan's five lines deleted → the marker run exits 0, red.
#[cfg(unix)]
#[test]
fn the_render_scripts_audit_refuses_a_text_chunk_and_a_marker() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new(
        std::env::temp_dir().join(format!("fastcull-icon-audit-{}", std::process::id())),
    );
    let bin = dir.0.join("bin");
    fs::create_dir_all(&bin).expect("creating the stand-in's directory");
    let standin = bin.join("magick");
    fs::write(
        &standin,
        "#!/usr/bin/env bash\n\
         case \"$1\" in\n\
           -version) echo \"Version: ImageMagick 0.0.0-0 Q16-HDRI stand-in\" ;;\n\
           -list) echo \"      SVG  RSVG      rw+   Scalable Vector Graphics (RSVG 2.62.3)\" ;;\n\
           identify) echo \"stand-in identify\" ;;\n\
           *) out=\"${@: -1}\"; out=\"${out#PNG32:}\"; \
              cp \"$STANDIN_DATA/$(basename \"$out\")\" \"$out\" ;;\n\
         esac\n",
    )
    .expect("writing the stand-in magick");
    fs::set_permissions(&standin, fs::Permissions::from_mode(0o755)).expect("chmod +x");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let icon = icon_dir();
    // The files the stand-in "renders", under the names the script writes.
    let stage = |label: &str| -> PathBuf {
        let data = dir.0.join(label).join("data");
        fs::create_dir_all(&data).expect("creating the stand-in's data");
        for n in PNG_SIZES {
            fs::copy(icon.join("png").join(png_name(n)), data.join(png_name(n)))
                .expect("copying a committed render");
        }
        fs::copy(
            icon.join("png").join(png_name(16)),
            data.join("_ico-20.png"),
        )
        .expect("copying the .ico's 20 px stand-in");
        fs::copy(icon.join("fastcull.ico"), data.join("fastcull.ico"))
            .expect("copying the committed .ico");
        data
    };
    let run = |label: &str, data: &Path| -> (Option<i32>, String, String) {
        let out = Command::new("bash")
            .arg(icon.join("render-icon.sh"))
            .arg(dir.0.join(label).join("out"))
            .env("PATH", &path)
            .env("STANDIN_DATA", data)
            .output()
            .expect("starting bash for assets/icon/render-icon.sh");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (code, stdout, stderr) = run("control", &stage("control"));
    assert!(
        code == Some(0) && stdout.contains("metadata audit: clean"),
        "render-icon.sh's audit must pass the committed renders: exit {code:?}; stdout:\n{stdout}\
         stderr:\n{stderr}"
    );

    let data = stage("text-chunk");
    fs::write(data.join(png_name(22)), png_with_a_text_chunk())
        .expect("writing the PNG with a text chunk");
    let (code, stdout, stderr) = run("text-chunk", &data);
    assert!(
        code == Some(1)
            && stderr
                .lines()
                .any(|line| line.contains("EXTRA CHUNKS in ") && line.contains("fastcull-22.png"))
            && stderr.contains("metadata audit: FAILED"),
        "render-icon.sh's audit must refuse a PNG with a tEXt chunk — exit 1, naming the file and \
         `metadata audit: FAILED` (app-icon.md, \"Metadata-free\"): exit {code:?}; stdout:\n\
         {stdout}stderr:\n{stderr}"
    );

    let data = stage("marker");
    let mut ico = read(&data.join("fastcull.ico"));
    ico.extend_from_slice(b"c2pa");
    fs::write(data.join("fastcull.ico"), ico).expect("writing the .ico with a marker");
    let (code, stdout, stderr) = run("marker", &data);
    assert!(
        code == Some(1)
            && stderr.lines().any(|line| {
                line.contains("METADATA MARKER FOUND in ") && line.contains("fastcull.ico")
            }),
        "render-icon.sh's audit must refuse an .ico carrying `c2pa` — exit 1, naming the file \
         (app-icon.md, \"Metadata-free\"): exit {code:?}; stdout:\n{stdout}stderr:\n{stderr}"
    );
}

/// The line that binds the window's icon, exactly as `MainWindow` carries it
/// in main.slint. The path is relative to main.slint's own directory:
/// `crates/fastcull-app/ui/`, three levels below the repository root.
const ICON_BINDING: &str = r#"icon: @image-url("../../../assets/icon/png/fastcull-48.png");"#;

/// One line of `MainWindow`'s block in main.slint: the brace depth at its
/// first character (1 is the block's own level), whether it starts inside a
/// `/* */` comment, and its text.
struct SlintLine<'a> {
    depth: usize,
    in_block_comment: bool,
    text: &'a str,
}

/// The lines of `MainWindow`'s block, from just after its opening brace
/// (depth 1) to the brace that closes it. Only braces that are code count:
/// the scan skips string literals (with their `\` escapes) — the block holds
/// `"{"` and `"}"`, the shifted bracket keys — `//` comments to the end of
/// their line, and `/* */` comments, and the block's comments hold braces
/// too. Every syntax character is ASCII, so a byte scan never splits a UTF-8
/// character.
fn main_window_lines(slint: &str) -> Vec<SlintLine<'_>> {
    const OPENING: &str = "export component MainWindow inherits Window {";
    #[derive(PartialEq)]
    enum Mode {
        Code,
        Text,
        LineComment,
        BlockComment,
    }
    let bytes = slint.as_bytes();
    let mut i = slint.find(OPENING).expect("main.slint declares MainWindow") + OPENING.len();
    let (mut mode, mut depth) = (Mode::Code, 1_usize);
    let (mut line_start, mut line_depth, mut line_in_comment) = (i, depth, false);
    let mut lines = Vec::new();
    while i < bytes.len() {
        let (c, next) = (bytes[i], bytes.get(i + 1).copied());
        if c == b'\n' {
            lines.push(SlintLine {
                depth: line_depth,
                in_block_comment: line_in_comment,
                text: &slint[line_start..i],
            });
            if mode == Mode::LineComment {
                mode = Mode::Code;
            }
            (line_start, line_depth, line_in_comment) = (i + 1, depth, mode == Mode::BlockComment);
            i += 1;
            continue;
        }
        match mode {
            Mode::Code => match (c, next) {
                (b'/', Some(b'/')) => mode = Mode::LineComment,
                (b'/', Some(b'*')) => {
                    mode = Mode::BlockComment;
                    i += 1;
                }
                (b'"', _) => mode = Mode::Text,
                (b'{', _) => depth += 1,
                (b'}', _) => {
                    depth -= 1;
                    if depth == 0 {
                        lines.push(SlintLine {
                            depth: line_depth,
                            in_block_comment: line_in_comment,
                            text: &slint[line_start..i],
                        });
                        return lines;
                    }
                }
                _ => {}
            },
            Mode::Text => match (c, next) {
                (b'\\', Some(escaped)) if escaped != b'\n' => i += 1,
                (b'"', _) => mode = Mode::Code,
                _ => {}
            },
            Mode::LineComment => {}
            Mode::BlockComment => {
                if (c, next) == (b'*', Some(b'/')) {
                    mode = Mode::Code;
                    i += 1;
                }
            }
        }
        i += 1;
    }
    panic!("MainWindow's block in main.slint never closes");
}

/// AC4: `MainWindow`'s block of main.slint carries the exact `icon:` line
/// for the 48 px PNG, and the path in it resolves to the committed file. A
/// wrong path already fails the build (the Slint compiler embeds the file),
/// so what this test adds is the SIZE, which the build cannot see. That the
/// OS receives the bitmap is review-verified (app-icon.md, "Slint and winit
/// facts this module depends on").
///
/// The line must stand on its own at MainWindow's own depth and outside a
/// comment (QE 2026-10-06, TP7): inside a child element the same line binds
/// that child's `icon` (a `Button` has one) or fails the build, never the
/// window's, and between a `/*` line and a `*/` line it binds nothing — and
/// a search for the line in the block's text passed both.
///
/// Mutants (2026-10-06, main.slint restored after each): `48` → `32` in the
/// line → red while the build stays green; the line deleted → red (the
/// window would silently show no icon, which is what this test exists to
/// catch); the line moved after a line ending in `{` → red at depth 2; the
/// line wrapped in `/* */` on its own line, or between a `/*` line and a
/// `*/` line → red. The line moved to the end of the block stays green, so
/// the scan comes back to depth 1 across the whole block; and an unbalanced
/// `{` in a `//` comment, in a string literal or in a `/* */` comment just
/// before the line stays green, but turns red ("never closes") with that
/// skip removed from the scan — today's block happens to balance its quoted
/// and commented braces, so only such a control shows the skips at work.
#[test]
fn the_window_binds_the_48_px_icon() {
    let root = repo_root();
    let ui = root.join("crates").join("fastcull-app").join("ui");
    let slint = String::from_utf8(read(&ui.join("main.slint"))).expect("main.slint is UTF-8");
    let lines = main_window_lines(&slint);
    let found: Vec<(usize, bool)> = lines
        .iter()
        .filter(|line| line.text.trim() == ICON_BINDING)
        .map(|line| (line.depth, line.in_block_comment))
        .collect();
    assert!(
        found.contains(&(1, false)),
        "MainWindow's block of main.slint must carry the line `{ICON_BINDING}` on its own at the \
         block's own depth (1) and outside a comment; found as a whole line {found:?} (depth, \
         inside /* */) (app-icon.md, \"The running window's icon\")"
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
/// Left of the title text and outside an HTML comment (QE 2026-10-06, TP8):
/// with every `<!-- -->` span removed, the heading opens with the `<img`
/// tag, the tag carries the three attributes, and the title text `FastCull`
/// follows it — a mark after the title, or one commented out, rendered
/// wrong or not at all while the line still held the attributes.
///
/// Mutants (2026-10-06, README.md restored after each): the `img` removed →
/// red; `width="96"` → red; `# FastCull <img …>` → red; the `img` inside
/// `<!-- -->` → red; an unclosed `<!--` before it → red.
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
    // What a reader sees of the heading: every HTML comment removed, and
    // none left open.
    let mut visible = String::new();
    let mut rest = title;
    while let Some(open) = rest.find("<!--") {
        visible.push_str(&rest[..open]);
        let close = rest[open..].find("-->").unwrap_or_else(|| {
            panic!("README.md's title row `{title}` opens an HTML comment it never closes")
        });
        rest = &rest[open + close + "-->".len()..];
    }
    visible.push_str(rest);
    let heading = visible
        .strip_prefix("# ")
        .expect("the title row starts with `# `")
        .trim();
    assert!(
        heading.starts_with("<img "),
        "README.md's title row `{title}` must open with the mark, left of the title and outside \
         any HTML comment (app-icon.md, \"The README mark\")"
    );
    let tag_end = heading
        .find('>')
        .unwrap_or_else(|| panic!("README.md's title row `{title}`: its `<img` tag never closes"));
    let tag = &heading[..=tag_end];
    for attribute in [
        r#"src="assets/icon/png/fastcull-64.png""#,
        r#"width="64""#,
        r#"height="64""#,
    ] {
        assert!(
            tag.contains(attribute),
            "README.md's title row `{title}`: its mark `{tag}` lacks {attribute} (app-icon.md, \
             \"The README mark\")"
        );
    }
    assert!(
        heading[tag_end + 1..].trim_start().starts_with("FastCull"),
        "README.md's title row `{title}`: the title text FastCull must follow the mark \
         (app-icon.md, \"The README mark\")"
    );
    assert!(
        icon_dir().join("png").join(png_name(64)).is_file(),
        "README.md's mark assets/icon/png/fastcull-64.png does not exist"
    );
}
