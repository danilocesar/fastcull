//! The settings file and every rule about it (`specs/modules/settings.md`,
//! ADR 0005): `settings.toml` in the config dir, read by the app and the CLI
//! alike, written by the app's Settings dialog.
//!
//! What lives here, and why here rather than in the app (hard rule 5): the
//! model (`Settings`, its defaults and its clamps), the file's grammar (TOML
//! as the INI the user described — a table per tab, a key per setting), the
//! memory grammar (`"2 GB"` or `"40%"` of total RAM), the reader and the
//! writer, the broken-file rule, the config-dir resolver both binaries share,
//! the environment-over-file precedence for the read workers, and the total
//! RAM figure the loupe memory setting is read against. The app only binds
//! these to Slint properties; nothing in it parses or clamps a value.
//!
//! Every function here is pure except the four that touch the OS, and each
//! of those says so: [`config_dir`] (the environment and the home dir),
//! [`load_default`] (one stderr line), [`write`] (the file) and
//! [`total_ram`] (the OS's memory figure). Callers run them on their own
//! thread — the app on its UI thread, by ADR 0005: a ~1 KB file on an
//! explicit user action inside a modal.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The settings file's name inside the config dir.
pub const FILE_NAME: &str = "settings.toml";

/// The selection wash's ceiling, in percent: above ~15 % the tint can
/// shift colour judgement, and 50 % is where the grid stops reading as
/// photographs at all (settings.md, "UI › Selection highlight").
pub const WASH_MAX: u32 = 50;

/// The selection wash's default, in percent — the 25 % chosen by eye on
/// 2026-07-28 (ui-grid.md, "The selection wash").
pub const WASH_DEFAULT: u32 = 25;

/// The thumbnail cache cap's floor: 256 MB holds one large shoot's
/// thumbnails (30–60 KB each); below it the "second open is instant"
/// promise of catalog-cache.md could not survive a single big folder
/// (settings.md, "Reading"; brief 008 D15). Exactly 0.25 of the app's GB.
pub const CACHE_CAP_FLOOR_BYTES: u64 = 256 * 1024 * 1024;

/// The bytes of one decoded A1 full-resolution frame, 8640 × 5760 × 3 —
/// the unit of the loupe memory hint's "≈ N A1 frames". An A1 number, and
/// the hint names the body because of it (M11).
pub const A1_FRAME_BYTES: u64 = 8640 * 5760 * 3;

/// The one environment variable that governs a setting today
/// (`performance.max_readers`); its semantics are raw-pipeline.md's.
pub const MAX_READERS_VAR: &str = "FASTCULL_MAX_READERS";

/// Harness plumbing (test-harness.md), not a setting: redirects the whole
/// config dir for a driven test that must prove a file was written. Wins
/// over [`NO_CONFIG_VAR`].
pub const CONFIG_DIR_VAR: &str = "FASTCULL_CONFIG_DIR";

/// Harness plumbing (test-harness.md): the config dir resolves to nothing,
/// so no config file is read or written. Any value counts as set, as it
/// always has for `ui.toml`.
pub const NO_CONFIG_VAR: &str = "FASTCULL_NO_CONFIG";

/// The limit a cleared "Adaptive" checkbox starts from: 4 — the read
/// pool's floor, the fixed gate `FASTCULL_MAX_READERS=4` restores
/// (raw-pipeline.md), and the FAQ's advice for a NAS. The setting is one
/// integer (0 = adaptive), so clearing the box has to name some limit, and
/// this is the one that changes least.
pub const READERS_LIMIT_START: u32 = 4;

/// The app's GB: 1,073,741,824 bytes, the unit the byte formatter prints,
/// so `"2 GB"` is exactly the loupe's and the cache's 2 GiB default
/// (settings.md, "The file").
const GB: u64 = 1 << 30;

/// The loupe memory default as the file spells it.
const LOUPE_MEMORY_DEFAULT: MemorySpec = MemorySpec::Gb(2.0);

/// The cache cap default, in the app's GB.
const CACHE_CAP_DEFAULT_GB: f64 = 2.0;

/// The cache cap floor in the app's GB (the same number as
/// [`CACHE_CAP_FLOOR_BYTES`]).
const CACHE_CAP_FLOOR_GB: f64 = 0.25;

/// Where a comment line wraps: the file is read in a terminal, and 78
/// columns is what a hand edit in one shows whole (settings.md, "Writing").
const NOTE_COLUMNS: usize = 78;

// ------------------------------------------------------------------ tabs

/// A tab of the dialog, and the TOML table its settings live in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Ui,
    Performance,
}

/// The tabs, in the order the dialog shows them. A LIST, so the next unit
/// adds a tab by adding an entry here and a body in the dialog
/// (settings.md, "The card").
pub const TABS: [Tab; 3] = [Tab::General, Tab::Ui, Tab::Performance];

impl Tab {
    /// The tab's title on the strip.
    pub fn title(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Ui => "UI",
            Tab::Performance => "Performance",
        }
    }

    /// The TOML table holding this tab's settings.
    pub fn table(self) -> &'static str {
        match self {
            Tab::General => "general",
            Tab::Ui => "ui",
            Tab::Performance => "performance",
        }
    }

    /// The tab at strip position `index`, if there is one.
    pub fn from_index(index: usize) -> Option<Tab> {
        TABS.get(index).copied()
    }

    /// This tab's settings, in the order the dialog lists them.
    pub fn keys(self) -> impl Iterator<Item = Key> {
        Key::ALL.into_iter().filter(move |k| k.tab() == self)
    }
}

// ------------------------------------------------------------------ keys

/// One setting: where it lives in the file and what its note says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    AutoAdvance,
    SelectionWash,
    LoupeMemory,
    CacheCap,
    MaxReaders,
}

impl Key {
    /// Every setting, in file order (which is also dialog order).
    pub const ALL: [Key; 5] = [
        Key::AutoAdvance,
        Key::SelectionWash,
        Key::LoupeMemory,
        Key::CacheCap,
        Key::MaxReaders,
    ];

    pub fn tab(self) -> Tab {
        match self {
            Key::AutoAdvance => Tab::General,
            Key::SelectionWash => Tab::Ui,
            Key::LoupeMemory | Key::CacheCap | Key::MaxReaders => Tab::Performance,
        }
    }

    /// The TOML table the key lives in.
    pub fn table(self) -> &'static str {
        self.tab().table()
    }

    /// The key's name inside its table — also the name the dialog commits
    /// a value under.
    pub fn name(self) -> &'static str {
        match self {
            Key::AutoAdvance => "auto_advance",
            Key::SelectionWash => "selection_wash",
            Key::LoupeMemory => "loupe_memory",
            Key::CacheCap => "cache_cap",
            Key::MaxReaders => "max_readers",
        }
    }

    /// The key named `name`, if it is one of ours.
    pub fn from_name(name: &str) -> Option<Key> {
        Key::ALL.into_iter().find(|k| k.name() == name)
    }

    /// What the setting does, its default and when it takes effect — THE
    /// one home of these sentences (settings.md, "The settings"): the
    /// dialog prints them under each row and the writer puts them above
    /// each key it creates, so the two can never say different things.
    pub fn note(self) -> &'static str {
        match self {
            Key::AutoAdvance => {
                "Y or N moves to the next frame and ends a selection, like an arrow \
                 (default on; applies at once). Off keeps the cursor on the frame you \
                 marked — unless the filter hides it, in which case the cursor moves to \
                 the next one."
            }
            Key::SelectionWash => {
                "How strongly selected frames are tinted in the grid, 0–50 % (default \
                 25; applies at once). Grid only — the loupe never tints. Above about \
                 15 % the tint can shift your colour judgement on a final scan."
            }
            Key::LoupeMemory => {
                "Memory for decoded full-size frames: a number in GB (2, 0.5 GB) or a \
                 share of this machine's RAM (40 %) (default 2 GB; applies at the next \
                 folder open — File › Open Folder…, the same folder is fine). The app's \
                 footprint runs 1–2 GB above this number."
            }
            Key::CacheCap => {
                "The most the thumbnail cache may keep on disk, in GB (default 2 GB, \
                 never below 0.25 GB; enforced when a folder is next opened)."
            }
            Key::MaxReaders => {
                "Adaptive (recommended): 4 readers, growing while the storage keeps up. \
                 Limit N: exactly N readers when N is 4 or less; above 4, at most N \
                 (default adaptive; applies at the next folder open)."
            }
        }
    }
}

/// The Clear cache row's note. Not a key — Clear is an action, and nothing
/// about it is stored — but its sentence lives beside the others.
pub const CLEAR_CACHE_NOTE: &str =
    "Clear removes every cached thumbnail; the next open of any folder re-reads its files once.";

/// The note a setting carries while an environment variable governs it
/// (settings.md, "Environment precedence"; today only `max_readers`).
pub fn environment_note(var: &str) -> String {
    format!("Set by {var} in your environment — unset it to change this here")
}

// -------------------------------------------------------- memory grammar

/// A memory figure as the file and the dialog spell it: a number of the
/// app's GB, or a share of the machine's total RAM (the user's rule,
/// 2026-10-01: "numbers are always expressed in GB, and percentage always
/// total RAM").
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MemorySpec {
    /// GB, decimals allowed (`0.5`).
    Gb(f64),
    /// Percent of total RAM, a whole number.
    Percent(u32),
}

/// The normalised string the file stores: the number, one space, `GB`; or
/// the number and `%` with no space (settings.md, "The file").
///
/// The number is `f64`'s own shortest form, which is the number as typed
/// less any trailing zeros: `2` → `2 GB`, `0.50` → `0.5 GB`.
impl std::fmt::Display for MemorySpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemorySpec::Gb(gb) => write!(f, "{gb} GB"),
            MemorySpec::Percent(p) => write!(f, "{p}%"),
        }
    }
}

/// Parse a memory string: a decimal number, optional spaces, then `GB` in
/// any case or nothing (GB), or `%` (a share of total RAM). Anything else
/// — an empty string, `abc`, a sign, an exponent, `TB`, a fractional
/// percentage — is garbage, and `None` (settings.md, "The file").
///
/// The number is checked by hand rather than by `str::parse::<f64>`, which
/// would also take `1e9`, `inf`, `+2` and `NaN`.
pub fn parse_memory(text: &str) -> Option<MemorySpec> {
    let text = text.trim();
    let digits_end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits_end);
    if !is_plain_decimal(number) {
        return None;
    }
    let unit = unit.trim();
    if unit.is_empty() || unit.eq_ignore_ascii_case("gb") {
        number.parse::<f64>().ok().map(MemorySpec::Gb)
    } else if unit == "%" {
        // A percentage is a whole number (`Percent(u32)`, settings.md's
        // contract): "40.5%" reads as garbage rather than being rounded.
        if number.contains('.') {
            return None;
        }
        // Digits only by now, so the one way this parse fails is a number
        // too long for u64; a share that large saturates and clamps to
        // 100 % on read like any other share above 100.
        let percent = number.parse::<u64>().unwrap_or(u64::MAX);
        Some(MemorySpec::Percent(
            u32::try_from(percent).unwrap_or(u32::MAX),
        ))
    } else {
        None
    }
}

/// `digits`, or `digits.digits` — nothing else.
fn is_plain_decimal(number: &str) -> bool {
    let mut parts = number.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(whole) && parts.next().is_none() && fraction.is_none_or(digits)
}

/// A share above 100 % reads as 100 % (settings.md, "Reading").
fn clamp_share(spec: MemorySpec) -> MemorySpec {
    match spec {
        MemorySpec::Percent(p) => MemorySpec::Percent(p.min(100)),
        gb => gb,
    }
}

/// The app's GB as bytes. `as` from a float to an integer saturates in
/// Rust (a figure past `u64::MAX` reads as `u64::MAX`), which is what a
/// value nobody's machine has should do before the clamps take it.
fn gb_to_bytes(gb: f64) -> u64 {
    (gb * GB as f64).round() as u64
}

/// How the loupe memory figure in force was arrived at — what the
/// dialog's hint says beside the field (settings.md, "Performance › Loupe
/// memory").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySource {
    /// The figure as given.
    AsGiven,
    /// Below the engine's floor ([`crate::loupe::BUDGET_FLOOR_BYTES`]),
    /// raised to it.
    FlooredAt200MB,
    /// Above the machine's total RAM, held at it.
    CappedAtTotal,
    /// A percentage with the total RAM unknown: the 2 GB default instead.
    PercentUnknownRamDefault,
}

/// The bytes a memory figure puts in force for the loupe: the parsed bytes
/// clamped to [the engine's 200 MB floor, total RAM when it is known]; a
/// percentage with no total RAM is the 2 GB default. Derived at read time,
/// never written back (settings.md, "Reading").
pub fn memory_bytes(spec: MemorySpec, total_ram: Option<u64>) -> (u64, MemorySource) {
    let floor = crate::loupe::BUDGET_FLOOR_BYTES as u64;
    let wanted = match clamp_share(spec) {
        MemorySpec::Gb(gb) => gb_to_bytes(gb),
        MemorySpec::Percent(p) => match total_ram {
            // u128: a share of a terabyte machine times 100 overflows
            // nothing, and the quotient fits u64 because p <= 100.
            Some(total) => (u128::from(total) * u128::from(p) / 100) as u64,
            None => {
                return (
                    crate::loupe::DEFAULT_BUDGET_BYTES as u64,
                    MemorySource::PercentUnknownRamDefault,
                )
            }
        },
    };
    if wanted < floor {
        return (floor, MemorySource::FlooredAt200MB);
    }
    match total_ram {
        Some(total) if wanted > total => (total, MemorySource::CappedAtTotal),
        _ => (wanted, MemorySource::AsGiven),
    }
}

/// How many decoded A1 full-resolution frames `bytes` holds — the hint's
/// "≈ N A1 frames", rounded down.
pub fn a1_frames(bytes: u64) -> u64 {
    bytes / A1_FRAME_BYTES
}

// ------------------------------------------------------------ the model

/// The settings, as the app and the CLI apply them: parsed and clamped,
/// with every unreadable value already replaced by its default.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// General › Auto-advance after Y/N.
    pub auto_advance: bool,
    /// UI › Selection highlight, percent, 0..=[`WASH_MAX`].
    pub selection_wash: u32,
    /// Performance › Loupe memory, as the user spelt it (a share is
    /// clamped to 100 %; the bytes are clamped when they are derived).
    pub loupe_memory: MemorySpec,
    /// Performance › Thumbnail cache cap, in the app's GB, never below
    /// 0.25 — clamped here, because no hint beside the field could say so.
    pub cache_cap_gb: f64,
    /// Performance › Read workers: 0 = adaptive, N >= 1 a limit with
    /// exactly `FASTCULL_MAX_READERS=N`'s meaning.
    pub max_readers: u32,
}

/// The spec's numbers (settings.md, "The file"); a test pins them to the
/// constants the engines used before there was a file.
impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_advance: true,
            selection_wash: WASH_DEFAULT,
            loupe_memory: LOUPE_MEMORY_DEFAULT,
            cache_cap_gb: CACHE_CAP_DEFAULT_GB,
            max_readers: 0,
        }
    }
}

/// A value the dialog could not make sense of. The setting is left as it
/// was and the field shows the value still in force.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a valid value for {key}")]
pub struct InvalidValue {
    pub key: &'static str,
    pub text: String,
}

impl Settings {
    /// The loupe engine's budget in force on a machine with `total_ram`,
    /// and how it was arrived at.
    pub fn loupe_memory_bytes(&self, total_ram: Option<u64>) -> (u64, MemorySource) {
        memory_bytes(self.loupe_memory, total_ram)
    }

    /// The thumbnail cache cap in force, in bytes (never below the floor).
    pub fn cache_cap_bytes(&self) -> u64 {
        gb_to_bytes(self.cache_cap_gb).max(CACHE_CAP_FLOOR_BYTES)
    }

    /// The cache cap as the file and the dialog spell it (`"2 GB"`).
    pub fn cache_cap_text(&self) -> String {
        MemorySpec::Gb(self.cache_cap_gb).to_string()
    }

    /// The value `key` holds, as the dialog's field shows it and the file
    /// stores it: `true`, `25`, `2 GB`, `40%`, `0`.
    pub fn text_of(&self, key: Key) -> String {
        match key {
            Key::AutoAdvance => self.auto_advance.to_string(),
            Key::SelectionWash => self.selection_wash.to_string(),
            Key::LoupeMemory => self.loupe_memory.to_string(),
            Key::CacheCap => self.cache_cap_text(),
            Key::MaxReaders => self.max_readers.to_string(),
        }
    }

    /// A value typed or clicked in the dialog: parse it, clamp it,
    /// normalise it, and keep it — or refuse it and keep what was there
    /// (settings.md, "Apply on commit": the field then shows the value in
    /// force, never the raw text).
    pub fn set_from_text(&mut self, key: Key, text: &str) -> Result<(), InvalidValue> {
        let invalid = || InvalidValue {
            key: key.name(),
            text: text.to_string(),
        };
        let text = text.trim();
        match key {
            Key::AutoAdvance => {
                self.auto_advance = text.parse::<bool>().map_err(|_| invalid())?;
            }
            Key::SelectionWash => {
                // The field carries a `%` suffix outside it; a `%` typed
                // inside is the same number.
                let number = text.strip_suffix('%').unwrap_or(text).trim();
                let percent = number.parse::<i64>().map_err(|_| invalid())?;
                self.selection_wash = clamp_wash(percent);
            }
            Key::LoupeMemory => {
                self.loupe_memory = clamp_share(parse_memory(text).ok_or_else(invalid)?);
            }
            Key::CacheCap => match parse_memory(text) {
                Some(MemorySpec::Gb(gb)) => self.cache_cap_gb = gb.max(CACHE_CAP_FLOOR_GB),
                // A share of RAM is not a size on disk: the cap takes GB only.
                _ => return Err(invalid()),
            },
            Key::MaxReaders => {
                let readers = text.parse::<i64>().map_err(|_| invalid())?;
                self.max_readers = clamp_readers(readers);
            }
        }
        Ok(())
    }

    /// The Read workers row's checkbox: on is adaptive (0); off keeps a
    /// limit already set, or starts one at [`READERS_LIMIT_START`].
    pub fn set_readers_adaptive(&mut self, adaptive: bool) {
        if adaptive {
            self.max_readers = 0;
        } else if self.max_readers == 0 {
            self.max_readers = READERS_LIMIT_START;
        }
    }

    /// Reset to defaults — the ACTIVE tab's settings only (settings.md,
    /// "The card").
    pub fn reset_tab(&mut self, tab: Tab) {
        let defaults = Settings::default();
        for key in tab.keys() {
            match key {
                Key::AutoAdvance => self.auto_advance = defaults.auto_advance,
                Key::SelectionWash => self.selection_wash = defaults.selection_wash,
                Key::LoupeMemory => self.loupe_memory = defaults.loupe_memory,
                Key::CacheCap => self.cache_cap_gb = defaults.cache_cap_gb,
                Key::MaxReaders => self.max_readers = defaults.max_readers,
            }
        }
    }

    /// Read every known key out of a parsed file: a missing key or one of
    /// the wrong type is THAT key's default, an out-of-range value is
    /// clamped, anything unknown is ignored (settings.md, "Reading").
    fn from_document(doc: &toml_edit::DocumentMut) -> Settings {
        let defaults = Settings::default();
        let get = |key: Key| {
            doc.get(key.table())
                .and_then(|table| table.as_table_like())
                .and_then(|table| table.get(key.name()))
        };
        Settings {
            auto_advance: get(Key::AutoAdvance)
                .and_then(|v| v.as_bool())
                .unwrap_or(defaults.auto_advance),
            selection_wash: get(Key::SelectionWash)
                .and_then(|v| v.as_integer())
                .map_or(defaults.selection_wash, clamp_wash),
            loupe_memory: get(Key::LoupeMemory)
                .and_then(|v| v.as_str())
                .and_then(parse_memory)
                .map_or(defaults.loupe_memory, clamp_share),
            cache_cap_gb: get(Key::CacheCap)
                .and_then(|v| v.as_str())
                .and_then(parse_memory)
                .and_then(|spec| match spec {
                    MemorySpec::Gb(gb) => Some(gb.max(CACHE_CAP_FLOOR_GB)),
                    MemorySpec::Percent(_) => None,
                })
                .unwrap_or(defaults.cache_cap_gb),
            max_readers: get(Key::MaxReaders)
                .and_then(|v| v.as_integer())
                .map_or(defaults.max_readers, clamp_readers),
        }
    }

    /// The value `key` holds, as the TOML value the writer emits.
    fn toml_value(&self, key: Key) -> toml_edit::Value {
        match key {
            Key::AutoAdvance => self.auto_advance.into(),
            Key::SelectionWash => i64::from(self.selection_wash).into(),
            Key::LoupeMemory => self.loupe_memory.to_string().into(),
            Key::CacheCap => self.cache_cap_text().into(),
            Key::MaxReaders => i64::from(self.max_readers).into(),
        }
    }
}

/// 51 → 50, −1 → 0 (settings.md, "Reading").
fn clamp_wash(percent: i64) -> u32 {
    percent.clamp(0, i64::from(WASH_MAX)) as u32
}

/// Below 0 → 0 (adaptive); no ceiling of its own, as the variable has
/// none — `u32::MAX` is only the type's.
fn clamp_readers(readers: i64) -> u32 {
    readers.clamp(0, i64::from(u32::MAX)) as u32
}

// ---------------------------------------------------- environment wins

/// The read pool's configuration, and where it came from (settings.md,
/// "Performance › Read workers").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readers {
    /// Neither the environment nor the file names a limit.
    Adaptive,
    /// The file's `max_readers = N`.
    Limit(usize),
    /// `FASTCULL_MAX_READERS=N`, which wins over the file.
    Environment(usize),
}

impl Readers {
    /// What `Pipeline::start` takes: the override, `None` for adaptive.
    pub fn override_for_pool(self) -> Option<usize> {
        match self {
            Readers::Adaptive => None,
            Readers::Limit(n) | Readers::Environment(n) => Some(n),
        }
    }
}

/// THE one place the environment and the file are reconciled for the read
/// pool (settings.md, "Environment precedence"). A variable that parses as
/// an integer ≥ 1 wins; any other value is ignored and the file governs —
/// exactly what the pool did with `parse().ok()` when it read the variable
/// itself. `setting` 0 is adaptive.
pub fn resolve_max_readers(env: Option<&OsStr>, setting: u32) -> Readers {
    let from_env = env
        .and_then(OsStr::to_str)
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n >= 1);
    match (from_env, setting) {
        (Some(n), _) => Readers::Environment(n),
        (None, 0) => Readers::Adaptive,
        (None, n) => Readers::Limit(n as usize),
    }
}

/// [`resolve_max_readers`] against this process's environment — the call
/// both binaries make where the pool used to read the variable.
pub fn resolve_max_readers_from_env(setting: u32) -> Readers {
    resolve_max_readers(std::env::var_os(MAX_READERS_VAR).as_deref(), setting)
}

// --------------------------------------------------------- config dir

/// Where the config dir resolved to, before it is announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigDir {
    /// `FASTCULL_CONFIG_DIR=<dir>`: a driven test's own scratch dir.
    Override(PathBuf),
    /// `FASTCULL_NO_CONFIG`: no config file is read or written.
    Hermetic,
    /// The per-user config dir of the `directories` crate.
    Real(PathBuf),
}

/// THE config-dir rule, with the environment passed in so a test never
/// touches the process's own: `FASTCULL_CONFIG_DIR` (non-empty) wins, then
/// `FASTCULL_NO_CONFIG` hides everything, then the per-user dir —
/// `~/.config/fastcull/` on Linux. `None` when the platform reports no
/// home directory at all.
pub fn config_dir_from(env: impl Fn(&str) -> Option<OsString>) -> Option<ConfigDir> {
    if let Some(dir) = env(CONFIG_DIR_VAR).filter(|dir| !dir.is_empty()) {
        return Some(ConfigDir::Override(PathBuf::from(dir)));
    }
    if env(NO_CONFIG_VAR).is_some() {
        return Some(ConfigDir::Hermetic);
    }
    directories::ProjectDirs::from("org", "fastcull", "fastcull")
        .map(|dirs| ConfigDir::Real(dirs.config_dir().to_path_buf()))
}

/// The directory `settings.toml`, `ui.toml` and `templates.toml` live in,
/// for this process — the ONE resolver all three use (brief 008 D11), so
/// `FASTCULL_NO_CONFIG` hides and `FASTCULL_CONFIG_DIR` moves all three.
/// Reads the environment; announces an override on stderr, once.
pub fn config_dir() -> Option<PathBuf> {
    match config_dir_from(|name| std::env::var_os(name))? {
        ConfigDir::Override(dir) => {
            static ANNOUNCED: std::sync::Once = std::sync::Once::new();
            ANNOUNCED.call_once(|| {
                eprintln!(
                    "fastcull: {CONFIG_DIR_VAR}={} — settings.toml, ui.toml and templates.toml \
                     are read and written there",
                    dir.display()
                );
            });
            Some(dir)
        }
        ConfigDir::Hermetic => None,
        ConfigDir::Real(dir) => Some(dir),
    }
}

/// `settings.toml`'s path for this process, or `None` under
/// `FASTCULL_NO_CONFIG`.
pub fn default_settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(FILE_NAME))
}

// -------------------------------------------------------------- reading

/// What a read found.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The settings in force: the file's, or the defaults where it said
    /// nothing usable.
    pub settings: Settings,
    /// Why the file could not be read at all, in full — `None` for a
    /// missing file, which is simply the defaults. While this is `Some`
    /// the file is the user's and is never overwritten in place.
    pub error: Option<String>,
    /// The file this came from, or `None` under `FASTCULL_NO_CONFIG`.
    pub path: Option<PathBuf>,
}

impl Loaded {
    /// The defaults, with no file behind them.
    pub fn defaults(path: Option<PathBuf>) -> Loaded {
        Loaded {
            settings: Settings::default(),
            error: None,
            path,
        }
    }

    /// The read error's first line — TOML's own is a header
    /// (`TOML parse error at line 1, column 9`) over a quoted snippet,
    /// and a single line is what stderr and the status line can carry.
    pub fn error_first_line(&self) -> Option<&str> {
        self.error
            .as_deref()
            .map(|e| e.lines().next().unwrap_or("").trim())
    }
}

/// Read the settings file at `path` (settings.md, "Reading"): a missing
/// file is the defaults and no error; a file that cannot be read or does
/// not parse is the defaults WITH the error, and is left exactly as it is.
/// Never writes.
pub fn load(path: &Path) -> Loaded {
    let failed = |error: String| Loaded {
        settings: Settings::default(),
        error: Some(error),
        path: Some(path.to_path_buf()),
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded::defaults(Some(path.to_path_buf()))
        }
        Err(e) => return failed(e.to_string()),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return failed("the file is not UTF-8 text".to_string());
    };
    match text.parse::<toml_edit::DocumentMut>() {
        Ok(doc) => Loaded {
            settings: Settings::from_document(&doc),
            error: None,
            path: Some(path.to_path_buf()),
        },
        Err(e) => failed(e.to_string().trim_end().to_string()),
    }
}

/// [`load`] for this process's own file, at startup. A file that cannot
/// be read says so on stderr, once, naming the file and the error's first
/// line (settings.md, "Reading"; test-harness.md: what the environment
/// changes explains itself on stderr). `None`-path defaults under
/// `FASTCULL_NO_CONFIG`.
pub fn load_default() -> Loaded {
    let Some(path) = default_settings_path() else {
        return Loaded::defaults(None);
    };
    let loaded = load(&path);
    if let Some(first) = loaded.error_first_line() {
        eprintln!(
            "fastcull: {} could not be read ({first}) — defaults in force",
            path.display()
        );
    }
    loaded
}

// -------------------------------------------------------------- writing

/// Why a write failed. The commit stays in force in memory either way
/// (settings.md, "Writing").
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Writing the fresh file failed. `moved_aside` says where the file
    /// that would not read went when the move-aside before the write had
    /// already succeeded — the user's hand-edited file is no longer at its
    /// name, and the caller must still be able to say where it is
    /// (settings.md, "Writing"; senior-developer review F4 of brief 008).
    #[error("{source}")]
    Io {
        source: std::io::Error,
        moved_aside: Option<PathBuf>,
    },
    #[error("the file that would not read could not be moved aside: {0}")]
    MoveAside(std::io::Error),
}

impl WriteError {
    /// Where this failed write had already moved the broken file, if it had.
    pub fn moved_aside(&self) -> Option<&Path> {
        match self {
            WriteError::Io { moved_aside, .. } => moved_aside.as_deref(),
            WriteError::MoveAside(_) => None,
        }
    }
}

/// Write `settings` to `path` (settings.md, "Writing"), as a
/// read-modify-write that keeps everything of the user's: unknown keys,
/// unknown tables, comments, and an existing key's own comment above it
/// and on its line. Every known key is emitted with its value; a key this
/// write CREATES gets its note above it as `#` lines, so the file
/// documents itself.
///
/// A file that does not parse is NEVER overwritten in place (brief 008 D5:
/// a hand-edited config is the user's data). When `broken` says the last
/// read failed — or when the file fails to parse now — it is first moved
/// aside to `settings.toml.broken`, or `settings.toml.broken.N` when that
/// name is taken, and a fresh file is written; the return value says where
/// it went — and so does the error, when the move succeeded and the write
/// after it did not.
pub fn write(
    path: &Path,
    settings: &Settings,
    broken: bool,
) -> Result<Option<PathBuf>, WriteError> {
    let existing = match std::fs::read(path) {
        Ok(bytes) => Some(String::from_utf8(bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Unreadable but present: not ours to overwrite either.
        Err(_) => Some(None),
    };
    let mut moved_aside = None;
    let mut doc = match existing {
        None => toml_edit::DocumentMut::new(),
        Some(text) => match text.and_then(|t| t.parse::<toml_edit::DocumentMut>().ok()) {
            Some(doc) if !broken => doc,
            _ => {
                moved_aside = Some(move_aside(path).map_err(WriteError::MoveAside)?);
                toml_edit::DocumentMut::new()
            }
        },
    };
    merge_into(&mut doc, settings);
    // From here on a failure must carry `moved_aside`: a broken file
    // renamed above is no longer at `path`, and a bare `?` would lose
    // where it went (senior-developer review F4 of brief 008).
    let failed = |source: std::io::Error| WriteError::Io {
        source,
        moved_aside: moved_aside.clone(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    // A plain write, not temp-and-rename: a hand-managed config is often a
    // symlink into someone's dotfiles, and a rename would replace the link
    // with a file. `ui.toml` has always been written this way.
    std::fs::write(path, doc.to_string()).map_err(failed)?;
    Ok(moved_aside)
}

/// Rename the file at `path` to the first free `<name>.broken[.N]`.
fn move_aside(path: &Path) -> std::io::Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| FILE_NAME.to_string());
    // `symlink_metadata`, not `exists`: a dangling link at that name is
    // still a name in use, and `rename` would replace it.
    let taken = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let mut aside = path.with_file_name(format!("{name}.broken"));
    let mut n = 1u32;
    while taken(&aside) {
        aside = path.with_file_name(format!("{name}.broken.{n}"));
        n += 1;
    }
    std::fs::rename(path, &aside)?;
    Ok(aside)
}

/// Put every known key's value into `doc`, touching nothing else.
fn merge_into(doc: &mut toml_edit::DocumentMut, settings: &Settings) {
    for tab in TABS {
        let root = doc.as_table_mut();
        let is_table = root.get(tab.table()).is_some_and(|t| t.is_table_like());
        if !is_table {
            let mut table = toml_edit::Table::new();
            // A blank line above every table that follows something.
            if !root.is_empty() {
                table.decor_mut().set_prefix("\n");
            }
            root.insert(tab.table(), toml_edit::Item::Table(table));
        }
        let Some(item) = root.get_mut(tab.table()) else {
            continue;
        };
        // An inline table (`general = { … }`) cannot hold comment lines,
        // so the notes go only into a real `[table]`.
        let notes_allowed = item.is_table();
        let Some(table) = item.as_table_like_mut() else {
            continue;
        };
        for key in tab.keys() {
            let mut value = settings.toml_value(key);
            match table.get_mut(key.name()) {
                // REPLACE THE VALUE IN PLACE, carrying its decor (the
                // spacing and any trailing `# comment` on the line). Never
                // `insert` over an existing key: that drops the user's
                // comment above it (measured on toml_edit 0.22.27,
                // 2026-10-01 — settings.md, "Writing").
                Some(toml_edit::Item::Value(old)) => {
                    *value.decor_mut() = old.decor().clone();
                    *old = value;
                }
                // A table or an array where a value belongs: the key is
                // ours, so it becomes the value; its comment (on the key)
                // survives.
                Some(other) => *other = toml_edit::Item::Value(value),
                None => {
                    table.insert(key.name(), toml_edit::Item::Value(value));
                    if notes_allowed {
                        if let Some(mut created) = table.key_mut(key.name()) {
                            created
                                .leaf_decor_mut()
                                .set_prefix(note_comment(key.note()));
                        }
                    }
                }
            }
        }
    }
}

/// A note as `#` comment lines wrapped at [`NOTE_COLUMNS`], each ending in
/// a newline — the prefix of the key line it documents.
fn note_comment(note: &str) -> String {
    let width = NOTE_COLUMNS - 2; // "# "
    let mut out = String::new();
    let mut line = String::new();
    for word in note.split_whitespace() {
        let wanted = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
        if !line.is_empty() && wanted > width {
            out.push_str("# ");
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str("# ");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

// ------------------------------------------------------------ total RAM

/// The machine's total RAM in bytes, read once at startup: `/proc/meminfo`'s
/// `MemTotal` on Linux, `GlobalMemoryStatusEx`'s `ullTotalPhys` on Windows,
/// nothing elsewhere (settings.md, "Performance › Loupe memory"; lifted
/// from the archived `screen-rung` branch's `budget.rs`, `sysinfo` refused
/// 2026-09-26).
#[cfg(target_os = "linux")]
pub fn total_ram() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| parse_mem_total(&meminfo))
}

/// Windows: `GlobalMemoryStatusEx`'s `ullTotalPhys`, through core's ONE
/// `unsafe` block: no safe std API reports the machine's memory on
/// Windows, and `sysinfo` was refused — a large dependency on every seat
/// for one number.
#[cfg(windows)]
pub fn total_ram() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a live, aligned, fully initialised MEMORYSTATUSEX
    // whose dwLength is set as the API requires; GlobalMemoryStatusEx writes
    // only within it and keeps no pointer past the call.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0).then_some(status.ullTotalPhys)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn total_ram() -> Option<u64> {
    None
}

/// `/proc/meminfo`'s `MemTotal: <n> kB`, in bytes; `None` without the key
/// or with a value that is not a number. Public, not private, because only
/// the Linux arm of [`total_ram`] calls it: a private function would be
/// dead code on the Windows build, which `-D warnings` refuses there, and
/// its table rows run on every seat.
pub fn parse_mem_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line["MemTotal:".len()..]
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn scratch(tag: &str) -> PathBuf {
        crate::testutil::scratch_dir(&format!("settings-{tag}"))
    }

    /// A file holding `text`, in a fresh scratch dir; returns its path.
    fn file_with(tag: &str, text: &str) -> PathBuf {
        let path = scratch(tag).join(FILE_NAME);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn non_default() -> Settings {
        Settings {
            auto_advance: false,
            selection_wash: 15,
            loupe_memory: MemorySpec::Percent(40),
            cache_cap_gb: 0.5,
            max_readers: 2,
        }
    }

    /// The defaults are the spec's numbers, and the two memory defaults are
    /// the very constants the engines used before there was a file — "2 GB"
    /// in the file is the loupe's and the cache's 2 GiB, not a decimal
    /// 2,000,000,000 (settings.md, "The file").
    #[test]
    fn defaults_are_the_specs_numbers() {
        let s = Settings::default();
        assert!(s.auto_advance);
        assert_eq!(s.selection_wash, 25);
        assert_eq!(s.selection_wash, WASH_DEFAULT);
        assert_eq!(s.loupe_memory, MemorySpec::Gb(2.0));
        assert_eq!(s.max_readers, 0);
        assert_eq!(
            s.loupe_memory_bytes(None),
            (
                crate::loupe::DEFAULT_BUDGET_BYTES as u64,
                MemorySource::AsGiven
            )
        );
        assert_eq!(s.cache_cap_bytes(), crate::cache::DEFAULT_CAP_BYTES);
        assert_eq!(s.cache_cap_text(), "2 GB");
        assert_eq!(s.text_of(Key::LoupeMemory), "2 GB");
        assert_eq!(CACHE_CAP_FLOOR_BYTES, gb_to_bytes(CACHE_CAP_FLOOR_GB));
        assert_eq!(resolve_max_readers(None, s.max_readers), Readers::Adaptive);
    }

    /// Every key survives a write and a read, with values none of which is
    /// a default.
    #[test]
    fn a_written_file_round_trips_every_key() {
        let path = scratch("roundtrip").join(FILE_NAME);
        let s = non_default();
        assert_eq!(write(&path, &s, false).unwrap(), None);
        let loaded = load(&path);
        assert_eq!(loaded.error, None);
        assert_eq!(loaded.settings, s);
        // And a second write over the file it made changes nothing.
        let before = std::fs::read(&path).unwrap();
        write(&path, &s, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// The file stores the normalised spelling, never the raw text typed
    /// (settings.md, "The file") — and a value the parser refuses changes
    /// nothing.
    ///
    /// Mutant (2026-10-01): `set_from_text` storing the typed text instead
    /// of the parsed value → the file holds `"2gb"` and this goes red.
    #[test]
    fn the_normalised_string_is_what_the_file_stores() {
        let path = scratch("normalised").join(FILE_NAME);
        let mut s = Settings::default();
        s.set_from_text(Key::LoupeMemory, "8gb").unwrap();
        s.set_from_text(Key::CacheCap, " 0.50 GB").unwrap();
        write(&path, &s, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("loupe_memory = \"8 GB\""), "{text}");
        assert!(text.contains("cache_cap = \"0.5 GB\""), "{text}");
        s.set_from_text(Key::LoupeMemory, "40 %").unwrap();
        write(&path, &s, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("loupe_memory = \"40%\""), "{text}");
        assert_eq!(s.text_of(Key::LoupeMemory), "40%");

        // Garbage is refused and leaves the value in force alone.
        let held = s.clone();
        assert!(s.set_from_text(Key::LoupeMemory, "lots").is_err());
        assert!(s.set_from_text(Key::CacheCap, "40%").is_err());
        assert!(s.set_from_text(Key::SelectionWash, "a little").is_err());
        assert!(s.set_from_text(Key::MaxReaders, "four").is_err());
        assert!(s.set_from_text(Key::AutoAdvance, "maybe").is_err());
        assert_eq!(s, held);
        // The wash field's own `%`, typed inside it, is the same number.
        s.set_from_text(Key::SelectionWash, "15%").unwrap();
        assert_eq!(s.selection_wash, 15);
    }

    /// The user's fixture: a top comment, a known key with a comment above
    /// it and one on its line, an unknown key in a known table, and an
    /// unknown table.
    const USERS_FILE: &str = "# My FastCull settings — tuned for the NAS\n\
                              [general]\n\
                              # I like it off on the second pass\n\
                              auto_advance = false # set 2026-09-30\n\
                              my_unknown = 1\n\
                              \n\
                              [extras]\n\
                              colour = \"teal\"\n";

    /// Everything of the user's survives a write byte for byte
    /// (settings.md, "Writing"; brief 008 R4).
    #[test]
    fn a_write_preserves_unknown_keys_and_the_users_comments() {
        let path = file_with("preserve", USERS_FILE);
        let mut s = load(&path).settings;
        assert!(!s.auto_advance, "the fixture's own value was read");
        s.selection_wash = 15;
        write(&path, &s, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# My FastCull settings — tuned for the NAS\n[general]\n",
            "my_unknown = 1\n",
            "\n[extras]\ncolour = \"teal\"\n",
        ] {
            assert!(text.contains(kept), "lost {kept:?}:\n{text}");
        }
        let reread = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(reread["general"]["my_unknown"].as_integer(), Some(1));
        assert_eq!(reread["extras"]["colour"].as_str(), Some("teal"));
        assert_eq!(load(&path).settings, s);
    }

    /// A key the write creates carries its note above it, wrapped inside
    /// 78 columns; a key that existed keeps the user's comment above it
    /// and on its line, with only the value changed.
    ///
    /// Mutant (2026-10-01): `table.insert(...)` over the existing key in
    /// `merge_into` instead of replacing the value in place → the comment
    /// line above `auto_advance` is gone and this goes red.
    #[test]
    fn a_created_key_carries_its_note_and_an_existing_key_keeps_its_comment() {
        let path = file_with("notes", USERS_FILE);
        let mut s = load(&path).settings;
        s.auto_advance = true;
        write(&path, &s, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(
                "# I like it off on the second pass\nauto_advance = true # set 2026-09-30\n"
            ),
            "the existing key lost its comment or its trailing comment:\n{text}"
        );
        // Created: the note, as comment lines, right above the key.
        for key in [
            Key::SelectionWash,
            Key::LoupeMemory,
            Key::CacheCap,
            Key::MaxReaders,
        ] {
            let at = text
                .find(&format!("\n{} = ", key.name()))
                .unwrap_or_else(|| panic!("no {} line:\n{text}", key.name()));
            let above: Vec<&str> = text[..at]
                .lines()
                .rev()
                .take_while(|l| l.starts_with("# "))
                .collect();
            let note: Vec<&str> = above.iter().rev().map(|l| &l[2..]).collect();
            assert_eq!(note.join(" "), key.note(), "{}:\n{text}", key.name());
            for line in &above {
                assert!(
                    line.chars().count() <= 78,
                    "a note line is too wide: {line:?}"
                );
            }
        }
        // The existing key got no note of ours on top of the user's.
        assert!(
            !text.contains("# Y or N moves"),
            "a note was added above a key the user already had:\n{text}"
        );
    }

    /// A file that does not parse yields the defaults and the error, and a
    /// read leaves it exactly as it was (brief 008 D5).
    #[test]
    fn a_malformed_file_yields_defaults_and_is_left_byte_identical() {
        let broken = "[general\nauto_advance = false\n";
        let path = file_with("malformed", broken);
        let loaded = load(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(
            loaded.error_first_line(),
            Some("TOML parse error at line 1, column 9")
        );
        assert!(
            loaded
                .error
                .as_deref()
                .unwrap()
                .contains("invalid table header"),
            "the whole error is kept for the dialog: {:?}",
            loaded.error
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        // A missing file is the defaults and NO error.
        let missing = load(&path.with_file_name("nothing.toml"));
        assert_eq!(missing.settings, Settings::default());
        assert_eq!(missing.error, None);
    }

    /// The first write after a failed read moves the broken file aside —
    /// to `.broken`, then `.broken.1` when that is taken — and writes a
    /// fresh one; a file found unparsable at write time is moved aside
    /// too, whatever the caller believed.
    ///
    /// Mutant (2026-10-01): the `move_aside` call in `write` replaced by
    /// writing over the file → `.broken` never exists and this goes red.
    #[test]
    fn the_first_write_moves_a_broken_file_aside_and_writes_a_fresh_one() {
        let first = "[general\n# the user's hours of work\n";
        let path = file_with("aside", first);
        let s = non_default();
        let aside = write(&path, &s, load(&path).error.is_some()).unwrap();
        let broken = path.with_file_name("settings.toml.broken");
        assert_eq!(aside.as_deref(), Some(broken.as_path()));
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), first);
        let fresh = load(&path);
        assert_eq!(fresh.error, None, "the fresh file does not parse");
        assert_eq!(fresh.settings, s);

        // Broken again: `.broken` is taken, so `.broken.1`, and the first
        // one is untouched.
        let second = "selection_wash = = 3\n";
        std::fs::write(&path, second).unwrap();
        let aside = write(&path, &s, true).unwrap();
        let numbered = path.with_file_name("settings.toml.broken.1");
        assert_eq!(aside.as_deref(), Some(numbered.as_path()));
        assert_eq!(std::fs::read_to_string(&numbered).unwrap(), second);
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), first);

        // Broken AFTER a good read: the caller says `false`, the write
        // finds it unparsable and still refuses to overwrite it in place.
        let third = "[ui]\nselection_wash = [\n";
        std::fs::write(&path, third).unwrap();
        let aside = write(&path, &s, false).unwrap();
        let numbered = path.with_file_name("settings.toml.broken.2");
        assert_eq!(aside.as_deref(), Some(numbered.as_path()));
        assert_eq!(std::fs::read_to_string(&numbered).unwrap(), third);
        assert_eq!(load(&path).settings, s);
    }

    /// Out-of-range values clamp on read, and the memory figures clamp
    /// where their bytes are derived (settings.md, "Reading").
    ///
    /// Mutant (2026-10-01): `clamp_wash`'s bound at `WASH_MAX + 1` → 51
    /// reads as 51 and this goes red.
    #[test]
    fn out_of_range_values_clamp_on_read() {
        let read = |text: &str| load(&file_with("clamp", text)).settings;
        assert_eq!(read("[ui]\nselection_wash = 51\n").selection_wash, 50);
        assert_eq!(read("[ui]\nselection_wash = 50\n").selection_wash, 50);
        assert_eq!(read("[ui]\nselection_wash = -1\n").selection_wash, 0);
        assert_eq!(read("[ui]\nselection_wash = 0\n").selection_wash, 0);
        assert_eq!(read("[performance]\nmax_readers = -3\n").max_readers, 0);
        assert_eq!(
            read("[performance]\nloupe_memory = \"150%\"\n").loupe_memory,
            MemorySpec::Percent(100)
        );
        let cap = read("[performance]\ncache_cap = \"0.1 GB\"\n");
        assert_eq!(cap.cache_cap_text(), "0.25 GB");
        assert_eq!(cap.cache_cap_bytes(), CACHE_CAP_FLOOR_BYTES);

        let total = 31 * GIB;
        let tiny = read("[performance]\nloupe_memory = \"0.01 GB\"\n");
        assert_eq!(
            tiny.loupe_memory_bytes(Some(total)),
            (
                crate::loupe::BUDGET_FLOOR_BYTES as u64,
                MemorySource::FlooredAt200MB
            )
        );
        let huge = read("[performance]\nloupe_memory = \"9999 GB\"\n");
        assert_eq!(
            huge.loupe_memory_bytes(Some(total)),
            (total, MemorySource::CappedAtTotal)
        );
        let all = read("[performance]\nloupe_memory = \"150%\"\n");
        assert_eq!(
            all.loupe_memory_bytes(Some(total)),
            (total, MemorySource::AsGiven)
        );
        // The same clamps on a commit.
        let mut s = Settings::default();
        s.set_from_text(Key::SelectionWash, "51").unwrap();
        assert_eq!(s.selection_wash, 50);
        s.set_from_text(Key::SelectionWash, "-1").unwrap();
        assert_eq!(s.selection_wash, 0);
        s.set_from_text(Key::LoupeMemory, "250%").unwrap();
        assert_eq!(s.loupe_memory, MemorySpec::Percent(100));
    }

    /// A value of the wrong type reads as THAT key's default, and the
    /// file's other values still count.
    #[test]
    fn wrong_typed_values_fall_back_to_that_keys_default() {
        let path = file_with(
            "types",
            "[general]\nauto_advance = \"yes\"\n\
             [ui]\nselection_wash = \"10\"\n\
             [performance]\nloupe_memory = 8\ncache_cap = \"40%\"\nmax_readers = \"4\"\n",
        );
        let loaded = load(&path);
        assert_eq!(loaded.error, None, "wrong types are not a broken file");
        assert_eq!(loaded.settings, Settings::default());
        let path = file_with(
            "types2",
            "[ui]\nselection_wash = 10.5\n[performance]\nmax_readers = 6\n",
        );
        let s = load(&path).settings;
        assert_eq!(s.selection_wash, WASH_DEFAULT, "a float is the wrong type");
        assert_eq!(s.max_readers, 6, "the good value beside it still counts");
    }

    /// The memory grammar (settings.md, "The file").
    #[test]
    fn the_memory_string_parser() {
        for (text, want) in [
            ("2", MemorySpec::Gb(2.0)),
            ("2 GB", MemorySpec::Gb(2.0)),
            ("2GB", MemorySpec::Gb(2.0)),
            ("2 gb", MemorySpec::Gb(2.0)),
            ("0.5 GB", MemorySpec::Gb(0.5)),
            (" 12.4 Gb ", MemorySpec::Gb(12.4)),
            ("40%", MemorySpec::Percent(40)),
            ("40 %", MemorySpec::Percent(40)),
            ("0%", MemorySpec::Percent(0)),
            ("150%", MemorySpec::Percent(150)),
        ] {
            assert_eq!(parse_memory(text), Some(want), "{text:?}");
        }
        for garbage in [
            "", "abc", "-1 GB", "2 TB", "1e9", "+2", "inf", "NaN", ".5", "2.", "GB", "%", "40.5%",
            "2 G", "2 GiB", "1.2.3",
        ] {
            assert_eq!(parse_memory(garbage), None, "{garbage:?}");
        }
        for (spec, text) in [
            (MemorySpec::Gb(2.0), "2 GB"),
            (MemorySpec::Gb(0.5), "0.5 GB"),
            (MemorySpec::Percent(40), "40%"),
        ] {
            assert_eq!(spec.to_string(), text);
            assert_eq!(parse_memory(text), Some(spec), "the normal form re-parses");
        }
        // Garbage in the file is the key's default.
        let path = file_with("garbage", "[performance]\nloupe_memory = \"lots\"\n");
        assert_eq!(load(&path).settings.loupe_memory, MemorySpec::Gb(2.0));
    }

    /// A percentage needs the machine's total RAM; without it the default
    /// is in force, and the source says so for the hint.
    #[test]
    fn a_percentage_with_unknown_ram_falls_back_to_the_default_and_says_so() {
        assert_eq!(
            memory_bytes(MemorySpec::Percent(40), None),
            (2 * GIB, MemorySource::PercentUnknownRamDefault)
        );
        // GB needs no RAM figure.
        assert_eq!(
            memory_bytes(MemorySpec::Gb(8.0), None),
            (8 * GIB, MemorySource::AsGiven)
        );
        assert_eq!(
            memory_bytes(MemorySpec::Percent(25), Some(32 * GIB)),
            (8 * GIB, MemorySource::AsGiven)
        );
    }

    /// The hint's frame count: one A1 frame is 149,299,200 bytes, rounded
    /// down (settings.md, "Performance › Loupe memory").
    #[test]
    fn the_frames_hint_counts_a1_frames() {
        assert_eq!(A1_FRAME_BYTES, 149_299_200);
        assert_eq!(a1_frames(2 * GIB), 14);
        assert_eq!(a1_frames(13_351_123_353), 89);
        assert_eq!(a1_frames(A1_FRAME_BYTES - 1), 0);
        assert_eq!(a1_frames(crate::loupe::BUDGET_FLOOR_BYTES as u64), 1);
    }

    #[test]
    fn parse_mem_total_reads_meminfo() {
        let meminfo = "MemTotal:       32595516 kB\nMemFree:          812344 kB\n";
        assert_eq!(parse_mem_total(meminfo), Some(32_595_516 * 1024));
        assert_eq!(parse_mem_total("MemFree: 812344 kB\n"), None, "no key");
        assert_eq!(parse_mem_total("MemTotal: lots kB\n"), None, "not a number");
    }

    /// The environment wins over the file; a value that is not an integer
    /// ≥ 1 is ignored and the file governs (settings.md, "Environment
    /// precedence").
    ///
    /// Mutant (2026-10-01): the file checked before the environment in
    /// `resolve_max_readers` → env 3 + file 7 reads `Limit(7)` and this
    /// goes red.
    #[test]
    fn the_environment_wins_over_the_file_for_max_readers() {
        let env = |v: &'static str| Some(OsStr::new(v));
        assert_eq!(resolve_max_readers(env("3"), 7), Readers::Environment(3));
        assert_eq!(resolve_max_readers(env("3"), 0), Readers::Environment(3));
        for ignored in ["abc", "0", "", " 3", "-2", "3.5"] {
            assert_eq!(
                resolve_max_readers(env(ignored), 7),
                Readers::Limit(7),
                "{ignored:?}"
            );
            assert_eq!(
                resolve_max_readers(env(ignored), 0),
                Readers::Adaptive,
                "{ignored:?}"
            );
        }
        assert_eq!(resolve_max_readers(None, 0), Readers::Adaptive);
        assert_eq!(resolve_max_readers(None, 7), Readers::Limit(7));
        assert_eq!(Readers::Adaptive.override_for_pool(), None);
        assert_eq!(Readers::Limit(7).override_for_pool(), Some(7));
        assert_eq!(Readers::Environment(3).override_for_pool(), Some(3));
    }

    /// Reset touches the named tab's settings and nothing else.
    #[test]
    fn reset_restores_one_tabs_defaults_only() {
        let mut s = non_default();
        s.reset_tab(Tab::Performance);
        assert_eq!(s.loupe_memory, MemorySpec::Gb(2.0));
        assert_eq!(s.cache_cap_gb, 2.0);
        assert_eq!(s.max_readers, 0);
        assert!(!s.auto_advance, "General was not reset");
        assert_eq!(s.selection_wash, 15, "UI was not reset");
        s.reset_tab(Tab::General);
        assert!(s.auto_advance);
        assert_eq!(s.selection_wash, 15);
        s.reset_tab(Tab::Ui);
        assert_eq!(s, Settings::default());
        // Every key belongs to exactly one tab, in the order of TABS.
        let order: Vec<Key> = TABS.iter().flat_map(|t| t.keys()).collect();
        assert_eq!(order, Key::ALL.to_vec());
        for key in Key::ALL {
            assert_eq!(Key::from_name(key.name()), Some(key));
        }
    }

    /// The Read workers checkbox: off starts a limit at 4, a limit already
    /// set is kept, on is adaptive.
    #[test]
    fn clearing_adaptive_starts_a_limit_at_four() {
        let mut s = Settings::default();
        s.set_readers_adaptive(false);
        assert_eq!(s.max_readers, READERS_LIMIT_START);
        s.max_readers = 2;
        s.set_readers_adaptive(false);
        assert_eq!(s.max_readers, 2);
        s.set_readers_adaptive(true);
        assert_eq!(s.max_readers, 0);
    }

    /// The config-dir rule, with the environment injected — never the
    /// process's own, which every test in this binary shares.
    ///
    /// Mutant (2026-10-01): `FASTCULL_NO_CONFIG` checked before
    /// `FASTCULL_CONFIG_DIR` → the first row reads `Hermetic` and this
    /// goes red.
    #[test]
    fn config_dir_honours_the_override_then_no_config() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            config_dir_from(env(&[
                (CONFIG_DIR_VAR, "/tmp/fc-scratch"),
                (NO_CONFIG_VAR, "1")
            ])),
            Some(ConfigDir::Override(PathBuf::from("/tmp/fc-scratch")))
        );
        assert_eq!(
            config_dir_from(env(&[(NO_CONFIG_VAR, "1")])),
            Some(ConfigDir::Hermetic)
        );
        assert_eq!(
            config_dir_from(env(&[(NO_CONFIG_VAR, "")])),
            Some(ConfigDir::Hermetic),
            "any value of FASTCULL_NO_CONFIG counts, as it always has"
        );
        assert_eq!(
            config_dir_from(env(&[(CONFIG_DIR_VAR, ""), (NO_CONFIG_VAR, "1")])),
            Some(ConfigDir::Hermetic),
            "an empty override is no override"
        );
        assert!(
            matches!(config_dir_from(env(&[])), None | Some(ConfigDir::Real(_))),
            "with neither variable the per-user dir (or none) is the answer"
        );
    }

    /// The comment wrapper keeps every word and every line inside 78
    /// columns, counting characters (an em dash is one column), and a word
    /// longer than a line stands on its own.
    #[test]
    fn notes_wrap_inside_78_columns() {
        for key in Key::ALL {
            let comment = note_comment(key.note());
            assert!(comment.ends_with('\n'));
            let words: Vec<&str> = comment
                .lines()
                .map(|l| l.strip_prefix("# ").expect("a comment line"))
                .flat_map(str::split_whitespace)
                .collect();
            assert_eq!(words.join(" "), key.note());
            for line in comment.lines() {
                assert!(line.chars().count() <= NOTE_COLUMNS, "{line:?}");
            }
        }
        let long = "x".repeat(100);
        assert_eq!(
            note_comment(&format!("a {long} b")),
            format!("# a\n# {long}\n# b\n")
        );
    }
}
