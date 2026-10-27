mod app;
mod engine;
mod headless;
mod runtime_env;
mod session;
mod ui_render;
pub mod vault;

use app::SiloApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let is_headless = args.iter().any(|a| {
        a == "--headless"
            || a == "-h"
            || a == "--test-tools"
            || a == "-t"
            || a == "--prompt"
            || a == "-p"
            || a == "--deep"
            || a == "-d"
            || a == "--eval"
            || a == "-e"
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to initialize Tokio runtime");

    if is_headless {
        rt.block_on(headless::run_headless(args));
        return Ok(());
    }

    let handle = rt.handle().clone();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([780.0, 480.0])
            .with_title("⚡ Silo — Privacy-First Local AI Workbench"),
        ..Default::default()
    };

    eframe::run_native(
        "Silo",
        native_options,
        Box::new(move |cc| Ok(Box::new(SiloApp::new(cc, handle)))),
    )
}
