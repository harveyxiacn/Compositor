use std::path::Path;

// The pixel kernels are shared with the macOS app: compile them straight from its source tree.
fn compile_kernels() {
    let rendering = Path::new("../Compositor/Rendering");
    let files = ["AdjustPixels.c", "LensPixels.c", "LevelsPixels.c", "NoisePixels.c"];
    let mut build = cc::Build::new();
    build.include(rendering).warnings(false);
    if build.get_compiler().is_like_msvc() {
        build.flag("/fp:precise");
    } else {
        build.flag("-std=c11");
    }
    for file in files {
        let path = rendering.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }
    build.compile("compositor_pixels");
}

/// Packs the macOS app icon's PNGs into an .ico (PNG-compressed entries) and links it into the .exe.
fn windows_icon() {
    let icons = Path::new("../Compositor/Assets.xcassets/AppIcon.appiconset");
    let sizes = [16u32, 32, 64, 128, 256];
    let images: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .filter_map(|s| std::fs::read(icons.join(format!("app-icon-{s}.png"))).ok().map(|b| (*s, b)))
        .collect();
    if images.is_empty() {
        return;
    }
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, bytes) in &images {
        let side = if *size >= 256 { 0 } else { *size as u8 };
        ico.extend_from_slice(&[side, side, 0, 0, 1, 0, 32, 0]);
        ico.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += bytes.len() as u32;
    }
    for (_, bytes) in &images {
        ico.extend_from_slice(bytes);
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out.join("compositor.ico");
    std::fs::write(&ico_path, ico).unwrap();
    let rc_path = out.join("compositor.rc");
    let escaped = ico_path.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc_path, format!("1 ICON \"{escaped}\"\n")).unwrap();
    for (size, _) in &images {
        println!("cargo:rerun-if-changed={}", icons.join(format!("app-icon-{size}.png")).display());
    }
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional().unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    compile_kernels();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_icon();
    }
}
