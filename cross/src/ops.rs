//! Document edits, independent of the window: each takes the document and changes it in place.

use crate::adjust::{Adjustment, AdjustmentKind};
use crate::geom::Point;
use crate::model::{Document, Layer, LayerMask, LayerTransform};
use crate::raster::{Gray, MaskAsset, PixelAsset, Rgba};
use crate::render::{render, Target};
use crate::selection::Selection;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// Where a new layer goes: just above the active one, in its folder — or inside the active folder.
fn insertion(doc: &Document) -> (usize, Option<Uuid>) {
    match doc.active_layer() {
        Some(a) if a.is_group => (doc.index(a.id).unwrap() + 1, Some(a.id)),
        Some(a) => (doc.index(a.id).unwrap() + 1, a.parent),
        None => (doc.layers.len(), None),
    }
}

pub fn add_layer(doc: &mut Document) -> Uuid {
    let mut layer = Layer::blank(&doc.unique_name("Layer"), doc.width, doc.height);
    let (index, parent) = insertion(doc);
    layer.parent = parent;
    let id = layer.id;
    doc.layers.insert(index, layer);
    doc.active = Some(id);
    id
}

pub fn add_folder(doc: &mut Document) -> Uuid {
    let mut folder = Layer::blank(&doc.unique_name("Folder"), doc.width, doc.height);
    folder.is_group = true;
    let (index, parent) = insertion(doc);
    folder.parent = parent;
    let id = folder.id;
    doc.layers.insert(index, folder);
    doc.active = Some(id);
    id
}

pub fn add_adjustment(doc: &mut Document, kind: AdjustmentKind, fg: [u8; 3], bg: [u8; 3]) -> Uuid {
    let mut layer = Layer::blank(kind.label(), doc.width, doc.height);
    let mut adjustment = Adjustment::new(kind);
    if kind == AdjustmentKind::GradientMap {
        let c = |v: [u8; 3]| crate::adjust::AdjustmentColor { red: v[0] as f64 / 255.0, green: v[1] as f64 / 255.0, blue: v[2] as f64 / 255.0 };
        adjustment.gradient_map_settings = Some(crate::adjust::GradientMapSettings { shadows: c(fg), highlights: c(bg), reversed: false });
    }
    layer.adjustment = Some(adjustment);
    let (index, parent) = insertion(doc);
    layer.parent = parent;
    let id = layer.id;
    doc.layers.insert(index, layer);
    doc.active = Some(id);
    id
}

/// A new layer holding `image`, centered on the canvas and scaled down to fit if it's larger.
pub fn place_image(doc: &mut Document, image: Rgba, name: &str) -> Uuid {
    let (w, h) = (image.w as f64, image.h as f64);
    let fit = (doc.width as f64 / w).min(doc.height as f64 / h).min(1.0);
    let (tw, th) = ((w * fit).round().max(1.0), (h * fit).round().max(1.0));
    let mut layer = Layer::blank(&doc.unique_name(name), doc.width, doc.height);
    layer.transform = LayerTransform::new(
        Point::new(((doc.width as f64 - tw) / 2.0).round(), ((doc.height as f64 - th) / 2.0).round()),
        tw,
        th,
    );
    layer.pixels = Some(Arc::new(PixelAsset::new(image)));
    let (index, parent) = insertion(doc);
    layer.parent = parent;
    let id = layer.id;
    doc.layers.insert(index, layer);
    doc.active = Some(id);
    id
}

/// Removes a layer — and, for a folder, everything inside — releasing clipping links to it.
pub fn delete_layer(doc: &mut Document, id: Uuid) {
    let mut gone: HashSet<Uuid> = doc.descendants(id).into_iter().collect();
    gone.insert(id);
    let index = doc.index(id).unwrap_or(0);
    doc.layers.retain(|l| !gone.contains(&l.id));
    for l in &mut doc.layers {
        if l.mask_source.is_some_and(|s| gone.contains(&s)) {
            l.mask_source = None;
        }
    }
    doc.active = doc.layers.get(index.saturating_sub(1).min(doc.layers.len().saturating_sub(1))).map(|l| l.id);
}

/// Copies a layer (a folder with everything inside) just above it.
pub fn duplicate_layer(doc: &mut Document, id: Uuid) -> Option<Uuid> {
    let index = doc.index(id)?;
    let mut ids: HashMap<Uuid, Uuid> = HashMap::new();
    ids.insert(id, Uuid::new_v4());
    let inside = doc.descendants(id);
    for d in &inside {
        ids.insert(*d, Uuid::new_v4());
    }
    let mut copies: Vec<Layer> = Vec::new();
    for l in doc.layers.iter().filter(|l| l.id == id || inside.contains(&l.id)) {
        let mut c = l.clone();
        c.id = ids[&l.id];
        if l.id == id {
            c.name = doc.unique_name(&format!("{} copy", l.name));
        } else {
            c.parent = l.parent.map(|p| *ids.get(&p).unwrap_or(&p));
        }
        c.mask_source = l.mask_source.map(|s| *ids.get(&s).unwrap_or(&s));
        copies.push(c);
    }
    // Insert after the original and everything inside it, so the copy draws above.
    let last = doc.layers.iter().rposition(|l| l.id == id || inside.contains(&l.id)).unwrap_or(index);
    let new_id = ids[&id];
    for (k, c) in copies.into_iter().enumerate() {
        doc.layers.insert(last + 1 + k, c);
    }
    doc.active = Some(new_id);
    Some(new_id)
}

/// Swaps a layer with its neighbor among the folder's children. `up` moves it toward the top.
pub fn move_layer(doc: &mut Document, id: Uuid, up: bool) -> bool {
    let Some(layer) = doc.layer(id) else { return false };
    let parent = layer.parent;
    let siblings: Vec<usize> = doc.layers.iter().enumerate().filter(|(_, l)| l.parent == parent).map(|(i, _)| i).collect();
    let Some(pos) = siblings.iter().position(|&i| doc.layers[i].id == id) else { return false };
    let other = if up { siblings.get(pos + 1) } else { pos.checked_sub(1).and_then(|p| siblings.get(p)) };
    let Some(&other) = other else { return false };
    let me = siblings[pos];
    doc.layers.swap(me, other);
    true
}

/// Puts a layer into a new folder in its place.
pub fn group_layer(doc: &mut Document, id: Uuid) -> Option<Uuid> {
    let index = doc.index(id)?;
    let mut folder = Layer::blank(&doc.unique_name("Folder"), doc.width, doc.height);
    folder.is_group = true;
    folder.parent = doc.layers[index].parent;
    let folder_id = folder.id;
    doc.layers[index].parent = Some(folder_id);
    doc.layers[index].mask_source = None;
    doc.layers.insert(index, folder);
    doc.active = Some(folder_id);
    Some(folder_id)
}

/// Moves a folder's contents out to where the folder was, and removes it.
pub fn ungroup(doc: &mut Document, folder: Uuid) {
    let Some(f) = doc.layer(folder) else { return };
    if !f.is_group {
        return;
    }
    let parent = f.parent;
    let opacity = f.opacity;
    for l in &mut doc.layers {
        if l.parent == Some(folder) {
            l.parent = parent;
            l.opacity *= opacity;
        }
    }
    let index = doc.index(folder).unwrap();
    doc.layers.remove(index);
    doc.active = doc.layers.get(index.min(doc.layers.len().saturating_sub(1))).map(|l| l.id);
}

/// Takes a layer out of its folder, placing it just above the folder.
pub fn release_from_folder(doc: &mut Document, id: Uuid) {
    let Some(parent) = doc.layer(id).and_then(|l| l.parent) else { return };
    let grand = doc.layer(parent).and_then(|p| p.parent);
    let index = doc.index(id).unwrap();
    let mut layer = doc.layers.remove(index);
    layer.parent = grand;
    layer.mask_source = None;
    let last_inside = doc
        .layers
        .iter()
        .rposition(|l| l.id == parent || doc.ancestors(l.id).contains(&parent))
        .unwrap_or(doc.layers.len() - 1);
    doc.layers.insert(last_inside + 1, layer);
}

/// The sibling directly below a layer, which clipping clips it to.
pub fn layer_below(doc: &Document, id: Uuid) -> Option<Uuid> {
    let layer = doc.layer(id)?;
    let siblings: Vec<&Layer> = doc.layers.iter().filter(|l| l.parent == layer.parent).collect();
    let pos = siblings.iter().position(|l| l.id == id)?;
    siblings.get(pos.checked_sub(1)?).map(|l| l.id)
}

pub fn create_clipping(doc: &mut Document, id: Uuid) -> Result<(), &'static str> {
    let below = layer_below(doc, id).ok_or("There is no layer below to clip to.")?;
    let b = doc.layer(below).unwrap();
    if b.is_group {
        return Err("A layer can't clip to a folder.");
    }
    // Clipped layers share their base: clip to the base the layer below is clipped to, if any.
    let base = b.mask_source.unwrap_or(below);
    if doc.layer(id).is_some_and(|l| l.is_group) {
        return Err("Folders can't be clipped.");
    }
    doc.layer_mut(id).unwrap().mask_source = Some(base);
    Ok(())
}

/// Coverage on a `w × h` grid placed by `rect`, read from the selection.
fn selection_in_grid(sel: &Selection, rect: &LayerTransform, w: usize, h: usize) -> Gray {
    let map = rect.pixel_to_document(w, h);
    let mut g = Gray::new(w, h, 0);
    g.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = (sel.at(map.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5))) * 255.0).round() as u8;
        }
    });
    g
}

/// Adds a mask: all white or all black, or from the selection when there is one.
pub fn add_mask(doc: &mut Document, id: Uuid, reveal: bool) {
    let selection = doc.selection.clone();
    let Some(layer) = doc.layer_mut(id) else { return };
    let mask = match selection {
        Some(sel) => {
            let (w, h) = match &layer.pixels {
                Some(p) => (p.image.w, p.image.h),
                None => (layer.transform.size.0.round().max(1.0) as usize, layer.transform.size.1.round().max(1.0) as usize),
            };
            let mut g = selection_in_grid(&sel, &layer.transform, w, h);
            if !reveal {
                g.data.iter_mut().for_each(|v| *v = 255 - *v);
            }
            LayerMask { asset: Arc::new(MaskAsset::new(g)), enabled: true, placement: None, linked: true }
        }
        None => LayerMask::solid(reveal),
    };
    layer.mask = Some(mask);
    doc.selection = None;
}

pub fn invert_mask(doc: &mut Document, id: Uuid) {
    if let Some(mask) = doc.layer_mut(id).and_then(|l| l.mask.as_mut()) {
        let mut g = mask.asset.image.clone();
        g.data.par_iter_mut().for_each(|v| *v = 255 - *v);
        mask.asset = Arc::new(MaskAsset::new(g));
    }
}

/// The layer drawn alone, with its mask and clipping, onto a canvas-sized buffer.
fn render_alone(doc: &Document, ids: &[Uuid]) -> Rgba {
    let mut temp = doc.clone();
    temp.selection = None;
    let keep: HashSet<Uuid> = ids.iter().copied().collect();
    temp.layers.retain(|l| keep.contains(&l.id));
    for l in &mut temp.layers {
        l.parent = None;
        l.visible = true;
    }
    render(&temp, Target::full(doc))
}

/// Merges a layer into the one below it.
pub fn merge_down(doc: &mut Document, id: Uuid) -> Result<(), &'static str> {
    let below = layer_below(doc, id).ok_or("There is no layer below to merge into.")?;
    let (upper, lower) = (doc.layer(id).unwrap(), doc.layer(below).unwrap());
    if upper.is_group || lower.is_group || lower.adjustment.is_some() {
        return Err("Merge Down needs two pixel layers.");
    }
    let mut ids = vec![below, id];
    if !upper.visible {
        ids.pop();
    }
    let merged = render_alone(doc, &ids);
    let index = doc.index(below).unwrap();
    let l = &mut doc.layers[index];
    l.pixels = Some(Arc::new(PixelAsset::new(merged)));
    l.transform = LayerTransform::new(Point::default(), doc.width as f64, doc.height as f64);
    l.mask = None;
    l.opacity = 1.0;
    l.visible = true;
    l.rasterize_metadata();
    l.extra.remove("effects");
    for other in &mut doc.layers {
        if other.mask_source == Some(id) {
            other.mask_source = Some(below);
        }
    }
    doc.layers.retain(|l| l.id != id);
    doc.active = Some(below);
    Ok(())
}

/// Replaces every layer with one holding the flattened image.
pub fn flatten(doc: &mut Document) {
    let flat = render(doc, Target::full(doc));
    let mut layer = Layer::blank("Background", doc.width, doc.height);
    layer.pixels = Some(Arc::new(PixelAsset::new(flat)));
    doc.active = Some(layer.id);
    doc.layers = vec![layer];
}

/// A selection from where a layer has alpha.
pub fn transparency_selection(doc: &Document, id: Uuid) -> Option<Selection> {
    let alone = render_alone(doc, &[id]);
    let g = Gray { w: alone.w, h: alone.h, data: alone.data.par_chunks(4).map(|p| p[3]).collect() };
    Selection::from_mask(g)
}

/// The composite color under a document point.
pub fn sample_color(doc: &Document, p: Point) -> Option<[u8; 4]> {
    if !doc.bounds().contains(p) {
        return None;
    }
    let t = Target { w: 1, h: 1, origin: Point::new(p.x.floor(), p.y.floor()), scale: 1.0 };
    let px = render(doc, t);
    let mut c = [px.data[0], px.data[1], px.data[2], px.data[3]];
    let a = c[3] as u32;
    if a > 0 && a < 255 {
        for v in &mut c[..3] {
            *v = ((*v as u32 * 255) / a).min(255) as u8;
        }
    }
    Some(c)
}

/// Moves a layer (or a folder with everything inside) by `dx, dy`, carrying linked masks.
pub fn translate(doc: &mut Document, id: Uuid, dx: f64, dy: f64) {
    let mut ids = doc.descendants(id);
    ids.push(id);
    for l in doc.layers.iter_mut().filter(|l| ids.contains(&l.id)) {
        let old = l.transform;
        l.transform.origin.x += dx;
        l.transform.origin.y += dy;
        let new = l.transform;
        if let Some(m) = l.mask.as_mut() {
            m.placement = m.placement_moving(&old, &new);
        }
    }
}

/// Sets a layer's transform, carrying its mask as linked masks follow.
pub fn set_transform(layer: &mut Layer, from: &LayerTransform, original_mask: Option<LayerTransform>, to: LayerTransform) {
    layer.transform = to;
    if let Some(m) = layer.mask.as_mut() {
        m.placement = original_mask;
        m.placement = m.placement_moving(from, &to);
    }
}
