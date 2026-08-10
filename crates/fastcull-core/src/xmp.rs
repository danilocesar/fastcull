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
//! Field mapping: Rejected → `xmp:Rating="-1"`, Picked → `xmp:Rating="1"`,
//! Unmarked → attribute absent. Reads accept the rating as an attribute or
//! a child element and any positive value counts as Picked (stars are v2).
//! Keywords (M5): read from `dc:subject`, written as `dc:subject` +
//! `lr:hierarchicalSubject` bags (both, like digiKam/LR — see
//! `write_keywords`); the darktable round-trip test covers both halves.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};

use crate::catalog::PickState;
use crate::iptc::IptcField;

#[derive(Debug, thiserror::Error)]
pub enum XmpError {
    #[error("sidecar I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sidecar XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("sidecar attribute error: {0}")]
    Attr(#[from] quick_xml::events::attributes::AttrError),
    #[error("sidecar is not valid UTF-8 — refusing to rewrite it")]
    NotUtf8,
    #[error("sidecar has no rdf:Description — refusing to guess its structure")]
    NoDescription,
}

/// State FastCull understands inside a sidecar. Everything else in the file
/// is preserved verbatim but not modeled. `iptc.keywords` mirrors the
/// `dc:subject` bag; the other IPTC fields follow the xmp-sidecars.md
/// mapping table (M5 panel).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidecarState {
    pub pick: PickState,
    pub keywords: Vec<String>,
    pub iptc: crate::iptc::IptcData,
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
    // IPTC field currently being read (element form): set on Start of a
    // mapped property, harvested from Text (directly or inside its
    // Alt/Seq rdf:li), cleared on the matching End.
    let mut in_iptc_field: Option<IptcField> = None;
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                // Stateful flags arm ONLY on real Start events: an Empty
                // (self-closed) element never produces the End that clears
                // them (gate H1: <photoshop:City/> captured the next text
                // node anywhere — losing element-form ratings and surfacing
                // foreign payloads as IPTC values).
                let is_start = matches!(event, Event::Start(_));
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if is_rdf_description(e) {
                    // xmp:Rating and simple IPTC properties as attributes
                    // (compact XMP form, common from exiv2/Lightroom).
                    for attr in e.attributes() {
                        let attr = attr?;
                        if matches!(attr.key.as_ref(), b"xmp:Rating" | b"xap:Rating") {
                            if let Ok(v) = std::str::from_utf8(&attr.value) {
                                if let Ok(r) = v.trim().parse::<i32>() {
                                    state.pick = rating_to_pick(r);
                                }
                            }
                        } else if let Some(field) = iptc_field_for(local_name(attr.key.as_ref())) {
                            if let Ok(v) = std::str::from_utf8(&attr.value) {
                                if !v.trim().is_empty() {
                                    set_iptc_field(&mut state.iptc, field, v.trim());
                                }
                            }
                        }
                    }
                } else if name == b"subject" {
                    in_subject = is_start;
                } else if name == b"li" && in_subject {
                    in_li = is_start;
                } else if matches!(e.name().as_ref(), b"xmp:Rating" | b"xap:Rating") {
                    in_rating_element = is_start;
                } else if let Some(field) = iptc_field_for(name) {
                    in_iptc_field = if is_start { Some(field) } else { None };
                }
            }
            Event::End(e) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if name == b"subject" {
                    in_subject = false;
                } else if name == b"li" {
                    in_li = false;
                } else if matches!(e.name().as_ref(), b"xmp:Rating" | b"xap:Rating") {
                    in_rating_element = false;
                } else if iptc_field_for(name).is_some() {
                    in_iptc_field = None;
                }
            }
            Event::Text(t) => {
                let text = t.unescape()?.into_owned();
                if in_li && in_subject && !text.trim().is_empty() {
                    state.keywords.push(text.trim().to_string());
                } else if let (Some(field), false) = (in_iptc_field, text.trim().is_empty()) {
                    // Element form: the text sits directly in the property
                    // or inside its rdf:Alt/rdf:Seq li — either way the
                    // first non-empty text wins (x-default first by
                    // convention; multi-li creator joins are v2).
                    set_iptc_field(&mut state.iptc, field, text.trim());
                    in_iptc_field = None;
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
    state.iptc.keywords = state.keywords.clone();
    Ok(state)
}

/// The XMP property each field maps to (xmp-sidecars.md table), and the
/// shape it is written in. The field LIST itself lives in
/// `iptc::IptcField`; this is the sidecar half of the mapping, and the
/// match is exhaustive, so a new field cannot be added without deciding
/// its property here.
fn xmp_property(field: IptcField) -> (&'static str, XmpForm) {
    match field {
        IptcField::Title => ("dc:title", XmpForm::Alt),
        IptcField::Description => ("dc:description", XmpForm::Alt),
        IptcField::Creator => ("dc:creator", XmpForm::Seq),
        IptcField::Rights => ("dc:rights", XmpForm::Alt),
        IptcField::Headline => ("photoshop:Headline", XmpForm::Simple),
        IptcField::City => ("photoshop:City", XmpForm::Simple),
        IptcField::Country => ("photoshop:Country", XmpForm::Simple),
        IptcField::Credit => ("photoshop:Credit", XmpForm::Simple),
        IptcField::Source => ("photoshop:Source", XmpForm::Simple),
        IptcField::JobId => ("photoshop:TransmissionReference", XmpForm::Simple),
        IptcField::Location => ("Iptc4xmpCore:Location", XmpForm::Simple),
    }
}

/// How a field's value is serialized inside its property element.
#[derive(Clone, Copy, PartialEq, Eq)]
enum XmpForm {
    /// `rdf:Alt` with an `x-default` li (title, description, rights).
    Alt,
    /// `rdf:Seq` with one li (creator).
    Seq,
    /// The value directly in the element (photoshop/Iptc4xmpCore set).
    Simple,
}

/// Map an XML local name to the field it feeds. Matching by local name
/// accepts alias prefixes, symmetric with the keyword reader — and it is
/// the LOCAL half of the same one-line-per-field table above, so the two
/// directions cannot disagree about which properties we own.
fn iptc_field_for(local: &[u8]) -> Option<IptcField> {
    IptcField::ALL
        .into_iter()
        .find(|f| local_name(xmp_property(*f).0.as_bytes()) == local)
}

/// First value wins: a property already filled by an earlier form
/// (attribute before element, x-default before other languages) is not
/// overwritten.
fn set_iptc_field(iptc: &mut crate::iptc::IptcData, field: IptcField, value: &str) {
    if field.get(iptc).is_none() {
        field.set(iptc, Some(value.to_string()));
    }
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

/// Fresh minimal sidecar; byte-compared against tests/golden/*.xmp.
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

/// Write `keywords` into the sidecar for `raw_path` as `dc:subject` +
/// `lr:hierarchicalSubject` bags (xmp-sidecars.md mapping: both, like
/// digiKam/Lightroom — darktable imports either). Read-modify-write:
/// existing bags of BOTH properties are replaced wholesale (the session's
/// keyword list is the full truth for them), everything else — rating,
/// foreign nodes, unknown namespaces — passes through unchanged. An empty
/// list removes the bags. Atomic like every sidecar write.
pub fn write_keywords(raw_path: &Path, keywords: &[String]) -> Result<(), XmpError> {
    let path = sidecar_path(raw_path);
    let existing = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            new_sidecar(PickState::Unmarked).into_bytes()
        }
        Err(e) => return Err(e.into()),
    };
    let output = rewrite_keywords(&existing, keywords)?;
    atomic_write(&path, output.as_bytes())
}

/// Write the FULL IPTC state (fields + keyword bags) into the sidecar for
/// `raw_path` (xmp-sidecars.md mapping table). Read-modify-write: every
/// property FastCull owns is replaced wholesale — a `None` field is simply
/// not written, which REMOVES the property (the tri-state clear; an empty
/// value is never emitted, per interop rule). Foreign nodes, the rating,
/// and unknown namespaces pass through unchanged. Atomic.
///
/// The panel routes ALL its writes here (via SidecarWriter::iptc) —
/// `write_keywords` remains as the keywords-only primitive but must not be
/// interleaved with this on the same path outside the writer thread.
pub fn write_iptc(raw_path: &Path, iptc: &crate::iptc::IptcData) -> Result<(), XmpError> {
    let path = sidecar_path(raw_path);
    let existing = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            new_sidecar(PickState::Unmarked).into_bytes()
        }
        Err(e) => return Err(e.into()),
    };
    let output = rewrite_iptc(&existing, iptc)?;
    atomic_write(&path, output.as_bytes())
}

/// Escape for XML text content AND strip control characters: raw controls
/// are invalid XML 1.0 — exiv2 rejects the whole packet (QE-proven via a
/// template that legally smuggled a BEL through TOML). This is the last
/// line of defense; the model-level sanitizer runs upstream.
fn xml_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Serialized IPTC block for the first rdf:Description: keyword bags plus
/// the mapped field elements. Alt/x-default for title/description/rights,
/// Seq for creator, simple elements for the photoshop/Iptc4xmpCore set.
fn iptc_block(iptc: &crate::iptc::IptcData) -> String {
    let mut out = String::new();
    if !iptc.keywords.is_empty() {
        out.push_str(&keyword_bags_only(&iptc.keywords));
    }
    let alt = |name: &str, v: &str| {
        format!(
            "\n   <{name}>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </{name}>",
            xml_escape(v)
        )
    };
    let seq = |name: &str, v: &str| {
        format!(
            "\n   <{name}>\n    <rdf:Seq>\n     <rdf:li>{}</rdf:li>\n    </rdf:Seq>\n   </{name}>",
            xml_escape(v)
        )
    };
    let simple = |name: &str, v: &str| format!("\n   <{name}>{}</{name}>", xml_escape(v));
    // Declaration order of IptcField IS the element order of the block.
    for field in IptcField::ALL {
        let Some(v) = field.get(iptc).map(String::as_str) else {
            continue;
        };
        let (name, form) = xmp_property(field);
        out.push_str(&match form {
            XmpForm::Alt => alt(name, v),
            XmpForm::Seq => seq(name, v),
            XmpForm::Simple => simple(name, v),
        });
    }
    out
}

/// The shared mechanics of a property rewrite: walk the sidecar's events,
/// drop the elements WE own (so they can be re-emitted from current
/// state), and hand every rdf:Description to a strategy that decides
/// whether to rewrite its start tag and what block to inject after it.
///
/// The held-whitespace rule lives here, once. Indentation text nodes of
/// removed elements must go WITH them, or every rewrite leaves an
/// orphaned blank line and sidecars grow without bound over a captioning
/// session (QE-measured +19 bytes per rewrite). Whitespace-only text is
/// therefore held back until the next event decides its fate.
///
/// `owns(name)` takes the RAW qualified name (strategies match by local
/// name themselves). `describe(element, first)` returns None to pass the
/// Description through untouched, or the replacement start tag plus an
/// optional block to write inside it; when the source element was
/// self-closing and it IS rewritten, the walker expands it to Start+End
/// so the block has somewhere to live.
///
/// Errors with `NoDescription` when the document has none — a sidecar we
/// cannot place our properties in is not one we may silently return.
fn rewrite_walk<O, D>(existing: &[u8], owns: O, mut describe: D) -> Result<String, XmpError>
where
    O: Fn(&[u8]) -> bool,
    D: FnMut(&BytesStart, bool) -> Result<Option<(BytesStart<'static>, Option<String>)>, XmpError>,
{
    if std::str::from_utf8(existing).is_err() {
        return Err(XmpError::NotUtf8);
    }
    let mut reader = quick_xml::Reader::from_reader(existing);
    reader.config_mut().trim_text(false);
    let mut writer = quick_xml::Writer::new(Vec::new());
    let mut buf = Vec::new();
    let mut held_ws: Option<Vec<u8>> = None;
    let mut seen_description = false;
    let mut skip_depth = 0usize; // >0: inside an element we own
    loop {
        let event = reader.read_event_into(&mut buf)?;
        // Flush or drop held whitespace depending on what follows it.
        let feeds_removed = match &event {
            Event::Start(e) | Event::Empty(e) => owns(e.name().as_ref()),
            _ => false,
        };
        if let Some(ws) = held_ws.take() {
            if !feeds_removed && skip_depth == 0 {
                writer.get_mut().extend_from_slice(&ws);
            }
        }
        if skip_depth == 0 {
            if let Event::Text(t) = &event {
                let raw = t.clone().into_inner().into_owned();
                if raw.iter().all(|b| b.is_ascii_whitespace()) {
                    held_ws = Some(raw);
                    buf.clear();
                    continue;
                }
            }
        }
        match event {
            Event::Eof => break,
            Event::Start(ref e) if owns(e.name().as_ref()) => skip_depth += 1,
            Event::Empty(ref e) if skip_depth == 0 && owns(e.name().as_ref()) => {}
            Event::End(ref e) if skip_depth > 0 && owns(e.name().as_ref()) => skip_depth -= 1,
            _ if skip_depth > 0 => {}
            Event::Start(ref e) | Event::Empty(ref e) if is_rdf_description(e) => {
                let first = !seen_description;
                seen_description = true;
                match describe(e, first)? {
                    Some((out, block)) => {
                        let was_empty = matches!(event, Event::Empty(_));
                        let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                        writer.write_event(Event::Start(out))?;
                        if let Some(block) = block {
                            writer.get_mut().extend_from_slice(block.as_bytes());
                        }
                        if was_empty {
                            writer
                                .write_event(Event::End(quick_xml::events::BytesEnd::new(name)))?;
                        }
                    }
                    None => writer.write_event(event.clone())?,
                }
            }
            other => writer.write_event(other)?,
        }
        buf.clear();
    }
    if !seen_description {
        return Err(XmpError::NoDescription);
    }
    Ok(String::from_utf8_lossy(&writer.into_inner()).into_owned())
}

/// Event rewrite for write_iptc: drop every owned property (element form,
/// any Description, matched by local name) and every owned attribute
/// (compact form), then inject the fresh block into the first Description
/// with the needed namespaces ensured.
fn rewrite_iptc(existing: &[u8], iptc: &crate::iptc::IptcData) -> Result<String, XmpError> {
    let owned = |name: &[u8]| {
        matches!(local_name(name), b"subject" | b"hierarchicalSubject")
            || iptc_field_for(local_name(name)).is_some()
    };
    let block = iptc_block(iptc);
    // EVERY Description is rewritten here, not just the first: owned
    // properties also exist in COMPACT (attribute) form, and one left
    // behind in a later block would resurrect a value the panel cleared.
    rewrite_walk(existing, owned, |e, first| {
        let mut out = BytesStart::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
        let (mut dc, mut lr, mut ps, mut core) = (false, false, false, false);
        for attr in e.attributes() {
            let attr = attr?;
            match attr.key.as_ref() {
                b"xmlns:dc" => dc = true,
                b"xmlns:lr" => lr = true,
                b"xmlns:photoshop" => ps = true,
                b"xmlns:Iptc4xmpCore" => core = true,
                // Owned compact-form attributes are replaced by the
                // element block (or removed, when the field is None).
                key if iptc_field_for(local_name(key)).is_some() => continue,
                _ => {}
            }
            out.push_attribute(attr);
        }
        if first && !block.is_empty() {
            if !dc {
                out.push_attribute(("xmlns:dc", "http://purl.org/dc/elements/1.1/"));
            }
            if !lr {
                out.push_attribute(("xmlns:lr", "http://ns.adobe.com/lightroom/1.0/"));
            }
            if !ps {
                out.push_attribute(("xmlns:photoshop", "http://ns.adobe.com/photoshop/1.0/"));
            }
            if !core {
                out.push_attribute((
                    "xmlns:Iptc4xmpCore",
                    "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
                ));
            }
        }
        Ok(Some((out, first.then(|| block.clone()))))
    })
}

/// XML-escaped `rdf:li` rows for a keyword bag, matching the fresh-sidecar
/// indentation (three spaces to the bag, four to the items). No trailing
/// closer-indent (iptc_block composes further; keyword_bags adds it).
fn keyword_bags_only(keywords: &[String]) -> String {
    let lis: String = keywords
        .iter()
        .map(|k| format!("     <rdf:li>{}</rdf:li>\n", xml_escape(k)))
        .collect();
    format!(
        "\n   <dc:subject>\n    <rdf:Bag>\n{lis}    </rdf:Bag>\n   </dc:subject>\
         \n   <lr:hierarchicalSubject>\n    <rdf:Bag>\n{lis}    </rdf:Bag>\n   </lr:hierarchicalSubject>"
    )
}

/// Event-level rewrite: drop every existing `dc:subject` /
/// `lr:hierarchicalSubject` element (any rdf:Description block), then emit
/// fresh bags inside the FIRST rdf:Description, ensuring its `dc:`/`lr:`
/// namespaces. The first Description in Empty form (`<rdf:Description/>`)
/// is expanded to Start+End so it can hold children.
fn rewrite_keywords(existing: &[u8], keywords: &[String]) -> Result<String, XmpError> {
    // ONLY the two properties the mapping table assigns to us; anything
    // else (digiKam:TagsList, acdsee:categories, …) is foreign and must
    // survive untouched (preserve-unknown rule). Matched by LOCAL name,
    // symmetric with read_sidecar: a sidecar binding the DC namespace to
    // another prefix (dcx:subject) would otherwise keep a stale bag that
    // resurrects deleted keywords on the next read (validator finding).
    let is_subject = |name: &[u8]| matches!(local_name(name), b"subject" | b"hierarchicalSubject");
    // Unlike the IPTC rewrite, only the FIRST Description is touched:
    // keywords have no compact form to strip, so later blocks pass
    // through byte for byte.
    rewrite_walk(existing, is_subject, |e, first| {
        if !first {
            return Ok(None);
        }
        let mut out = BytesStart::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
        let (mut has_dc, mut has_lr) = (false, false);
        for attr in e.attributes() {
            let attr = attr?;
            match attr.key.as_ref() {
                b"xmlns:dc" => has_dc = true,
                b"xmlns:lr" => has_lr = true,
                _ => {}
            }
            out.push_attribute(attr);
        }
        if !keywords.is_empty() {
            if !has_dc {
                out.push_attribute(("xmlns:dc", "http://purl.org/dc/elements/1.1/"));
            }
            if !has_lr {
                out.push_attribute(("xmlns:lr", "http://ns.adobe.com/lightroom/1.0/"));
            }
        }
        Ok(Some((
            out,
            (!keywords.is_empty()).then(|| keyword_bags_only(keywords)),
        )))
    })
}

/// Event-level rewrite of an existing sidecar: only the first
/// `rdf:Description`'s `xmp:Rating` attribute is added/replaced/removed;
/// everything else round-trips. Also ensures the xmp namespace exists on
/// that element when a rating is written.
fn rewrite_rating(existing: &[u8], pick: PickState) -> Result<String, XmpError> {
    // Refuse non-UTF8 up front: the lossy path would silently mangle a
    // sidecar we promised to preserve (QE defect).
    if std::str::from_utf8(existing).is_err() {
        return Err(XmpError::NotUtf8);
    }
    let mut reader = quick_xml::Reader::from_reader(existing);
    reader.config_mut().trim_text(false);
    let mut writer = quick_xml::Writer::new(Vec::new());
    let mut buf = Vec::new();
    let mut done = false;
    let mut desc_depth = 0usize; // element depth inside ANY rdf:Description
    let mut skip_rating_depth = 0usize; // >0: inside an old rating ELEMENT
    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Eof => break,
            // Drop the legacy rating ELEMENT entirely (validator HIGH: the
            // attribute+element pair contradicted itself and clear() could
            // never clear). Both xmp: and xap: prefixes count.
            Event::Start(ref e)
                if desc_depth > 0 && matches!(e.name().as_ref(), b"xmp:Rating" | b"xap:Rating") =>
            {
                skip_rating_depth += 1;
            }
            Event::Empty(ref e)
                if desc_depth > 0
                    && skip_rating_depth == 0
                    && matches!(e.name().as_ref(), b"xmp:Rating" | b"xap:Rating") => {}
            Event::End(ref e)
                if skip_rating_depth > 0
                    && matches!(e.name().as_ref(), b"xmp:Rating" | b"xap:Rating") =>
            {
                skip_rating_depth -= 1;
            }
            _ if skip_rating_depth > 0 => {} // swallow the old element's body
            // Sanitize EVERY Description (canonical RDF splits schemas into
            // one block each — validator M-1: a rating in the second block
            // previously survived and contradicted ours); the NEW rating
            // goes into the first block only.
            Event::Start(ref e) | Event::Empty(ref e) if is_rdf_description(e) => {
                let empty = matches!(event, Event::Empty(_));
                let mut out =
                    BytesStart::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
                let mut has_xmp_ns = false;
                for attr in e.attributes() {
                    let attr = attr?;
                    match attr.key.as_ref() {
                        b"xmp:Rating" | b"xap:Rating" => continue, // replaced below
                        b"xmlns:xmp" => has_xmp_ns = true,
                        _ => {}
                    }
                    out.push_attribute(attr);
                }
                if !done {
                    if let Some(rating) = pick_to_rating(pick) {
                        if !has_xmp_ns {
                            out.push_attribute(("xmlns:xmp", "http://ns.adobe.com/xap/1.0/"));
                        }
                        out.push_attribute(("xmp:Rating", rating));
                    }
                }
                if empty {
                    writer.write_event(Event::Empty(out))?;
                } else {
                    writer.write_event(Event::Start(out))?;
                    desc_depth += 1;
                }
                done = true;
            }
            Event::Start(ref _e) if desc_depth > 0 => {
                desc_depth += 1;
                writer.write_event(event)?;
            }
            Event::End(ref _e) if desc_depth > 0 => {
                desc_depth -= 1;
                writer.write_event(event)?;
            }
            other => writer.write_event(other)?,
        }
        buf.clear();
    }
    if !done {
        // A sidecar without rdf:Description (zero-byte, bare text, junk that
        // parses): the mark would silently vanish (QE defect) — error out so
        // the writer surfaces it.
        return Err(XmpError::NoDescription);
    }
    Ok(String::from_utf8_lossy(&writer.into_inner()).into_owned())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), XmpError> {
    // Unique temp per call (QE race harness: a FIXED tmp name let two
    // concurrent writers interleave bytes into one tmp and rename the
    // merge into place — permanently corrupt sidecar within 2 iterations).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(format!(".fastcull-tmp-{}-{unique}", std::process::id()));
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
        crate::testutil::scratch_dir("xmp")
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

    /// Keywords round-trip: write -> read yields the same list, in order,
    /// for plain, Unicode, XML-hostile and pipe-hierarchy keywords
    /// (property-style over a seeded set — no fuzzing dep, deterministic).
    #[test]
    fn keywords_roundtrip_hostile_and_unicode() {
        let dir = tmp();
        let cases: Vec<Vec<String>> = vec![
            vec![],
            vec!["bird".into()],
            vec!["são joão".into(), "写真".into(), "Grünheide".into()],
            vec!["a&b".into(), "x<y>z".into(), "\"quoted\"".into()],
            vec!["Nature|Birds|Owls".into(), "flat".into()],
            (1..=40).map(|i| format!("kw{i}")).collect(),
        ];
        for (i, kws) in cases.iter().enumerate() {
            let raw = dir.join(format!("kw{i}.ARW"));
            write_keywords(&raw, kws).unwrap();
            let state = read_sidecar(&sidecar_path(&raw)).unwrap();
            assert_eq!(&state.keywords, kws, "case {i}");
            // Idempotent second write (replacement, not accumulation).
            write_keywords(&raw, kws).unwrap();
            assert_eq!(read_sidecar(&sidecar_path(&raw)).unwrap().keywords, *kws);
            // Both mapped properties present when non-empty (dt reads
            // either; digiKam/LR read lr:).
            let text = std::fs::read_to_string(sidecar_path(&raw)).unwrap();
            assert_eq!(text.contains("dc:subject"), !kws.is_empty());
            assert_eq!(text.contains("lr:hierarchicalSubject"), !kws.is_empty());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Keywords and rating edits compose: neither write clobbers the other,
    /// and clearing keywords removes both bags.
    #[test]
    fn keywords_and_rating_compose_and_clear() {
        let dir = tmp();
        let raw = dir.join("c.ARW");
        write_pick(&raw, PickState::Picked).unwrap();
        write_keywords(&raw, &["owl".into(), "eule".into()]).unwrap();
        let state = read_sidecar(&sidecar_path(&raw)).unwrap();
        assert_eq!(state.pick, PickState::Picked);
        assert_eq!(state.keywords, vec!["owl", "eule"]);
        // Rating rewrite preserves keyword bags.
        write_pick(&raw, PickState::Rejected).unwrap();
        let state = read_sidecar(&sidecar_path(&raw)).unwrap();
        assert_eq!(state.pick, PickState::Rejected);
        assert_eq!(state.keywords, vec!["owl", "eule"]);
        // Clearing removes the bags entirely.
        write_keywords(&raw, &[]).unwrap();
        let text = std::fs::read_to_string(sidecar_path(&raw)).unwrap();
        assert!(!text.contains("dc:subject") && !text.contains("hierarchicalSubject"));
        assert_eq!(
            read_sidecar(&sidecar_path(&raw)).unwrap().pick,
            PickState::Rejected,
            "clearing keywords must not touch the rating"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Preserve-unknown for keyword writes: foreign keyword-ADJACENT nodes
    /// (digiKam:TagsList) and everything else survive; only our two mapped
    /// properties are replaced.
    #[test]
    fn keyword_write_preserves_foreign_nodes() {
        let dir = tmp();
        let raw = dir.join("d.ARW");
        let sc = sidecar_path(&raw);
        std::fs::write(
            &sc,
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:digiKam="http://www.digikam.org/ns/1.0/" xmp:Rating="1" xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <dc:subject><rdf:Bag><rdf:li>old</rdf:li></rdf:Bag></dc:subject>
   <digiKam:TagsList><rdf:Seq><rdf:li>People/Ana</rdf:li></rdf:Seq></digiKam:TagsList>
   <dt:history xmlns:dt="http://darktable.sf.net/">opaque</dt:history>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#,
        )
        .unwrap();
        write_keywords(&raw, &["new".into()]).unwrap();
        let text = std::fs::read_to_string(&sc).unwrap();
        assert!(!text.contains(">old<"), "our old bag replaced");
        assert!(text.contains(">new<"));
        assert!(text.contains("People/Ana"), "digiKam list preserved");
        assert!(text.contains("dt:history"), "darktable history preserved");
        assert!(text.contains("xmp:Rating=\"1\""), "rating preserved");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// IPTC field reading (M5 panel): element form (darktable-style
    /// Alt/Seq containers) and compact attribute form (exiv2/LR-style)
    /// both land in SidecarState.iptc per the mapping table.
    #[test]
    fn iptc_fields_read_element_and_attribute_forms() {
        let dir = tmp();
        let sc = dir.join("f.ARW.xmp");
        std::fs::write(
            &sc,
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"
    photoshop:City="Sintra" photoshop:TransmissionReference="JOB-7"
    Iptc4xmpCore:Location="Palácio da Pena">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Herons</rdf:li></rdf:Alt></dc:title>
   <dc:creator><rdf:Seq><rdf:li>João Ribeiro</rdf:li></rdf:Seq></dc:creator>
   <photoshop:Headline>Morning hunt</photoshop:Headline>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#,
        )
        .unwrap();
        let state = read_sidecar(&sc).unwrap();
        assert_eq!(state.iptc.title.as_deref(), Some("Herons"));
        assert_eq!(state.iptc.creator.as_deref(), Some("João Ribeiro"));
        assert_eq!(state.iptc.headline.as_deref(), Some("Morning hunt"));
        assert_eq!(state.iptc.city.as_deref(), Some("Sintra"));
        assert_eq!(state.iptc.job_id.as_deref(), Some("JOB-7"));
        assert_eq!(state.iptc.location.as_deref(), Some("Palácio da Pena"));
        assert_eq!(state.iptc.description, None, "unset stays None");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Full IPTC write -> read round-trip (M5 panel contract): every mapped
    /// field survives, None fields REMOVE the property (tri-state clear,
    /// never an empty value), rating and foreign nodes pass through, and
    /// compact-form attributes from other tools are replaced not duplicated.
    #[test]
    fn iptc_write_read_roundtrip_clear_and_preserve() {
        let dir = tmp();
        let raw = dir.join("h.ARW");
        write_pick(&raw, PickState::Picked).unwrap();
        let mut iptc = crate::iptc::IptcData {
            title: Some("Herons & <friends>".into()),
            creator: Some("João Ribeiro".into()),
            city: Some("Sintra".into()),
            job_id: Some("JOB-7".into()),
            location: Some("Palácio da Pena".into()),
            keywords: vec!["owl".into(), "são joão".into()],
            ..Default::default()
        };
        write_iptc(&raw, &iptc).unwrap();
        let state = read_sidecar(&sidecar_path(&raw)).unwrap();
        assert_eq!(state.iptc, iptc);
        assert_eq!(state.pick, PickState::Picked, "rating preserved");

        // Clear city + drop a keyword; rewrite. Property must be GONE from
        // the file (not empty), keywords replaced wholesale.
        iptc.city = None;
        iptc.keywords = vec!["owl".into()];
        write_iptc(&raw, &iptc).unwrap();
        let text = std::fs::read_to_string(sidecar_path(&raw)).unwrap();
        assert!(!text.contains("photoshop:City"), "clear removes property");
        assert!(!text.contains("são joão"));
        let state = read_sidecar(&sidecar_path(&raw)).unwrap();
        assert_eq!(state.iptc, iptc);
        // Idempotence.
        write_iptc(&raw, &iptc).unwrap();
        assert_eq!(read_sidecar(&sidecar_path(&raw)).unwrap().iptc, iptc);

        // Foreign compact attribute for an owned field is replaced, foreign
        // nodes survive, and a fully-empty IptcData removes everything.
        let sc = sidecar_path(&raw);
        std::fs::write(
            &sc,
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    photoshop:City="OldTown" xmlns:dt="http://darktable.sf.net/">
   <dt:history>opaque</dt:history>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#,
        )
        .unwrap();
        write_iptc(&raw, &crate::iptc::IptcData::default()).unwrap();
        let text = std::fs::read_to_string(&sc).unwrap();
        assert!(!text.contains("OldTown"), "owned attribute removed");
        assert!(text.contains("dt:history"), "foreign node preserved");
        assert_eq!(
            read_sidecar(&sc).unwrap().iptc,
            crate::iptc::IptcData::default()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QE finding: rewrites must not grow the file without bound (orphaned
    /// indentation of removed elements accumulated +19 bytes per rewrite —
    /// a long captioning session inflated sidecars forever).
    #[test]
    fn repeated_rewrites_do_not_grow_the_sidecar() {
        let dir = tmp();
        let raw = dir.join("g2.ARW");
        let iptc = crate::iptc::IptcData {
            title: Some("stable".into()),
            keywords: vec!["kw".into()],
            ..Default::default()
        };
        write_iptc(&raw, &iptc).unwrap();
        let first = std::fs::metadata(sidecar_path(&raw)).unwrap().len();
        for _ in 0..5 {
            write_iptc(&raw, &iptc).unwrap();
        }
        let last = std::fs::metadata(sidecar_path(&raw)).unwrap().len();
        assert_eq!(first, last, "identical rewrites must be byte-stable");
        for _ in 0..3 {
            write_keywords(&raw, &["kw".into()]).unwrap();
        }
        let after_kw = std::fs::metadata(sidecar_path(&raw)).unwrap().len();
        assert!(
            after_kw <= last + 2,
            "keyword rewrites must not accumulate ({last} -> {after_kw})"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Gate H1 regression: a SELF-CLOSED mapped property must not arm the
    /// field reader — it captured the next text node anywhere (losing
    /// element-form ratings, surfacing darktable history as IPTC values).
    #[test]
    fn self_closed_mapped_element_leaks_nothing() {
        let dir = tmp();
        let sc = dir.join("g.ARW.xmp");
        std::fs::write(
            &sc,
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dt="http://darktable.sf.net/">
   <photoshop:City/>
   <xmp:Rating>-1</xmp:Rating>
   <dt:history>opaquehistoryblob</dt:history>
   <photoshop:Country></photoshop:Country>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#,
        )
        .unwrap();
        let state = read_sidecar(&sc).unwrap();
        assert_eq!(state.pick, PickState::Rejected, "rating must survive");
        assert_eq!(state.iptc.city, None, "self-closed empty stays unset");
        assert_eq!(state.iptc.country, None, "start+end empty stays unset");
        let all = format!("{:?}", state.iptc);
        assert!(
            !all.contains("opaque"),
            "foreign payload must not leak: {all}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A nonstandard prefix bound to the DC namespace must not keep a
    /// stale bag that resurrects deleted keywords (validator finding: the
    /// write side previously matched literal prefixes only, asymmetric
    /// with the local-name read side).
    #[test]
    fn keyword_write_replaces_alias_prefixed_bags() {
        let dir = tmp();
        let raw = dir.join("e.ARW");
        let sc = sidecar_path(&raw);
        std::fs::write(
            &sc,
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:dcx="http://purl.org/dc/elements/1.1/">
   <dcx:subject><rdf:Bag><rdf:li>legacy</rdf:li></rdf:Bag></dcx:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#,
        )
        .unwrap();
        write_keywords(&raw, &["new".into()]).unwrap();
        let state = read_sidecar(&sc).unwrap();
        assert_eq!(state.keywords, vec!["new"], "stale alias bag must go");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Concurrency (QE race harness, 500 iterations → corruption within 2
    /// with the old fixed tmp name): parallel pick + keyword writers on
    /// ONE sidecar must never leave an unreadable file. Last-writer-wins
    /// per property is acceptable; corruption is not.
    #[test]
    fn concurrent_pick_and_keyword_writes_never_corrupt() {
        let dir = tmp();
        let raw = dir.join("race.ARW");
        write_pick(&raw, PickState::Picked).unwrap();
        let r1 = raw.clone();
        let t1 = std::thread::spawn(move || {
            for i in 0..200 {
                let pick = if i % 2 == 0 {
                    PickState::Picked
                } else {
                    PickState::Rejected
                };
                // Transient rename races are the OS's business; corruption
                // below is ours.
                let _ = write_pick(&r1, pick);
            }
        });
        let r2 = raw.clone();
        let t2 = std::thread::spawn(move || {
            for i in 0..200 {
                let _ = write_keywords(&r2, &[format!("kw{i}")]);
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();
        // The file must parse and read cleanly after the storm.
        read_sidecar(&sidecar_path(&raw)).expect("sidecar must never be corrupted");
        std::fs::remove_dir_all(&dir).ok();
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

    /// Regression (validator HIGH / QE D1): element-form ratings must be
    /// replaced/removed, never left to contradict the attribute.
    #[test]
    fn element_form_rating_is_replaced_and_clearable() {
        let dir = tmp();
        let raw = dir.join("e.ARW");
        let sc = sidecar_path(&raw);
        std::fs::write(
            &sc,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <xmp:Rating>3</xmp:Rating>
  <dc:title><rdf:Alt><rdf:li xml:lang="x-default">keep me</rdf:li></rdf:Alt></dc:title>
 </rdf:Description>
</rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        write_pick(&raw, PickState::Rejected).unwrap();
        let text = std::fs::read_to_string(&sc).unwrap();
        assert!(!text.contains("<xmp:Rating>"), "old element must be gone");
        assert!(text.contains("keep me"), "sibling elements preserved");
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Rejected);
        write_pick(&raw, PickState::Unmarked).unwrap();
        let cleared = std::fs::read_to_string(&sc).unwrap();
        assert!(!cleared.contains("Rating"), "clear must actually clear");
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Unmarked);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (validator M-1): canonical RDF puts each schema in its own
    /// Description — ratings must be sanitized in ALL of them, written into
    /// the first only.
    #[test]
    fn multi_description_ratings_are_sanitized_everywhere() {
        let dir = tmp();
        let raw = dir.join("m.ARW");
        let sc = sidecar_path(&raw);
        std::fs::write(
            &sc,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:subject><rdf:Bag><rdf:li>bird</rdf:li></rdf:Bag></dc:subject>
 </rdf:Description>
 <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="5">
  <xmp:Rating>3</xmp:Rating>
 </rdf:Description>
</rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        write_pick(&raw, PickState::Rejected).unwrap();
        let text = std::fs::read_to_string(&sc).unwrap();
        assert_eq!(
            text.matches("Rating").count(),
            1,
            "exactly one rating may remain: {text}"
        );
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Rejected);
        assert!(text.contains("bird"), "foreign content preserved");
        write_pick(&raw, PickState::Unmarked).unwrap();
        let cleared = std::fs::read_to_string(&sc).unwrap();
        assert!(!cleared.contains("Rating"));
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Unmarked);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (QE D4): the legacy xap: prefix is the same namespace.
    #[test]
    fn legacy_xap_prefix_is_read_and_replaced() {
        let dir = tmp();
        let raw = dir.join("f.ARW");
        let sc = sidecar_path(&raw);
        std::fs::write(
            &sc,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:xap="http://ns.adobe.com/xap/1.0/" xap:Rating="2"/>
</rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Picked);
        write_pick(&raw, PickState::Rejected).unwrap();
        let text = std::fs::read_to_string(&sc).unwrap();
        assert!(!text.contains("xap:Rating"), "legacy attr replaced");
        assert_eq!(read_sidecar(&sc).unwrap().pick, PickState::Rejected);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (QE D2/D3): junk that parses and description-less XML must
    /// error, never be mangled or silently dropped.
    #[test]
    fn junk_and_descriptionless_sidecars_error_untouched() {
        let dir = tmp();
        for (name, bytes) in [
            ("jpeg.ARW", &b"\xff\xd8\xff\xe0\x00\x10JFIF\x00"[..]),
            ("empty.ARW", &b""[..]),
            (
                "nodesc.ARW",
                &b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>"[..],
            ),
        ] {
            let raw = dir.join(name);
            let sc = sidecar_path(&raw);
            std::fs::write(&sc, bytes).unwrap();
            assert!(write_pick(&raw, PickState::Picked).is_err(), "{name}");
            assert_eq!(std::fs::read(&sc).unwrap(), bytes, "{name} clobbered");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Golden files: fresh sidecars are byte-identical to checked-in
    /// fixtures (spec acceptance criterion).
    #[test]
    fn fresh_sidecars_match_golden_fixtures() {
        for (pick, golden) in [
            (PickState::Picked, "picked.xmp"),
            (PickState::Rejected, "rejected.xmp"),
            (PickState::Unmarked, "unmarked.xmp"),
        ] {
            let expected = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/golden")
                    .join(golden),
            )
            .unwrap();
            assert_eq!(new_sidecar(pick), expected, "golden drift: {golden}");
        }
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
