use std::process::Command;

use serde::Deserialize;

use crate::model::Throughput;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct IperfOptions {
    pub server: String,
    pub port: Option<u16>,
    pub duration_s: u32,
    /// `-R`: server sends to us, i.e. a download test. Default is upload.
    pub reverse: bool,
    pub parallel: u32,
    pub udp: bool,
}

impl IperfOptions {
    pub fn new(server: impl Into<String>) -> IperfOptions {
        IperfOptions { server: server.into(), port: None, duration_s: 5, reverse: false, parallel: 1, udp: false }
    }

    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["-c".to_string(), self.server.clone(), "-J".into(), "-t".into(), self.duration_s.to_string()];
        if let Some(p) = self.port {
            a.extend(["-p".to_string(), p.to_string()]);
        }
        if self.parallel > 1 {
            a.extend(["-P".to_string(), self.parallel.to_string()]);
        }
        if self.reverse {
            a.push("-R".into());
        }
        if self.udp {
            a.push("-u".into());
        }
        a
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IperfResult {
    pub mbps: f64,
    pub reverse: bool,
    pub udp: bool,
    pub retransmits: Option<u64>,
    pub jitter_ms: Option<f64>,
    pub lost_percent: Option<f64>,
}

impl IperfResult {
    /// Maps to the project's throughput slot: reverse tests measure download.
    pub fn into_throughput(&self) -> Throughput {
        if self.reverse {
            Throughput { down_mbps: Some(self.mbps), up_mbps: None }
        } else {
            Throughput { down_mbps: None, up_mbps: Some(self.mbps) }
        }
    }
}

/// Combines a download and an upload run for one survey point.
pub fn merge(down: &IperfResult, up: &IperfResult) -> Throughput {
    Throughput { down_mbps: Some(down.mbps), up_mbps: Some(up.mbps) }
}

pub fn run(opts: &IperfOptions) -> Result<IperfResult> {
    let out = Command::new("iperf3")
        .args(opts.args())
        .output()
        .map_err(|e| Error::Command(format!("could not run iperf3 (is it installed and on PATH?): {e}")))?;
    // iperf3 -J prints a JSON error object to stdout even on failure, so parse first.
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse(&stdout, opts.reverse) {
        Ok(r) => Ok(r),
        Err(e) if !out.status.success() && stdout.trim().is_empty() => {
            Err(Error::Command(format!("iperf3 failed: {} ({e})", String::from_utf8_lossy(&out.stderr).trim())))
        }
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
struct Raw {
    error: Option<String>,
    start: Option<RawStart>,
    end: Option<RawEnd>,
}

#[derive(Deserialize)]
struct RawStart {
    test_start: Option<RawTestStart>,
}

#[derive(Deserialize)]
struct RawTestStart {
    protocol: Option<String>,
    reverse: Option<u8>,
}

#[derive(Deserialize)]
struct RawEnd {
    sum_received: Option<RawSum>,
    sum_sent: Option<RawSum>,
    sum: Option<RawSum>,
}

#[derive(Deserialize)]
struct RawSum {
    bits_per_second: f64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost_percent: Option<f64>,
}

/// Parses `iperf3 -J` output. `reverse_hint` is used only if the JSON doesn't say.
pub fn parse(json: &str, reverse_hint: bool) -> Result<IperfResult> {
    let raw: Raw = serde_json::from_str(json).map_err(|e| Error::Parse(format!("iperf3 JSON: {e}")))?;
    if let Some(err) = raw.error {
        return Err(Error::Command(format!("iperf3: {err}")));
    }
    let ts = raw.start.and_then(|s| s.test_start);
    let udp = ts.as_ref().and_then(|t| t.protocol.as_deref()) == Some("UDP");
    let reverse = ts.as_ref().and_then(|t| t.reverse).map_or(reverse_hint, |r| r != 0);
    let end = raw.end.ok_or_else(|| Error::Parse("iperf3 JSON has no \"end\" section".into()))?;

    let sent_retransmits = end.sum_sent.as_ref().and_then(|s| s.retransmits);
    let sum = if udp {
        end.sum.or(end.sum_received)
    } else {
        // Receiver-side goodput is what the user actually experiences.
        end.sum_received.or(end.sum_sent)
    }
    .ok_or_else(|| Error::Parse("iperf3 JSON has no summary".into()))?;

    let retransmits = sent_retransmits.or(sum.retransmits);
    Ok(IperfResult {
        mbps: sum.bits_per_second / 1e6,
        reverse,
        udp,
        retransmits,
        jitter_ms: sum.jitter_ms,
        lost_percent: sum.lost_percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = r#"{
      "start": {"test_start": {"protocol": "TCP", "reverse": 1}},
      "end": {
        "sum_sent":     {"bits_per_second": 310000000.0, "retransmits": 12},
        "sum_received": {"bits_per_second": 305500000.0}
      }}"#;

    const UDP: &str = r#"{
      "start": {"test_start": {"protocol": "UDP", "reverse": 0}},
      "end": {"sum": {"bits_per_second": 1000000.0, "jitter_ms": 0.42, "lost_percent": 1.5}}}"#;

    #[test]
    fn parses_tcp_download() {
        let r = parse(TCP, false).unwrap();
        assert!((r.mbps - 305.5).abs() < 1e-9);
        assert!(r.reverse && !r.udp);
        assert_eq!(r.retransmits, Some(12));
        assert_eq!(r.into_throughput(), Throughput { down_mbps: Some(305.5), up_mbps: None });
    }

    #[test]
    fn parses_udp_upload() {
        let r = parse(UDP, true).unwrap();
        assert!(r.udp && !r.reverse); // JSON wins over the hint
        assert_eq!((r.jitter_ms, r.lost_percent), (Some(0.42), Some(1.5)));
        assert_eq!(r.into_throughput().up_mbps, Some(1.0));
    }

    #[test]
    fn server_errors_surface() {
        let e = parse(r#"{"error": "unable to connect to server: Connection refused"}"#, false).unwrap_err();
        assert!(e.to_string().contains("Connection refused"));
        assert!(parse("not json", false).is_err());
        assert!(parse("{}", false).is_err());
    }

    #[test]
    fn builds_args() {
        let mut o = IperfOptions::new("10.0.0.5");
        o.port = Some(5202);
        o.reverse = true;
        o.parallel = 4;
        o.udp = true;
        assert_eq!(o.args(), ["-c", "10.0.0.5", "-J", "-t", "5", "-p", "5202", "-P", "4", "-R", "-u"]);
        assert_eq!(IperfOptions::new("h").args(), ["-c", "h", "-J", "-t", "5"]);
    }

    #[test]
    fn merges_down_and_up() {
        let d = parse(TCP, false).unwrap();
        let mut u = d.clone();
        u.reverse = false;
        u.mbps = 120.0;
        assert_eq!(merge(&d, &u), Throughput { down_mbps: Some(305.5), up_mbps: Some(120.0) });
    }
}
