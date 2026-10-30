//! BrainScan core engine: survey data model, calibration, heatmaps,
//! channel/interference analysis, Wi-Fi scanner backends, and iperf3 runner.

pub mod analysis;
pub mod calibration;
pub mod heatmap;
pub mod iperf;
pub mod model;
pub mod scanner;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// Scanner or tool output that could not be understood.
    Parse(String),
    /// Caller supplied something unusable (zero distance, empty data, ...).
    InvalidInput(String),
    /// An external command failed to run or exited with an error.
    Command(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Json(e) => write!(f, "JSON error: {e}"),
            Error::Parse(s) => write!(f, "parse error: {s}"),
            Error::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Error::Command(s) => write!(f, "command failed: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
