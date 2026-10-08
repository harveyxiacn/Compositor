// No console window behind the app in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use compositor::app::App;
use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    // wgpu draws through Direct3D 12 or Vulkan; COMPOSITOR_RENDERER=glow falls back to OpenGL on
    // machines whose drivers don't manage either.
    let renderer = match std::env::var("COMPOSITOR_RENDERER").as_deref() {
        Ok("glow") | Ok("gl") | Ok("opengl") => eframe::Renderer::Glow,
        _ => eframe::Renderer::Wgpu,
    };
    let icon = image::load_from_memory(include_bytes!("../../Compositor/Assets.xcassets/AppIcon.appiconset/app-icon-256.png"))
        .ok()
        .map(|i| {
            let i = i.to_rgba8();
            let (width, height) = i.dimensions();
            egui::IconData { rgba: i.into_raw(), width, height }
        });
    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title("Compositor")
            .with_app_id("compositor")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true),
        renderer,
        ..Default::default()
    };
    eframe::run_native("Compositor", options, Box::new(move |cc| Ok(Box::new(App::new(cc, args)))))
}
