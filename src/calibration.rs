use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Floor plan scale: how many image pixels make one meter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scale {
    pub pixels_per_meter: f64,
}

impl Scale {
    /// Calibrate from two clicked plan points (pixels) and the real distance between them.
    pub fn from_two_points(p1: (f64, f64), p2: (f64, f64), real_meters: f64) -> Result<Scale> {
        if !(real_meters.is_finite() && real_meters > 0.0) {
            return Err(Error::InvalidInput("real distance must be greater than zero".into()));
        }
        let px = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();
        if !(px.is_finite() && px > 0.0) {
            return Err(Error::InvalidInput("the two calibration points are identical".into()));
        }
        Ok(Scale { pixels_per_meter: px / real_meters })
    }

    pub fn px_to_m(&self, px: f64) -> f64 {
        px / self.pixels_per_meter
    }

    pub fn m_to_px(&self, m: f64) -> f64 {
        m * self.pixels_per_meter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrates_from_3_4_5_triangle() {
        let s = Scale::from_two_points((0.0, 0.0), (300.0, 400.0), 10.0).unwrap();
        assert!((s.pixels_per_meter - 50.0).abs() < 1e-9);
        assert!((s.px_to_m(125.0) - 2.5).abs() < 1e-9);
        assert!((s.m_to_px(2.5) - 125.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Scale::from_two_points((1.0, 1.0), (1.0, 1.0), 5.0).is_err());
        assert!(Scale::from_two_points((0.0, 0.0), (10.0, 0.0), 0.0).is_err());
        assert!(Scale::from_two_points((0.0, 0.0), (10.0, 0.0), -3.0).is_err());
        assert!(Scale::from_two_points((0.0, 0.0), (10.0, 0.0), f64::NAN).is_err());
    }
}
