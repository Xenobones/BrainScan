use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::calibration::Scale;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Band {
    #[serde(rename = "2.4GHz")]
    Ghz2_4,
    #[serde(rename = "5GHz")]
    Ghz5,
    #[serde(rename = "6GHz")]
    Ghz6,
}

impl Band {
    pub fn from_freq_mhz(mhz: u32) -> Option<Band> {
        match mhz {
            2400..=2500 => Some(Band::Ghz2_4),
            5150..=5924 => Some(Band::Ghz5),
            5925..=7125 => Some(Band::Ghz6),
            _ => None,
        }
    }

    /// Best guess when only a channel number is known. 6 GHz shares channel
    /// numbers with 5 GHz, so callers should prefer frequency or an explicit band.
    pub fn guess_from_channel(channel: u16) -> Band {
        if channel <= 14 { Band::Ghz2_4 } else { Band::Ghz5 }
    }

    pub fn label(self) -> &'static str {
        match self {
            Band::Ghz2_4 => "2.4 GHz",
            Band::Ghz5 => "5 GHz",
            Band::Ghz6 => "6 GHz",
        }
    }
}

/// Display units for distances. Everything is stored in meters; this only changes what the
/// user sees and types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Units {
    #[default]
    Meters,
    Feet,
}

const FEET_PER_METER: f64 = 3.280_839_895;

impl Units {
    /// Units per meter (1 for meters, ~3.28 for feet).
    pub fn per_meter(self) -> f64 {
        match self {
            Units::Meters => 1.0,
            Units::Feet => FEET_PER_METER,
        }
    }

    pub fn from_m(self, meters: f64) -> f64 {
        meters * self.per_meter()
    }

    pub fn to_m(self, value: f64) -> f64 {
        value / self.per_meter()
    }

    pub fn label(self) -> &'static str {
        match self {
            Units::Meters => "m",
            Units::Feet => "ft",
        }
    }

    pub fn area_label(self) -> &'static str {
        match self {
            Units::Meters => "m²",
            Units::Feet => "ft²",
        }
    }
}

/// Lowercase, colon-separated MAC form so BSSIDs compare reliably across scanners.
pub fn normalize_bssid(s: &str) -> String {
    s.trim().replace('-', ":").to_ascii_lowercase()
}

/// One access point heard at one survey point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApReading {
    pub bssid: String,
    pub ssid: String,
    pub rssi_dbm: i32,
    pub channel: u16,
    pub band: Band,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Throughput {
    pub down_mbps: Option<f64>,
    pub up_mbps: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurveyPoint {
    pub id: u32,
    /// Position on the floor plan, in image pixels.
    pub x: f64,
    pub y: f64,
    pub readings: Vec<ApReading>,
    #[serde(default)]
    pub throughput: Option<Throughput>,
}

impl SurveyPoint {
    pub fn strongest(&self) -> Option<&ApReading> {
        self.readings.iter().max_by_key(|r| r.rssi_dbm)
    }

    pub fn reading_for(&self, bssid: &str) -> Option<&ApReading> {
        let b = normalize_bssid(bssid);
        self.readings.iter().find(|r| normalize_bssid(&r.bssid) == b)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloorPlan {
    pub image_path: String,
    pub width_px: u32,
    pub height_px: u32,
}

fn default_noise_floor() -> i32 {
    -95
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub floor_plan: Option<FloorPlan>,
    #[serde(default)]
    pub scale: Option<Scale>,
    #[serde(default)]
    pub points: Vec<SurveyPoint>,
    /// BSSID (normalized) -> friendly name, e.g. "Lobby AP".
    #[serde(default)]
    pub ap_aliases: BTreeMap<String, String>,
    /// Used for SNR when the scanner doesn't report noise.
    #[serde(default = "default_noise_floor")]
    pub noise_floor_dbm: i32,
    #[serde(default)]
    pub units: Units,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Project {
        Project {
            name: name.into(),
            floor_plan: None,
            scale: None,
            points: Vec::new(),
            ap_aliases: BTreeMap::new(),
            noise_floor_dbm: default_noise_floor(),
            units: Units::default(),
        }
    }

    /// Adds a survey point and returns its id.
    pub fn add_point(&mut self, x: f64, y: f64, readings: Vec<ApReading>) -> u32 {
        let id = self.points.iter().map(|p| p.id).max().map_or(1, |m| m + 1);
        self.points.push(SurveyPoint { id, x, y, readings, throughput: None });
        id
    }

    pub fn remove_point(&mut self, id: u32) -> bool {
        let before = self.points.len();
        self.points.retain(|p| p.id != id);
        self.points.len() != before
    }

    pub fn set_alias(&mut self, bssid: &str, alias: impl Into<String>) {
        self.ap_aliases.insert(normalize_bssid(bssid), alias.into());
    }

    pub fn alias_for(&self, bssid: &str) -> Option<&str> {
        self.ap_aliases.get(&normalize_bssid(bssid)).map(String::as_str)
    }

    /// Alias if set, otherwise the SSID, otherwise the BSSID.
    pub fn display_name(&self, bssid: &str, ssid: &str) -> String {
        match self.alias_for(bssid) {
            Some(a) => a.to_string(),
            None if !ssid.is_empty() => ssid.to_string(),
            None => bssid.to_string(),
        }
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Project> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_json()?)?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Project> {
        Project::from_json(&std::fs::read_to_string(path)?)
    }

    /// One row per (survey point, AP heard). Points that heard nothing still get a row.
    pub fn to_csv(&self) -> String {
        let mut out = String::from(
            "point_id,x_px,y_px,x_m,y_m,bssid,ssid,alias,channel,band,rssi_dbm,down_mbps,up_mbps\n",
        );
        for p in &self.points {
            let (xm, ym) = match &self.scale {
                Some(s) => (fmt_opt(Some(s.px_to_m(p.x))), fmt_opt(Some(s.px_to_m(p.y)))),
                None => (String::new(), String::new()),
            };
            let (down, up) = match &p.throughput {
                Some(t) => (fmt_opt(t.down_mbps), fmt_opt(t.up_mbps)),
                None => (String::new(), String::new()),
            };
            let head = format!("{},{:.2},{:.2},{},{}", p.id, p.x, p.y, xm, ym);
            if p.readings.is_empty() {
                let _ = writeln!(out, "{head},,,,,,,{down},{up}");
            }
            for r in &p.readings {
                let _ = writeln!(
                    out,
                    "{head},{},{},{},{},{},{},{down},{up}",
                    csv_escape(&r.bssid),
                    csv_escape(&r.ssid),
                    csv_escape(self.alias_for(&r.bssid).unwrap_or("")),
                    r.channel,
                    r.band.label(),
                    r.rssi_dbm,
                );
            }
        }
        out
    }
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_default()
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(bssid: &str, ssid: &str, rssi: i32, ch: u16) -> ApReading {
        ApReading {
            bssid: bssid.into(),
            ssid: ssid.into(),
            rssi_dbm: rssi,
            channel: ch,
            band: Band::guess_from_channel(ch),
        }
    }

    #[test]
    fn json_round_trip() {
        let mut p = Project::new("Office");
        p.scale = Some(Scale { pixels_per_meter: 42.0 });
        let id = p.add_point(10.0, 20.0, vec![reading("aa:bb:cc:dd:ee:01", "Corp", -55, 36)]);
        p.points[0].throughput = Some(Throughput { down_mbps: Some(300.0), up_mbps: None });
        p.set_alias("AA-BB-CC-DD-EE-01", "Lobby");
        assert_eq!(id, 1);
        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.alias_for("aa:bb:cc:dd:ee:01"), Some("Lobby"));
    }

    #[test]
    fn old_files_missing_optional_fields_still_load() {
        let p = Project::from_json(r#"{"name":"x"}"#).unwrap();
        assert!(p.points.is_empty());
        assert_eq!(p.noise_floor_dbm, -95);
    }

    #[test]
    fn ids_increment_and_removal_works() {
        let mut p = Project::new("t");
        let a = p.add_point(0.0, 0.0, vec![]);
        let b = p.add_point(1.0, 1.0, vec![]);
        assert_eq!((a, b), (1, 2));
        assert!(p.remove_point(a));
        assert!(!p.remove_point(a));
        assert_eq!(p.add_point(2.0, 2.0, vec![]), 3);
    }

    #[test]
    fn csv_escapes_and_covers_empty_points() {
        let mut p = Project::new("t");
        p.scale = Some(Scale { pixels_per_meter: 10.0 });
        p.add_point(100.0, 50.0, vec![reading("aa:bb:cc:dd:ee:01", "My, \"Net\"", -60, 6)]);
        p.add_point(0.0, 0.0, vec![]);
        let csv = p.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains("\"My, \"\"Net\"\"\""));
        assert!(lines[1].contains(",10.00,5.00,"));
        assert!(lines[2].starts_with("2,0.00,0.00,0.00,0.00,"));
    }

    #[test]
    fn strongest_and_lookup() {
        let mut p = Project::new("t");
        p.add_point(
            0.0,
            0.0,
            vec![reading("aa:aa:aa:aa:aa:01", "A", -70, 1), reading("aa:aa:aa:aa:aa:02", "B", -50, 6)],
        );
        let pt = &p.points[0];
        assert_eq!(pt.strongest().unwrap().ssid, "B");
        assert_eq!(pt.reading_for("AA-AA-AA-AA-AA-01").unwrap().rssi_dbm, -70);
        assert!(pt.reading_for("00:00:00:00:00:00").is_none());
    }

    #[test]
    fn unit_conversion_round_trips() {
        assert!((Units::Feet.from_m(1.0) - 3.2808).abs() < 1e-4);
        assert!((Units::Feet.to_m(Units::Feet.from_m(12.5)) - 12.5).abs() < 1e-9);
        assert_eq!(Units::Meters.from_m(7.0), 7.0);
        let mut p = Project::new("t");
        p.units = Units::Feet;
        assert_eq!(Project::from_json(&p.to_json().unwrap()).unwrap().units, Units::Feet);
    }

    #[test]
    fn band_detection() {
        assert_eq!(Band::from_freq_mhz(2437), Some(Band::Ghz2_4));
        assert_eq!(Band::from_freq_mhz(5180), Some(Band::Ghz5));
        assert_eq!(Band::from_freq_mhz(6115), Some(Band::Ghz6));
        assert_eq!(Band::from_freq_mhz(900), None);
    }
}
