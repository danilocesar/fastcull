//! The machine's loupe sizes (raw-pipeline.md, "Memory" and "The decode
//! workers"; the user's redesign of 2026-09-26): the pixel cache from the
//! machine's TOTAL RAM, the decoder count from its physical cores (capped by
//! its RAM), the `FASTCULL_DECODERS` switch, the whole-app worst case, and
//! the startup line that says all of it once on stderr. Read once, at
//! startup: nothing here changes during a session, and there is no setting
//! (#39 parked; the persona: "a toggle you can't see in the UI is worse than
//! none").
//!
//! `derive` is pure, so every rule is a table row; `Machine::probe` is the one
//! place the OS is asked, and on Windows it holds core's one `unsafe` block
//! (Manager ruling 2026-09-26, brief 008 Q1).

use std::ffi::OsStr;

use crate::loupe::{fullres_ring_ahead, REF_FRAME_BYTES, RING_AHEAD, RING_BEHIND};

const GIB: u64 = 1 << 30;

/// The pixel cache's floor — the loupe's cache before brief 008, which a
/// small machine keeps (the user's rule: "never below today's 2 GiB").
pub const CACHE_FLOOR: u64 = crate::loupe::DEFAULT_BUDGET_BYTES as u64;
/// The pixel cache's cap (the user, 2026-09-26: "Would a 10G limit help to
/// have more textures live so it would be faster to move between images?").
pub const CACHE_CAP: u64 = 10 * GIB;
/// An unreadable, zero or absurd total is read as this: the machine the
/// 2 GiB floor already assumes (Manager ruling 2026-09-26, brief 008 Q-C).
pub const TOTAL_FALLBACK: u64 = 8 * GIB;
/// "Absurd": a total over 16 TiB is not believed.
pub const TOTAL_ABSURD: u64 = 16 << 40;
/// Two backlog workers and the focus-reserved lane.
pub const DECODERS_FLOOR: usize = 3;
/// The ring holds 18 frames, so more than 18 decoders could never all be
/// busy, and 16 is the core count of the one culling machine whose CPU is
/// known (raw-pipeline.md, "The decode workers").
pub const DECODERS_CAP: usize = 16;
/// A zero or missing physical-core count is read as this.
pub const DECODERS_FALLBACK: usize = 4;
/// `FASTCULL_DECODERS` below this reads as it: one backlog worker beside
/// the reserved lane, the least that still reads ahead.
pub const DECODERS_OVERRIDE_MIN: usize = 2;
/// `FASTCULL_DECODERS` above this is clamped to it, with a stderr line
/// (raw-pipeline.md, "The decode workers"; QE 2026-09-28, D3): each decoder
/// is a thread, and `99999` crashed the app at the spawn. Four times the cap:
/// far past what the ring (18 frames) or a screen of grid cells can keep
/// busy, so every diagnosis the switch exists for — the cap, the user's
/// 16-core machine with its 32 threads — stays below it, and far below the
/// thread count an OS refuses.
pub const DECODERS_OVERRIDE_MAX: usize = 64;
/// The decoder-count switch, in the mould of `FASTCULL_MAX_READERS`: an
/// environment variable, so a release build honours it (test-harness.md).
pub const DECODERS_VAR: &str = "FASTCULL_DECODERS";
/// The app's cap on mid textures (~5 MB each) beyond the prune-to-visible
/// window (recorded decision: 4K + 6 columns worst case). Here, not in the
/// app, because the whole-app worst case counts it: one home.
pub const MIDS_CAP: usize = 64;
/// The input each decoder holds while it decodes (`read_jpeg`'s whole
/// stream): the largest of the three reference A1 files' embedded full
/// JPEGs, `A1_full_compressed.ARW`'s.
pub const REF_JPEG_BYTES: u64 = 12_313_510;
/// glibc's mmap threshold the app sets first in `main` on Linux with glibc
/// (raw-pipeline.md, "Memory", The Linux allocator): the power of two under
/// the smallest rung an A1 caches, its 5,235,840 B mid — an A1 property
/// (M11). ONE home: the app's `mallopt`, the RSS ceiling test's
/// `GLIBC_TUNABLES` and the startup line's clause all read it.
pub const MMAP_THRESHOLD: u64 = 4 << 20;

/// A 4K screen's screen rung, 3240 × 2160 × 3 bytes: the screen-rung ring's
/// texture in the worst case (raw-pipeline.md, "Memory").
const REF_RUNG_BYTES: u64 = 20_995_200;
/// An A1 mid, 1616 × 1080 × 3 bytes.
const REF_MID_BYTES: u64 = 5_235_840;

/// What the OS reports about the machine, read once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Machine {
    /// The total RAM the OS reports, in bytes — not the free figure.
    pub total_ram: Option<u64>,
    /// The physical cores, not the logical ones.
    pub physical_cores: Option<usize>,
}

impl Machine {
    /// The OS's figures: total RAM from `/proc/meminfo`'s `MemTotal` on
    /// Linux and `GlobalMemoryStatusEx`'s `ullTotalPhys` on Windows (none
    /// elsewhere); physical cores from `num_cpus::get_physical`, which never
    /// answers 0 — it falls back to the logical count when the topology is
    /// unreadable (raw-pipeline.md, "The decode workers").
    pub fn probe() -> Self {
        Machine {
            total_ram: total_ram(),
            physical_cores: Some(num_cpus::get_physical()),
        }
    }
}

#[cfg(target_os = "linux")]
fn total_ram() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| parse_mem_total(&meminfo))
}

/// Windows: `GlobalMemoryStatusEx`'s `ullTotalPhys`, through core's ONE
/// `unsafe` block (Manager ruling 2026-09-26, brief 008 Q1): no safe std API
/// reports the machine's memory on Windows, and `sysinfo` was refused — a
/// large dependency on every seat for one number.
#[cfg(windows)]
fn total_ram() -> Option<u64> {
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
fn total_ram() -> Option<u64> {
    None
}

/// `/proc/meminfo`'s `MemTotal: <n> kB`, in bytes; `None` without the key
/// or with a value that is not a number. Public, not private, because only
/// the Linux arm of the probe calls it: a private function would be dead
/// code on the Windows build, which `-D warnings` refuses there, and its
/// table rows run on every seat.
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

/// Where the cache's size came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheSource {
    /// A quarter of the total RAM.
    Quarter,
    /// The 2 GiB floor (a quarter would be less).
    Floor,
    /// The 10 GiB cap (a quarter would be more).
    Cap,
    /// The total was unreadable, zero or absurd: read as 8 GiB.
    Unreadable,
}

/// Where the decoder count came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderSource {
    /// The physical cores, within 3 to 16.
    Cores,
    /// Half the RAM in GiB, which is fewer.
    RamCap,
    /// `FASTCULL_DECODERS`.
    Override,
    /// `FASTCULL_DECODERS` above its ceiling (`DECODERS_OVERRIDE_MAX`),
    /// clamped to it: `given` is the value the variable held.
    OverrideClamped { given: usize },
    /// The core count was zero or missing: read as 4.
    Unreadable,
}

/// The total as the rules read it: an unreadable, zero or absurd one as
/// 8 GiB (Manager ruling 2026-09-26, brief 008 Q-C).
fn total_as_read(total: Option<u64>) -> (u64, bool) {
    match total {
        Some(t) if t > 0 && t <= TOTAL_ABSURD => (t, false),
        _ => (TOTAL_FALLBACK, true),
    }
}

/// The pixel cache: a quarter of the machine's TOTAL RAM, never below
/// 2 GiB and never above 10 GiB — `clamp(total ÷ 4, 2 GiB, 10 GiB)` — and
/// 2 GiB when the total is unreadable, zero or absurd (read as 8 GiB)
/// (raw-pipeline.md, "Memory"; the user's rule, brief 008 R6).
pub fn cache_bytes(total: Option<u64>) -> (u64, CacheSource) {
    let (total, unreadable) = total_as_read(total);
    let quarter = total / 4;
    if unreadable {
        (
            quarter.clamp(CACHE_FLOOR, CACHE_CAP),
            CacheSource::Unreadable,
        )
    } else if quarter < CACHE_FLOOR {
        (CACHE_FLOOR, CacheSource::Floor)
    } else if quarter > CACHE_CAP {
        (CACHE_CAP, CacheSource::Cap)
    } else {
        (quarter, CacheSource::Quarter)
    }
}

/// The loupe's decoders (raw-pipeline.md, "The decode workers"): one per
/// PHYSICAL core, floor 3 and cap 16 — `min(16, max(3, cores))`, a zero or
/// missing count read as 4 — and at most half the RAM in GiB,
/// `max(3, ⌊total ÷ 2 GiB⌋)` of the total as read for the cache (Manager
/// rulings 2026-09-26, brief 008 Q4, Q-C and Q-D): the smaller wins.
/// `FASTCULL_DECODERS` (already parsed, `parse_decoders_override`) replaces
/// all of it, above either cap too, up to its ceiling of 64
/// (`DECODERS_OVERRIDE_MAX`), which a larger value is clamped to (QE
/// 2026-09-28, D3).
pub fn decoders(
    cores: Option<usize>,
    total: Option<u64>,
    over: Option<usize>,
) -> (usize, DecoderSource) {
    if let Some(n) = over {
        return if n > DECODERS_OVERRIDE_MAX {
            (
                DECODERS_OVERRIDE_MAX,
                DecoderSource::OverrideClamped { given: n },
            )
        } else {
            (n, DecoderSource::Override)
        };
    }
    let (cores, cores_source) = match cores {
        Some(n) if n > 0 => (n, DecoderSource::Cores),
        _ => (DECODERS_FALLBACK, DecoderSource::Unreadable),
    };
    let by_cores = cores.clamp(DECODERS_FLOOR, DECODERS_CAP);
    let (total, _) = total_as_read(total);
    let by_ram = usize::try_from(total / (2 * GIB))
        .unwrap_or(usize::MAX)
        .max(DECODERS_FLOOR);
    if by_ram < by_cores {
        (by_ram, DecoderSource::RamCap)
    } else {
        (by_cores, cores_source)
    }
}

/// `FASTCULL_DECODERS`: unset → `Ok(None)`; a positive integer N →
/// `Ok(Some(max(N, 2)))`, 1 reading as 2; anything else → `Err` with the
/// stderr line that says it is ignored (raw-pipeline.md, "The decode
/// workers": a leaked value must explain itself, test-harness.md).
pub fn parse_decoders_override(value: Option<&OsStr>) -> Result<Option<usize>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.to_str().map(str::parse::<usize>) {
        Some(Ok(n)) if n > 0 => Ok(Some(n.max(DECODERS_OVERRIDE_MIN))),
        _ => Err(format!(
            "fastcull: {DECODERS_VAR}=\"{}\" is not a positive integer — ignored",
            value.to_string_lossy()
        )),
    }
}

/// The whole-app worst case (raw-pipeline.md, "Memory") — A1 frames, a 4K
/// screen, a long session at 1:1 with the cache full, every decoder rotating
/// a portrait frame and the kitchen filling a full-res texture: the cache +
/// the full-res ring's frames × 149,299,200 B (their texture copies) + 18 ×
/// 20,995,200 B (the screen-rung ring's) + 64 × 5,235,840 B (the mids) + the
/// decoders × (2 × 149,299,200 B + 12,313,510 B) (two decoded frames and the
/// JPEG each reads) + 149,299,200 B (the kitchen's fill in flight).
pub fn peak_bytes(cache: u64, fullres_frames: usize, decoders: usize) -> u64 {
    let frames = u64::try_from(fullres_frames).unwrap_or(u64::MAX);
    let decoders = u64::try_from(decoders).unwrap_or(u64::MAX);
    let rung_ring = u64::try_from(1 + RING_BEHIND + RING_AHEAD).unwrap_or(u64::MAX);
    let mids = u64::try_from(MIDS_CAP).unwrap_or(u64::MAX);
    cache
        + frames * REF_FRAME_BYTES
        + rung_ring * REF_RUNG_BYTES
        + mids * REF_MID_BYTES
        + decoders * (2 * REF_FRAME_BYTES + REF_JPEG_BYTES)
        + REF_FRAME_BYTES
}

/// The machine's loupe sizes, and the startup line that says them
/// (`Display`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoupeSizes {
    pub cache_bytes: u64,
    pub cache_source: CacheSource,
    /// The total RAM the OS reported (`Machine::probe`); the rules read an
    /// unreadable, zero or absurd one as 8 GiB.
    pub total_ram: Option<u64>,
    pub decoders: usize,
    pub decoder_source: DecoderSource,
    /// How far ahead the full-res ring reaches at 1:1 on this cache
    /// (`loupe::fullres_ring_ahead`).
    pub fullres_ahead: usize,
    /// The whole-app worst case on this machine (`peak_bytes`).
    pub peak_bytes: u64,
    /// The stderr line for a `FASTCULL_DECODERS` that was ignored, or
    /// clamped to its ceiling.
    pub warning: Option<String>,
    /// The mmap threshold the APP reports it set: `None` from `derive` and
    /// `from_machine` — core never calls `mallopt` — and set by the app's
    /// `main` to what `mallopt` accepted, before the line is printed.
    /// `Some(t)` puts `mmap threshold <t in MiB> MiB` on the line; `None`
    /// puts no `mmap threshold` clause there at all.
    pub mmap_threshold: Option<u64>,
}

impl LoupeSizes {
    /// The sizes for `machine`, with `FASTCULL_DECODERS` already parsed —
    /// pure, every rule a table row.
    pub fn derive(machine: &Machine, over: Result<Option<usize>, String>) -> Self {
        let (cache_bytes, cache_source) = self::cache_bytes(machine.total_ram);
        let (over, warning) = match over {
            Ok(n) => (n, None),
            Err(line) => (None, Some(line)),
        };
        let (decoders, decoder_source) =
            self::decoders(machine.physical_cores, machine.total_ram, over);
        // A value clamped to the ceiling explains itself on stderr too
        // (test-harness.md: a leaked value must); an ignored one already has
        // its line, and the two never meet.
        let warning = match decoder_source {
            DecoderSource::OverrideClamped { given } => Some(format!(
                "fastcull: {DECODERS_VAR}={given} is above its ceiling of \
                 {DECODERS_OVERRIDE_MAX} — {DECODERS_OVERRIDE_MAX} decoders"
            )),
            _ => warning,
        };
        let fullres_ahead = fullres_ring_ahead(cache_bytes);
        let peak_bytes = peak_bytes(cache_bytes, 1 + RING_BEHIND + fullres_ahead, decoders);
        LoupeSizes {
            cache_bytes,
            cache_source,
            total_ram: machine.total_ram,
            decoders,
            decoder_source,
            fullres_ahead,
            peak_bytes,
            warning,
            mmap_threshold: None,
        }
    }

    /// The sizes for THIS machine: `Machine::probe` and the environment's
    /// `FASTCULL_DECODERS`, whose stderr line — when the value is ignored or
    /// clamped — is printed here, once.
    pub fn from_machine() -> Self {
        let over = parse_decoders_override(std::env::var_os(DECODERS_VAR).as_deref());
        let sizes = Self::derive(&Machine::probe(), over);
        if let Some(line) = &sizes.warning {
            eprintln!("{line}");
        }
        sizes
    }
}

/// Bytes as GiB with one decimal, the startup line's unit.
fn gib(bytes: u64) -> String {
    // Display only: an f64 holds these sizes exactly enough for one decimal.
    format!("{:.1}", bytes as f64 / GIB as f64)
}

impl std::fmt::Display for LoupeSizes {
    /// THE startup line (raw-pipeline.md, "Memory"): the cache and where it
    /// came from, the ring and the full-res ring ahead at 1:1, the decoders
    /// and where they came from, and the whole-app worst case — and, when the
    /// app reports one, the mmap threshold it set. Its `fastcull: loupe
    /// cache ` prefix and its tokens are what the tests read.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total = gib(total_as_read(self.total_ram).0);
        let cache_source = match self.cache_source {
            CacheSource::Quarter => format!("a quarter of {total} GiB total RAM"),
            CacheSource::Floor => format!("the 2 GiB floor; {total} GiB total RAM"),
            CacheSource::Cap => format!("the 10 GiB cap; {total} GiB total RAM"),
            CacheSource::Unreadable => "total RAM unreadable, read as 8 GiB".to_owned(),
        };
        let decoder_source = match self.decoder_source {
            DecoderSource::Cores => "physical cores, 3 to 16".to_owned(),
            DecoderSource::RamCap => format!("half of {total} GiB RAM"),
            DecoderSource::Override => DECODERS_VAR.to_owned(),
            DecoderSource::OverrideClamped { given } => {
                format!("{DECODERS_VAR}={given}, clamped to its ceiling")
            }
            DecoderSource::Unreadable => "core count unreadable, read as 4".to_owned(),
        };
        write!(
            f,
            "fastcull: loupe cache {} GiB ({cache_source}), ring {RING_BEHIND} behind / \
             {RING_AHEAD} ahead, full-res {} ahead at 1:1, {} decoders ({decoder_source}), \
             worst case {} GiB for A1 frames on a 4K screen plus ~0.2 GB per 1,000 thumbnails",
            gib(self.cache_bytes),
            self.fullres_ahead,
            self.decoders,
            gib(self.peak_bytes),
        )?;
        if let Some(threshold) = self.mmap_threshold {
            write!(f, "; glibc mmap threshold {} MiB", threshold >> 20)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brief 008 A4 (raw-pipeline.md, "Memory"): the cache is a quarter of
    /// the TOTAL RAM between 2 and 10 GiB — 4 / 8 / 16 / 32 / 64 GiB give 2,
    /// 2, 4, 8, 10 GiB — and an unreadable, zero or absurd total gives 2 GiB
    /// (read as 8). Red with the floor dropped (the 4 GiB row reads 1 GiB)
    /// and with the cap dropped (the 64 GiB row reads 16 GiB).
    #[test]
    fn the_pixel_cache_is_a_quarter_of_total_ram_between_2_and_10_gib() {
        for (total, cache, source) in [
            (4 * GIB, 2 * GIB, CacheSource::Floor),
            (8 * GIB, 2 * GIB, CacheSource::Quarter),
            (16 * GIB, 4 * GIB, CacheSource::Quarter),
            (32 * GIB, 8 * GIB, CacheSource::Quarter),
            (64 * GIB, 10 * GIB, CacheSource::Cap),
        ] {
            assert_eq!(
                cache_bytes(Some(total)),
                (cache, source),
                "{} GiB total",
                total / GIB
            );
        }
        for total in [None, Some(0), Some(17 << 40)] {
            assert_eq!(
                cache_bytes(total),
                (2 * GIB, CacheSource::Unreadable),
                "total {total:?}"
            );
        }
        let meminfo = "MemTotal:       32595516 kB\nMemFree:          812344 kB\n";
        assert_eq!(parse_mem_total(meminfo), Some(32_595_516 * 1024));
        assert_eq!(parse_mem_total("MemFree: 812344 kB\n"), None, "no key");
        assert_eq!(parse_mem_total("MemTotal: lots kB\n"), None, "not a number");
    }

    /// Brief 008 A4 (raw-pipeline.md, "The decode workers"): one decoder per
    /// PHYSICAL core, floor 3, cap 16 — on 64 GiB, 2 / 4 / 16 / 32 cores give
    /// 3, 4, 16, 16 — and a zero or missing count reads as 4. Red with no cap
    /// (32).
    #[test]
    fn the_decoders_follow_the_physical_cores() {
        for (cores, n, source) in [
            (2, 3, DecoderSource::Cores),
            (4, 4, DecoderSource::Cores),
            (16, 16, DecoderSource::Cores),
            (32, 16, DecoderSource::Cores),
        ] {
            assert_eq!(
                decoders(Some(cores), Some(64 * GIB), None),
                (n, source),
                "{cores} cores"
            );
        }
        for cores in [None, Some(0)] {
            assert_eq!(
                decoders(cores, Some(64 * GIB), None),
                (4, DecoderSource::Unreadable),
                "cores {cores:?}"
            );
        }
    }

    /// Brief 008 A4 (Manager rulings 2026-09-26, Q4, Q-C and Q-D): the RAM
    /// caps the decoders at half its GiB, rounded down, never below 3 — on
    /// 16 cores, 6 / 8 / 16 / 31 / 32 GiB give 3, 4, 8, 15, 16 (a 32 GB
    /// machine reports 31.x GiB and runs 15) — and an unreadable, zero or
    /// absurd total, read as 8 GiB, gives 4. Red with no RAM cap (the 8 and
    /// 16 GiB rows) and with an unreadable total leaving the decoders
    /// uncapped (16).
    #[test]
    fn the_decoders_are_capped_at_half_the_ram_in_gib() {
        for (total, n, source) in [
            (6 * GIB, 3, DecoderSource::RamCap),
            (8 * GIB, 4, DecoderSource::RamCap),
            (16 * GIB, 8, DecoderSource::RamCap),
            (31 * GIB, 15, DecoderSource::RamCap),
            (32 * GIB, 16, DecoderSource::Cores),
        ] {
            assert_eq!(
                decoders(Some(16), Some(total), None),
                (n, source),
                "16 cores, {} GiB",
                total / GIB
            );
        }
        for total in [None, Some(0), Some(17 << 40)] {
            assert_eq!(
                decoders(Some(16), total, None).0,
                4,
                "16 cores, total {total:?}: read as 8 GiB"
            );
        }
        assert_eq!(
            decoders(Some(4), Some(8 * GIB), None),
            (4, DecoderSource::Cores),
            "4 cores on 8 GiB: the cap does not bind"
        );
    }

    /// Brief 008 A4 (raw-pipeline.md, "The decode workers"):
    /// `FASTCULL_DECODERS` wins — above both caps too — and 1 reads as 2; a
    /// value that is not a positive integer is ignored with its stderr line.
    ///
    /// And (QE round 1's D3, 2026-09-28) it wins up to its ceiling of 64:
    /// the ceiling itself is taken as given, with no line; 65 and 99999 —
    /// which crashed the app at the thread spawn — are 64, each with a
    /// stderr line naming the variable, the value and the ceiling, and the
    /// startup line names the clamp. The "above both caps" row keeps its 20,
    /// strictly between the cap and the ceiling. Red with no ceiling (99999
    /// decoders) and with the comparison one off (64 clamped with a line, or
    /// 65 taken as given).
    #[test]
    fn a_decoder_override_wins_and_a_bad_one_is_ignored() {
        assert_eq!(parse_decoders_override(None), Ok(None));
        assert_eq!(
            parse_decoders_override(Some(OsStr::new("12"))),
            Ok(Some(12))
        );
        assert_eq!(parse_decoders_override(Some(OsStr::new("1"))), Ok(Some(2)));
        for bad in ["0", "-3", "abc", ""] {
            let err =
                parse_decoders_override(Some(OsStr::new(bad))).expect_err("not a positive integer");
            assert_eq!(
                err,
                format!(
                    "fastcull: FASTCULL_DECODERS=\"{bad}\" is not a positive integer — ignored"
                )
            );
        }
        assert_eq!(
            decoders(Some(16), Some(8 * GIB), Some(20)),
            (20, DecoderSource::Override),
            "above both caps"
        );
        assert_eq!(
            decoders(Some(16), Some(8 * GIB), Some(2)),
            (2, DecoderSource::Override),
            "below the floor"
        );
        let sizes = LoupeSizes::derive(
            &Machine {
                total_ram: Some(8 * GIB),
                physical_cores: Some(16),
            },
            parse_decoders_override(Some(OsStr::new("abc"))),
        );
        assert_eq!(sizes.decoders, 4, "an ignored value leaves the rule");
        assert!(sizes.warning.is_some_and(|w| w.contains("\"abc\"")));

        // The ceiling (QE round 1's D3).
        // The premise, checked when the test compiles: the above-both-caps
        // row's 20 sits strictly between the cap and the ceiling.
        const {
            assert!(
                DECODERS_CAP < 20 && 20 < DECODERS_OVERRIDE_MAX,
                "the above-both-caps row sits below the ceiling"
            )
        };
        let sixteen_cores = Machine {
            total_ram: Some(8 * GIB),
            physical_cores: Some(16),
        };
        let at_ceiling = LoupeSizes::derive(
            &sixteen_cores,
            parse_decoders_override(Some(OsStr::new("64"))),
        );
        assert_eq!(
            (at_ceiling.decoders, at_ceiling.decoder_source),
            (64, DecoderSource::Override),
            "the ceiling itself is taken as given"
        );
        assert_eq!(at_ceiling.warning, None, "and says nothing");
        for given in [65, 99_999] {
            let sizes = LoupeSizes::derive(
                &sixteen_cores,
                parse_decoders_override(Some(OsStr::new(&given.to_string()))),
            );
            assert_eq!(
                (sizes.decoders, sizes.decoder_source),
                (64, DecoderSource::OverrideClamped { given }),
                "{given}: clamped to the ceiling"
            );
            assert_eq!(
                sizes.warning,
                Some(format!(
                    "fastcull: FASTCULL_DECODERS={given} is above its ceiling of 64 — 64 decoders"
                )),
                "{given}: the line names the variable, the value and the ceiling"
            );
            let line = sizes.to_string();
            assert!(
                line.contains(&format!(
                    "64 decoders (FASTCULL_DECODERS={given}, clamped to its ceiling)"
                )),
                "{given}: the startup line names the clamp: {line}"
            );
        }
    }

    /// Brief 008 A4 (raw-pipeline.md, "Memory", the startup line): the line
    /// names the cache and its source, the ring and the full-res ring ahead,
    /// the decoders and their source, and the worst case — for the
    /// development laptop (31.1 GiB, 4 cores) and for an 8 GiB machine with
    /// 16 cores, whose RAM caps it at 4 decoders and 3 frames ahead (Q-G (i))
    /// — and `peak_bytes` is the table's own figure, exactly: the kitchen's
    /// fill and each decoder's input JPEG counted. Red with either term left
    /// out.
    #[test]
    fn the_startup_line_names_the_cache_the_ring_the_decoders_and_the_peak() {
        let laptop = LoupeSizes::derive(
            &Machine {
                total_ram: Some(32_595_516 * 1024),
                physical_cores: Some(4),
            },
            Ok(None),
        );
        assert_eq!(laptop.peak_bytes, 13_137_791_896);
        assert_eq!(
            laptop.to_string(),
            "fastcull: loupe cache 7.8 GiB (a quarter of 31.1 GiB total RAM), ring 2 behind \
             / 15 ahead, full-res 15 ahead at 1:1, 4 decoders (physical cores, 3 to 16), \
             worst case 12.2 GiB for A1 frames on a 4K screen plus ~0.2 GB per 1,000 \
             thumbnails"
        );
        let small = LoupeSizes::derive(
            &Machine {
                total_ram: Some(8 * GIB),
                physical_cores: Some(16),
            },
            Ok(None),
        );
        assert_eq!(
            small.to_string(),
            "fastcull: loupe cache 2.0 GiB (a quarter of 8.0 GiB total RAM), ring 2 behind \
             / 15 ahead, full-res 3 ahead at 1:1, 4 decoders (half of 8.0 GiB RAM), worst \
             case 4.8 GiB for A1 frames on a 4K screen plus ~0.2 GB per 1,000 thumbnails"
        );
        assert_eq!(peak_bytes(2 << 30, 6, 4), 5_149_233_048, "the 8 GiB row");
        let unknown = LoupeSizes::derive(&Machine::default(), Ok(None));
        assert!(
            unknown
                .to_string()
                .contains("(total RAM unreadable, read as 8 GiB)")
                && unknown
                    .to_string()
                    .contains("4 decoders (core count unreadable, read as 4)"),
            "{unknown}"
        );
    }

    /// Brief 008, the Linux allocator (raw-pipeline.md, "The app sets glibc's
    /// mmap threshold and says so"): the threshold is 4 MiB, a whole number
    /// of MiB, and the startup line carries `mmap threshold 4 MiB` only when
    /// the app reports the threshold it set — the line `derive` builds has no
    /// clause, and the same line with the app's report is that line with the
    /// clause appended. Red when the clause is printed whatever the app
    /// reports (the None row), and — on every seat, CI included, where the
    /// RSS ceiling test never runs — when the constant moves (the value row).
    #[test]
    fn the_startup_line_names_the_mmap_threshold_only_when_set() {
        assert_eq!(MMAP_THRESHOLD, 4_194_304);
        assert_eq!(MMAP_THRESHOLD % (1 << 20), 0, "a whole number of MiB");
        let mut sizes = LoupeSizes::derive(
            &Machine {
                total_ram: Some(8 * GIB),
                physical_cores: Some(16),
            },
            Ok(None),
        );
        let without = sizes.to_string();
        assert!(
            !without.contains("mmap threshold"),
            "no threshold reported, no clause: {without}"
        );
        sizes.mmap_threshold = Some(MMAP_THRESHOLD);
        let with = sizes.to_string();
        assert!(with.contains("mmap threshold 4 MiB"), "{with}");
        assert!(
            with.starts_with(&without),
            "the clause is appended to the same line: {with}"
        );
    }

    /// The environment variable that turns [`stderr_child`] on.
    const STDERR_CHILD_VAR: &str = "FASTCULL_BUDGET_STDERR_CHILD";

    /// The child half of [`the_decoder_override_lines_reach_stderr`]: runs
    /// only in the child process that test starts from this same test binary,
    /// and returns at once in any other run (loupe.rs's `stderr_child`
    /// pattern, no `#[ignore]`). It derives THIS machine's sizes the way the
    /// app does, with whatever `FASTCULL_DECODERS` the parent handed it, so
    /// what it prints on stderr is what the app prints.
    #[test]
    fn stderr_child() {
        if std::env::var_os(STDERR_CHILD_VAR).is_none() {
            return; // not the child: nothing to do
        }
        let _ = LoupeSizes::from_machine();
    }

    /// Brief 008 A4 (raw-pipeline.md, "The decode workers"; test-harness.md:
    /// a value leaked into some environment must explain itself on stderr):
    /// the two lines `derive` builds for `FASTCULL_DECODERS` — a value above
    /// the ceiling, clamped, and one that is not a positive integer, ignored
    /// — REACH stderr, once each, when the machine's sizes are derived; a
    /// value the rule takes as given (12, and the ceiling itself, 64) and an
    /// unset variable print none. `derive`'s `warning` field is asserted
    /// above, but nothing read the print itself: with `from_machine`'s
    /// `eprintln!` removed the suite stayed green (the senior developer's
    /// review of QE round 1's fixes, F4). Read from a child process of this
    /// test binary ([`stderr_child`]), the variable set on the child's
    /// command, never in this process's environment, which other tests
    /// share; the unset row removes it from the child's, since the runner's
    /// own environment may carry one. Red with that print removed (the 99999
    /// and the "abc" rows).
    ///
    /// Review-verified, not driven: that the app prints the same lines —
    /// its `main` calls `LoupeSizes::from_machine()` before it prints the
    /// startup line (crates/fastcull-app/src/main.rs), and no other code
    /// derives the sizes.
    #[test]
    fn the_decoder_override_lines_reach_stderr() {
        let lines = |value: Option<&str>| -> Vec<String> {
            let exe = std::env::current_exe().expect("the test binary");
            let mut child = std::process::Command::new(exe);
            child
                .args([
                    "budget::tests::stderr_child",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(STDERR_CHILD_VAR, "1");
            match value {
                Some(value) => child.env(DECODERS_VAR, value),
                None => child.env_remove(DECODERS_VAR),
            };
            let out = child.output().expect("the child runs");
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            assert!(
                out.status.success(),
                "{value:?}: the child failed:\n{stderr}"
            );
            stderr
                .lines()
                .filter(|l| l.starts_with("fastcull: FASTCULL_DECODERS="))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(
            lines(Some("99999")),
            ["fastcull: FASTCULL_DECODERS=99999 is above its ceiling of 64 — 64 decoders"],
            "a value above the ceiling explains itself on stderr, once"
        );
        assert_eq!(
            lines(Some("abc")),
            ["fastcull: FASTCULL_DECODERS=\"abc\" is not a positive integer — ignored"],
            "a value that is not a positive integer explains itself on stderr, once"
        );
        for taken in ["12", "64"] {
            assert_eq!(
                lines(Some(taken)),
                Vec::<String>::new(),
                "{taken}: a value taken as given prints no line"
            );
        }
        assert_eq!(
            lines(None),
            Vec::<String>::new(),
            "an unset variable prints no line"
        );
    }
}
