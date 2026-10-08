//! `.comp` projects: a folder holding `manifest.json` and `images/<layer UUID>.png` (plus
//! `<layer UUID>.mask.png` for masks), exactly as the macOS app writes them. Fields this build
//! doesn't edit are carried through untouched, so a project round-trips without losing anything.

use crate::adjust::Adjustment;
use crate::model::{upper_uuid, uuid_string, BlendMode, Document, Guide, Layer, LayerMask, LayerTransform, MAX_LAYERS, MAX_SIDE};
use crate::raster::{decode_png_gray, decode_png_rgba, encode_png_gray, encode_png_rgba, MaskAsset, PixelAsset};
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub const FORMAT: &str = "com.compositor.project";
/// The version new saves write, and the newest this build reads.
pub const CURRENT_VERSION: i64 = 11;
const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const MAX_ASSET: u64 = 512 * 1024 * 1024;
const PIXEL_BUDGET: usize = 100_000_000;

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: i64,
    #[serde(rename = "colorSpace")]
    color_space: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolution: Option<f64>,
    #[serde(rename = "documentID", with = "upper_uuid")]
    document_id: Uuid,
    width: i64,
    height: i64,
    #[serde(rename = "activeLayerID", default, with = "upper_uuid::option", skip_serializing_if = "Option::is_none")]
    active_layer_id: Option<Uuid>,
    layers: Vec<Record>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guides: Option<Vec<Guide>>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    #[serde(with = "upper_uuid")]
    id: Uuid,
    name: String,
    #[serde(rename = "isVisible")]
    is_visible: bool,
    transform: LayerTransform,
    #[serde(rename = "imageFile", default, skip_serializing_if = "Option::is_none")]
    image_file: Option<String>,
    #[serde(rename = "parentID", default, with = "upper_uuid::option", skip_serializing_if = "Option::is_none")]
    parent_id: Option<Uuid>,
    #[serde(rename = "isGroup", default, skip_serializing_if = "Option::is_none")]
    is_group: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opacity: Option<f64>,
    #[serde(rename = "blendMode", default, skip_serializing_if = "Option::is_none")]
    blend_mode: Option<BlendMode>,
    #[serde(rename = "maskFile", default, skip_serializing_if = "Option::is_none")]
    mask_file: Option<String>,
    #[serde(rename = "maskEnabled", default, skip_serializing_if = "Option::is_none")]
    mask_enabled: Option<bool>,
    #[serde(rename = "maskSourceID", default, with = "upper_uuid::option", skip_serializing_if = "Option::is_none")]
    mask_source_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adjustment: Option<Adjustment>,
    #[serde(rename = "maskPlacement", default, skip_serializing_if = "Option::is_none")]
    mask_placement: Option<LayerTransform>,
    #[serde(rename = "maskLinked", default, skip_serializing_if = "Option::is_none")]
    mask_linked: Option<bool>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Deserialize)]
struct Header {
    format: String,
    version: i64,
}

/// Accepts the project folder itself or its `manifest.json`.
pub fn project_root(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("manifest.json")) {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

pub fn is_project(path: &Path) -> bool {
    let root = project_root(path);
    root.is_dir() && root.join("manifest.json").is_file()
}

fn check_file(file: &Path, root: &Path, max: u64) -> Result<()> {
    let meta = fs::symlink_metadata(file).with_context(|| format!("{} is missing", file.display()))?;
    if !meta.file_type().is_file() {
        bail!("{} is not a regular file", file.display());
    }
    if meta.len() > max {
        bail!("{} is too large", file.display());
    }
    let canonical = fs::canonicalize(file)?;
    if !canonical.starts_with(fs::canonicalize(root)?) {
        bail!("{} lies outside the project", file.display());
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<Document> {
    let root = project_root(path);
    if !root.is_dir() {
        bail!("This is not a valid Compositor project.");
    }
    let manifest_path = root.join("manifest.json");
    check_file(&manifest_path, &root, MAX_MANIFEST)?;
    let bytes = fs::read(&manifest_path)?;
    let header: Header = serde_json::from_slice(&bytes).map_err(|_| anyhow!("This is not a valid Compositor project, or its metadata is damaged."))?;
    if header.format != FORMAT {
        bail!("This is not a valid Compositor project.");
    }
    if !(1..=CURRENT_VERSION).contains(&header.version) {
        bail!("This project uses format version {}. This build supports versions 1–{CURRENT_VERSION}.", header.version);
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).context("The project's metadata is damaged")?;
    validate(&manifest)?;

    let images_dir = root.join("images");
    // Decode every PNG on its own core.
    struct Loaded {
        pixels: Option<Arc<PixelAsset>>,
        mask: Option<Arc<MaskAsset>>,
    }
    let loaded: Vec<Result<Loaded>> = manifest
        .layers
        .par_iter()
        .map(|r| {
            let pixels = match &r.image_file {
                Some(name) => {
                    let file = images_dir.join(name);
                    check_file(&file, &root, MAX_ASSET)?;
                    let img = decode_png_rgba(&fs::read(&file)?).with_context(|| format!("Image {name} is damaged"))?;
                    Some(Arc::new(PixelAsset::new(img)))
                }
                None => None,
            };
            let mask = match &r.mask_file {
                Some(name) => {
                    let file = images_dir.join(name);
                    check_file(&file, &root, MAX_ASSET)?;
                    let img = decode_png_gray(&fs::read(&file)?).with_context(|| format!("Mask {name} is damaged"))?;
                    Some(Arc::new(MaskAsset::new(img)))
                }
                None => None,
            };
            Ok(Loaded { pixels, mask })
        })
        .collect();

    let mut pixels_used = 0usize;
    let mut layers = Vec::with_capacity(manifest.layers.len());
    for (r, l) in manifest.layers.into_iter().zip(loaded) {
        let l = l?;
        if let Some(p) = &l.pixels {
            pixels_used += p.image.w * p.image.h;
            if p.image.w > MAX_SIDE || p.image.h > MAX_SIDE || pixels_used > PIXEL_BUDGET {
                bail!("This project exceeds the supported canvas or 100-megapixel document limit.");
            }
        }
        layers.push(Layer {
            id: r.id,
            name: r.name,
            visible: r.is_visible,
            transform: r.transform,
            parent: r.parent_id,
            is_group: r.is_group.unwrap_or(false),
            opacity: r.opacity.unwrap_or(1.0),
            blend: r.blend_mode.unwrap_or_default(),
            pixels: l.pixels,
            mask: l.mask.map(|asset| LayerMask {
                asset,
                enabled: r.mask_enabled.unwrap_or(true),
                placement: r.mask_placement,
                linked: r.mask_linked.unwrap_or(true),
            }),
            mask_source: r.mask_source_id,
            adjustment: r.adjustment,
            extra: r.extra,
        });
    }
    Ok(Document {
        id: manifest.document_id,
        width: manifest.width as usize,
        height: manifest.height as usize,
        resolution: manifest.resolution.unwrap_or(72.0),
        active: manifest.active_layer_id,
        layers,
        guides: manifest.guides.unwrap_or_default(),
        selection: None,
        extra: manifest.extra,
    })
}

fn validate(m: &Manifest) -> Result<()> {
    let invalid = || anyhow!("This is not a valid Compositor project, or its metadata is damaged.");
    if m.color_space != "sRGB" {
        return Err(invalid());
    }
    if let Some(r) = m.resolution {
        if !r.is_finite() || !(1.0..=9600.0).contains(&r) {
            return Err(invalid());
        }
    }
    if !(1..=MAX_SIDE as i64).contains(&m.width) || !(1..=MAX_SIDE as i64).contains(&m.height) || m.layers.len() > MAX_LAYERS {
        bail!("This project exceeds the supported canvas or layer limits.");
    }
    let mut ids = HashSet::new();
    let groups: HashMap<Uuid, Option<Uuid>> =
        m.layers.iter().filter(|l| l.is_group == Some(true)).map(|l| (l.id, l.parent_id)).collect();
    for l in &m.layers {
        if !ids.insert(l.id) || !l.transform.is_valid() || l.name.trim().is_empty() {
            return Err(invalid());
        }
        let id = uuid_string(&l.id);
        // File names are fixed by the layer's id, so nothing can point outside `images/`.
        if l.image_file.as_ref().is_some_and(|f| !f.eq_ignore_ascii_case(&format!("{id}.png")))
            || l.mask_file.as_ref().is_some_and(|f| !f.eq_ignore_ascii_case(&format!("{id}.mask.png")))
        {
            return Err(invalid());
        }
        if l.is_group == Some(true) && (l.image_file.is_some() || l.adjustment.is_some()) {
            return Err(invalid());
        }
        let opacity = l.opacity.unwrap_or(1.0);
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(invalid());
        }
        // Parents must be folders, without cycles, at most 64 deep.
        let mut seen = HashSet::from([l.id]);
        let mut parent = l.parent_id;
        while let Some(p) = parent {
            if seen.len() > 64 || !seen.insert(p) {
                return Err(invalid());
            }
            parent = *groups.get(&p).ok_or_else(invalid)?;
        }
    }
    if m.active_layer_id.is_some_and(|id| !ids.contains(&id)) {
        return Err(invalid());
    }
    // Clipping links must not loop.
    let sources: HashMap<Uuid, Uuid> = m.layers.iter().filter_map(|l| l.mask_source_id.map(|s| (l.id, s))).collect();
    for (&start, _) in &sources {
        let mut seen = HashSet::from([start]);
        let mut next = sources.get(&start).copied();
        while let Some(s) = next {
            if !ids.contains(&s) || !seen.insert(s) || seen.len() > 256 {
                return Err(invalid());
            }
            next = sources.get(&s).copied();
        }
    }
    Ok(())
}

pub fn save(doc: &Document, path: &Path) -> Result<()> {
    let root = project_root(path);
    // New saves always write the current version, as the Mac app does.
    let version = CURRENT_VERSION;
    let mut records = Vec::with_capacity(doc.layers.len());
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let encoded: Vec<Result<(Option<Vec<u8>>, Option<Vec<u8>>)>> = doc
        .layers
        .par_iter()
        .map(|l| {
            let image = l.pixels.as_ref().map(|p| encode_png_rgba(&p.image)).transpose()?;
            let mask = l.mask.as_ref().map(|m| encode_png_gray(&m.asset.image)).transpose()?;
            Ok((image, mask))
        })
        .collect();
    for (l, e) in doc.layers.iter().zip(encoded) {
        let (image, mask) = e.context("An image could not be encoded. The previous project has not been replaced.")?;
        let id = uuid_string(&l.id);
        let image_file = image.map(|bytes| {
            let name = format!("{id}.png");
            files.push((name.clone(), bytes));
            name
        });
        let mask_file = mask.map(|bytes| {
            let name = format!("{id}.mask.png");
            files.push((name.clone(), bytes));
            name
        });
        records.push(Record {
            id: l.id,
            name: l.name.clone(),
            is_visible: l.visible,
            transform: l.transform,
            image_file,
            parent_id: l.parent,
            is_group: l.is_group.then_some(true),
            opacity: Some(l.opacity),
            blend_mode: (!l.is_group).then_some(l.blend),
            mask_enabled: l.mask.as_ref().map(|m| m.enabled),
            mask_file,
            mask_source_id: l.mask_source,
            adjustment: l.adjustment.clone(),
            mask_placement: l.mask.as_ref().and_then(|m| m.placement),
            mask_linked: l.mask.as_ref().and_then(|m| (!m.linked).then_some(false)),
            extra: l.extra.clone(),
        });
    }
    let manifest = Manifest {
        format: FORMAT.to_string(),
        version,
        color_space: "sRGB".to_string(),
        resolution: Some(doc.resolution),
        document_id: doc.id,
        width: doc.width as i64,
        height: doc.height as i64,
        active_layer_id: doc.active.filter(|id| doc.layer(*id).is_some()),
        layers: records,
        guides: Some(doc.guides.clone()),
        extra: doc.extra.clone(),
    };
    // Sorted keys, as the Mac app writes them.
    let value = serde_json::to_value(&manifest)?;
    let metadata = serde_json::to_vec_pretty(&value)?;
    if metadata.len() as u64 > MAX_MANIFEST {
        bail!("This project's metadata is too large to save.");
    }

    // Write a complete sibling folder, then swap it in, so a failed save never leaves half a project.
    let parent = root.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = root.file_name().ok_or_else(|| anyhow!("Choose a file name for the project"))?.to_string_lossy().to_string();
    let tag = Uuid::new_v4().simple().to_string();
    let staging = parent.join(format!(".{name}.saving-{}", &tag[..8]));
    let backup = parent.join(format!(".{name}.previous-{}", &tag[..8]));
    let write = || -> Result<()> {
        fs::create_dir_all(staging.join("images"))?;
        fs::write(staging.join("manifest.json"), &metadata)?;
        files.par_iter().try_for_each(|(file, bytes)| fs::write(staging.join("images").join(file), bytes))?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = fs::remove_dir_all(&staging);
        return Err(e.context("The project could not be written. The previous project has not been replaced."));
    }
    let existed = root.exists();
    if existed {
        if let Err(e) = fs::rename(&root, &backup) {
            let _ = fs::remove_dir_all(&staging);
            return Err(anyhow!(e).context(format!("Could not replace {} (is a file in it open in another program?)", root.display())));
        }
    }
    if let Err(e) = fs::rename(&staging, &root) {
        if existed {
            let _ = fs::rename(&backup, &root);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(anyhow!(e).context("The project could not be saved"));
    }
    if existed {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Opens an ordinary image as a one-layer document.
pub fn import_image(path: &Path) -> Result<(crate::raster::Rgba, String)> {
    let bytes = fs::read(path)?;
    let img = image::load_from_memory(&bytes).with_context(|| format!("{} is not an image this build can read", path.display()))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w as usize > MAX_SIDE || h as usize > MAX_SIDE || (w as usize * h as usize) > PIXEL_BUDGET {
        bail!("Images can be at most {MAX_SIDE} pixels on a side and 100 megapixels.");
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Image".into());
    Ok((crate::raster::Rgba::from_straight(w as usize, h as usize, rgba.into_raw()), name))
}

pub enum ExportFormat {
    Png,
    Jpeg { quality: u8, background: [u8; 3] },
}

/// Flattens the document and writes it as PNG or JPEG with its resolution.
pub fn export(doc: &Document, path: &Path, format: ExportFormat) -> Result<()> {
    let flat = crate::render::render(doc, crate::render::Target::full(doc));
    let (w, h) = (flat.w as u32, flat.h as u32);
    let mut out = Vec::new();
    match format {
        ExportFormat::Png => {
            let encoder = image::codecs::png::PngEncoder::new(&mut out);
            image::ImageEncoder::write_image(encoder, &flat.to_straight(), w, h, image::ExtendedColorType::Rgba8)?;
            out = set_png_dpi(out, doc.resolution);
        }
        ExportFormat::Jpeg { quality, background } => {
            // Flatten onto the background color: JPEG has no transparency.
            let mut rgb = Vec::with_capacity(flat.w * flat.h * 3);
            for p in flat.data.chunks_exact(4) {
                let k = 255 - p[3] as u32;
                for c in 0..3 {
                    rgb.push((p[c] as u32 + (background[c] as u32 * k + 127) / 255).min(255) as u8);
                }
            }
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            encoder.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(doc.resolution.round().clamp(1.0, 65535.0) as u16));
            encoder.encode(&rgb, w, h, image::ExtendedColorType::Rgb8)?;
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", &Uuid::new_v4().simple().to_string()[..8]));
    fs::write(&tmp, &out)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Inserts a `pHYs` chunk after `IHDR` so the PNG carries the document's resolution.
fn set_png_dpi(png: Vec<u8>, dpi: f64) -> Vec<u8> {
    if png.len() < 33 || &png[12..16] != b"IHDR" {
        return png;
    }
    let ppm = (dpi / 0.0254).round() as u32;
    let mut chunk = Vec::with_capacity(21);
    chunk.extend_from_slice(&9u32.to_be_bytes());
    let mut body = Vec::with_capacity(13);
    body.extend_from_slice(b"pHYs");
    body.extend_from_slice(&ppm.to_be_bytes());
    body.extend_from_slice(&ppm.to_be_bytes());
    body.push(1);
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..33]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[33..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}
