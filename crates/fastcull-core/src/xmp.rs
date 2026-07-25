//! XMP sidecar read/write — the darktable interoperability contract
//! (`specs/modules/xmp-sidecars.md`).
//!
//! Invariants enforced here:
//! - A RAW file is NEVER opened for writing (ADR 0003); all state goes to
//!   `<name>.<ext>.xmp` (darktable's native naming — never `<name>.xmp`).
//! - Read-modify-write with preservation: an existing sidecar (Photo
//!   Mechanic, Lightroom, darktable) is round-tripped event-by-event and only
//!   the `xmp:Rating` attribute is touched; unknown elements, namespaces and
//!   attributes pass through unchanged.
//! - Writes are atomic: temp file in the same directory, fsync, rename.
//!
//! Field mapping (M3 scope): Rejected → `xmp:Rating="-1"`, Picked →
//! `xmp:Rating="1"`, Unmarked → attribute absent. Reads accept the rating as
//! an attribute or a child element and any positive value counts as Picked
//! (stars are v2). `dc:subject` keywords are read (for display and future
//! IPTC work) but not yet written — keyword writing lands with M5.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};

use crate::catalog::PickState;

#[derive(Debug, thiserror::Error)]
pub enum XmpError {
    #[error("sidecar I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sidecar XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("sidecar attribute error: {0}")]
    Attr(#[from] quick_xml::events::attributes::AttrError),
}

/// State FastCull understands inside a sidecar. Everything else in the file
/// is preserved verbatim but not modeled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidecarState {
    pub pick: PickState,
    pub keywords: Vec<String>,
}

/// `DSC01234.ARW` → `DSC01234.ARW.xmp` (darktable's convention; the
/// `<name>.xmp` form has known darktable import bugs — never emit it).
pub fn sidecar_path(raw: &Path) -> PathBuf {
    let mut os = raw.as_os_str().to_owned();
    os.push(".xmp");
    PathBuf::from(os)
}

fn rating_to_pick(rating: i32) -> PickState {
    match rating {
        r if r < 0 => PickState::Rejected,
        r if r > 0 => PickState::Picked,
        _ => PickState::Unmarked,
    }
}

fn pick_to_rating(pick: PickState) -> Option<&'static str> {
    match pick {
        PickState::Picked => Some("1"),
        PickState::Rejected => Some("-1"),
        PickState::Unmarked => None,
    }
}

/// Read pick state and keywords from a sidecar. A missing file is the
/// default state, not an error; a malformed file is an error (the caller
/// decides whether to surface or ignore — never overwrite silently).
pub fn read_sidecar(path: &Path) -> Result<SidecarState, XmpError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SidecarState::default()),
        Err(e) => return Err(e.into()),
    };
    let mut reader = quick_xml::Reader::from_reader(bytes.as_slice());
    let mut state = SidecarState::default();
    let mut in_subject = false;
    let mut in_li = false;
    let mut in_rating_element = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) | Event::Empty(e) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if is_rdf_description(&e) {
                    // xmp:Rating as attribute.
                    for attr in e.attributes() {
                        let attr = attr?;
                        if attr.key.as_ref() == b"xmp:Rating" {
                            if let Ok(v) = std::str::from_utf8(&attr.value) {
                                if let Ok(r) = v.trim().parse::<i32>() {
                                    state.pick = rating_to_pick(r);
                                }
                            }
                        }
                    }
                } else if name == b"subject" {
                    in_subject = true;
                } else if name == b"li" && in_subject {
                    in_li = true;
                } else if e.name().as_ref() == b"xmp:Rating" {
                    in_rating_element = true;
                }
            }
            Event::End(e) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if name == b"subject" {
                    in_subject = false;
                } else if name == b"li" {
                    in_li = false;
                } else if e.name().as_ref() == b"xmp:Rating" {
                    in_rating_element = false;
                }
            }
            Event::Text(t) => {
                let text = t.unescape()?.into_owned();
                if in_li && in_subject && !text.trim().is_empty() {
                    state.keywords.push(text.trim().to_string());
                } else if in_rating_element {
                    if let Ok(r) = text.trim().parse::<i32>() {
                        state.pick = rating_to_pick(r);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(state)
}

fn local_name(qname: &[u8]) -> &[u8] {
    qname.rsplit(|b| *b == b':').next().unwrap_or(qname)
}

fn is_rdf_description(e: &BytesStart) -> bool {
    local_name(e.name().as_ref()) == b"Description"
}

/// Write `pick` into the sidecar for `raw_path`, creating the file if absent
/// and preserving every foreign node if present. Atomic (temp + fsync +
/// rename).
pub fn write_pick(raw_path: &Path, pick: PickState) -> Result<(), XmpError> {
    let path = sidecar_path(raw_path);
    let output = match std::fs::read(&path) {
        Ok(existing) => rewrite_rating(&existing, pick)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new_sidecar(pick),
        Err(e) => return Err(e.into()),
    };
    atomic_write(&path, output.as_bytes())
}

/// Fresh minimal sidecar (deterministic — golden-file tested).
fn new_sidecar(pick: PickState) -> String {
    let rating_attr = pick_to_rating(pick)
        .map(|r| format!("\n    xmp:Rating=\"{r}\""))
        .unwrap_or_default();
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="FastCull">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"{rating_attr}>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#
    )
}

/// Event-level rewrite of an existing sidecar: only the first
/// `rdf:Description`'s `xmp:Rating` attribute is added/replaced/removed;
/// everything else round-trips. Also ensures the xmp namespace exists on
/// that element when a rating is written.
fn rewrite_rating(existing: &[u8], pick: PickState) -> Result<String, XmpError> {
    let mut reader = quick_xml::Reader::from_reader(existing);
    reader.config_mut().trim_text(false);
    let mut writer = quick_xml::Writer::new(Vec::new());
    let mut buf = Vec::new();
    let mut done = false;
    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) if !done && is_rdf_description(e) => {
                let empty = matches!(event, Event::Empty(_));
                let mut out =
                    BytesStart::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
                let mut has_xmp_ns = false;
                for attr in e.attributes() {
                    let attr = attr?;
                    match attr.key.as_ref() {
                        b"xmp:Rating" => continue, // replaced below
                        b"xmlns:xmp" => has_xmp_ns = true,
                        _ => {}
                    }
                    out.push_attribute(attr);
                }
                if let Some(rating) = pick_to_rating(pick) {
                    if !has_xmp_ns {
                        out.push_attribute(("xmlns:xmp", "http://ns.adobe.com/xap/1.0/"));
                    }
                    out.push_attribute(("xmp:Rating", rating));
                }
                if empty {
                    writer.write_event(Event::Empty(out))?;
                } else {
                    writer.write_event(Event::Start(out))?;
                }
                done = true;
            }
            other => writer.write_event(other)?,
        }
        buf.clear();
    }
    Ok(String::from_utf8_lossy(&writer.into_inner()).into_owned())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), XmpError> {
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".fastcull-tmp");
    let tmp = PathBuf::from(tmp_os);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fastcull-xmp-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sidecar_naming_is_darktable_convention() {
        assert_eq!(
            sidecar_path(Path::new("/p/DSC01234.ARW")),
            PathBuf::from("/p/DSC01234.ARW.xmp")
        );
    }

    #[test]
    fn fresh_sidecar_roundtrips_all_pick_states() {
        let dir = tmp();
        for (pick, expect_attr) in [
            (PickState::Picked, true),
            (PickState::Rejected, true),
            (PickState::Unmarked, false),
        ] {
            let raw = dir.join("a.ARW");
            let sc = sidecar_path(&raw);
            std::fs::remove_file(&sc).ok();
            write_pick(&raw, pick).unwrap();
            let text = std::fs::read_to_string(&sc).unwrap();
            assert_eq!(text.contains("xmp:Rating"), expect_attr, "{pick:?}");
            assert_eq!(read_sidecar(&sc).unwrap().pick, pick);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_sidecar_reads_as_default() {
        assert_eq!(
            read_sidecar(Path::new("/nope/none.ARW.xmp")).unwrap(),
            SidecarState::default()
        );
    }

    /// The trust contract: foreign nodes (darktable history, Lightroom crs)
    /// survive our edits untouched.
    #[test]
    fn foreign_nodes_survive_rating_edits() {
        let dir = tmp();
        let raw = dir.join("b.ARW");
        let sc = sidecar_path(&raw);
        let foreign = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="XMP Core 4.4.0-Exiv2">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:darktable="http://darktable.sf.net/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    crs:Exposure="+0.35"
    darktable:xmp_version="5">
   <darktable:history>
    <rdf:Seq><rdf:li darktable:operation="exposure" darktable:params="deadbeef"/></rdf:Seq>
   </darktable:history>
   <dc:subject><rdf:Bag><rdf:li>heron</rdf:li><rdf:li>água doce</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;
        std::fs::write(&sc, foreign).unwrap();

        write_pick(&raw, PickState::Picked).unwrap();
        let after = std::fs::read_to_string(&sc).unwrap();
        for needle in [
            "crs:Exposure=\"+0.35\"",
            "darktable:xmp_version=\"5\"",
            "darktable:operation=\"exposure\"",
            "darktable:params=\"deadbeef\"",
            "<rdf:li>heron</rdf:li>",
            "água doce",
            "x:xmptk=\"XMP Core 4.4.0-Exiv2\"",
        ] {
            assert!(after.contains(needle), "lost foreign content: {needle}");
        }
        let state = read_sidecar(&sc).unwrap();
        assert_eq!(state.pick, PickState::Picked);
        assert_eq!(state.keywords, ["heron", "água doce"]);

        // Flip to rejected, then clear: foreign content still intact.
        write_pick(&raw, PickState::Rejected).unwrap();
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Rejected);
        write_pick(&raw, PickState::Unmarked).unwrap();
        let cleared = std::fs::read_to_string(&sc).unwrap();
        assert!(!cleared.contains("xmp:Rating"));
        assert!(cleared.contains("darktable:params=\"deadbeef\""));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rating_element_form_and_star_values_are_read() {
        let dir = tmp();
        let sc = dir.join("c.ARW.xmp");
        std::fs::write(
            &sc,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:Rating>3</xmp:Rating></rdf:Description>
</rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Picked);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_sidecar_is_an_error_not_a_clobber() {
        let dir = tmp();
        let raw = dir.join("d.ARW");
        let sc = sidecar_path(&raw);
        std::fs::write(&sc, "<not xml at all").unwrap();
        assert!(write_pick(&raw, PickState::Picked).is_err());
        // Original bytes untouched.
        assert_eq!(std::fs::read_to_string(&sc).unwrap(), "<not xml at all");
        std::fs::remove_dir_all(&dir).ok();
    }
}
