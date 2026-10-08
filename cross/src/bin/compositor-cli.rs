//! Command-line companion to the Compositor window, for scripts and batch work.
//!
//! compositor-cli export <project.comp | image> <output.png | output.jpg> [--quality N]
//! compositor-cli info <project.comp>

use compositor::model::Document;
use compositor::project::{self, ExportFormat};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn open(input: &Path) -> anyhow::Result<Document> {
    if project::is_project(input) {
        return project::load(input);
    }
    let (image, name) = project::import_image(input)?;
    let mut doc = Document::new(image.w, image.h, None);
    doc.layers[0].name = name;
    doc.layers[0].pixels = Some(std::sync::Arc::new(compositor::raster::PixelAsset::new(image)));
    Ok(doc)
}

fn usage() -> ExitCode {
    eprintln!("usage:\n  compositor-cli export <project.comp | image> <output.png | output.jpg> [--quality 1-100]\n  compositor-cli info <project.comp>");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("export") if args.len() >= 3 => {
            let (input, output) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
            let quality = args.iter().position(|a| a == "--quality").and_then(|i| args.get(i + 1)).and_then(|q| q.parse().ok()).unwrap_or(90);
            let jpeg = output.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
            let format = if jpeg { ExportFormat::Jpeg { quality, background: [255, 255, 255] } } else { ExportFormat::Png };
            open(&input).and_then(|doc| project::export(&doc, &output, format)).map(|()| println!("wrote {}", output.display()))
        }
        Some("info") if args.len() >= 2 => open(Path::new(&args[1])).map(|doc| {
            println!("{} × {} px, {} ppi, {} layers", doc.width, doc.height, doc.resolution, doc.layers.len());
            let collapsed = Default::default();
            for e in doc.entries(true, &collapsed) {
                let l = &doc.layers[e.index];
                let mut notes = Vec::new();
                if let Some(a) = &l.adjustment {
                    notes.push(a.kind.label().to_string());
                }
                if l.mask.is_some() {
                    notes.push("mask".into());
                }
                if l.mask_source.is_some() {
                    notes.push("clipped".into());
                }
                if l.has_effects() {
                    notes.push("effects (not drawn)".into());
                }
                println!(
                    "{}{} {} [{}{}{}]",
                    "  ".repeat(e.depth),
                    if l.visible { "●" } else { "○" },
                    l.name,
                    l.kind_label(),
                    if l.opacity < 1.0 { format!(", {:.0}%", l.opacity * 100.0) } else { String::new() },
                    if notes.is_empty() { String::new() } else { format!(", {}", notes.join(", ")) }
                );
            }
        }),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("compositor-cli: {e:#}");
            ExitCode::FAILURE
        }
    }
}
