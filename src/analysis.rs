use std::collections::BTreeMap;

use crate::heatmap::Grid;
use crate::model::{normalize_bssid, Band, Project};

/// Common "good enough for voice/video" design target.
pub const DEFAULT_TARGET_DBM: f64 = -67.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub total_cells: usize,
    /// Cells that have any data within the interpolation cutoff.
    pub surveyed_cells: usize,
    /// Surveyed cells at or above the target.
    pub covered_cells: usize,
}

impl Coverage {
    /// % of surveyed area meeting the target (what you can actually claim).
    pub fn pct_of_surveyed(&self) -> f64 {
        pct(self.covered_cells, self.surveyed_cells)
    }

    /// % of the whole plan meeting the target (unsurveyed counts against you).
    pub fn pct_of_plan(&self) -> f64 {
        pct(self.covered_cells, self.total_cells)
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { 100.0 * n as f64 / d as f64 }
}

pub fn coverage(grid: &Grid, target: f64) -> Coverage {
    let surveyed = grid.values.iter().flatten().count();
    let covered = grid.values.iter().flatten().filter(|v| **v >= target).count();
    Coverage { total_cells: grid.values.len(), surveyed_cells: surveyed, covered_cells: covered }
}

/// A contiguous area that was surveyed but fell below the target.
#[derive(Debug, Clone, PartialEq)]
pub struct DeadZone {
    pub cells: usize,
    pub area_px2: f64,
    /// Centroid in plan pixels.
    pub centroid: (f64, f64),
    pub worst_value: f64,
}

/// Finds connected (4-neighbour) regions of cells below `target`, largest first.
/// Regions smaller than `min_cells` are ignored as noise.
pub fn dead_zones(grid: &Grid, target: f64, min_cells: usize) -> Vec<DeadZone> {
    let mut seen = vec![false; grid.values.len()];
    let mut zones = Vec::new();
    let weak = |c: usize, r: usize| grid.get(c, r).is_some_and(|v| v < target);

    for r in 0..grid.rows {
        for c in 0..grid.cols {
            let i = r * grid.cols + c;
            if seen[i] || !weak(c, r) {
                continue;
            }
            let mut stack = vec![(c, r)];
            seen[i] = true;
            let (mut n, mut sx, mut sy, mut worst) = (0usize, 0.0, 0.0, f64::INFINITY);
            while let Some((cc, rr)) = stack.pop() {
                n += 1;
                let (x, y) = grid.cell_center(cc, rr);
                sx += x;
                sy += y;
                worst = worst.min(grid.get(cc, rr).unwrap());
                let mut visit = |nc: usize, nr: usize| {
                    let ni = nr * grid.cols + nc;
                    if !seen[ni] && weak(nc, nr) {
                        seen[ni] = true;
                        stack.push((nc, nr));
                    }
                };
                if cc > 0 { visit(cc - 1, rr); }
                if rr > 0 { visit(cc, rr - 1); }
                if cc + 1 < grid.cols { visit(cc + 1, rr); }
                if rr + 1 < grid.rows { visit(cc, rr + 1); }
            }
            if n >= min_cells {
                zones.push(DeadZone {
                    cells: n,
                    area_px2: n as f64 * grid.cell_px * grid.cell_px,
                    centroid: (sx / n as f64, sy / n as f64),
                    worst_value: worst,
                });
            }
        }
    }
    zones.sort_by(|a, b| b.cells.cmp(&a.cells));
    zones
}

/// One access point aggregated over every survey point that heard it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApSummary {
    pub bssid: String,
    pub ssid: String,
    pub name: String,
    pub channel: u16,
    pub band: Band,
    pub max_rssi_dbm: i32,
    pub mean_rssi_dbm: f64,
    pub samples: usize,
}

/// Aggregates readings per BSSID, strongest AP first.
pub fn summarize_aps(project: &Project) -> Vec<ApSummary> {
    struct Acc {
        ssid: String,
        channel: u16,
        band: Band,
        max: i32,
        sum: i64,
        n: usize,
    }
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    for p in &project.points {
        for r in &p.readings {
            let key = normalize_bssid(&r.bssid);
            let a = map.entry(key).or_insert(Acc {
                ssid: r.ssid.clone(),
                channel: r.channel,
                band: r.band,
                max: r.rssi_dbm,
                sum: 0,
                n: 0,
            });
            a.max = a.max.max(r.rssi_dbm);
            a.sum += r.rssi_dbm as i64;
            a.n += 1;
            if a.ssid.is_empty() {
                a.ssid = r.ssid.clone();
            }
        }
    }
    let mut out: Vec<ApSummary> = map
        .into_iter()
        .map(|(bssid, a)| ApSummary {
            name: project.display_name(&bssid, &a.ssid),
            bssid,
            ssid: a.ssid,
            channel: a.channel,
            band: a.band,
            max_rssi_dbm: a.max,
            mean_rssi_dbm: a.sum as f64 / a.n as f64,
            samples: a.n,
        })
        .collect();
    out.sort_by(|a, b| b.max_rssi_dbm.cmp(&a.max_rssi_dbm));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterferenceKind {
    CoChannel,
    Adjacent,
}

/// How much two channels on the same band step on each other, 0.0 (none) to 1.0 (identical).
/// 2.4 GHz channels are 5 MHz apart but ~20 MHz wide, so channels within 4 overlap;
/// 5/6 GHz 20 MHz channels only interfere when identical.
pub fn channel_overlap(band: Band, a: u16, b: u16) -> f64 {
    if a == b {
        return 1.0;
    }
    match band {
        Band::Ghz2_4 => {
            let d = a.abs_diff(b);
            if d < 5 { 1.0 - d as f64 / 5.0 } else { 0.0 }
        }
        Band::Ghz5 | Band::Ghz6 => 0.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interference {
    pub a: String,
    pub b: String,
    pub band: Band,
    pub kind: InterferenceKind,
    pub overlap: f64,
    /// The weaker of the two APs' peak signals: both must be heard to matter.
    pub weaker_peak_dbm: i32,
}

/// Pairs of APs that contend with each other. Only APs whose peak signal is at or
/// above `min_rssi_dbm` are considered (typically about -85). BSSIDs that share their
/// first five octets are treated as the same radio (multi-SSID) and skipped.
pub fn interference(aps: &[ApSummary], min_rssi_dbm: i32) -> Vec<Interference> {
    let mut out = Vec::new();
    for (i, a) in aps.iter().enumerate() {
        for b in &aps[i + 1..] {
            if a.band != b.band
                || a.max_rssi_dbm < min_rssi_dbm
                || b.max_rssi_dbm < min_rssi_dbm
                || same_radio(&a.bssid, &b.bssid)
            {
                continue;
            }
            let overlap = channel_overlap(a.band, a.channel, b.channel);
            if overlap <= 0.0 {
                continue;
            }
            out.push(Interference {
                a: a.name.clone(),
                b: b.name.clone(),
                band: a.band,
                kind: if a.channel == b.channel { InterferenceKind::CoChannel } else { InterferenceKind::Adjacent },
                overlap,
                weaker_peak_dbm: a.max_rssi_dbm.min(b.max_rssi_dbm),
            });
        }
    }
    out.sort_by(|x, y| y.weaker_peak_dbm.cmp(&x.weaker_peak_dbm));
    out
}

fn same_radio(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_bssid(a), normalize_bssid(b));
    a.len() >= 14 && b.len() >= 14 && a[..14] == b[..14]
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelScore {
    pub channel: u16,
    /// Total weighted interference power on this channel (dBm); lower is better.
    pub interference_dbm: f64,
    pub dfs: bool,
}

const NOISE_FLOOR_DBM: f64 = -100.0;
const CH_5GHZ: [u16; 25] = [
    36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161,
    165,
];

fn candidates(band: Band) -> Vec<(u16, bool)> {
    match band {
        Band::Ghz2_4 => vec![(1, false), (6, false), (11, false)],
        Band::Ghz5 => CH_5GHZ.iter().map(|&c| (c, (52..=144).contains(&c))).collect(),
        // Preferred scanning channels: every 16th starting at 5.
        Band::Ghz6 => (5..=229).step_by(16).map(|c| (c as u16, false)).collect(),
    }
}

/// Ranks candidate channels (20 MHz assumed) on `band`, best first. APs whose BSSID is in
/// `own_bssids` are excluded, so you can ask "where should my AP go?" without it
/// counting against itself.
pub fn suggest_channels(aps: &[ApSummary], band: Band, own_bssids: &[String], include_dfs: bool) -> Vec<ChannelScore> {
    let own: Vec<String> = own_bssids.iter().map(|b| normalize_bssid(b)).collect();
    let neighbors: Vec<&ApSummary> = aps
        .iter()
        .filter(|a| a.band == band && !own.contains(&normalize_bssid(&a.bssid)))
        .collect();

    let mut scores: Vec<ChannelScore> = candidates(band)
        .into_iter()
        .filter(|(_, dfs)| include_dfs || !dfs)
        .map(|(channel, dfs)| {
            let mw: f64 = neighbors
                .iter()
                .map(|a| channel_overlap(band, channel, a.channel) * 10f64.powf(a.max_rssi_dbm as f64 / 10.0))
                .sum();
            let interference_dbm = if mw > 0.0 { (10.0 * mw.log10()).max(NOISE_FLOOR_DBM) } else { NOISE_FLOOR_DBM };
            ChannelScore { channel, interference_dbm, dfs }
        })
        .collect();
    scores.sort_by(|a, b| a.interference_dbm.total_cmp(&b.interference_dbm).then(a.channel.cmp(&b.channel)));
    scores
}

/// Guesses where an AP is: the average of the survey points that heard it strongest, weighted
/// by received power in mW. Needs at least one reading; a rough guess that you can drag-correct
/// by placing the AP by hand. Plan pixels.
pub fn estimate_ap_position(project: &Project, bssid: &str) -> Option<(f64, f64)> {
    let key = normalize_bssid(bssid);
    let mut heard: Vec<(f64, f64, i32)> = project
        .points
        .iter()
        .filter_map(|p| p.readings.iter().find(|r| normalize_bssid(&r.bssid) == key).map(|r| (p.x, p.y, r.rssi_dbm)))
        .collect();
    heard.sort_by_key(|h| std::cmp::Reverse(h.2));
    heard.truncate(3);
    let weights: Vec<f64> = heard.iter().map(|h| 10f64.powf(h.2 as f64 / 10.0)).collect();
    let total: f64 = weights.iter().sum();
    if heard.is_empty() || total <= 0.0 {
        return None;
    }
    let x = heard.iter().zip(&weights).map(|(h, w)| h.0 * w).sum::<f64>() / total;
    let y = heard.iter().zip(&weights).map(|(h, w)| h.1 * w).sum::<f64>() / total;
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ApReading;

    fn grid(cols: usize, rows: usize, vals: Vec<Option<f64>>) -> Grid {
        Grid { cols, rows, cell_px: 10.0, values: vals }
    }

    #[test]
    fn estimate_leans_toward_the_strongest_readings() {
        let mut p = Project::new("t");
        let r = |dbm| ApReading { bssid: "aa:bb:cc:dd:ee:01".into(), ssid: "x".into(), rssi_dbm: dbm, channel: 1, band: Band::Ghz2_4 };
        p.add_point(0.0, 0.0, vec![r(-40)]);
        p.add_point(100.0, 0.0, vec![r(-70)]);
        let (x, y) = estimate_ap_position(&p, "AA:BB:CC:DD:EE:01").unwrap();
        assert!(x < 5.0 && y == 0.0, "x = {x}");
        assert!(estimate_ap_position(&p, "11:22:33:44:55:66").is_none());
    }

    #[test]
    fn coverage_counts_only_surveyed_cells_in_the_denominator() {
        let g = grid(2, 2, vec![Some(-50.0), Some(-80.0), None, Some(-60.0)]);
        let c = coverage(&g, -67.0);
        assert_eq!((c.total_cells, c.surveyed_cells, c.covered_cells), (4, 3, 2));
        assert!((c.pct_of_surveyed() - 66.666).abs() < 0.01);
        assert!((c.pct_of_plan() - 50.0).abs() < 1e-9);
        assert_eq!(coverage(&grid(1, 1, vec![None]), -67.0).pct_of_surveyed(), 0.0);
    }

    #[test]
    fn dead_zones_are_found_sorted_and_filtered() {
        // W = weak(-80), G = good(-50), . = no data
        // W W G W .
        // W G G W W
        let w = Some(-80.0);
        let gd = Some(-50.0);
        let g = grid(5, 2, vec![w, w, gd, w, None, w, gd, gd, w, w]);
        let z = dead_zones(&g, -67.0, 1);
        assert_eq!(z.len(), 2);
        assert_eq!(z[0].cells, 3); // right-hand blob
        assert_eq!(z[1].cells, 3);
        assert_eq!(dead_zones(&g, -67.0, 4).len(), 0);
        let left = z.iter().find(|d| d.centroid.0 < 20.0).unwrap();
        assert_eq!(left.cells, 3);
        assert_eq!(left.worst_value, -80.0);
        assert_eq!(left.area_px2, 300.0);
    }

    #[test]
    fn unsurveyed_cells_are_not_dead_zones() {
        let g = grid(2, 1, vec![None, None]);
        assert!(dead_zones(&g, -67.0, 1).is_empty());
    }

    #[test]
    fn overlap_rules() {
        assert_eq!(channel_overlap(Band::Ghz2_4, 6, 6), 1.0);
        assert!((channel_overlap(Band::Ghz2_4, 1, 3) - 0.6).abs() < 1e-9);
        assert_eq!(channel_overlap(Band::Ghz2_4, 1, 6), 0.0);
        assert_eq!(channel_overlap(Band::Ghz5, 36, 40), 0.0);
        assert_eq!(channel_overlap(Band::Ghz5, 36, 36), 1.0);
    }

    fn proj(aps: &[(&str, u16, i32)]) -> Project {
        let mut p = Project::new("t");
        let readings = aps
            .iter()
            .map(|(b, ch, rssi)| ApReading {
                bssid: (*b).into(),
                ssid: format!("ssid-{b}"),
                rssi_dbm: *rssi,
                channel: *ch,
                band: Band::guess_from_channel(*ch),
            })
            .collect();
        p.add_point(0.0, 0.0, readings);
        p
    }

    #[test]
    fn summarize_aggregates_and_sorts() {
        let mut p = proj(&[("aa:00:00:00:00:01", 1, -70), ("aa:00:00:00:00:02", 6, -50)]);
        p.add_point(
            5.0,
            5.0,
            vec![ApReading {
                bssid: "AA:00:00:00:00:01".into(),
                ssid: "x".into(),
                rssi_dbm: -60,
                channel: 1,
                band: Band::Ghz2_4,
            }],
        );
        p.set_alias("aa:00:00:00:00:01", "Lobby");
        let s = summarize_aps(&p);
        assert_eq!(s[0].bssid, "aa:00:00:00:00:02");
        let lobby = &s[1];
        assert_eq!((lobby.max_rssi_dbm, lobby.samples), (-60, 2));
        assert!((lobby.mean_rssi_dbm + 65.0).abs() < 1e-9);
        assert_eq!(lobby.name, "Lobby");
    }

    #[test]
    fn interference_detects_co_and_adjacent_and_skips_same_radio() {
        let p = proj(&[
            ("aa:00:00:00:00:01", 1, -50),
            ("bb:00:00:00:00:01", 1, -60),
            ("cc:00:00:00:00:01", 3, -55),
            ("dd:00:00:00:00:01", 11, -40),
            ("aa:00:00:00:00:02", 1, -52), // same radio as the first AP
            ("ee:00:00:00:00:01", 1, -95), // too weak to matter
            ("ff:00:00:00:00:01", 36, -40),
            ("ff:00:00:00:00:02", 40, -40), // 5 GHz neighbours: no overlap
        ]);
        let aps = summarize_aps(&p);
        let i = interference(&aps, -85);
        let co = i.iter().filter(|x| x.kind == InterferenceKind::CoChannel).count();
        let adj = i.iter().filter(|x| x.kind == InterferenceKind::Adjacent).count();
        // co-channel: aa:01<->bb:01 and aa:02<->bb:01 (but NOT aa:01<->aa:02, same radio)
        assert_eq!(co, 2);
        // adjacent: cc (ch3) vs aa:01, aa:02 and bb:01 (all ch1)
        assert_eq!(adj, 3);
        assert!(!i.iter().any(|x| x.a.contains("aa:00:00:00:00:0") && x.b.contains("aa:00:00:00:00:0")));
        assert!(i.iter().all(|x| x.band == Band::Ghz2_4));
    }

    #[test]
    fn suggests_the_quiet_channel() {
        let p = proj(&[
            ("aa:00:00:00:00:01", 1, -45),
            ("bb:00:00:00:00:01", 6, -50),
            ("cc:00:00:00:00:01", 6, -60),
            ("dd:00:00:00:00:01", 36, -50),
        ]);
        let aps = summarize_aps(&p);
        let s = suggest_channels(&aps, Band::Ghz2_4, &[], false);
        assert_eq!(s[0].channel, 11);
        assert_eq!(s.last().unwrap().channel, 1); // loudest neighbor sits there
        let s5 = suggest_channels(&aps, Band::Ghz5, &[], false);
        assert!(s5.iter().all(|c| !c.dfs));
        assert_ne!(s5[0].channel, 36);
        assert!(s5.iter().find(|c| c.channel == 36).unwrap().interference_dbm > -100.0);
        assert!(suggest_channels(&aps, Band::Ghz5, &[], true).iter().any(|c| c.dfs));
    }

    #[test]
    fn own_aps_are_excluded_from_scoring() {
        let p = proj(&[("aa:00:00:00:00:01", 1, -45)]);
        let aps = summarize_aps(&p);
        let with = suggest_channels(&aps, Band::Ghz2_4, &[], false);
        let without = suggest_channels(&aps, Band::Ghz2_4, &["AA:00:00:00:00:01".into()], false);
        assert_eq!(with.last().unwrap().channel, 1);
        assert!(without.iter().all(|c| c.interference_dbm == -100.0));
    }
}
