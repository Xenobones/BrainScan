//! egui front end: floor plan canvas, click-to-survey, heatmap overlay, analysis panels.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use brainscan::analysis::{
    self, ApSummary, ChannelScore, Coverage, DeadZone, Interference, InterferenceKind, DEFAULT_TARGET_DBM,
};
use brainscan::calibration::Scale;
use brainscan::heatmap::{self, Grid, IdwParams, Metric, Ramp};
use brainscan::iperf::{self, IperfOptions};
use brainscan::model::{ApReading, Band, FloorPlan, Project, Throughput};
use brainscan::scanner::{default_scanner, MockAp, MockScanner, Scanner};
use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};

#[derive(PartialEq, Clone, Copy)]
enum Tool {
    Survey,
    Calibrate,
}

#[derive(PartialEq, Clone, Copy)]
enum Backend {
    Real,
    Simulated,
}

struct ScanDone {
    x: f64,
    y: f64,
    readings: Result<Vec<ApReading>, String>,
    throughput: Option<Result<Throughput, String>>,
}

#[derive(Default)]
struct Analysis {
    aps: Vec<ApSummary>,
    coverage: Option<Coverage>,
    zones: Vec<DeadZone>,
    interference: Vec<Interference>,
    ch24: Vec<ChannelScore>,
    ch5: Vec<ChannelScore>,
}

pub struct App {
    project: Project,
    path: Option<PathBuf>,
    plan_tex: Option<TextureHandle>,

    zoom: f32,
    pan: Vec2,
    fit_pending: bool,

    tool: Tool,
    backend: Backend,
    metric: Metric,
    opacity: f32,
    show_points: bool,
    show_zones: bool,
    target_dbm: f64,

    heat_dirty: bool,
    heat_grid: Option<Grid>,
    heat_tex: Option<TextureHandle>,
    analysis: Analysis,

    cal_points: Vec<(f64, f64)>,
    cal_ready: Option<((f64, f64), (f64, f64))>,
    cal_text: String,

    scanning: bool,
    tx: Sender<ScanDone>,
    rx: Receiver<ScanDone>,
    iperf_on: bool,
    iperf_server: String,
    iperf_secs: u32,
    mock_aps: Vec<MockAp>,

    status: String,
}

const DEFAULT_PX_PER_M: f64 = 20.0;

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, open: Option<String>) -> App {
        let (tx, rx) = channel();
        let mut app = App {
            project: Project::new("Untitled"),
            path: None,
            plan_tex: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit_pending: true,
            tool: Tool::Survey,
            backend: Backend::Simulated,
            metric: Metric::Signal,
            opacity: 0.85,
            show_points: true,
            show_zones: true,
            target_dbm: DEFAULT_TARGET_DBM,
            heat_dirty: true,
            heat_grid: None,
            heat_tex: None,
            analysis: Analysis::default(),
            cal_points: Vec::new(),
            cal_ready: None,
            cal_text: String::new(),
            scanning: false,
            tx,
            rx,
            iperf_on: false,
            iperf_server: String::new(),
            iperf_secs: 5,
            mock_aps: Vec::new(),
            status: "Open a floor plan (File menu) or start a blank 40 x 25 m canvas.".into(),
        };
        app.new_blank(40.0, 25.0);
        if let Some(p) = open {
            app.open_project(&PathBuf::from(p), &_cc.egui_ctx);
        }
        app
    }

    // ------------------------------------------------------------ project I/O

    fn new_blank(&mut self, w_m: f64, h_m: f64) {
        self.project = Project::new("Blank canvas");
        self.project.scale = Some(Scale { pixels_per_meter: DEFAULT_PX_PER_M });
        self.project.floor_plan = Some(FloorPlan {
            image_path: String::new(),
            width_px: (w_m * DEFAULT_PX_PER_M) as u32,
            height_px: (h_m * DEFAULT_PX_PER_M) as u32,
        });
        self.plan_tex = None;
        self.path = None;
        self.fit_pending = true;
        self.heat_dirty = true;
        self.reset_mock_aps();
    }

    fn plan_size(&self) -> (f64, f64) {
        self.project.floor_plan.as_ref().map_or((800.0, 500.0), |p| (p.width_px as f64, p.height_px as f64))
    }

    fn px_per_m(&self) -> f64 {
        self.project.scale.map_or(DEFAULT_PX_PER_M, |s| s.pixels_per_meter)
    }

    /// Four simulated APs spread over the plan, two bands each.
    fn reset_mock_aps(&mut self) {
        let (w, h) = self.plan_size();
        let ppm = self.px_per_m();
        let (wm, hm) = (w / ppm, h / ppm);
        let spots = [(0.2, 0.25), (0.8, 0.25), (0.2, 0.8), (0.8, 0.8)];
        let ch24 = [1u16, 6, 11, 6];
        let ch5 = [36u16, 48, 149, 161];
        self.mock_aps.clear();
        for (i, (fx, fy)) in spots.iter().enumerate() {
            for (band, ch) in [(Band::Ghz2_4, ch24[i]), (Band::Ghz5, ch5[i])] {
                self.mock_aps.push(MockAp {
                    bssid: format!("02:b0:{:02x}:{:02x}:00:{:02x}", i, ch, i),
                    ssid: if i == 3 { "Guest".into() } else { "Corp".into() },
                    x_m: fx * wm,
                    y_m: fy * hm,
                    channel: ch,
                    band,
                    tx_dbm: 15.0,
                });
            }
        }
    }

    fn load_plan(&mut self, path: &Path, ctx: &egui::Context) -> Result<(), String> {
        let img = image::open(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
        let (w, h) = (img.width(), img.height());
        // GPUs cap texture size; draw a downscaled copy but keep plan coordinates in original pixels.
        let shown = if w.max(h) > 4096 { img.resize(4096, 4096, image::imageops::FilterType::Triangle) } else { img };
        let rgba = shown.to_rgba8();
        let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
        self.plan_tex = Some(ctx.load_texture("floor_plan", ci, egui::TextureOptions::LINEAR));
        self.project.floor_plan = Some(FloorPlan { image_path: path.to_string_lossy().into_owned(), width_px: w, height_px: h });
        self.fit_pending = true;
        self.heat_dirty = true;
        Ok(())
    }

    fn open_plan_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg"]).pick_file() else { return };
        match self.load_plan(&path, ctx) {
            Ok(()) => {
                self.reset_mock_aps();
                self.status = if self.project.points.is_empty() {
                    "Floor plan loaded. Use the Calibrate tool to set the scale.".into()
                } else {
                    "Floor plan replaced; existing survey points were kept.".into()
                };
            }
            Err(e) => self.status = e,
        }
    }

    fn open_project(&mut self, path: &Path, ctx: &egui::Context) {
        match Project::load(path) {
            Ok(p) => {
                self.project = p;
                self.path = Some(path.to_path_buf());
                self.plan_tex = None;
                let plan_path = self.project.floor_plan.as_ref().map(|f| f.image_path.clone()).unwrap_or_default();
                self.status = format!("Opened {}", path.display());
                if !plan_path.is_empty() {
                    let dims = self.project.floor_plan.clone();
                    if let Err(e) = self.load_plan(&PathBuf::from(&plan_path), ctx) {
                        self.status = format!("{e} (showing a blank canvas instead)");
                    }
                    // Keep the saved dimensions authoritative so survey points still line up.
                    self.project.floor_plan = dims;
                }
                self.fit_pending = true;
                self.heat_dirty = true;
                self.reset_mock_aps();
            }
            Err(e) => self.status = format!("Could not open project: {e}"),
        }
    }

    fn save(&mut self, as_new: bool) {
        let path = match (&self.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => {
                let Some(p) = rfd::FileDialog::new()
                    .add_filter("BrainScan project", &["brainscan"])
                    .set_file_name(format!("{}.brainscan", self.project.name))
                    .save_file()
                else {
                    return;
                };
                p
            }
        };
        match self.project.save(&path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.path = Some(path);
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn export_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("survey.csv").save_file() else {
            return;
        };
        self.status = match std::fs::write(&path, self.project.to_csv()) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    // ---------------------------------------------------------------- scanning

    fn start_scan(&mut self, ctx: &egui::Context, x_px: f64, y_px: f64) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.status = "Scanning...".into();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let backend = self.backend;
        let mock = self.mock_aps.clone();
        let ppm = self.px_per_m();
        let iperf_opts = (self.iperf_on && !self.iperf_server.trim().is_empty()).then(|| {
            let mut o = IperfOptions::new(self.iperf_server.trim());
            o.duration_s = self.iperf_secs;
            o
        });
        let sim_iperf = self.iperf_on && backend == Backend::Simulated;

        std::thread::spawn(move || {
            let readings = match backend {
                Backend::Real => default_scanner().scan(None),
                Backend::Simulated => {
                    let mut m = MockScanner::new(mock, 42);
                    m.path_loss_exponent = 3.9; // walls and clutter
                    m.scan(Some((x_px / ppm, y_px / ppm)))
                }
            }
            .map_err(|e| e.to_string());

            let throughput = match (&readings, backend) {
                (Ok(r), Backend::Simulated) if sim_iperf => {
                    let best = r.iter().map(|a| a.rssi_dbm).max().unwrap_or(-100) as f64;
                    let down = ((best + 95.0) * 9.0).clamp(2.0, 700.0);
                    Some(Ok(Throughput { down_mbps: Some(down), up_mbps: Some(down * 0.8) }))
                }
                (Ok(_), Backend::Real) => iperf_opts.map(|mut o| {
                    o.reverse = true;
                    let down = iperf::run(&o).map_err(|e| e.to_string())?;
                    o.reverse = false;
                    let up = iperf::run(&o).map_err(|e| e.to_string())?;
                    Ok(iperf::merge(&down, &up))
                }),
                _ => None,
            };
            let _ = tx.send(ScanDone { x: x_px, y: y_px, readings, throughput });
            ctx.request_repaint();
        });
    }

    fn poll_scans(&mut self) {
        while let Ok(done) = self.rx.try_recv() {
            self.scanning = false;
            match done.readings {
                Ok(readings) => {
                    let n = readings.len();
                    let id = self.project.add_point(done.x, done.y, readings);
                    self.status = format!("Point {id}: heard {n} APs");
                    match done.throughput {
                        Some(Ok(t)) => {
                            if let Some(p) = self.project.points.last_mut() {
                                p.throughput = Some(t);
                            }
                            self.status.push_str(&format!(
                                ", {:.0} down / {:.0} up Mbps",
                                t.down_mbps.unwrap_or(0.0),
                                t.up_mbps.unwrap_or(0.0)
                            ));
                        }
                        Some(Err(e)) => self.status.push_str(&format!(" (throughput test failed: {e})")),
                        None => {}
                    }
                    self.heat_dirty = true;
                }
                Err(e) => self.status = format!("Scan failed: {e}"),
            }
        }
    }

    // ---------------------------------------------------------------- analysis

    fn idw_params(&self) -> IdwParams {
        let (w, h) = self.plan_size();
        let max_dim = w.max(h);
        match self.project.scale {
            Some(s) => IdwParams {
                cell_px: s.m_to_px(0.5).max(max_dim / 200.0),
                power: 2.0,
                max_distance_px: s.m_to_px(8.0),
            },
            None => IdwParams { cell_px: (max_dim / 150.0).max(2.0), power: 2.0, max_distance_px: max_dim * 0.2 },
        }
    }

    fn current_ramp(&self) -> Ramp {
        match &self.metric {
            Metric::Snr => Ramp::snr(),
            Metric::Throughput => {
                let max = heatmap::samples(&self.project, &Metric::Throughput).iter().map(|s| s.value).fold(100.0, f64::max);
                Ramp::throughput(max)
            }
            _ => Ramp::signal(),
        }
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        if !self.heat_dirty {
            return;
        }
        self.heat_dirty = false;
        let params = self.idw_params();
        let signal = heatmap::build(&self.project, &Metric::Signal, &params);
        let view = if self.metric == Metric::Signal { signal.clone() } else { heatmap::build(&self.project, &self.metric, &params) };

        self.heat_tex = view.as_ref().map(|g| {
            let ci = egui::ColorImage::from_rgba_unmultiplied([g.cols, g.rows], &g.to_rgba(&self.current_ramp()));
            ctx.load_texture("heat", ci, egui::TextureOptions::LINEAR)
        });
        self.heat_grid = view;

        let aps = analysis::summarize_aps(&self.project);
        self.analysis = Analysis {
            coverage: signal.as_ref().map(|g| analysis::coverage(g, self.target_dbm)),
            zones: signal.as_ref().map(|g| analysis::dead_zones(g, self.target_dbm, 8)).unwrap_or_default(),
            interference: analysis::interference(&aps, -85),
            ch24: analysis::suggest_channels(&aps, Band::Ghz2_4, &[], false),
            ch5: analysis::suggest_channels(&aps, Band::Ghz5, &[], false),
            aps,
        };
    }

    // ---------------------------------------------------------------- UI pieces

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New blank canvas (40 x 25 m)").clicked() {
                    self.new_blank(40.0, 25.0);
                    ui.close();
                }
                if ui.button("Open floor plan image...").clicked() {
                    ui.close();
                    self.open_plan_dialog(&ctx);
                }
                ui.separator();
                if ui.button("Open project...").clicked() {
                    ui.close();
                    if let Some(p) = rfd::FileDialog::new().add_filter("BrainScan project", &["brainscan", "json"]).pick_file() {
                        self.open_project(&p, &ctx);
                    }
                }
                if ui.button("Save").clicked() {
                    ui.close();
                    self.save(false);
                }
                if ui.button("Save as...").clicked() {
                    ui.close();
                    self.save(true);
                }
                ui.separator();
                if ui.button("Export CSV...").clicked() {
                    ui.close();
                    self.export_csv();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Fit plan to window").clicked() {
                    self.fit_pending = true;
                    ui.close();
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tool, Tool::Survey, "Survey");
            if ui.selectable_value(&mut self.tool, Tool::Calibrate, "Calibrate scale").clicked() {
                self.cal_points.clear();
                self.status = "Click two points on the plan whose real-world distance you know.".into();
            }
            ui.separator();
            ui.label("Heatmap:");
            let before = self.metric.clone();
            let label = match &self.metric {
                Metric::Signal => "Signal".to_string(),
                Metric::Snr => "SNR".to_string(),
                Metric::Throughput => "Throughput".to_string(),
                Metric::Ap(b) => self.project.alias_for(b).map(str::to_string).unwrap_or_else(|| format!("AP {b}")),
            };
            egui::ComboBox::from_id_salt("metric").selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.metric, Metric::Signal, "Signal (strongest AP)");
                ui.selectable_value(&mut self.metric, Metric::Snr, "SNR");
                ui.selectable_value(&mut self.metric, Metric::Throughput, "Throughput (download)");
                ui.separator();
                for ap in &self.analysis.aps {
                    let text = format!("{} ch{} ({})", ap.name, ap.channel, ap.band.label());
                    ui.selectable_value(&mut self.metric, Metric::Ap(ap.bssid.clone()), text);
                }
            });
            if self.metric != before {
                self.heat_dirty = true;
            }
            ui.add(egui::Slider::new(&mut self.opacity, 0.0..=1.0).text("opacity"));
            ui.checkbox(&mut self.show_points, "points");
            ui.checkbox(&mut self.show_zones, "dead zones");
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Survey");
            ui.horizontal(|ui| {
                ui.label("Scanner:");
                ui.radio_value(&mut self.backend, Backend::Simulated, "Simulated");
                ui.radio_value(&mut self.backend, Backend::Real, "This computer's Wi-Fi");
            });
            ui.checkbox(&mut self.iperf_on, "Measure throughput with iperf3");
            if self.iperf_on {
                ui.horizontal(|ui| {
                    ui.label("Server:");
                    ui.text_edit_singleline(&mut self.iperf_server);
                });
                ui.add(egui::Slider::new(&mut self.iperf_secs, 2..=30).text("seconds per test"));
                if self.backend == Backend::Simulated {
                    ui.weak("Simulated scanner fakes throughput from signal; no server needed.");
                }
            }
            ui.label(format!("{} survey points", self.project.points.len()));
            ui.horizontal(|ui| {
                ui.label("Scale:");
                match self.project.scale {
                    Some(s) => ui.label(format!("{:.1} px/m", s.pixels_per_meter)),
                    None => ui.colored_label(Color32::from_rgb(200, 120, 0), "not calibrated"),
                };
            });
            ui.separator();

            ui.heading("Coverage");
            let mut changed = false;
            changed |= ui.add(egui::Slider::new(&mut self.target_dbm, -85.0..=-50.0).text("target dBm")).changed();
            if changed {
                self.heat_dirty = true;
            }
            match &self.analysis.coverage {
                Some(c) if c.surveyed_cells > 0 => {
                    ui.label(format!("{:.1}% of surveyed area meets target", c.pct_of_surveyed()));
                    ui.label(format!("{:.1}% of whole plan", c.pct_of_plan()));
                }
                _ => {
                    ui.weak("Click on the plan to add survey points.");
                }
            }
            let ppm = self.px_per_m();
            for (i, z) in self.analysis.zones.iter().take(6).enumerate() {
                ui.label(format!(
                    "Dead zone {}: {:.0} m² near ({:.0}, {:.0}) m, worst {:.0} dBm",
                    i + 1,
                    z.area_px2 / (ppm * ppm),
                    z.centroid.0 / ppm,
                    z.centroid.1 / ppm,
                    z.worst_value
                ));
            }
            ui.separator();

            ui.heading("Interference");
            if self.analysis.interference.is_empty() {
                ui.weak("None detected.");
            }
            for i in self.analysis.interference.iter().take(10) {
                let kind = if i.kind == InterferenceKind::CoChannel { "Co-channel" } else { "Adjacent" };
                ui.label(format!("{kind} {}: {} / {}", i.band.label(), i.a, i.b));
            }
            for (title, list) in [("2.4 GHz", &self.analysis.ch24), ("5 GHz", &self.analysis.ch5)] {
                if let Some(best) = list.first() {
                    ui.label(format!("Quietest {title} channel: {}", best.channel));
                }
            }
            ui.separator();

            ui.heading("Access points");
            let mut alias_edits: Vec<(String, String)> = Vec::new();
            let mut pick: Option<String> = None;
            egui::Grid::new("aps").striped(true).show(ui, |ui| {
                for h in ["Name", "Ch", "Peak", "Mean", "n", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for ap in &self.analysis.aps {
                    let mut name = ap.name.clone();
                    if ui.add_sized([120.0, 20.0], egui::TextEdit::singleline(&mut name)).changed() {
                        alias_edits.push((ap.bssid.clone(), name));
                    }
                    ui.label(format!("{} ({})", ap.channel, ap.band.label().trim_end_matches(" GHz")));
                    ui.label(format!("{}", ap.max_rssi_dbm));
                    ui.label(format!("{:.0}", ap.mean_rssi_dbm));
                    ui.label(ap.samples.to_string());
                    if ui.small_button("map").on_hover_text(&ap.bssid).clicked() {
                        pick = Some(ap.bssid.clone());
                    }
                    ui.end_row();
                }
            });
            for (bssid, name) in alias_edits {
                if name.trim().is_empty() {
                    self.project.ap_aliases.remove(&bssid);
                } else {
                    self.project.set_alias(&bssid, name);
                }
                self.heat_dirty = true;
            }
            if let Some(b) = pick {
                self.metric = Metric::Ap(b);
                self.heat_dirty = true;
            }
        });
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        let (pw, ph) = self.plan_size();

        if self.fit_pending && rect.width() > 10.0 {
            self.zoom = ((rect.width() / pw as f32).min(rect.height() / ph as f32) * 0.95).max(0.01);
            self.pan = Vec2::new((rect.width() - pw as f32 * self.zoom) / 2.0, (rect.height() - ph as f32 * self.zoom) / 2.0);
            self.fit_pending = false;
        }

        if resp.hovered() {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(m) = resp.hover_pos() {
                    let old = self.zoom;
                    self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.02, 40.0);
                    let anchor = m - rect.min - self.pan;
                    self.pan += anchor - anchor * (self.zoom / old);
                }
            }
        }
        if resp.dragged() {
            self.pan += resp.drag_delta();
        }

        let origin = rect.min + self.pan;
        let to_screen = |x: f64, y: f64| origin + Vec2::new(x as f32, y as f32) * self.zoom;
        let to_plan = |p: Pos2| {
            let v = (p - origin) / self.zoom;
            (v.x as f64, v.y as f64)
        };

        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let plan_rect = Rect::from_min_size(origin, Vec2::new(pw as f32, ph as f32) * self.zoom);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        // Document content never follows the UI theme: the plan stays white paper.
        match &self.plan_tex {
            Some(t) => painter.image(t.id(), plan_rect, uv, Color32::WHITE),
            None => painter.rect_filled(plan_rect, 0.0, Color32::WHITE),
        };
        painter.rect_stroke(plan_rect, 0.0, Stroke::new(1.0, Color32::GRAY), egui::StrokeKind::Outside);

        let clipped = painter.with_clip_rect(plan_rect.intersect(rect));
        if let (Some(tex), Some(g)) = (&self.heat_tex, &self.heat_grid) {
            let size = Vec2::new(g.cols as f32, g.rows as f32) * g.cell_px as f32 * self.zoom;
            let tint = Color32::from_white_alpha((self.opacity * 255.0) as u8);
            clipped.image(tex.id(), Rect::from_min_size(origin, size), uv, tint);
        }
        if self.show_zones {
            for z in &self.analysis.zones {
                let r = (z.area_px2 / std::f64::consts::PI).sqrt() as f32 * self.zoom;
                clipped.circle_stroke(to_screen(z.centroid.0, z.centroid.1), r, Stroke::new(2.0, Color32::from_rgb(200, 30, 30)));
            }
        }

        let hover_plan = resp.hover_pos().map(to_plan);
        let mut hovered_point: Option<u32> = None;
        if self.show_points {
            for p in &self.project.points {
                let c = to_screen(p.x, p.y);
                let near = resp.hover_pos().is_some_and(|m| m.distance(c) < 8.0);
                if near {
                    hovered_point = Some(p.id);
                }
                clipped.circle_filled(c, if near { 6.0 } else { 4.0 }, Color32::from_rgb(30, 30, 30));
                clipped.circle_stroke(c, if near { 6.0 } else { 4.0 }, Stroke::new(1.5, Color32::WHITE));
            }
        }
        if let Some(id) = hovered_point {
            if let Some(p) = self.project.points.iter().find(|p| p.id == id) {
                let lines: Vec<String> = p
                    .readings
                    .iter()
                    .take(6)
                    .map(|r| format!("{} ch{}  {} dBm", self.project.display_name(&r.bssid, &r.ssid), r.channel, r.rssi_dbm))
                    .collect();
                resp.clone().on_hover_text(format!("Point {id} (Delete removes it)\n{}", lines.join("\n")));
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.project.remove_point(id);
                self.heat_dirty = true;
            }
        }

        // Calibration overlay
        for &(x, y) in &self.cal_points {
            clipped.circle_filled(to_screen(x, y), 5.0, Color32::from_rgb(255, 200, 0));
        }
        if let [a, b] = self.cal_points[..] {
            clipped.line_segment([to_screen(a.0, a.1), to_screen(b.0, b.1)], Stroke::new(2.0, Color32::from_rgb(255, 200, 0)));
        }

        // Clicks
        if resp.clicked_by(PointerButton::Primary) && self.cal_ready.is_none() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let (x, y) = to_plan(pos);
                if x >= 0.0 && y >= 0.0 && x <= pw && y <= ph && hovered_point.is_none() {
                    match self.tool {
                        Tool::Survey => self.start_scan(&ctx, x, y),
                        Tool::Calibrate => {
                            self.cal_points.push((x, y));
                            if self.cal_points.len() == 2 {
                                self.cal_ready = Some((self.cal_points[0], self.cal_points[1]));
                                self.cal_text.clear();
                            }
                        }
                    }
                }
            }
        }

        self.draw_legend(&painter, rect, ui);
        if let Some((x, y)) = hover_plan {
            let ppm = self.px_per_m();
            let t = format!("{:.1} m, {:.1} m", x / ppm, y / ppm);
            painter.text(rect.right_bottom() - Vec2::new(8.0, 8.0), egui::Align2::RIGHT_BOTTOM, t, egui::FontId::monospace(12.0), ui.visuals().text_color());
        }
        if self.scanning {
            painter.text(rect.center_top() + Vec2::new(0.0, 12.0), egui::Align2::CENTER_TOP, "Scanning...", egui::FontId::proportional(18.0), ui.visuals().strong_text_color());
        }
    }

    fn draw_legend(&self, painter: &egui::Painter, rect: Rect, ui: &egui::Ui) {
        let ramp = self.current_ramp();
        let (w, h) = (160.0, 12.0);
        let tl = rect.left_bottom() + Vec2::new(12.0, -44.0);
        painter.rect_filled(Rect::from_min_size(tl - Vec2::splat(6.0), Vec2::new(w + 12.0, h + 36.0)), 4.0, ui.visuals().panel_fill);
        let steps = 40;
        for i in 0..steps {
            let t = i as f64 / (steps - 1) as f64;
            let c = ramp.color(ramp.min + t * (ramp.max - ramp.min));
            let x = tl.x + w * i as f32 / steps as f32;
            painter.rect_filled(Rect::from_min_size(Pos2::new(x, tl.y), Vec2::new(w / steps as f32 + 1.0, h)), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
        }
        let unit = match self.metric {
            Metric::Snr => "dB",
            Metric::Throughput => "Mbps",
            _ => "dBm",
        };
        let font = egui::FontId::proportional(11.0);
        let col = ui.visuals().text_color();
        painter.text(tl + Vec2::new(0.0, h + 3.0), egui::Align2::LEFT_TOP, format!("{:.0}", ramp.min), font.clone(), col);
        painter.text(tl + Vec2::new(w, h + 3.0), egui::Align2::RIGHT_TOP, format!("{:.0} {unit}", ramp.max), font, col);
    }

    fn calibration_dialog(&mut self, ctx: &egui::Context) {
        let Some((a, b)) = self.cal_ready else { return };
        let mut close = false;
        egui::Window::new("Set scale").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label("Real-world distance between the two points (meters):");
            let r = ui.text_edit_singleline(&mut self.cal_text);
            r.request_focus();
            ui.horizontal(|ui| {
                let ok = ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ok {
                    match self.cal_text.trim().parse::<f64>().map_err(|e| e.to_string()).and_then(|m| Scale::from_two_points(a, b, m).map_err(|e| e.to_string())) {
                        Ok(s) => {
                            self.status = format!("Scale set: {:.1} px per meter", s.pixels_per_meter);
                            self.project.scale = Some(s);
                            self.heat_dirty = true;
                            self.reset_mock_aps();
                            self.tool = Tool::Survey;
                            close = true;
                        }
                        Err(e) => self.status = format!("Invalid distance: {e}"),
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.cal_ready = None;
            self.cal_points.clear();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_scans();
        let ctx = ui.ctx().clone();
        self.refresh(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            self.menu_bar(ui);
            self.toolbar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(&self.status);
        });
        egui::Panel::right("side").default_size(380.0).show(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.canvas(ui));
        self.calibration_dialog(&ctx);
    }
}
