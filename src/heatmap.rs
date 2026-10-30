use crate::calibration::Scale;
use crate::model::{normalize_bssid, Project};

/// What a heatmap visualizes.
#[derive(Debug, Clone, PartialEq)]
pub enum Metric {
    /// Strongest AP heard at each point (dBm).
    Signal,
    /// Strongest AP minus the project's noise floor (dB).
    Snr,
    /// Downlink iperf3 throughput (Mbps).
    Throughput,
    /// Signal from one specific AP (BSSID). Points that didn't hear it are skipped.
    Ap(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub x: f64,
    pub y: f64,
    pub value: f64,
}

pub fn samples(project: &Project, metric: &Metric) -> Vec<Sample> {
    let ap = match metric {
        Metric::Ap(b) => Some(normalize_bssid(b)),
        _ => None,
    };
    project
        .points
        .iter()
        .filter_map(|p| {
            let value = match metric {
                Metric::Signal => p.strongest()?.rssi_dbm as f64,
                Metric::Snr => (p.strongest()?.rssi_dbm - project.noise_floor_dbm) as f64,
                Metric::Throughput => p.throughput?.down_mbps?,
                Metric::Ap(_) => p.reading_for(ap.as_deref()?)?.rssi_dbm as f64,
            };
            Some(Sample { x: p.x, y: p.y, value })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdwParams {
    /// Grid cell edge, in plan pixels.
    pub cell_px: f64,
    /// IDW exponent; 2 is the classic choice, higher makes peaks more local.
    pub power: f64,
    /// Cells farther than this from every sample stay empty instead of faking coverage.
    pub max_distance_px: f64,
}

impl IdwParams {
    pub fn from_meters(scale: &Scale, cell_m: f64, max_distance_m: f64) -> IdwParams {
        IdwParams {
            cell_px: scale.m_to_px(cell_m),
            power: 2.0,
            max_distance_px: scale.m_to_px(max_distance_m),
        }
    }
}

impl Default for IdwParams {
    fn default() -> Self {
        IdwParams { cell_px: 10.0, power: 2.0, max_distance_px: 200.0 }
    }
}

/// Row-major grid of interpolated values; `None` means "no data nearby".
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub cell_px: f64,
    pub values: Vec<Option<f64>>,
}

impl Grid {
    pub fn get(&self, col: usize, row: usize) -> Option<f64> {
        if col < self.cols && row < self.rows { self.values[row * self.cols + col] } else { None }
    }

    /// Value of the cell containing plan position (x, y) in pixels.
    pub fn value_at(&self, x: f64, y: f64) -> Option<f64> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        self.get((x / self.cell_px) as usize, (y / self.cell_px) as usize)
    }

    pub fn cell_center(&self, col: usize, row: usize) -> (f64, f64) {
        ((col as f64 + 0.5) * self.cell_px, (row as f64 + 0.5) * self.cell_px)
    }

    /// One RGBA pixel per cell; empty cells are fully transparent.
    pub fn to_rgba(&self, ramp: &Ramp) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.values.len() * 4);
        for v in &self.values {
            out.extend_from_slice(&v.map_or([0, 0, 0, 0], |v| ramp.color(v)));
        }
        out
    }
}

/// Interpolates `samples` over a `width_px` x `height_px` plan.
pub fn idw_grid(samples: &[Sample], width_px: f64, height_px: f64, params: &IdwParams) -> Grid {
    let cell = params.cell_px.max(1.0);
    let cols = (width_px / cell).ceil().max(1.0) as usize;
    let rows = (height_px / cell).ceil().max(1.0) as usize;
    let mut values = vec![None; cols * rows];
    let max_d2 = params.max_distance_px * params.max_distance_px;

    for row in 0..rows {
        for col in 0..cols {
            let cx = (col as f64 + 0.5) * cell;
            let cy = (row as f64 + 0.5) * cell;
            let (mut num, mut den, mut hit) = (0.0, 0.0, false);
            let mut exact = None;
            for s in samples {
                let d2 = (s.x - cx).powi(2) + (s.y - cy).powi(2);
                if d2 > max_d2 {
                    continue;
                }
                hit = true;
                if d2 < 1e-9 {
                    exact = Some(s.value);
                    break;
                }
                let w = 1.0 / d2.powf(params.power / 2.0);
                num += w * s.value;
                den += w;
            }
            if hit {
                values[row * cols + col] = exact.or(Some(num / den));
            }
        }
    }
    Grid { cols, rows, cell_px: cell, values }
}

/// Convenience: samples + interpolation sized to the project's floor plan.
/// Returns `None` if the project has no floor plan.
pub fn build(project: &Project, metric: &Metric, params: &IdwParams) -> Option<Grid> {
    let plan = project.floor_plan.as_ref()?;
    Some(idw_grid(&samples(project, metric), plan.width_px as f64, plan.height_px as f64, params))
}

/// Piecewise-linear color ramp over [min, max].
#[derive(Debug, Clone, PartialEq)]
pub struct Ramp {
    pub min: f64,
    pub max: f64,
    pub alpha: u8,
}

// Red -> orange -> yellow -> green -> teal: bad to good.
const STOPS: [[u8; 3]; 5] = [[215, 48, 39], [252, 141, 89], [254, 224, 80], [102, 189, 99], [26, 152, 160]];

impl Ramp {
    pub fn signal() -> Ramp {
        Ramp { min: -90.0, max: -40.0, alpha: 160 }
    }

    pub fn snr() -> Ramp {
        Ramp { min: 0.0, max: 50.0, alpha: 160 }
    }

    pub fn throughput(max_mbps: f64) -> Ramp {
        Ramp { min: 0.0, max: max_mbps.max(1.0), alpha: 160 }
    }

    pub fn color(&self, v: f64) -> [u8; 4] {
        let t = ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        let pos = t * (STOPS.len() - 1) as f64;
        let i = (pos.floor() as usize).min(STOPS.len() - 2);
        let f = pos - i as f64;
        let mix = |c: usize| (STOPS[i][c] as f64 * (1.0 - f) + STOPS[i + 1][c] as f64 * f).round() as u8;
        [mix(0), mix(1), mix(2), self.alpha]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ApReading, Band, FloorPlan, Throughput};

    fn s(x: f64, y: f64, value: f64) -> Sample {
        Sample { x, y, value }
    }

    #[test]
    fn single_sample_fills_only_within_cutoff() {
        let p = IdwParams { cell_px: 10.0, power: 2.0, max_distance_px: 30.0 };
        let g = idw_grid(&[s(15.0, 15.0, -50.0)], 100.0, 100.0, &p);
        assert_eq!(g.value_at(15.0, 15.0), Some(-50.0));
        assert_eq!(g.value_at(35.0, 15.0), Some(-50.0)); // 20 px away, inside cutoff
        assert_eq!(g.value_at(95.0, 95.0), None); // far away: no fake coverage
    }

    #[test]
    fn midpoint_of_two_equal_distance_samples_is_the_mean() {
        let p = IdwParams { cell_px: 10.0, power: 2.0, max_distance_px: 200.0 };
        let g = idw_grid(&[s(5.0, 5.0, -40.0), s(45.0, 5.0, -80.0)], 50.0, 10.0, &p);
        let mid = g.value_at(25.0, 5.0).unwrap();
        assert!((mid + 60.0).abs() < 1e-9, "got {mid}");
    }

    #[test]
    fn interpolated_values_stay_between_sample_extremes() {
        let p = IdwParams::default();
        let g = idw_grid(&[s(20.0, 20.0, -45.0), s(180.0, 20.0, -85.0)], 200.0, 40.0, &p);
        for v in g.values.iter().flatten() {
            assert!((-85.0..=-45.0).contains(v));
        }
    }

    #[test]
    fn grid_dimensions_round_up() {
        let g = idw_grid(&[], 105.0, 31.0, &IdwParams::default());
        assert_eq!((g.cols, g.rows), (11, 4));
        assert!(g.values.iter().all(Option::is_none));
    }

    fn rd(bssid: &str, rssi: i32) -> ApReading {
        ApReading { bssid: bssid.into(), ssid: "n".into(), rssi_dbm: rssi, channel: 36, band: Band::Ghz5 }
    }

    #[test]
    fn metrics_pick_the_right_values() {
        let mut p = Project::new("t");
        p.floor_plan = Some(FloorPlan { image_path: "x".into(), width_px: 100, height_px: 100 });
        p.add_point(10.0, 10.0, vec![rd("aa:00:00:00:00:01", -60), rd("aa:00:00:00:00:02", -50)]);
        p.add_point(50.0, 50.0, vec![rd("aa:00:00:00:00:01", -70)]);
        p.points[1].throughput = Some(Throughput { down_mbps: Some(200.0), up_mbps: Some(100.0) });

        let sig = samples(&p, &Metric::Signal);
        assert_eq!(sig.iter().map(|s| s.value).collect::<Vec<_>>(), vec![-50.0, -70.0]);
        let snr = samples(&p, &Metric::Snr);
        assert_eq!(snr[0].value, 45.0); // -50 - (-95)
        let thr = samples(&p, &Metric::Throughput);
        assert_eq!(thr.len(), 1);
        assert_eq!(thr[0].value, 200.0);
        let ap2 = samples(&p, &Metric::Ap("AA:00:00:00:00:02".into()));
        assert_eq!(ap2.len(), 1); // second point never heard AP 02
        assert_eq!(ap2[0].value, -50.0);
        assert!(build(&p, &Metric::Signal, &IdwParams::default()).is_some());
        p.floor_plan = None;
        assert!(build(&p, &Metric::Signal, &IdwParams::default()).is_none());
    }

    #[test]
    fn ramp_is_monotone_in_hue_and_clamps() {
        let r = Ramp::signal();
        assert_eq!(r.color(-200.0)[..3], STOPS[0]);
        assert_eq!(r.color(0.0)[..3], STOPS[4]);
        let mid = r.color(-65.0);
        assert_eq!(mid[..3], STOPS[2]);
    }

    #[test]
    fn rgba_has_transparent_empty_cells() {
        let g = Grid { cols: 2, rows: 1, cell_px: 10.0, values: vec![None, Some(-50.0)] };
        let px = g.to_rgba(&Ramp::signal());
        assert_eq!(&px[0..4], &[0, 0, 0, 0]);
        assert_eq!(px[7], 160);
    }
}
