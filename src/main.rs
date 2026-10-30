mod app;

use eframe::egui;

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_title("BrainScan"),
        ..Default::default()
    };
    let open = std::env::args().nth(1);
    eframe::run_native("BrainScan", opts, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}
