use std::process::Command;

use crate::model::{normalize_bssid, ApReading, Band};
use crate::{Error, Result};

/// A source of Wi-Fi readings. `position_m` is the surveyor's position in meters on the
/// plan; real adapters ignore it, the mock uses it to simulate signal strength.
pub trait Scanner {
    fn scan(&mut self, position_m: Option<(f64, f64)>) -> Result<Vec<ApReading>>;
}

/// OS-appropriate real scanner.
pub fn default_scanner() -> Box<dyn Scanner + Send> {
    #[cfg(windows)]
    {
        Box::new(NetshScanner)
    }
    #[cfg(not(windows))]
    {
        // `iw` reports exact dBm and works on iwd-based distros (e.g. Omarchy) where
        // NetworkManager/nmcli may not be installed.
        if Command::new("iw").arg("--version").output().is_ok() {
            Box::new(IwScanner::default())
        } else {
            Box::new(NmcliScanner)
        }
    }
}

/// Both NetworkManager and Windows report signal *quality* (0-100). NetworkManager's
/// mapping is `quality = 2 * (dBm + 100)` clamped to 0..=100, so invert that.
/// Approximate (±a few dB); the native WLAN API would give exact dBm.
pub fn percent_to_dbm(percent: u32) -> i32 {
    (percent.min(100) as i32) / 2 - 100
}

// ---------------------------------------------------------------- Linux: nmcli

pub struct NmcliScanner;

impl Scanner for NmcliScanner {
    fn scan(&mut self, _: Option<(f64, f64)>) -> Result<Vec<ApReading>> {
        let out = run("nmcli", &["-t", "-f", "BSSID,SSID,CHAN,FREQ,SIGNAL", "dev", "wifi", "list", "--rescan", "yes"])?;
        parse_nmcli(&out)
    }
}

/// Splits an `nmcli -t` line on unescaped colons and undoes `\:` / `\\` escapes.
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n) => fields.last_mut().unwrap().push(n),
                None => fields.last_mut().unwrap().push('\\'),
            },
            ':' => fields.push(String::new()),
            _ => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// Parses `nmcli -t -f BSSID,SSID,CHAN,FREQ,SIGNAL dev wifi list` output.
pub fn parse_nmcli(output: &str) -> Result<Vec<ApReading>> {
    let mut out = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let f = split_terse(line);
        if f.len() != 5 {
            return Err(Error::Parse(format!("unexpected nmcli line: {line}")));
        }
        let channel: u16 = f[2].trim().parse().map_err(|_| Error::Parse(format!("bad channel in: {line}")))?;
        let freq: u32 = f[3].split_whitespace().next().unwrap_or("").parse().unwrap_or(0);
        let signal: u32 = f[4].trim().parse().map_err(|_| Error::Parse(format!("bad signal in: {line}")))?;
        out.push(ApReading {
            bssid: normalize_bssid(&f[0]),
            ssid: f[1].clone(),
            rssi_dbm: percent_to_dbm(signal),
            channel,
            band: Band::from_freq_mhz(freq).unwrap_or_else(|| Band::guess_from_channel(channel)),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------- Linux: iw

/// Uses `iw dev <if> scan` (exact dBm; needs root or CAP_NET_ADMIN to trigger a fresh
/// scan) and falls back to `scan dump`, the kernel's cached results, which any user can read.
#[derive(Default)]
pub struct IwScanner {
    /// Wi-Fi interface name; auto-detected from `iw dev` when `None`.
    pub interface: Option<String>,
}

impl Scanner for IwScanner {
    fn scan(&mut self, _: Option<(f64, f64)>) -> Result<Vec<ApReading>> {
        let iface = match &self.interface {
            Some(i) => i.clone(),
            None => {
                let i = parse_iw_interface(&run("iw", &["dev"])?)
                    .ok_or_else(|| Error::Command("no Wi-Fi interface found in `iw dev`".into()))?;
                self.interface = Some(i.clone());
                i
            }
        };
        let out = run("iw", &["dev", &iface, "scan"]).or_else(|_| run("iw", &["dev", &iface, "scan", "dump"]))?;
        parse_iw(&out)
    }
}

pub fn parse_iw_interface(output: &str) -> Option<String> {
    output.lines().find_map(|l| l.trim().strip_prefix("Interface ").map(|s| s.trim().to_string()))
}

/// Channel number for a center frequency in MHz.
pub fn freq_to_channel(mhz: u32) -> Option<u16> {
    match mhz {
        2412..=2472 => Some(((mhz - 2407) / 5) as u16),
        2484 => Some(14),
        5150..=5924 => Some(((mhz - 5000) / 5) as u16),
        5955..=7115 => Some(((mhz - 5950) / 5) as u16),
        _ => None,
    }
}

/// Parses `iw dev <if> scan` / `scan dump` output.
pub fn parse_iw(output: &str) -> Result<Vec<ApReading>> {
    #[derive(Default)]
    struct Cur {
        bssid: String,
        freq: Option<u32>,
        signal: Option<f64>,
        ssid: String,
    }
    fn flush(cur: Option<Cur>, out: &mut Vec<ApReading>) {
        if let Some(c) = cur {
            if let (Some(f), Some(sig)) = (c.freq, c.signal) {
                if let (Some(ch), Some(band)) = (freq_to_channel(f), Band::from_freq_mhz(f)) {
                    out.push(ApReading {
                        bssid: normalize_bssid(&c.bssid),
                        ssid: c.ssid,
                        rssi_dbm: sig.round() as i32,
                        channel: ch,
                        band,
                    });
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut cur: Option<Cur> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("BSS ") {
            flush(cur.take(), &mut out);
            let bssid: String = rest.chars().take_while(|c| c.is_ascii_hexdigit() || *c == ':').collect();
            cur = Some(Cur { bssid, ..Default::default() });
        } else if let Some(c) = cur.as_mut() {
            let l = line.trim();
            if let Some(v) = l.strip_prefix("freq:") {
                c.freq = v.trim().split('.').next().and_then(|n| n.parse().ok());
            } else if let Some(v) = l.strip_prefix("signal:") {
                c.signal = v.split_whitespace().next().and_then(|n| n.parse().ok());
            } else if let Some(v) = l.strip_prefix("SSID:") {
                c.ssid = v.trim().to_string();
            }
        }
    }
    flush(cur, &mut out);
    if out.is_empty() && output.contains("BSS ") {
        return Err(Error::Parse("iw output had BSS entries but none were usable".into()));
    }
    Ok(out)
}

// -------------------------------------------------------------- Windows: netsh

pub struct NetshScanner;

impl Scanner for NetshScanner {
    fn scan(&mut self, _: Option<(f64, f64)>) -> Result<Vec<ApReading>> {
        let out = run("netsh", &["wlan", "show", "networks", "mode=bssid"])?;
        parse_netsh(&out)
    }
}

/// Parses `netsh wlan show networks mode=bssid`. Unlike `show interfaces`, this lists every
/// AP in range, not just the connected one. Field labels are English; localized Windows
/// installs will need the native WLAN API instead.
pub fn parse_netsh(output: &str) -> Result<Vec<ApReading>> {
    #[derive(Default)]
    struct Cur {
        bssid: String,
        signal: Option<u32>,
        channel: Option<u16>,
        band: Option<Band>,
    }

    fn flush(cur: &mut Option<Cur>, ssid: &str, out: &mut Vec<ApReading>) {
        if let Some(c) = cur.take() {
            if let (Some(sig), Some(ch)) = (c.signal, c.channel) {
                out.push(ApReading {
                    bssid: normalize_bssid(&c.bssid),
                    ssid: ssid.to_string(),
                    rssi_dbm: percent_to_dbm(sig),
                    channel: ch,
                    band: c.band.unwrap_or_else(|| Band::guess_from_channel(ch)),
                });
            }
        }
    }

    let mut out = Vec::new();
    let mut ssid = String::new();
    let mut cur: Option<Cur> = None;
    let mut saw_any = false;

    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        let numbered = |prefix: &str| {
            key.strip_prefix(prefix)
                .is_some_and(|rest| rest.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit()))
        };
        if numbered("BSSID") {
            flush(&mut cur, &ssid, &mut out);
            cur = Some(Cur { bssid: value.to_string(), ..Default::default() });
            saw_any = true;
        } else if numbered("SSID") {
            flush(&mut cur, &ssid, &mut out);
            ssid = value.to_string();
            saw_any = true;
        } else if let Some(c) = cur.as_mut() {
            match key {
                "Signal" => c.signal = value.trim_end_matches('%').trim().parse().ok(),
                "Channel" => c.channel = value.parse().ok(),
                "Band" => {
                    c.band = match value.split_whitespace().next() {
                        Some("2.4") => Some(Band::Ghz2_4),
                        Some("5") => Some(Band::Ghz5),
                        Some("6") => Some(Band::Ghz6),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
    }
    flush(&mut cur, &ssid, &mut out);

    if !saw_any && !output.trim().is_empty() && !output.contains("0 networks") {
        return Err(Error::Parse("no SSID/BSSID entries found in netsh output".into()));
    }
    Ok(out)
}

fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::Command(format!("could not run {program}: {e}")))?;
    if !out.status.success() {
        return Err(Error::Command(format!(
            "{program} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ------------------------------------------------------------------------ Mock

#[derive(Debug, Clone)]
pub struct MockAp {
    pub bssid: String,
    pub ssid: String,
    pub x_m: f64,
    pub y_m: f64,
    pub channel: u16,
    pub band: Band,
    /// Transmit power plus antenna gain, dBm.
    pub tx_dbm: f64,
}

/// Simulates APs with a log-distance path-loss model plus repeatable shadowing,
/// for tests and demos without hardware.
pub struct MockScanner {
    pub aps: Vec<MockAp>,
    pub path_loss_exponent: f64,
    pub shadow_sigma_db: f64,
    /// Weakest signal the "adapter" can still report.
    pub sensitivity_dbm: f64,
    seed: u64,
}

impl MockScanner {
    pub fn new(aps: Vec<MockAp>, seed: u64) -> MockScanner {
        MockScanner { aps, path_loss_exponent: 3.0, shadow_sigma_db: 2.0, sensitivity_dbm: -92.0, seed }
    }

    /// Reference loss at 1 m: free space, 2.4 GHz ~40 dB, 5 GHz ~47 dB, 6 GHz ~49 dB.
    fn pl0(band: Band) -> f64 {
        match band {
            Band::Ghz2_4 => 40.0,
            Band::Ghz5 => 47.0,
            Band::Ghz6 => 49.0,
        }
    }

    /// Deterministic noise in [-1, 1] derived from position and AP index, so the same
    /// spot always reads the same value.
    fn shadow(&self, x: f64, y: f64, ap: usize) -> f64 {
        let mut h = self.seed ^ (x.to_bits().rotate_left(13)) ^ (y.to_bits().rotate_left(37)) ^ (ap as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        h ^= h >> 33;
        (h as f64 / u64::MAX as f64) * 2.0 - 1.0
    }

    pub fn rssi_at(&self, ap_index: usize, x: f64, y: f64) -> f64 {
        let ap = &self.aps[ap_index];
        let d = ((ap.x_m - x).powi(2) + (ap.y_m - y).powi(2)).sqrt().max(1.0);
        ap.tx_dbm - Self::pl0(ap.band) - 10.0 * self.path_loss_exponent * d.log10()
            + self.shadow(x, y, ap_index) * self.shadow_sigma_db
    }
}

impl Scanner for MockScanner {
    fn scan(&mut self, position_m: Option<(f64, f64)>) -> Result<Vec<ApReading>> {
        let (x, y) = position_m.ok_or_else(|| Error::InvalidInput("MockScanner needs a position".into()))?;
        Ok((0..self.aps.len())
            .filter_map(|i| {
                let rssi = self.rssi_at(i, x, y);
                (rssi >= self.sensitivity_dbm).then(|| {
                    let ap = &self.aps[i];
                    ApReading {
                        bssid: ap.bssid.clone(),
                        ssid: ap.ssid.clone(),
                        rssi_dbm: rssi.round() as i32,
                        channel: ap.channel,
                        band: ap.band,
                    }
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_conversion() {
        assert_eq!(percent_to_dbm(100), -50);
        assert_eq!(percent_to_dbm(0), -100);
        assert_eq!(percent_to_dbm(80), -60);
        assert_eq!(percent_to_dbm(250), -50);
    }

    const IW: &str = "\
BSS aa:bb:cc:dd:ee:01(on wlan0) -- associated
\tlast seen: 12.3s [boottime]
\tfreq: 5180.0
\tsignal: -52.00 dBm
\tSSID: Corp
BSS aa:bb:cc:dd:ee:02(on wlan0)
\tfreq: 2437
\tsignal: -71.50 dBm
\tSSID:
BSS aa:bb:cc:dd:ee:03(on wlan0)
\tfreq: 6115
\tsignal: -80.00 dBm
\tSSID: Six
";

    #[test]
    fn parses_iw_exact_dbm_and_channels() {
        let r = parse_iw(IW).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].rssi_dbm, r[0].channel, r[0].band), (-52, 36, Band::Ghz5));
        assert_eq!((r[1].rssi_dbm, r[1].channel, r[1].ssid.as_str()), (-72, 6, ""));
        assert_eq!((r[2].channel, r[2].band), (33, Band::Ghz6));
        assert_eq!(parse_iw("").unwrap().len(), 0);
        assert_eq!(parse_iw_interface("phy#0\n\tInterface wlan0\n\t\tifindex 3"), Some("wlan0".into()));
        assert_eq!(freq_to_channel(2484), Some(14));
    }

    #[test]
    fn parses_nmcli_with_escaped_colons_and_ssid_colon() {
        let out = "AA\\:BB\\:CC\\:DD\\:EE\\:01:Home\\:Net:36:5180 MHz:82\n\
                   AA\\:BB\\:CC\\:DD\\:EE\\:02::6:2437 MHz:40\n";
        let r = parse_nmcli(out).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].bssid, "aa:bb:cc:dd:ee:01");
        assert_eq!(r[0].ssid, "Home:Net");
        assert_eq!((r[0].channel, r[0].band, r[0].rssi_dbm), (36, Band::Ghz5, -59));
        assert_eq!(r[1].ssid, "");
        assert_eq!((r[1].band, r[1].rssi_dbm), (Band::Ghz2_4, -80));
    }

    #[test]
    fn nmcli_garbage_is_an_error() {
        assert!(parse_nmcli("not:enough").is_err());
        assert!(parse_nmcli("aa\\:bb\\:cc\\:dd\\:ee\\:ff:x:abc:2437 MHz:50").is_err());
        assert!(parse_nmcli("").unwrap().is_empty());
    }

    const NETSH: &str = "\
Interface name : Wi-Fi
There are 2 networks currently visible.

SSID 1 : Corp
    Network type            : Infrastructure
    Authentication          : WPA2-Enterprise
    Encryption              : CCMP
    BSSID 1                 : aa:bb:cc:dd:ee:01
         Signal             : 90%
         Radio type         : 802.11ax
         Band               : 5 GHz
         Channel            : 44
    BSSID 2                 : aa:bb:cc:dd:ee:02
         Signal             : 64%
         Radio type         : 802.11n
         Band               : 2.4 GHz
         Channel            : 6

SSID 2 :
    Network type            : Infrastructure
    BSSID 1                 : 11:22:33:44:55:66
         Signal             : 30%
         Band               : 6 GHz
         Channel            : 5
";

    #[test]
    fn parses_netsh_multi_bssid_and_hidden_ssid() {
        let r = parse_netsh(NETSH).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].ssid.as_str(), r[0].bssid.as_str(), r[0].rssi_dbm), ("Corp", "aa:bb:cc:dd:ee:01", -55));
        assert_eq!((r[0].channel, r[0].band), (44, Band::Ghz5));
        assert_eq!((r[1].ssid.as_str(), r[1].rssi_dbm, r[1].band), ("Corp", -68, Band::Ghz2_4));
        assert_eq!((r[2].ssid.as_str(), r[2].band, r[2].channel), ("", Band::Ghz6, 5));
    }

    #[test]
    fn netsh_without_band_line_falls_back_to_channel() {
        let r = parse_netsh("SSID 1 : X\n    BSSID 1 : aa:aa:aa:aa:aa:aa\n         Signal : 50%\n         Channel : 11\n").unwrap();
        assert_eq!(r[0].band, Band::Ghz2_4);
    }

    #[test]
    fn netsh_unrecognized_output_is_an_error() {
        assert!(parse_netsh("The wireless local area network interface is powered down").is_err());
        assert!(parse_netsh("").unwrap().is_empty());
    }

    fn ap(bssid: &str, x: f64, y: f64, ch: u16, band: Band) -> MockAp {
        MockAp { bssid: bssid.into(), ssid: "Mock".into(), x_m: x, y_m: y, channel: ch, band, tx_dbm: 20.0 }
    }

    #[test]
    fn mock_signal_falls_with_distance_and_is_repeatable() {
        let mut m = MockScanner::new(vec![ap("aa:00:00:00:00:01", 0.0, 0.0, 36, Band::Ghz5)], 7);
        let near = m.scan(Some((2.0, 0.0))).unwrap()[0].rssi_dbm;
        let mid = m.scan(Some((10.0, 0.0))).unwrap()[0].rssi_dbm;
        assert!(near > mid, "{near} vs {mid}");
        assert_eq!(m.scan(Some((10.0, 0.0))).unwrap()[0].rssi_dbm, mid);
        // 5 GHz at 1 m: 20 - 47 = -27 dBm ± shadowing
        assert!((m.rssi_at(0, 1.0, 0.0) + 27.0).abs() <= 2.0);
    }

    #[test]
    fn mock_drops_aps_below_sensitivity_and_needs_a_position() {
        let mut m = MockScanner::new(vec![ap("aa:00:00:00:00:01", 0.0, 0.0, 6, Band::Ghz2_4)], 1);
        assert!(m.scan(Some((500.0, 0.0))).unwrap().is_empty());
        assert!(m.scan(None).is_err());
    }
}
