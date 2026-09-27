//! The licence file carries libjpeg-turbo's notices (brief 008 R15;
//! 01-architecture.md, "Native dependencies").
//!
//! `turbojpeg-sys` vendors libjpeg-turbo, the loupe's C decoder, which is
//! IJG and BSD-3-Clause, with zlib on its SIMD sources. `cargo about` keys
//! on the crate's own SPDX expression (`Unlicense OR MIT`), so `about.toml`
//! carries a clarification naming the three vendored texts, each
//! checksummed. cargo-about 0.9.2 does NOT fail when a checksum stops
//! matching — after a version bump that rewords one of the texts, say: it
//! exits 0 with a warning and lists the crate under MIT with an unrelated
//! notice (measured 2026-09-26, brief 008 step 1). The binaries would then
//! ship without the C library's notices and nothing would say so. This
//! test reads the generated `THIRD-PARTY-LICENSES.md` and requires the
//! three licences, each naming `turbojpeg-sys` and carrying its text.
//!
//! When it fails: regenerate the file with the clarification in
//! `about.toml` intact (`cargo about generate about.hbs -o
//! THIRD-PARTY-LICENSES.md`), and if the vendored texts changed, update
//! the clarification's checksums from them. Do not loosen this test.

/// One `### <licence>` section of the generated file: its heading, the
/// crates its `Used by:` list names (`<crate> <version>`), and the text of
/// its fenced block.
struct Section {
    heading: String,
    used_by: Vec<String>,
    text: String,
}

/// Split the file into its licence sections, line by line. `str::lines`
/// also strips a `\r`, and every line is compared `trim_end()`ed: the
/// Windows runner checks the file out with CRLF endings (only the golden
/// XMPs are exempt, `.gitattributes`), where a split on '\n' alone would
/// leave a `\r` on every line of one runner.
fn sections(file: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let mut current: Option<Section> = None;
    let mut in_fence = false;
    for line in file.lines().map(str::trim_end) {
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if let Some(section) = current.as_mut() {
                section.text.push_str(line);
                section.text.push('\n');
            }
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            out.extend(current.take());
            current = Some(Section {
                heading: heading.to_string(),
                used_by: Vec::new(),
                text: String::new(),
            });
        } else if line.starts_with("## ") {
            out.extend(current.take());
        } else if let (Some(section), Some(item)) = (current.as_mut(), line.strip_prefix("- [")) {
            if let Some((name, _)) = item.split_once(']') {
                section.used_by.push(name.to_string());
            }
        }
    }
    out.extend(current);
    out
}

#[test]
fn the_licence_file_carries_libjpeg_turbos_notices() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../THIRD-PARTY-LICENSES.md");
    let file = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("THIRD-PARTY-LICENSES.md must be readable at {path}: {e}"));
    let sections = sections(&file);
    assert!(
        sections.len() > 10,
        "the parser must find the file's licence sections, found {}",
        sections.len()
    );
    // (licence heading, a line of the notice text the clarification names)
    let required = [
        ("Independent JPEG Group License", "LEGAL ISSUES"),
        (
            "BSD 3-Clause \"New\" or \"Revised\" License",
            "libjpeg-turbo Licenses",
        ),
        ("zlib License", "Copyright 2009 Pierre Ossman"),
    ];
    let missing: Vec<&str> = required
        .iter()
        .filter(|(licence, notice)| {
            !sections.iter().any(|s| {
                s.heading == *licence
                    && s.used_by.iter().any(|c| c.starts_with("turbojpeg-sys "))
                    && s.text.contains(notice)
            })
        })
        .map(|(licence, _)| *licence)
        .collect();
    assert!(
        missing.is_empty(),
        "THIRD-PARTY-LICENSES.md does not list turbojpeg-sys (libjpeg-turbo) under {missing:?} \
         with its notice text: regenerate with the clarification in about.toml intact \
         (cargo-about falls back silently — ADR 0005)"
    );
}
