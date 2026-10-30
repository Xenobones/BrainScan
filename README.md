# BrainScan

Wi-Fi site-survey and heatmapping tool. Load a floor plan, calibrate the scale, walk the
space clicking survey points, and get signal / SNR / throughput / per-AP heatmaps plus
coverage, dead-zone, interference and channel-planning analysis.

Status: core engine done and tested; first GUI working (survey, heatmaps, analysis). Builds for Linux and Windows.

## Engine (`src/`)

| Module | What it does |
| --- | --- |
| `model` | Project, survey points, AP readings and aliases; JSON save/load; CSV export |
| `calibration` | Two-point pixels-per-meter calibration |
| `heatmap` | IDW interpolation with a distance cutoff; signal, SNR, throughput and per-AP views; color ramps |
| `analysis` | Coverage %, dead zones, per-AP summaries, co/adjacent-channel interference, channel suggestions |
| `scanner` | `Scanner` trait with `iw` and `nmcli` (Linux), `netsh` (Windows) and mock backends |
| `iperf` | Builds and parses `iperf3 -J` runs (TCP and UDP) |

## GUI (`src/app.rs`)

egui app: open a floor plan image (PNG/JPEG) or use the blank canvas, calibrate the scale by
clicking two points, then click the plan to survey. Heatmap views: signal, SNR, throughput,
per-AP. Side panel shows coverage vs. target, dead zones, interference, channel suggestions
and an editable AP table. Projects save as JSON; survey data exports to CSV. The simulated
scanner lets you try everything without hardware.

```sh
cargo run --release                      # GUI
cargo build --release --target x86_64-pc-windows-gnu   # Windows .exe (needs mingw-w64)
```

## Engine checks

```sh
cargo test
cargo run --example demo -- /some/output/dir   # simulated office survey -> PNG heatmaps
```

## Roadmap

1. egui GUI (floor plan import, click-to-survey, heatmap overlay, AP table)
2. Exact-dBm scanning (`iw` on Linux, native WLAN API on Windows)
3. PDF reports and predictive AP placement
4. Optional packet-level features on supported adapters
