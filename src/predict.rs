//! Predictive "what if" modelling: estimate signal everywhere from where the APs are, how far
//! the signal travels (log-distance path loss, fitted to the survey) and which walls it crosses.

use crate::calibration::Scale;
use crate::heatmap::Grid;
use crate::model::{normalize_bssid, Band, Project};
use crate::walls::{path_loss_db, Wall};

/// Path-loss exponent used when the survey can't pin one down.
pub const DEFAULT_EXPONENT: f64 = 3.0;

/// Typical RSSI one meter from an AP (20 dBm EIRP minus free-space loss at 1 m), for APs we
/// have no readings for, such as a candidate you add to the plan.
pub fn default_rssi_at_1m(band: Band) -> f64 {
    match band {
        Band::Ghz2_4 => -20.0,
        _ => -27.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PredAp {
    /// Empty for a candidate AP that doesn't exist yet.
    pub bssid: String,
    /// Plan pixels.
    pub pos: (f64, f64),
    pub band: Band,
    pub rssi_at_1m: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub exponent: f64,
    /// Placed APs that were heard in the survey, at their placed positions.
    pub aps: Vec<PredAp>,
    /// Survey readings the fit used (from placed APs).
    pub samples: usize,
    /// How far predictions are from the survey readings, dB RMS. `None` without enough data.
    pub rms_db: Option<f64>,
}

struct Sample {
    ap: usize,
    dist_m: f64,
    wall_db: f64,
    rssi: f64,
}

/// Fits the path-loss exponent and each placed AP's 1 m signal to the survey readings.
pub fn fit(project: &Project, scale: &Scale) -> Model {
    let mut aps: Vec<PredAp> = Vec::new();
    let mut samples: Vec<Sample> = Vec::new();
    for (bssid, &pos) in &project.ap_positions {
        let heard: Vec<_> = project
            .points
            .iter()
            .filter_map(|p| p.readings.iter().find(|r| normalize_bssid(&r.bssid) == *bssid).map(|r| (p, r)))
            .collect();
        let Some((_, first)) = heard.first() else { continue };
        let band = first.band;
        let idx = aps.len();
        aps.push(PredAp { bssid: bssid.clone(), pos, band, rssi_at_1m: default_rssi_at_1m(band) });
        for (p, r) in heard {
            samples.push(Sample {
                ap: idx,
                dist_m: scale.px_to_m(((p.x - pos.0).powi(2) + (p.y - pos.1).powi(2)).sqrt()).max(1.0),
                wall_db: path_loss_db(&project.walls, pos, (p.x, p.y), band),
                rssi: r.rssi_dbm as f64,
            });
        }
    }

    // For a given exponent each AP's best 1 m level is the mean of its implied levels.
    let intercepts = |n: f64| -> Vec<Option<f64>> {
        (0..aps.len())
            .map(|i| {
                let v: Vec<f64> = samples.iter().filter(|s| s.ap == i).map(|s| s.rssi + 10.0 * n * s.dist_m.log10() + s.wall_db).collect();
                (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
            })
            .collect()
    };
    let sse = |n: f64| -> (f64, usize) {
        let a = intercepts(n);
        let mut total = 0.0;
        let mut count = 0;
        for i in 0..aps.len() {
            let mine: Vec<&Sample> = samples.iter().filter(|s| s.ap == i).collect();
            if mine.len() < 2 {
                continue; // a single reading fits any exponent
            }
            for s in mine {
                let pred = a[i].unwrap() - 10.0 * n * s.dist_m.log10() - s.wall_db;
                total += (pred - s.rssi).powi(2);
                count += 1;
            }
        }
        (total, count)
    };

    let mut exponent = DEFAULT_EXPONENT;
    let mut best = f64::INFINITY;
    if sse(DEFAULT_EXPONENT).1 > 0 {
        for step in 0..=35 {
            let n = 2.0 + step as f64 * 0.1;
            let (e, _) = sse(n);
            if e < best - 1e-9 {
                best = e;
                exponent = n;
            }
        }
    }
    let levels = intercepts(exponent);
    let (e, count) = sse(exponent);
    for (ap, a) in aps.iter_mut().zip(levels) {
        if let Some(a) = a {
            ap.rssi_at_1m = a;
        }
    }
    Model { exponent, aps, samples: samples.len(), rms_db: (count > 0).then(|| (e / count as f64).sqrt()) }
}

/// Strongest predicted signal at `p` from the APs on `band`, or `None` if there are none.
pub fn predict_at(aps: &[PredAp], walls: &[Wall], scale: &Scale, exponent: f64, band: Band, p: (f64, f64)) -> Option<f64> {
    aps.iter()
        .filter(|a| a.band == band)
        .map(|a| {
            let d = scale.px_to_m(((p.0 - a.pos.0).powi(2) + (p.1 - a.pos.1).powi(2)).sqrt()).max(1.0);
            a.rssi_at_1m - 10.0 * exponent * d.log10() - path_loss_db(walls, a.pos, p, band)
        })
        .max_by(|a, b| a.total_cmp(b))
}

/// Predicted best-signal grid over the whole plan.
pub fn predict_grid(project: &Project, scale: &Scale, aps: &[PredAp], exponent: f64, band: Band, cell_px: f64) -> Option<Grid> {
    let plan = project.floor_plan.as_ref()?;
    let cell = cell_px.max(1.0);
    let cols = (plan.width_px as f64 / cell).ceil().max(1.0) as usize;
    let rows = (plan.height_px as f64 / cell).ceil().max(1.0) as usize;
    let mut values = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let p = ((c as f64 + 0.5) * cell, (r as f64 + 0.5) * cell);
            values.push(predict_at(aps, &project.walls, scale, exponent, band, p));
        }
    }
    Some(Grid { cols, rows, cell_px: cell, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::coverage;
    use crate::model::{ApReading, FloorPlan};
    use crate::walls::Material;

    const SCALE: Scale = Scale { pixels_per_meter: 10.0 };
    const BSSID: &str = "aa:bb:cc:00:00:01";

    fn project(ap: (f64, f64), n: f64, a1: f64) -> Project {
        let mut p = Project::new("t");
        p.floor_plan = Some(FloorPlan { image_path: String::new(), width_px: 600, height_px: 300 });
        p.scale = Some(SCALE);
        p.set_ap_position(BSSID, ap);
        for &(x, y) in &[(150.0, 50.0), (250.0, 100.0), (400.0, 200.0), (550.0, 250.0), (100.0, 280.0)] {
            let d = SCALE.px_to_m(((x - ap.0).powi(2) + (y - ap.1).powi(2)).sqrt()).max(1.0);
            let rssi = (a1 - 10.0 * n * d.log10()).round() as i32;
            p.add_point(x, y, vec![ApReading { bssid: BSSID.into(), ssid: "x".into(), rssi_dbm: rssi, channel: 36, band: Band::Ghz5 }]);
        }
        p
    }

    #[test]
    fn fit_recovers_exponent_and_level() {
        let p = project((50.0, 50.0), 3.4, -30.0);
        let m = fit(&p, &SCALE);
        assert!((m.exponent - 3.4).abs() <= 0.2, "n = {}", m.exponent);
        assert!((m.aps[0].rssi_at_1m + 30.0).abs() < 3.0, "a = {}", m.aps[0].rssi_at_1m);
        assert!(m.rms_db.unwrap() < 2.0);
    }

    #[test]
    fn walls_weaken_the_prediction() {
        let mut p = project((50.0, 150.0), 3.0, -30.0);
        let ap = PredAp { bssid: BSSID.into(), pos: (50.0, 150.0), band: Band::Ghz5, rssi_at_1m: -30.0 };
        let open = predict_at(&[ap.clone()], &p.walls, &SCALE, 3.0, Band::Ghz5, (300.0, 150.0)).unwrap();
        p.add_wall((200.0, 0.0), (200.0, 300.0), 4.0, Some(Material::Concrete));
        let blocked = predict_at(&[ap], &p.walls, &SCALE, 3.0, Band::Ghz5, (300.0, 150.0)).unwrap();
        assert!((open - blocked - 18.0).abs() < 1e-6, "{open} vs {blocked}");
    }

    #[test]
    fn other_bands_are_ignored() {
        let ap = PredAp { bssid: String::new(), pos: (0.0, 0.0), band: Band::Ghz5, rssi_at_1m: -30.0 };
        assert!(predict_at(&[ap], &[], &SCALE, 3.0, Band::Ghz2_4, (10.0, 10.0)).is_none());
    }

    #[test]
    fn moving_an_ap_to_the_middle_improves_coverage() {
        let p = project((20.0, 20.0), 3.0, -30.0);
        let corner = PredAp { bssid: BSSID.into(), pos: (20.0, 20.0), band: Band::Ghz5, rssi_at_1m: -30.0 };
        let center = PredAp { pos: (300.0, 150.0), ..corner.clone() };
        let cov = |ap: &PredAp| {
            let g = predict_grid(&p, &SCALE, std::slice::from_ref(ap), 3.0, Band::Ghz5, 10.0).unwrap();
            coverage(&g, -67.0).covered_cells
        };
        assert!(cov(&center) > cov(&corner));
    }
}
