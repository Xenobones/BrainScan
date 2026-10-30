//! Simulates a survey of a 40 x 25 m office and renders heatmaps to PNG.
//! Run: cargo run --example demo -- [output_dir]

use brainscan::analysis::{coverage, dead_zones, interference, summarize_aps, suggest_channels, DEFAULT_TARGET_DBM};
use brainscan::calibration::Scale;
use brainscan::heatmap::{build, Grid, IdwParams, Metric, Ramp};
use brainscan::model::{Band, FloorPlan, Project};
use brainscan::scanner::{MockAp, MockScanner, Scanner};

const W: usize = 800;
const H: usize = 500;

fn ap(n: u8, name: &str, x: f64, y: f64, ch: u16, band: Band) -> MockAp {
    MockAp { bssid: format!("aa:bb:cc:{n:02x}:00:01"), ssid: name.into(), x_m: x, y_m: y, channel: ch, band, tx_dbm: 15.0 }
}

fn main() -> brainscan::Result<()> {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let scale = Scale::from_two_points((0.0, 0.0), (400.0, 0.0), 20.0)?; // 20 px/m
    let mut project = Project::new("Demo Office");
    project.scale = Some(scale);
    project.floor_plan = Some(FloorPlan { image_path: "demo.png".into(), width_px: W as u32, height_px: H as u32 });

    let mut scanner = MockScanner::new(
        vec![
            ap(1, "Corp", 8.0, 6.0, 1, Band::Ghz2_4),
            ap(2, "Corp", 32.0, 6.0, 6, Band::Ghz2_4),
            ap(3, "Guest", 20.0, 20.0, 6, Band::Ghz2_4), // co-channel with AP 2
            ap(4, "Corp", 8.0, 6.0, 36, Band::Ghz5),
            ap(5, "Corp", 32.0, 6.0, 40, Band::Ghz5),
            ap(6, "Guest", 20.0, 20.0, 44, Band::Ghz5),
        ],
        42,
    );
    scanner.path_loss_exponent = 3.8; // walls and clutter
    project.set_alias("aa:bb:cc:01:00:01", "NW corner");
    project.set_alias("aa:bb:cc:02:00:01", "NE corner");
    project.set_alias("aa:bb:cc:03:00:01", "South");

    // Walk the floor on a 3 m grid, leaving the bottom-right corner unsurveyed.
    for gy in 0..8 {
        for gx in 0..13 {
            let (xm, ym) = (1.5 + gx as f64 * 3.0, 1.5 + gy as f64 * 3.0);
            if xm > 28.0 && ym > 16.0 {
                continue;
            }
            let readings = scanner.scan(Some((xm, ym)))?;
            project.add_point(scale.m_to_px(xm), scale.m_to_px(ym), readings);
        }
    }
    project.save(format!("{out_dir}/demo.brainscan.json"))?;
    std::fs::write(format!("{out_dir}/demo.csv"), project.to_csv())?;

    let params = IdwParams::from_meters(&scale, 0.5, 6.0);
    let signal = build(&project, &Metric::Signal, &params).unwrap();
    let cov = coverage(&signal, DEFAULT_TARGET_DBM);
    println!(
        "{} points | coverage >= {DEFAULT_TARGET_DBM} dBm: {:.1}% of surveyed, {:.1}% of plan",
        project.points.len(),
        cov.pct_of_surveyed(),
        cov.pct_of_plan()
    );
    for z in dead_zones(&signal, DEFAULT_TARGET_DBM, 20) {
        println!("  dead zone: {:.0} m^2 around ({:.1}, {:.1}) m, worst {:.0} dBm",
            z.area_px2 / (scale.pixels_per_meter.powi(2)), scale.px_to_m(z.centroid.0), scale.px_to_m(z.centroid.1), z.worst_value);
    }
    let aps = summarize_aps(&project);
    for i in interference(&aps, -80) {
        println!("  {:?} on {}: {} <-> {} (weaker peak {} dBm)", i.kind, i.band.label(), i.a, i.b, i.weaker_peak_dbm);
    }
    for band in [Band::Ghz2_4, Band::Ghz5] {
        let best = &suggest_channels(&aps, band, &[], false)[0];
        println!("  quietest {} channel: {} ({:.1} dBm)", band.label(), best.channel, best.interference_dbm);
    }

    render(&signal, &Ramp::signal(), &project, &format!("{out_dir}/demo_signal.png"));
    let snr = build(&project, &Metric::Snr, &params).unwrap();
    render(&snr, &Ramp::snr(), &project, &format!("{out_dir}/demo_snr.png"));
    let ap2 = build(&project, &Metric::Ap("aa:bb:cc:02:00:01".into()), &params).unwrap();
    render(&ap2, &Ramp::signal(), &project, &format!("{out_dir}/demo_ap_ne.png"));
    println!("wrote PNGs and project files to {out_dir}");
    Ok(())
}

/// Draws a white "paper" background, the heatmap blended on top, and the survey points.
fn render(grid: &Grid, ramp: &Ramp, project: &Project, path: &str) {
    let mut img = vec![255u8; W * H * 3];
    let rgba = grid.to_rgba(ramp);
    for y in 0..H {
        for x in 0..W {
            let (c, r) = ((x as f64 / grid.cell_px) as usize, (y as f64 / grid.cell_px) as usize);
            if c >= grid.cols || r >= grid.rows {
                continue;
            }
            let px = &rgba[(r * grid.cols + c) * 4..][..4];
            let a = px[3] as f64 / 255.0;
            for k in 0..3 {
                let i = (y * W + x) * 3 + k;
                img[i] = (img[i] as f64 * (1.0 - a) + px[k] as f64 * a) as u8;
            }
        }
    }
    for p in &project.points {
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let (x, y) = (p.x as i32 + dx, p.y as i32 + dy);
                if x >= 0 && y >= 0 && (x as usize) < W && (y as usize) < H {
                    let i = (y as usize * W + x as usize) * 3;
                    img[i..i + 3].copy_from_slice(&[30, 30, 30]);
                }
            }
        }
    }
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), W as u32, H as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img).unwrap();
}
