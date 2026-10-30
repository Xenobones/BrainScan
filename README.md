# BrainScan

Wi-Fi site-survey and heatmapping tool. Load a floor plan, calibrate the scale, walk the
space clicking survey points, and get signal / SNR / throughput / per-AP heatmaps plus
coverage, dead-zone, interference and channel-planning analysis.

Status: core engine done and tested; GUI in progress.

## Engine (`src/`)

| Module | What it does |
| --- | --- |
| `model` | Project, survey points, AP readings and aliases; JSON save/load; CSV export |
| `calibration` | Two-point pixels-per-meter calibration |
| `heatmap` | IDW interpolation with a distance cutoff; signal, SNR, throughput and per-AP views; color ramps |
| `analysis` | Coverage %, dead zones, per-AP summaries, co/adjacent-channel interference, channel suggestions |
| `scanner` | `Scanner` trait with `netsh` (Windows), `nmcli` (Linux) and mock backends |
| `iperf` | Builds and parses `iperf3 -J` runs (TCP and UDP) |

## Try it

```sh
cargo test
cargo run --example demo -- /some/output/dir   # simulated office survey -> PNG heatmaps
```

## Roadmap

1. egui GUI (floor plan import, click-to-survey, heatmap overlay, AP table)
2. Exact-dBm scanning (`iw` on Linux, native WLAN API on Windows)
3. PDF reports and predictive AP placement
4. Optional packet-level features on supported adapters
