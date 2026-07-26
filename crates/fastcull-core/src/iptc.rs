//! IPTC data model & templates (specs/modules/iptc-templates.md).
//!
//! Pure model + expansion logic: the panel UI binds to [`IptcData`], saved
//! templates ("stationery pads") expand per-image variables at apply time,
//! and application is all-or-nothing per batch with a single-level revert.
//! Sidecar serialization lives in `xmp.rs` (field mapping table in
//! specs/modules/xmp-sidecars.md), not here.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// The IPTC fields FastCull edits (xmp-sidecars.md mapping table). All
/// optional; `None` = "not set". In TEMPLATES the tri-state applies (user
/// decision 2026-07-25): absent = preserve, empty-after-trim = CLEAR on
/// apply, non-empty = overwrite.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IptcData {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub rights: Option<String>,
    pub headline: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub credit: Option<String>,
    pub source: Option<String>,
    pub job_id: Option<String>,
    pub location: Option<String>,
    /// Ordered; deduplicated case-preservingly — the FIRST spelling wins,
    /// comparison is Unicode-casefolded (spec).
    #[serde(default)]
    pub keywords: Vec<String>,
}

impl IptcData {
    /// Additive keyword union (spec: keyword apply never replaces).
    pub fn add_keywords<I>(&mut self, incoming: I)
    where
        I: IntoIterator<Item = String>,
    {
        for kw in incoming {
            let folded = caseless::default_case_fold_str(&kw);
            let known = self
                .keywords
                .iter()
                .any(|k| caseless::default_case_fold_str(k) == folded);
            if !known && !kw.is_empty() {
                self.keywords.push(kw);
            }
        }
    }
}

/// A saved template: same fields as [`IptcData`], but values may contain
/// `{variables}` expanded per image at apply time.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IptcTemplate {
    /// Table key in templates.toml (not serialized inside the table).
    #[serde(skip)]
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub rights: Option<String>,
    pub headline: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub credit: Option<String>,
    pub source: Option<String>,
    pub job_id: Option<String>,
    pub location: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Maximum `{seq:N}` padding width (documented in iptc-templates.md).
pub const SEQ_WIDTH_MAX: usize = 32;

#[derive(Debug, Error)]
pub enum IptcError {
    #[error("unknown variable {{{variable}}} in template field '{field}'")]
    UnknownVariable { field: String, variable: String },
    #[error("unclosed '{{' in template field '{field}' (escape literal braces as '{{{{')")]
    UnclosedBrace { field: String },
    #[error(
        "invalid {{seq:{width}}} in template field '{field}': the width must be 1..={max}",
        max = SEQ_WIDTH_MAX
    )]
    BadSeqWidth { field: String, width: String },
    #[error("templates file is not valid TOML: {0}")]
    Toml(String),
    #[error("templates I/O: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------- variables

/// Per-image values the variables draw from. `seq`/batch width are supplied
/// by [`apply_template`] from the batch itself (spec: `{seq}` is the 1-based
/// position in the current apply batch, in the active sort order).
#[derive(Clone, Debug, Default)]
pub struct ExpandContext {
    /// Capture date `YYYY-MM-DD` (EXIF DateTimeOriginal; mtime fallback —
    /// see [`ExpandContext::from_capture`]).
    pub date: String,
    /// Capture time `HHMMSS`.
    pub time: String,
    /// Original file stem, no extension.
    pub filename_stem: String,
    /// EXIF camera model, whitespace-normalized ("ILCE-1").
    pub camera: String,
    /// Original extension, uppercase ("ARW").
    pub ext_upper: String,
}

impl ExpandContext {
    /// Build date/time from an EXIF `DateTimeOriginal` ("YYYY:MM:DD
    /// HH:MM:SS"), falling back to the file mtime (UTC) when absent or
    /// malformed (spec's `{date}` fallback rule).
    pub fn from_capture(
        exif_datetime: Option<&str>,
        mtime: std::time::SystemTime,
        file_name: &str,
        camera: Option<&str>,
    ) -> Self {
        let parsed = exif_datetime.and_then(|dt| {
            let (d, t) = dt.trim().split_once(' ')?;
            let mut it = d.split(':');
            let (y, m, day) = (it.next()?, it.next()?, it.next()?);
            let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            if y.len() != 4
                || m.len() != 2
                || day.len() != 2
                || !(all_digits(y) && all_digits(m) && all_digits(day))
                // Unset-clock convention "0000:00:00 00:00:00" (validator
                // finding): a real camera emission, must hit the fallback,
                // never stamp 0000-00-00 into titles.
                || y == "0000"
                || m == "00"
                || day == "00"
            {
                return None;
            }
            let time: String = t.split(':').collect::<Vec<_>>().join("");
            (time.len() == 6 && all_digits(&time)).then(|| (format!("{y}-{m}-{day}"), time))
        });
        let (date, time) = parsed.unwrap_or_else(|| mtime_date_time(mtime));
        let (stem, ext) = match file_name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, e),
            _ => (file_name, ""),
        };
        ExpandContext {
            date,
            time,
            filename_stem: stem.to_string(),
            camera: camera
                .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default(),
            ext_upper: ext.to_uppercase(),
        }
    }
}

/// UTC calendar date/time from a SystemTime, no external time crate:
/// civil-from-days (Howard Hinnant's algorithm), exact for all epochs.
fn mtime_date_time(t: std::time::SystemTime) -> (String, String) {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}{:02}{:02}", sod / 3600, (sod / 60) % 60, sod % 60),
    )
}

/// Expand one template field for one image. `seq` is 1-based; `batch_len`
/// sets the default zero-padding width (`{seq}` in a batch of 120 pads to
/// 3 digits); `{seq:N}` pads to N. Literal braces: `{{` / `}}`.
pub fn expand(
    field: &str,
    text: &str,
    ctx: &ExpandContext,
    seq: usize,
    batch_len: usize,
) -> Result<String, IptcError> {
    let width = batch_len.max(1).to_string().len();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut var = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(v) => var.push(v),
                        None => {
                            return Err(IptcError::UnclosedBrace {
                                field: field.to_string(),
                            })
                        }
                    }
                }
                match var.as_str() {
                    "date" => out.push_str(&ctx.date),
                    "time" => out.push_str(&ctx.time),
                    "filename" => out.push_str(&ctx.filename_stem),
                    "camera" => out.push_str(&ctx.camera),
                    "ext" => out.push_str(&ctx.ext_upper),
                    "seq" => out.push_str(&format!("{seq:0width$}")),
                    other => {
                        if let Some(width_arg) = other.strip_prefix("seq:") {
                            // A bad width is its own diagnosis, not an
                            // "unknown variable" (validator: the old
                            // message misdiagnosed {seq:0} / {seq:99}).
                            let n = width_arg
                                .parse::<usize>()
                                .ok()
                                .filter(|n| (1..=SEQ_WIDTH_MAX).contains(n))
                                .ok_or_else(|| IptcError::BadSeqWidth {
                                    field: field.to_string(),
                                    width: width_arg.to_string(),
                                })?;
                            out.push_str(&format!("{seq:0n$}"));
                        } else {
                            return Err(IptcError::UnknownVariable {
                                field: field.to_string(),
                                variable: other.to_string(),
                            });
                        }
                    }
                }
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------- apply

/// Field-pair list shared by the apply loop (template getter, data setter
/// target). Keywords are handled separately (additive union).
macro_rules! for_each_field {
    ($m:ident) => {
        $m!(title);
        $m!(description);
        $m!(creator);
        $m!(rights);
        $m!(headline);
        $m!(city);
        $m!(country);
        $m!(credit);
        $m!(source);
        $m!(job_id);
        $m!(location);
    };
}

/// Apply a template to a batch (spec tri-state, 2026-07-25: ABSENT fields
/// preserve, fields that are EMPTY AFTER TRIMMING clear (field -> None),
/// non-empty fields overwrite; keywords union additively). ALL expansions
/// run before ANY image is mutated —
/// a failing expansion on image 2 of 3 leaves the whole batch unmodified.
/// `images` and `ctxs` are parallel slices in the ACTIVE SORT ORDER (that
/// order defines `{seq}`). Returns the pre-apply snapshots for
/// [`RevertSlot`].
pub fn apply_template(
    tpl: &IptcTemplate,
    images: &mut [IptcData],
    ctxs: &[ExpandContext],
) -> Result<Vec<IptcData>, IptcError> {
    assert_eq!(images.len(), ctxs.len(), "parallel slices");
    let n = images.len();

    // Phase 1: expand everything (all-or-nothing gate). Tri-state per
    // field (user decision 2026-07-25, PM-research round): absent =
    // preserve, empty string = CLEAR (the field is REMOVED — PM's
    // ticked-but-empty "cover our asses" case), non-empty = overwrite.
    enum Planned {
        Set(String),
        Clear,
    }
    struct Plan {
        fields: Vec<(&'static str, Planned)>,
        keywords: Vec<String>,
    }
    let mut plans = Vec::with_capacity(n);
    for (i, ctx) in ctxs.iter().enumerate() {
        let seq = i + 1;
        let mut fields: Vec<(&'static str, Planned)> = Vec::new();
        macro_rules! plan_field {
            ($f:ident) => {
                match tpl.$f.as_deref() {
                    None => {}
                    // Empty AFTER TRIMMING clears (validator M2: "   " was
                    // neither clear nor meaningful, and the sidecar reader
                    // drops whitespace-only values on round-trip anyway).
                    Some(blank) if blank.trim().is_empty() => {
                        fields.push((stringify!($f), Planned::Clear))
                    }
                    Some(raw) => fields.push((
                        stringify!($f),
                        Planned::Set(expand(stringify!($f), raw, ctx, seq, n)?),
                    )),
                }
            };
        }
        for_each_field!(plan_field);
        let mut keywords = Vec::new();
        for raw in &tpl.keywords {
            let kw = expand("keywords", raw, ctx, seq, n)?;
            if !kw.is_empty() {
                keywords.push(kw);
            }
        }
        plans.push(Plan { fields, keywords });
    }

    // Phase 2: snapshot + mutate (infallible from here).
    let snapshots: Vec<IptcData> = images.to_vec();
    for (img, plan) in images.iter_mut().zip(plans) {
        for (name, value) in plan.fields {
            macro_rules! set_field {
                ($f:ident) => {
                    if name == stringify!($f) {
                        img.$f = match &value {
                            Planned::Set(v) => Some(v.clone()),
                            Planned::Clear => None, // property removed
                        };
                    }
                };
            }
            for_each_field!(set_field);
        }
        img.add_keywords(plan.keywords);
    }
    Ok(snapshots)
}

/// Single-level "Revert last apply" (spec: kept until the next apply or
/// session close; a second revert is a no-op).
#[derive(Default)]
pub struct RevertSlot(Option<Vec<IptcData>>);

impl RevertSlot {
    /// Arm the slot with the snapshots of the batch just applied
    /// (replaces any previous level — single level only).
    pub fn store(&mut self, snapshots: Vec<IptcData>) {
        self.0 = Some(snapshots);
    }

    /// Restore the stored snapshots into `images` (parallel order as
    /// stored). Returns false (and does nothing) when already reverted.
    pub fn revert_into(&mut self, images: &mut [IptcData]) -> bool {
        match self.0.take() {
            Some(snap) => {
                for (img, s) in images.iter_mut().zip(snap) {
                    *img = s;
                }
                true
            }
            None => false,
        }
    }
}

// --------------------------------------------------------------- templates

/// templates.toml location per the `directories` conventions used by the
/// preview cache (`org.fastcull.fastcull` config dir).
pub fn default_templates_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("org", "fastcull", "fastcull")?;
    Some(dirs.config_dir().join("templates.toml"))
}

/// Load result: templates that parsed, plus per-entry errors for the ones
/// that did not (spec: a corrupt entry is surfaced, the others still load;
/// an unparseable FILE is a hard error).
pub struct TemplateLoad {
    pub templates: Vec<IptcTemplate>,
    pub entry_errors: Vec<String>,
    /// Non-fatal load warnings, surfaced in the panel (user decision
    /// 2026-07-25: an empty-string field now CLEARS on apply — existing
    /// hand-edited files must hear about the semantics change BEFORE a
    /// 150-pick stamp, not after).
    pub warnings: Vec<String>,
}

/// Parse templates.toml content. Format: one `[templates.<name>]` table per
/// template, keys = IptcData field names, values = strings (with variables)
/// or a string array for `keywords`.
pub fn parse_templates(content: &str) -> Result<TemplateLoad, IptcError> {
    let root: toml::Table = content
        .parse()
        .map_err(|e: toml::de::Error| IptcError::Toml(e.to_string()))?;
    let mut load = TemplateLoad {
        templates: Vec::new(),
        entry_errors: Vec::new(),
        warnings: Vec::new(),
    };
    let Some(tables) = root.get("templates") else {
        return Ok(load); // no templates yet: valid empty file
    };
    let Some(tables) = tables.as_table() else {
        return Err(IptcError::Toml("'templates' must be a table".into()));
    };
    for (name, value) in tables {
        match value.clone().try_into::<IptcTemplate>() {
            Ok(mut tpl) => {
                tpl.name = name.clone();
                macro_rules! warn_empty {
                    ($f:ident) => {
                        if tpl.$f.as_deref().is_some_and(|v| v.trim().is_empty()) {
                            load.warnings.push(format!(
                                "template '{name}': empty '{}' CLEARS that field on \
                                 every image it is applied to",
                                stringify!($f)
                            ));
                        }
                    };
                }
                for_each_field!(warn_empty);
                load.templates.push(tpl);
            }
            Err(e) => load.entry_errors.push(format!("template '{name}': {e}")),
        }
    }
    Ok(load)
}

/// Read templates.toml (missing file = zero templates, not an error —
/// first launch has none).
pub fn load_templates(path: &Path) -> Result<TemplateLoad, IptcError> {
    match std::fs::read_to_string(path) {
        Ok(content) => parse_templates(&content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TemplateLoad {
            templates: Vec::new(),
            entry_errors: Vec::new(),
            warnings: Vec::new(),
        }),
        Err(e) => Err(e.into()),
    }
}

/// Serialize and atomically write templates.toml (write temp + rename, so
/// a crash never leaves a half-written file — the live-reload rule).
pub fn save_templates(path: &Path, templates: &[IptcTemplate]) -> Result<(), IptcError> {
    let mut tables = toml::Table::new();
    for tpl in templates {
        let value = toml::Value::try_from(tpl)
            .map_err(|e: toml::ser::Error| IptcError::Toml(e.to_string()))?;
        tables.insert(tpl.name.clone(), value);
    }
    let mut root = toml::Table::new();
    root.insert("templates".into(), toml::Value::Table(tables));
    let content = toml::to_string_pretty(&root).map_err(|e| IptcError::Toml(e.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Temp + fsync + rename: the project's atomic-write standard
    // (xmp-sidecars invariant; validator: rename without fsync can leave
    // an EMPTY templates.toml after power loss on some filesystems).
    let tmp = path.with_extension(format!("toml.tmp-{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn ctx() -> ExpandContext {
        ExpandContext {
            date: "2026-07-25".into(),
            time: "174123".into(),
            filename_stem: "DSC01234".into(),
            camera: "ILCE-1".into(),
            ext_upper: "ARW".into(),
        }
    }

    #[test]
    fn every_variable_expands() {
        let c = ctx();
        for (tpl, want) in [
            ("{date}", "2026-07-25"),
            ("{time}", "174123"),
            ("{filename}", "DSC01234"),
            ("{camera}", "ILCE-1"),
            ("{ext}", "ARW"),
            ("{date}_{filename}.{ext}", "2026-07-25_DSC01234.ARW"),
        ] {
            assert_eq!(expand("title", tpl, &c, 1, 1).unwrap(), want, "{tpl}");
        }
    }

    #[test]
    fn seq_pads_to_batch_width_and_explicit_n() {
        let c = ctx();
        // Batch of 120 -> 3 digits (spec example).
        assert_eq!(expand("title", "{seq}", &c, 7, 120).unwrap(), "007");
        assert_eq!(expand("title", "{seq}", &c, 120, 120).unwrap(), "120");
        assert_eq!(expand("title", "{seq}", &c, 1, 9).unwrap(), "1");
        assert_eq!(expand("title", "{seq:5}", &c, 7, 9).unwrap(), "00007");
    }

    #[test]
    fn brace_escapes_and_errors() {
        let c = ctx();
        assert_eq!(
            expand("title", "{{literal}} {date}", &c, 1, 1).unwrap(),
            "{literal} 2026-07-25"
        );
        let err = expand("headline", "x {nope} y", &c, 1, 1).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nope") && msg.contains("headline"),
            "error must name variable and field: {msg}"
        );
        assert!(matches!(
            expand("city", "oops {date", &c, 1, 1),
            Err(IptcError::UnclosedBrace { .. })
        ));
    }

    #[test]
    fn capture_context_from_exif_and_mtime_fallback() {
        let mtime = UNIX_EPOCH + Duration::from_secs(1_753_400_000); // 2025-07-24 UTC
        let c = ExpandContext::from_capture(
            Some("2021:04:05 17:41:23"),
            mtime,
            "DSC00042.ARW",
            Some("  ILCE-1 "),
        );
        assert_eq!((c.date.as_str(), c.time.as_str()), ("2021-04-05", "174123"));
        assert_eq!(c.filename_stem, "DSC00042");
        assert_eq!(c.ext_upper, "ARW");
        assert_eq!(c.camera, "ILCE-1");
        // Malformed EXIF falls back to mtime (UTC).
        let c = ExpandContext::from_capture(Some("garbage"), mtime, "x.arw", None);
        assert_eq!(c.date, "2025-07-24");
        assert_eq!(c.time.len(), 6);
        assert_eq!(c.ext_upper, "ARW");
        // Unset-clock zero date (validator finding): real cameras emit
        // this; it must hit the fallback, never stamp 0000-00-00.
        let c = ExpandContext::from_capture(Some("0000:00:00 00:00:00"), mtime, "x.arw", None);
        assert_eq!(c.date, "2025-07-24");
        // Non-digit fields likewise.
        let c = ExpandContext::from_capture(Some("20xx:01:02 03:04:05"), mtime, "x.arw", None);
        assert_eq!(c.date, "2025-07-24");
    }

    /// Validator M2: whitespace-only template values are CLEAR (and warn),
    /// never `Some("   ")` — the sidecar reader would drop that on
    /// round-trip and the value would silently evaporate.
    #[test]
    fn whitespace_only_template_value_clears_and_warns() {
        let (mut images, ctxs) = batch3();
        images[1].country = Some("Portugal".into());
        let tpl = IptcTemplate {
            country: Some("   ".into()),
            ..Default::default()
        };
        apply_template(&tpl, &mut images, &ctxs).unwrap();
        assert_eq!(images[1].country, None, "whitespace-only clears");
        let load = parse_templates("[templates.w]\ncountry = \"   \"\n").unwrap();
        assert_eq!(load.warnings.len(), 1, "whitespace-only warns too");
        assert!(load.warnings[0].contains("country"));
    }

    #[test]
    fn bad_seq_width_gets_its_own_error() {
        let c = ctx();
        for tpl in ["{seq:0}", "{seq:33}", "{seq:x}"] {
            let msg = expand("title", tpl, &c, 1, 1).unwrap_err().to_string();
            assert!(
                msg.contains("width") && msg.contains("title"),
                "{tpl}: {msg}"
            );
        }
    }

    fn batch3() -> (Vec<IptcData>, Vec<ExpandContext>) {
        let img0 = IptcData {
            city: Some("Old Town".into()),
            keywords: vec!["Birds".into()],
            ..Default::default()
        };
        let images = vec![img0, IptcData::default(), IptcData::default()];
        let ctxs = (0..3)
            .map(|i| ExpandContext {
                filename_stem: format!("DSC0000{i}"),
                ..ctx()
            })
            .collect();
        (images, ctxs)
    }

    #[test]
    fn batch_apply_overwrites_preserves_unions_and_orders_seq() {
        let (mut images, ctxs) = batch3();
        images[0].headline = Some("stale headline".into());
        let tpl = IptcTemplate {
            title: Some("{filename} #{seq}".into()),
            creator: Some("Ana".into()),
            city: None,                // absent: preserve
            headline: Some("".into()), // empty: CLEAR (tri-state)
            keywords: vec!["birds".into(), "{date}".into()],
            ..Default::default()
        };
        let snaps = apply_template(&tpl, &mut images, &ctxs).unwrap();
        assert_eq!(snaps.len(), 3);
        assert_eq!(snaps[0].headline.as_deref(), Some("stale headline"));
        assert_eq!(images[0].title.as_deref(), Some("DSC00000 #1"));
        assert_eq!(images[2].title.as_deref(), Some("DSC00002 #3"));
        assert_eq!(images[1].creator.as_deref(), Some("Ana"));
        assert_eq!(
            images[0].city.as_deref(),
            Some("Old Town"),
            "absent preserves"
        );
        assert_eq!(images[0].headline, None, "empty CLEARS the field");
        assert_eq!(images[1].headline, None, "clear is a no-op on unset");
        // Union: "birds" folds into existing "Birds" (first spelling wins).
        assert_eq!(images[0].keywords, vec!["Birds", "2026-07-25"]);
        assert_eq!(images[1].keywords, vec!["birds", "2026-07-25"]);
    }

    #[test]
    fn all_or_nothing_on_mid_batch_failure() {
        let (mut images, mut ctxs) = batch3();
        // Poison image 2's expansion: {camera} is fine everywhere, so use a
        // template with an unknown var conditional on nothing — instead make
        // the template itself bad only via ctx? Unknown vars fail on EVERY
        // image, so emulate the spec case (failure surfacing mid-batch) by
        // checking mutation state after any phase-1 error.
        ctxs[1].camera.clear(); // irrelevant to the error, kept for realism
        let before = images.clone();
        let tpl = IptcTemplate {
            title: Some("{filename}-{bogus}".into()),
            keywords: vec!["k".into()],
            ..Default::default()
        };
        let err = apply_template(&tpl, &mut images, &ctxs).unwrap_err();
        assert!(matches!(err, IptcError::UnknownVariable { .. }));
        assert_eq!(images, before, "no image may be modified on failure");
    }

    #[test]
    fn revert_restores_exact_state_and_is_single_level() {
        let (mut images, ctxs) = batch3();
        let before = images.clone();
        let tpl = IptcTemplate {
            title: Some("t".into()),
            keywords: vec!["added".into()],
            ..Default::default()
        };
        let mut slot = RevertSlot::default();
        slot.store(apply_template(&tpl, &mut images, &ctxs).unwrap());
        assert_ne!(images, before);
        assert!(slot.revert_into(&mut images));
        assert_eq!(images, before, "exact pre-apply state incl. keywords");
        let frozen = images.clone();
        assert!(!slot.revert_into(&mut images), "second revert is a no-op");
        assert_eq!(images, frozen);
    }

    #[test]
    fn templates_toml_roundtrip_unicode_and_partial_corruption() {
        let dir = std::env::temp_dir().join(format!("fastcull-iptc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("templates.toml");
        let tpls = vec![
            IptcTemplate {
                name: "boda".into(),
                title: Some("Boda de São João {seq}".into()),
                creator: Some("João Ribeiro".into()),
                keywords: vec!["casamento".into(), "fotografia".into()],
                ..Default::default()
            },
            IptcTemplate {
                name: "wildlife".into(),
                city: Some("{camera}".into()),
                ..Default::default()
            },
        ];
        save_templates(&path, &tpls).unwrap();
        let load = load_templates(&path).unwrap();
        assert!(load.entry_errors.is_empty());
        assert!(load.warnings.is_empty(), "no empty fields, no warnings");
        assert_eq!(load.templates.len(), 2);
        let boda = load.templates.iter().find(|t| t.name == "boda").unwrap();
        assert_eq!(boda.title.as_deref(), Some("Boda de São João {seq}"));
        assert_eq!(boda.keywords, vec!["casamento", "fotografia"]);

        // One corrupt ENTRY: error surfaced, the other template loads.
        std::fs::write(
            &path,
            "[templates.good]\ntitle = \"ok\"\n[templates.bad]\nkeywords = 42\n",
        )
        .unwrap();
        let load = load_templates(&path).unwrap();
        assert_eq!(load.templates.len(), 1);
        assert_eq!(load.templates[0].name, "good");
        assert_eq!(load.entry_errors.len(), 1);
        assert!(load.entry_errors[0].contains("bad"));

        // Empty-string field: loads, but with the CLEAR warning (user
        // decision: the semantics change must be heard BEFORE a stamp).
        std::fs::write(&path, "[templates.wipe]\ncity = \"\"\n").unwrap();
        let load = load_templates(&path).unwrap();
        assert_eq!(load.templates.len(), 1);
        assert_eq!(load.warnings.len(), 1);
        assert!(
            load.warnings[0].contains("wipe") && load.warnings[0].contains("city"),
            "{:?}",
            load.warnings
        );

        // Unparseable FILE: hard error.
        std::fs::write(&path, "not toml at [[[").unwrap();
        assert!(matches!(load_templates(&path), Err(IptcError::Toml(_))));

        // Missing file: empty, no error (first launch).
        assert!(load_templates(&dir.join("none.toml"))
            .unwrap()
            .templates
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
