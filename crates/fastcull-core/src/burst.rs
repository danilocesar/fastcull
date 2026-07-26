//! Burst grouping (specs/modules/burst-grouping.md): pure functions over
//! per-frame capture metadata. Display metadata only — grouping never
//! reorders images and has no effect on picks, sidecars, or copies.
//!
//! Behavior differs by brand (see the spec's "Sony vs. other brands"
//! table): Sony frames carry maker-note sequence numbers, so 2-frame
//! bursts qualify and back-to-back squeezes split on the sequence
//! RESET. Every other brand (and any Sony file with a missing/corrupt
//! maker note) uses the generic time-only path: gaps within threshold,
//! same body, run length >= `min_run`.

/// Per-frame input, in CAPTURE-TIME order (the session sort's tiebreak
/// rules applied by the caller).
#[derive(Clone, Debug, Default)]
pub struct FrameMeta {
    /// Capture instant in milliseconds (any epoch — only gaps matter).
    /// None = no usable timestamp: the frame never joins a group.
    pub time_ms: Option<i64>,
    /// True when SubSecTime contributed (millisecond precision). Without
    /// it timestamps have 1 s granularity and the gap threshold widens
    /// (spec rule 3 — persona fix: whole-second steps are within-burst).
    pub has_subsec: bool,
    /// Camera identity for the generic path (EXIF serial, else model).
    pub camera: Option<String>,
    /// Sony maker-note SequenceNumber: 0 = single shot, >=1 = position
    /// in a burst. None = tag absent (generic path).
    pub seq: Option<u32>,
}

impl FrameMeta {
    /// Build from an EXIF summary (the app's MetadataReady payload):
    /// time from the chronological sort key, SubSec presence from the
    /// summary, camera identity = serial else model, seq from the Sony
    /// maker-note pass.
    pub fn from_summary(exif: &crate::exif::ExifSummary) -> Self {
        FrameMeta {
            time_ms: exif.sort_key().as_deref().and_then(parse_sort_key_ms),
            has_subsec: exif.subsec.is_some(),
            camera: exif
                .serial_number
                .clone()
                .or_else(|| exif.camera_model.clone()),
            seq: exif.sequence_number,
        }
    }
}

/// "YYYY:MM:DD HH:MM:SS.mmm" (the filter.rs sort key) → epoch-ish
/// milliseconds. Only DIFFERENCES matter to grouping, so the exact epoch
/// is irrelevant; days-from-civil keeps month/year boundaries exact.
pub fn parse_sort_key_ms(key: &str) -> Option<i64> {
    let (date, time) = key.split_once(' ')?;
    let mut d = date.split(':');
    let (y, m, day) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    let (hms, millis) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':');
    let (h, min, s) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let ms = format!("{millis:0<3}")[..3].parse::<i64>().ok()?;
    // days-from-civil (Howard Hinnant), mirror of the civil-from-days in
    // iptc.rs.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((((days * 24 + h) * 60 + min) * 60 + s) * 1000 + ms)
}

/// Grouping knobs (config values, not constants — fixed defaults, no
/// settings UI in v1).
#[derive(Clone, Copy, Debug)]
pub struct BurstConfig {
    /// Max gap between consecutive frames of one burst when both carry
    /// SubSec precision.
    pub max_gap_ms: i64,
    /// Generic path only: minimum run length that counts as a burst.
    pub min_run: usize,
}

impl Default for BurstConfig {
    fn default() -> Self {
        Self {
            max_gap_ms: 600,
            min_run: 3,
        }
    }
}

/// Per-frame result: `group[i]` = None (single) or Some(group index);
/// group indices are dense and increase in input order. `size[g]` =
/// member count of group g.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grouping {
    pub group: Vec<Option<usize>>,
    pub size: Vec<usize>,
}

impl Grouping {
    /// True when `i` is the FIRST frame of its group (the badge cell).
    /// Group members need NOT be contiguous (interleaved bodies), so
    /// "first" means no earlier member, not "previous differs".
    pub fn is_group_start(&self, i: usize) -> bool {
        self.position_in_group(i).is_some_and(|(pos, _)| pos == 1)
    }

    /// 1-based position of `i` inside its group ("burst 7/23"), with the
    /// group size. None for singles. O(i) — use [`Grouping::positions`]
    /// when querying every frame.
    pub fn position_in_group(&self, i: usize) -> Option<(usize, usize)> {
        let g = self.group.get(i).copied().flatten()?;
        let pos = self.group[..=i].iter().filter(|x| **x == Some(g)).count();
        Some((pos, self.size[g]))
    }

    /// All frames' (position, group size) in one O(n) pass — the app's
    /// per-recompute path (per-frame `position_in_group` would be O(n²)).
    pub fn positions(&self) -> Vec<Option<(usize, usize)>> {
        let mut counter = vec![0usize; self.size.len()];
        self.group
            .iter()
            .map(|g| {
                g.map(|g| {
                    counter[g] += 1;
                    (counter[g], self.size[g])
                })
            })
            .collect()
    }
}

/// Effective gap threshold between two neighbors (spec rule 3): 1 s
/// granularity wins when either side lacks SubSec.
fn threshold(cfg: &BurstConfig, a: &FrameMeta, b: &FrameMeta) -> i64 {
    if a.has_subsec && b.has_subsec {
        cfg.max_gap_ms
    } else {
        cfg.max_gap_ms.max(1000)
    }
}

/// Group frames (input already in capture order). Sony path where
/// SequenceNumber exists; generic camera+gap runs otherwise. Frames are
/// partitioned by camera identity FIRST, so two bodies shooting
/// simultaneously (interleaved in capture order) group independently
/// (spec criterion 4) — gaps are measured between consecutive frames of
/// the SAME body.
pub fn group(frames: &[FrameMeta], cfg: &BurstConfig) -> Grouping {
    let n = frames.len();
    let mut group: Vec<Option<usize>> = vec![None; n];

    // Partition capture-order indices by body. Identity-less frames
    // (camera == None) share one partition, as before.
    let mut bodies: Vec<(&Option<String>, Vec<usize>)> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let cam = &f.camera;
        match bodies.iter_mut().find(|(c, _)| *c == cam) {
            Some((_, idxs)) => idxs.push(i),
            None => bodies.push((cam, vec![i])),
        }
    }

    // Pass 1 per body: provisional runs over that body's frames. A run
    // continues from the body's previous frame p to f when:
    // - both have timestamps and the gap is within threshold, AND
    // - Sony: seq did not reset (f.seq > p.seq) and both are >=1.
    //   A frame with seq == Some(0) is a declared single: never joins.
    // Runs hold GLOBAL indices; (start_global, members, sony).
    let mut runs: Vec<(usize, Vec<usize>, bool)> = Vec::new();
    fn flush(runs: &mut Vec<(usize, Vec<usize>, bool)>, cur: &mut Vec<usize>, sony: &mut bool) {
        if !cur.is_empty() {
            runs.push((cur[0], std::mem::take(cur), *sony));
        }
        *sony = false;
    }
    for (_, idxs) in &bodies {
        let mut cur: Vec<usize> = Vec::new();
        let mut cur_sony = false;
        for &i in idxs {
            let f = &frames[i];
            let declared_single = f.seq == Some(0);
            let joinable = f.time_ms.is_some() && !declared_single;
            if !joinable {
                flush(&mut runs, &mut cur, &mut cur_sony);
                // Emit the unjoinable frame as its own single-frame run.
                runs.push((i, vec![i], false));
                continue;
            }
            if let Some(&pi) = cur.last() {
                let p = &frames[pi];
                let gap_ok = match (p.time_ms, f.time_ms) {
                    (Some(a), Some(b)) => (b - a).abs() <= threshold(cfg, p, f),
                    _ => false,
                };
                let sony_pair = p.seq.is_some_and(|s| s >= 1) && f.seq.is_some_and(|s| s >= 1);
                let seq_ok = if sony_pair {
                    // Persona fix: a reset (seq <= previous) starts a NEW
                    // squeeze even inside the gap window.
                    f.seq.unwrap() > p.seq.unwrap()
                } else {
                    // Generic path: no mixing Sony-burst and plain frames.
                    !(p.seq.is_some_and(|s| s >= 1) ^ f.seq.is_some_and(|s| s >= 1))
                };
                if gap_ok && seq_ok {
                    cur.push(i);
                    cur_sony = cur_sony || sony_pair;
                    continue;
                }
                flush(&mut runs, &mut cur, &mut cur_sony);
            }
            cur.push(i);
            cur_sony = f.seq.is_some_and(|s| s >= 1);
        }
        flush(&mut runs, &mut cur, &mut cur_sony);
    }

    // Pass 2: a run is a group when Sony (any length >= 2 — the sequence
    // numbers vouch) or generic with length >= min_run. Dense group ids
    // follow capture order of the groups' FIRST frames, so adjacent
    // groups in a capture-sorted view get consecutive indices.
    runs.sort_by_key(|(start, _, _)| *start);
    let mut size: Vec<usize> = Vec::new();
    for (_, members, sony) in runs {
        let len = members.len();
        let qualifies = if sony { len >= 2 } else { len >= cfg.min_run };
        if qualifies {
            let g = size.len();
            for i in members {
                group[i] = Some(g);
            }
            size.push(len);
        }
    }
    Grouping { group, size }
}

/// `[` / `]` navigation (spec UI contract): from view position `pos`,
/// the next/prev VIEW position whose group differs from the cursor's
/// (singles are their own territory — each ungrouped frame is a distinct
/// "group" for boundary purposes). Clamps at the ends; `group_of` maps a
/// view position to the grouping's Option<usize> for that image.
pub fn next_boundary(
    pos: usize,
    len: usize,
    group_of: impl Fn(usize) -> Option<usize>,
    forward: bool,
) -> usize {
    if len == 0 {
        return 0;
    }
    let here = group_of(pos);
    if forward {
        let mut j = pos;
        while j + 1 < len {
            j += 1;
            if group_of(j) != here || group_of(j).is_none() {
                return j;
            }
        }
        len - 1
    } else {
        // Backwards, CD-player convention (persona decision 2026-07-26):
        // from mid-group, `[` first RE-ANCHORS on the current group's
        // first visible frame (the compare-against-the-opener move, many
        // times per burst); only from that first frame does it cross to
        // the previous group/single. Landing is always a group's first
        // visible member, never its last.
        // Group members need not be contiguous in the view (interleaved
        // bodies, non-capture sorts), so "first member" is the earliest
        // view position with the same group — a scan, not a walk.
        if here.is_some() {
            if let Some(first) = (0..pos).find(|&p| group_of(p) == here) {
                return first; // re-anchor on the group's first visible frame
            }
        }
        let mut j = pos;
        while j > 0 {
            j -= 1;
            let target = group_of(j);
            if target != here || target.is_none() {
                if target.is_some() {
                    return (0..=j).find(|&p| group_of(p) == target).unwrap_or(j);
                }
                return j; // a single is its own territory
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sony(t: i64, seq: u32) -> FrameMeta {
        FrameMeta {
            time_ms: Some(t),
            has_subsec: true,
            camera: Some("A1#123".into()),
            seq: Some(seq),
        }
    }

    fn generic(t: i64, subsec: bool, cam: &str) -> FrameMeta {
        FrameMeta {
            time_ms: Some(t),
            has_subsec: subsec,
            camera: Some(cam.into()),
            seq: None,
        }
    }

    #[test]
    fn sort_key_ms_differences_are_exact() {
        let a = parse_sort_key_ms("2021:04:05 17:41:23.570").unwrap();
        let b = parse_sort_key_ms("2021:04:05 17:41:23.620").unwrap();
        assert_eq!(b - a, 50);
        // Midnight rollover and month boundary stay exact.
        let x = parse_sort_key_ms("2021:04:30 23:59:59.900").unwrap();
        let y = parse_sort_key_ms("2021:05:01 00:00:00.100").unwrap();
        assert_eq!(y - x, 200);
        assert!(parse_sort_key_ms("garbage").is_none());
    }

    #[test]
    fn singles_never_group_and_a1_burst_is_one_group() {
        // Seq=0 singles interleaved with a 20 fps squeeze (50 ms gaps).
        let mut frames = vec![sony(0, 0)];
        for i in 0..20 {
            frames.push(sony(10_000 + i * 50, (i + 1) as u32));
        }
        frames.push(sony(60_000, 0));
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.group[0], None);
        assert_eq!(g.group[21], None);
        assert!(g.group[1..21].iter().all(|x| *x == Some(0)));
        assert_eq!(g.size, vec![20]);
        assert!(g.is_group_start(1));
        assert!(!g.is_group_start(2));
        assert_eq!(g.position_in_group(7), Some((7, 20)));
    }

    #[test]
    fn pause_splits_and_seq_reset_splits() {
        // 700 ms pause splits.
        let frames = vec![sony(0, 1), sony(50, 2), sony(750, 3), sony(800, 4)];
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![2, 2]);
        assert_ne!(g.group[1], g.group[2]);
        // Persona fix: two squeezes 300 ms apart — Seq 1..8 then 1..5 —
        // form TWO groups despite gaps within the window.
        let mut frames = Vec::new();
        for i in 0..8 {
            frames.push(sony(i * 50, (i + 1) as u32));
        }
        for i in 0..5 {
            frames.push(sony(700 + i * 50, (i + 1) as u32));
        }
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![8, 5], "reset must split: {:?}", g.group);
    }

    #[test]
    fn generic_path_min_run_and_mixed_bodies() {
        // 2 quick frames: no group. 3: group.
        let two = vec![generic(0, true, "X"), generic(100, true, "X")];
        assert_eq!(
            group(&two, &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
        let three = vec![
            generic(0, true, "X"),
            generic(100, true, "X"),
            generic(200, true, "X"),
        ];
        assert_eq!(group(&three, &BurstConfig::default()).size, vec![3]);
        // Interleaved bodies with 2 frames each: per-body runs of 2 stay
        // below min_run (interleaving itself no longer breaks runs — see
        // interleaved_bodies_group_independently for the 3-per-body case).
        let mixed = vec![
            generic(0, true, "X"),
            generic(50, true, "Y"),
            generic(100, true, "X"),
            generic(150, true, "Y"),
        ];
        assert_eq!(
            group(&mixed, &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
    }

    #[test]
    fn no_subsec_whole_second_steps_stay_one_group() {
        // Persona fix: 1 s granularity — consecutive whole seconds are
        // within-burst; a 2 s step splits.
        let frames = vec![
            generic(1000, false, "X"),
            generic(1000, false, "X"),
            generic(2000, false, "X"),
            generic(3000, false, "X"),
            generic(5000, false, "X"),
            generic(6000, false, "X"),
            generic(7000, false, "X"),
        ];
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![4, 3], "2 s step splits: {:?}", g.group);
    }

    #[test]
    fn boundary_navigation_over_groups_and_singles() {
        // view groups: [A A A] [s] [B B] [s] (s = single)
        let gmap = [Some(0), Some(0), Some(0), None, Some(1), Some(1), None];
        let gof = |i: usize| gmap[i];
        // forward from inside A -> the single after A
        assert_eq!(next_boundary(1, 7, gof, true), 3);
        // forward from the single -> first of B
        assert_eq!(next_boundary(3, 7, gof, true), 4);
        // forward from inside B -> trailing single; clamps at end after
        assert_eq!(next_boundary(4, 7, gof, true), 6);
        assert_eq!(next_boundary(6, 7, gof, true), 6);
        // backward from B's second frame -> RE-ANCHOR on B's first frame
        // (CD-player convention, persona decision)
        assert_eq!(next_boundary(5, 7, gof, false), 4);
        // backward from B's FIRST frame -> the single before it
        assert_eq!(next_boundary(4, 7, gof, false), 3);
        // backward from the single at 3 -> FIRST frame of A
        assert_eq!(next_boundary(3, 7, gof, false), 0);
        // backward from mid-A -> re-anchor on A's first, then clamp
        assert_eq!(next_boundary(2, 7, gof, false), 0);
        assert_eq!(next_boundary(0, 7, gof, false), 0);
        assert_eq!(next_boundary(0, 0, gof, true), 0);
    }

    /// Two bodies shooting simultaneously, frames interleaved in capture
    /// order, group independently (spec criterion 4) — gaps measured
    /// between same-body frames, badge on each group's first frame.
    #[test]
    fn interleaved_bodies_group_independently() {
        // Generic path: X,Y,X,Y,X,Y at 25 ms interleave (50 ms per body).
        let frames: Vec<FrameMeta> = (0..6)
            .map(|i| generic(i * 25, true, if i % 2 == 0 { "X" } else { "Y" }))
            .collect();
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![3, 3], "each body forms its own group");
        assert_eq!(
            g.group,
            vec![Some(0), Some(1), Some(0), Some(1), Some(0), Some(1)]
        );
        assert!(g.is_group_start(0) && g.is_group_start(1));
        assert!(!g.is_group_start(2) && !g.is_group_start(3));
        assert_eq!(g.position_in_group(4), Some((3, 3)));
        assert_eq!(g.positions()[3], Some((2, 3)));

        // Sony path: two A1 bodies, seq 1..3 each, interleaved.
        let two = |t: i64, cam: &str, seq: u32| FrameMeta {
            time_ms: Some(t),
            has_subsec: true,
            camera: Some(cam.into()),
            seq: Some(seq),
        };
        let frames = vec![
            two(0, "A", 1),
            two(10, "B", 1),
            two(50, "A", 2),
            two(60, "B", 2),
            two(100, "A", 3),
            two(110, "B", 3),
        ];
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![3, 3], "sequence numbers per body");
    }

    /// `[`/`]` with interleaved groups: landings are the group's first
    /// VISIBLE frame even when members are not adjacent.
    #[test]
    fn boundary_navigation_with_interleaved_groups() {
        // view: A B A B A B (two interleaved groups)
        let gmap = [Some(0), Some(1), Some(0), Some(1), Some(0), Some(1)];
        let gof = |i: usize| gmap[i];
        // backward from mid-A (pos 4) -> re-anchor on A's first (pos 0)
        assert_eq!(next_boundary(4, 6, gof, false), 0);
        // backward from mid-B (pos 3) -> re-anchor on B's first (pos 1)
        assert_eq!(next_boundary(3, 6, gof, false), 1);
        // backward from B's first (pos 1) -> A's first (pos 0)
        assert_eq!(next_boundary(1, 6, gof, false), 0);
    }

    /// Integration-shaped: the three real A1 reference files are single
    /// shots — their metadata (Seq=0 or absent, seconds apart) yields
    /// zero groups. Mirrors the real-file criterion at the unit level;
    /// the exif-plumbing integration test covers the file path.
    #[test]
    fn sparse_singles_produce_zero_groups() {
        let frames = vec![sony(0, 0), sony(27_000, 0), sony(42_000, 0)];
        assert_eq!(
            group(&frames, &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
    }

    // ---- QE re-verification probes (adopted from the M7 QE pass) ----

    /// Sequential (non-interleaved) two-body control: the partition must
    /// not change the trivial case.
    #[test]
    fn sequential_two_bodies_form_two_groups() {
        let frames = vec![
            generic(0, true, "X"),
            generic(50, true, "X"),
            generic(100, true, "X"),
            generic(150, true, "Y"),
            generic(200, true, "Y"),
            generic(250, true, "Y"),
        ];
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![3, 3]);
        assert_eq!(
            g.group,
            vec![Some(0), Some(0), Some(0), Some(1), Some(1), Some(1)]
        );
    }

    /// A frame WITH identity but WITHOUT a timestamp splits its own
    /// body's run (unjoinable); an all-None-timestamp session groups
    /// nothing and panics nowhere.
    #[test]
    fn no_timestamp_frames_split_their_own_body() {
        let no_time = FrameMeta {
            time_ms: None,
            has_subsec: false,
            camera: Some("X".into()),
            seq: None,
        };
        let frames = vec![
            generic(0, true, "X"),
            generic(50, true, "X"),
            no_time.clone(),
            generic(100, true, "X"),
            generic(150, true, "X"),
        ];
        let g = group(&frames, &BurstConfig::default());
        // 2 + 2 around the hole — both below min_run.
        assert_eq!(g.size, Vec::<usize>::new(), "{:?}", g.group);
        assert!(g.group.iter().all(|x| x.is_none()));
        let frames = vec![no_time.clone(), no_time.clone(), no_time];
        assert_eq!(
            group(&frames, &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
    }

    /// Spec consequence of the partition (validator observation, made
    /// deliberate): an identity-less frame (corrupt EXIF) mid-burst does
    /// NOT split the body's run — the body bridges over it.
    #[test]
    fn identityless_frame_mid_burst_bridges() {
        let no_cam = FrameMeta {
            time_ms: Some(75),
            has_subsec: true,
            camera: None,
            seq: None,
        };
        let frames = vec![
            generic(0, true, "X"),
            generic(50, true, "X"),
            no_cam,
            generic(100, true, "X"),
        ];
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![3], "X bridges over the no-EXIF frame");
        assert_eq!(g.group, vec![Some(0), Some(0), None, Some(0)]);
    }

    /// A Sony burst interleaved with a generic body's run: independent
    /// groups, and the XOR guard never mixes seq and non-seq frames.
    #[test]
    fn sony_body_interleaved_with_generic_body() {
        let a1 = |t: i64, seq: u32| FrameMeta {
            time_ms: Some(t),
            has_subsec: true,
            camera: Some("A1#1".into()),
            seq: Some(seq),
        };
        let frames = vec![
            a1(0, 1),
            generic(25, true, "Z9"),
            a1(50, 2),
            generic(75, true, "Z9"),
            a1(100, 3),
            generic(125, true, "Z9"),
            a1(150, 4),
            generic(175, true, "Z9"),
        ];
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![4, 4], "{:?}", g.group);
        assert_eq!(g.group[0], g.group[2]);
        assert_eq!(g.group[1], g.group[3]);
        assert_ne!(g.group[0], g.group[1]);
    }

    /// Corrupt maker note: identical sequence numbers read as resets
    /// everywhere — runs of 1, no groups, no panic.
    #[test]
    fn duplicate_seq_numbers_never_group() {
        let frames = vec![sony(0, 1), sony(50, 1), sony(100, 1)];
        assert_eq!(
            group(&frames, &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
    }

    /// A dropped frame (seq 1,2,4) is still strictly increasing — one
    /// group; empty and single-frame inputs are inert.
    #[test]
    fn seq_gaps_group_and_degenerate_inputs_are_inert() {
        let frames = vec![sony(0, 1), sony(50, 2), sony(100, 4)];
        assert_eq!(group(&frames, &BurstConfig::default()).size, vec![3]);
        assert_eq!(
            group(&[], &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
        let one = vec![sony(0, 5)];
        assert_eq!(
            group(&one, &BurstConfig::default()).size,
            Vec::<usize>::new()
        );
    }

    /// Many interleaved bodies: positions and sizes stay consistent.
    #[test]
    fn many_interleaved_bodies_stress() {
        let mut frames = Vec::new();
        for i in 0..80i64 {
            let body = format!("B{}", i % 4);
            frames.push(generic(i * 5, true, &body));
        }
        let g = group(&frames, &BurstConfig::default());
        assert_eq!(g.size, vec![20; 4]);
        let pos = g.positions();
        assert_eq!(pos[0], Some((1, 20)));
        assert_eq!(pos[79], Some((20, 20)));
        assert!(g.is_group_start(0) && g.is_group_start(3));
        assert!(!g.is_group_start(4));
    }
}
