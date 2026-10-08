//! The right-hand panels: the layer list, the active layer's properties, and adjustment editors.

use crate::adjust::{Adjustment, AdjustmentColor, AdjustmentKind, ColorRange, CurvePoint, LevelsChannel};
use crate::app::{App, Project};
use crate::model::{BlendMode, Sampling};
use crate::ops;
use eframe::egui::{self, Color32, Pos2, Sense, Stroke as Line, Vec2};
use uuid::Uuid;

const THUMB: f32 = 30.0;

/// A small texture of a layer's pixels or mask, rebuilt when they change.
fn thumbnail(ctx: &egui::Context, p: &mut Project, id: Uuid, mask: bool) -> Option<egui::TextureId> {
    let layer = p.doc.layer(id)?;
    let (key, image) = if mask {
        let m = layer.mask.as_ref()?;
        let ptr = std::sync::Arc::as_ptr(&m.asset) as usize;
        if let Some((k, t)) = p.thumbs.get(&(id, true)) {
            if *k == ptr {
                return Some(t.id());
            }
        }
        let level = m.asset.mips.iter().rev().find(|g| g.w.max(g.h) >= 48).unwrap_or(&m.asset.image);
        let (w, h) = fit(level.w, level.h);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let v = level.data[(y * level.h / h) * level.w + x * level.w / w];
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        (ptr, egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
    } else {
        let px = layer.pixels.as_ref()?;
        let ptr = std::sync::Arc::as_ptr(px) as usize;
        if let Some((k, t)) = p.thumbs.get(&(id, false)) {
            if *k == ptr {
                return Some(t.id());
            }
        }
        let level = px.mips.iter().rev().find(|g| g.w.max(g.h) >= 48).unwrap_or(&px.image);
        let (w, h) = fit(level.w, level.h);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * level.h / h) * level.w + x * level.w / w) * 4;
                // Over a light checker so transparency reads.
                let a = level.data[i + 3] as u32;
                let bg = if (x / 4 + y / 4) % 2 == 0 { 255 } else { 205 };
                for c in 0..3 {
                    rgba.push((level.data[i + c] as u32 + bg * (255 - a) / 255).min(255) as u8);
                }
                rgba.push(255);
            }
        }
        (ptr, egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
    };
    let texture = ctx.load_texture(format!("thumb-{id}-{mask}"), image, egui::TextureOptions::LINEAR);
    let tid = texture.id();
    p.thumbs.insert((id, mask), (key, texture));
    Some(tid)
}

fn fit(w: usize, h: usize) -> (usize, usize) {
    let s = 48.0 / w.max(h) as f64;
    (((w as f64 * s).round() as usize).max(1), ((h as f64 * s).round() as usize).max(1))
}

impl App {
    pub fn side_panels(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        if self.project_ref().is_none() {
            ui.label("No document open.");
            return;
        }
        egui::Panel::bottom("properties")
            .resizable(true)
            .default_size(360.0)
            .min_size(120.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.properties(ctx, ui);
                });
            });
        ui.horizontal(|ui| {
            ui.heading("Layers");
        });
        self.layer_controls(ui);
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.layer_list(ctx, ui);
        });
    }

    /// Blend mode and opacity for the active layer, above the list as in Photoshop.
    fn layer_controls(&mut self, ui: &mut egui::Ui) {
        let Some(p) = self.project() else { return };
        let Some(id) = p.doc.active else { return };
        let Some(layer) = p.doc.layer(id).cloned() else { return };
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!layer.is_group, |ui| {
                let mut mode = layer.blend;
                egui::ComboBox::from_id_salt("blend")
                    .width(130.0)
                    .selected_text(mode.label())
                    .show_ui(ui, |ui| {
                        for (gi, group) in BlendMode::GROUPS.iter().enumerate() {
                            if gi > 0 {
                                ui.separator();
                            }
                            for m in group.iter() {
                                ui.selectable_value(&mut mode, *m, m.label());
                            }
                        }
                    });
                if mode != layer.blend {
                    p.checkpoint("Layer Blend Mode");
                    p.doc.layer_mut(id).unwrap().blend = mode;
                    p.changed();
                }
            });
            let mut opacity = layer.opacity * 100.0;
            let r = ui.add(egui::DragValue::new(&mut opacity).range(0.0..=100.0).suffix("%").speed(0.5).max_decimals(0));
            if r.changed() {
                p.begin_edit("Layer Opacity");
                p.doc.layer_mut(id).unwrap().opacity = (opacity / 100.0).clamp(0.0, 1.0);
                p.revision += 1;
            }
            r.on_hover_text("Opacity");
        });
    }

    fn layer_list(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let p = &mut self.projects[self.current];
        // Thumbnails of layers that are gone.
        let ids: std::collections::HashSet<Uuid> = p.doc.layers.iter().map(|l| l.id).collect();
        p.thumbs.retain(|(id, _), _| ids.contains(id));
        let entries = p.doc.entries(true, &p.collapsed);
        let active = p.doc.active;
        let mut clicked: Option<(Uuid, bool)> = None;
        let mut toggle_visible = None;
        let mut toggle_collapse = None;
        let mut start_rename = None;
        let mut finish_rename = None;
        for entry in entries {
            let layer = p.doc.layers[entry.index].clone();
            let selected = Some(layer.id) == active;
            let row = ui.horizontal(|ui| {
                ui.set_min_height(THUMB + 4.0);
                ui.add_space(entry.depth as f32 * 14.0);
                let eye = if layer.visible { "👁" } else { "  " };
                if ui.add(egui::Button::new(eye).frame(false).min_size(Vec2::new(20.0, THUMB))).on_hover_text("Show or hide").clicked() {
                    toggle_visible = Some(layer.id);
                }
                if layer.is_group {
                    let arrow = if p.collapsed.contains(&layer.id) { "▶" } else { "▼" };
                    if ui.add(egui::Button::new(arrow).frame(false)).clicked() {
                        toggle_collapse = Some(layer.id);
                    }
                }
                if layer.mask_source.is_some() {
                    ui.label("↳").on_hover_text("Clipped to the layer below");
                }
                // Pixels (or a symbol for folders and adjustments), then the mask.
                let target_pixels = selected && !p.on_mask;
                if layer.is_group {
                    ui.label(egui::RichText::new("📁").size(20.0));
                } else if let Some(adj) = &layer.adjustment {
                    ui.label(egui::RichText::new("◐").size(20.0)).on_hover_text(adj.kind.label());
                } else if let Some(tex) = thumbnail(ctx, p, layer.id, false) {
                    let r = ui.add(egui::Image::new((tex, Vec2::splat(THUMB))).sense(Sense::click()));
                    if target_pixels && layer.mask.is_some() {
                        ui.painter().rect_stroke(r.rect.expand(1.0), 0.0, Line::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
                    }
                    if r.clicked() {
                        clicked = Some((layer.id, false));
                    }
                } else {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(THUMB), Sense::click());
                    ui.painter().rect_stroke(r, 0.0, Line::new(1.0, Color32::from_gray(90)), egui::StrokeKind::Inside);
                }
                if layer.mask.is_some() {
                    if let Some(tex) = thumbnail(ctx, p, layer.id, true) {
                        let r = ui.add(egui::Image::new((tex, Vec2::splat(THUMB))).sense(Sense::click())).on_hover_text("Click to paint on the mask");
                        if selected && p.on_mask {
                            ui.painter().rect_stroke(r.rect.expand(1.0), 0.0, Line::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
                        }
                        if layer.mask.as_ref().is_some_and(|m| !m.enabled) {
                            ui.painter().line_segment([r.rect.left_top(), r.rect.right_bottom()], Line::new(2.0, Color32::RED));
                        }
                        if r.clicked() {
                            clicked = Some((layer.id, true));
                        }
                    }
                }
                match &mut p.renaming {
                    Some((rid, text)) if *rid == layer.id => {
                        let r = ui.text_edit_singleline(text);
                        r.request_focus();
                        if r.lost_focus() {
                            finish_rename = Some((layer.id, text.clone()));
                        }
                    }
                    _ => {
                        let mut name = egui::RichText::new(&layer.name);
                        if !layer.visible {
                            name = name.weak();
                        }
                        if layer.has_effects() {
                            name = name.italics();
                        }
                        let r = ui.add(egui::Label::new(name).sense(Sense::click()).truncate());
                        if r.double_clicked() {
                            start_rename = Some((layer.id, layer.name.clone()));
                        } else if r.clicked() {
                            clicked = Some((layer.id, false));
                        }
                    }
                }
            });
            let rect = row.response.rect;
            if selected {
                ui.painter().rect_filled(rect, 3.0, Color32::from_rgba_unmultiplied(70, 130, 220, 50));
            }
            let row_click = ui.interact(rect, ui.id().with(("row", layer.id)), Sense::click());
            if row_click.clicked() && clicked.is_none() {
                clicked = Some((layer.id, false));
            }
        }
        if let Some(id) = toggle_visible {
            p.checkpoint("Layer Visibility");
            let l = p.doc.layer_mut(id).unwrap();
            l.visible = !l.visible;
            p.changed();
        }
        if let Some(id) = toggle_collapse {
            if !p.collapsed.remove(&id) {
                p.collapsed.insert(id);
            }
        }
        if let Some((id, mask)) = clicked {
            p.doc.active = Some(id);
            p.on_mask = mask && p.doc.layer(id).is_some_and(|l| l.mask.is_some());
        }
        if let Some(r) = start_rename {
            p.renaming = Some(r);
        }
        if let Some((id, name)) = finish_rename {
            p.renaming = None;
            let name = name.trim().to_string();
            if !name.is_empty() && p.doc.layer(id).is_some_and(|l| l.name != name) {
                p.checkpoint("Rename Layer");
                p.doc.layer_mut(id).unwrap().name = name;
                p.changed();
            }
        }
    }

    fn properties(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("＋ Layer").on_hover_text("New layer (Ctrl+Shift+N)").clicked() {
                self.edit("New Layer", |d| {
                    ops::add_layer(d);
                });
            }
            if ui.button("📁 Folder").clicked() {
                self.edit("New Folder", |d| {
                    ops::add_folder(d);
                });
            }
            ui.menu_button("◐ Adjustment", |ui| {
                for kind in AdjustmentKind::ALL {
                    if ui.button(kind.label()).clicked() {
                        ui.close();
                        self.add_adjustment(kind);
                    }
                }
            });
            let id = self.active_id();
            if let Some(id) = id {
                if ui.button("⬆").on_hover_text("Move up").clicked() {
                    self.edit("Move Layer", |d| {
                        ops::move_layer(d, id, true);
                    });
                }
                if ui.button("⬇").on_hover_text("Move down").clicked() {
                    self.edit("Move Layer", |d| {
                        ops::move_layer(d, id, false);
                    });
                }
                if ui.button("⧉").on_hover_text("Duplicate (Ctrl+J)").clicked() {
                    self.edit("Duplicate Layer", |d| {
                        ops::duplicate_layer(d, id);
                    });
                }
                if ui.button("🗑").on_hover_text("Delete layer").clicked() {
                    self.edit("Delete Layer", |d| ops::delete_layer(d, id));
                }
            }
        });
        ui.separator();
        let Some(id) = self.active_id() else { return };
        let Some(layer) = self.project_ref().and_then(|p| p.doc.layer(id).cloned()) else { return };
        ui.label(egui::RichText::new(format!("{} — {}", layer.kind_label(), layer.name)).strong());
        if layer.has_effects() {
            ui.label(egui::RichText::new("Layer effects are kept in the file but not drawn by this version.").weak());
        }
        if layer.extra.contains_key("text") {
            ui.label(egui::RichText::new("Editable text is kept; painting on it turns it into pixels.").weak());
        }

        if let Some(adjustment) = &layer.adjustment {
            egui::CollapsingHeader::new(format!("{} settings", adjustment.kind.label())).default_open(true).show(ui, |ui| {
                let mut edited = adjustment.clone();
                if adjustment_editor(ui, &mut edited, &mut self.balance_tone) && edited != *adjustment {
                    if let Some(p) = self.project() {
                        p.begin_edit("Adjustment");
                        p.doc.layer_mut(id).unwrap().adjustment = Some(edited);
                        p.revision += 1;
                    }
                }
            });
        }

        egui::CollapsingHeader::new("Transform").default_open(layer.adjustment.is_none()).show(ui, |ui| {
            let mut t = layer.transform;
            let mut changed = false;
            egui::Grid::new("transform").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                ui.label("X");
                changed |= ui.add(egui::DragValue::new(&mut t.origin.x).speed(1.0).max_decimals(1)).changed();
                ui.label("Y");
                changed |= ui.add(egui::DragValue::new(&mut t.origin.y).speed(1.0).max_decimals(1)).changed();
                ui.end_row();
                ui.label("W");
                changed |= ui.add(egui::DragValue::new(&mut t.size.0).speed(1.0).range(1.0..=300_000.0).max_decimals(1)).changed();
                ui.label("H");
                changed |= ui.add(egui::DragValue::new(&mut t.size.1).speed(1.0).range(1.0..=300_000.0).max_decimals(1)).changed();
                ui.end_row();
                ui.label("Angle");
                changed |= ui.add(egui::DragValue::new(&mut t.rotation).speed(0.5).suffix("°").max_decimals(1)).changed();
                ui.label("");
                ui.horizontal(|ui| {
                    changed |= ui.toggle_value(&mut t.flip_x, "⇋").on_hover_text("Flipped horizontally").changed();
                    changed |= ui.toggle_value(&mut t.flip_y, "⇵").on_hover_text("Flipped vertically").changed();
                });
                ui.end_row();
            });
            if let Some(px) = &layer.pixels {
                ui.horizontal(|ui| {
                    ui.label(format!("{:.1}% of {} × {} px", t.size.0 / px.image.w as f64 * 100.0, px.image.w, px.image.h));
                    if ui.small_button("100%").clicked() {
                        let c = t.center();
                        t.size = (px.image.w as f64, px.image.h as f64);
                        t.origin = crate::geom::Point::new((c.x - t.size.0 / 2.0).round(), (c.y - t.size.1 / 2.0).round());
                        changed = true;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label("Sampling");
                egui::ComboBox::from_id_salt("sampling").selected_text(t.sampling.label()).show_ui(ui, |ui| {
                    for s in Sampling::ALL {
                        changed |= ui.selectable_value(&mut t.sampling, s, s.label()).changed();
                    }
                });
            });
            if changed && t.is_valid() && t != layer.transform {
                if let Some(p) = self.project() {
                    p.begin_edit("Transform");
                    let original_mask = layer.mask.as_ref().and_then(|m| m.placement);
                    if let Some(l) = p.doc.layer_mut(id) {
                        ops::set_transform(l, &layer.transform, original_mask, t);
                    }
                    p.revision += 1;
                }
            }
        });

        egui::CollapsingHeader::new("Mask").default_open(layer.mask.is_some()).show(ui, |ui| match &layer.mask {
            None => {
                ui.horizontal(|ui| {
                    if ui.button("Add (Reveal)").clicked() {
                        self.edit("Add Layer Mask", |d| ops::add_mask(d, id, true));
                    }
                    if ui.button("Add (Hide)").clicked() {
                        self.edit("Add Layer Mask", |d| ops::add_mask(d, id, false));
                    }
                });
                ui.label(egui::RichText::new("With a selection, the mask reveals (or hides) just the selection.").weak());
            }
            Some(mask) => {
                let mut enabled = mask.enabled;
                let mut linked = mask.linked;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut enabled, "Enabled").changed() {
                        self.edit("Mask Enabled", |d| d.layer_mut(id).unwrap().mask.as_mut().unwrap().enabled = enabled);
                    }
                    if ui.checkbox(&mut linked, "Linked").on_hover_text("Linked masks move with the layer").changed() {
                        self.edit("Link Mask", |d| {
                            let l = d.layer_mut(id).unwrap();
                            let t = l.transform;
                            let m = l.mask.as_mut().unwrap();
                            if !linked && m.placement.is_none() && (m.asset.image.w > 1 || m.asset.image.h > 1) {
                                m.placement = Some(t);
                            }
                            m.linked = linked;
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if let Some(p) = self.project() {
                        ui.label("Paint on:");
                        ui.selectable_value(&mut p.on_mask, false, "Pixels");
                        ui.selectable_value(&mut p.on_mask, true, "Mask");
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Invert").clicked() {
                        self.edit("Invert Mask", |d| ops::invert_mask(d, id));
                    }
                    if ui.button("Delete").clicked() {
                        self.edit("Delete Mask", |d| d.layer_mut(id).unwrap().mask = None);
                        if let Some(p) = self.project() {
                            p.on_mask = false;
                        }
                    }
                });
            }
        });

        if !layer.is_group {
            ui.horizontal(|ui| {
                let label = if layer.mask_source.is_some() { "Release Clipping Mask" } else { "Clip to Layer Below" };
                if ui.button(label).clicked() {
                    self.toggle_clipping(ctx);
                }
            });
        }
    }
}

fn slider(ui: &mut egui::Ui, label: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>) -> bool {
    ui.add(egui::Slider::new(value, range).text(label)).changed()
}

/// Edits an adjustment's settings in place; true when anything changed.
pub fn adjustment_editor(ui: &mut egui::Ui, a: &mut Adjustment, tone: &mut usize) -> bool {
    let mut changed = false;
    match a.kind {
        AdjustmentKind::Hsv => {
            let mut s = a.hsv();
            egui::ComboBox::from_id_salt("hsv-range").selected_text(s.range.label()).show_ui(ui, |ui| {
                for r in ColorRange::ALL {
                    changed |= ui.selectable_value(&mut s.range, r, r.label()).changed();
                }
            });
            let colorize = s.colorize;
            let cur = s.current_mut();
            let (hue_range, sat_range) = if colorize { (0.0..=360.0, 0.0..=100.0) } else { (-180.0..=180.0, -100.0..=100.0) };
            changed |= slider(ui, "Hue", &mut cur.hue, hue_range);
            changed |= slider(ui, "Saturation", &mut cur.saturation, sat_range);
            changed |= slider(ui, "Lightness", &mut cur.lightness, -100.0..=100.0);
            let mut c = s.colorize;
            if ui.checkbox(&mut c, "Colorize").changed() {
                changed = true;
                s.colorize = c;
                if c {
                    s.range = ColorRange::Master;
                    let cur = s.current_mut();
                    if cur.saturation <= 0.0 {
                        cur.saturation = 25.0;
                    }
                    cur.hue = cur.hue.rem_euclid(360.0);
                }
            }
            if changed {
                a.hsv_settings = Some(s);
            }
        }
        AdjustmentKind::Levels => {
            let l = &mut a.levels;
            egui::ComboBox::from_id_salt("levels-channel").selected_text(l.channel.label()).show_ui(ui, |ui| {
                for c in LevelsChannel::ALL {
                    changed |= ui.selectable_value(&mut l.channel, c, c.label()).changed();
                }
            });
            let i = l.channel.index();
            let mut r = l.ranges[i];
            let mut edited = false;
            edited |= slider(ui, "Black", &mut r.black, 0.0..=254.0);
            edited |= ui.add(egui::Slider::new(&mut r.gamma, 0.1..=9.99).logarithmic(true).text("Gamma")).changed();
            edited |= slider(ui, "White", &mut r.white, 1.0..=255.0);
            edited |= slider(ui, "Output black", &mut r.output_black, 0.0..=255.0);
            edited |= slider(ui, "Output white", &mut r.output_white, 0.0..=255.0);
            if edited {
                l.ranges[i] = r.normalized();
                changed = true;
            }
            if ui.small_button("Reset channel").clicked() {
                l.ranges[i] = Default::default();
                changed = true;
            }
        }
        AdjustmentKind::Curves => {
            let c = &mut a.curves;
            egui::ComboBox::from_id_salt("curves-channel").selected_text(c.channel.label()).show_ui(ui, |ui| {
                for ch in LevelsChannel::ALL {
                    changed |= ui.selectable_value(&mut c.channel, ch, ch.label()).changed();
                }
            });
            changed |= curve_widget(ui, c);
            ui.label(egui::RichText::new("Click to add a point · drag to move · double-click a point to remove it").weak());
        }
        AdjustmentKind::Exposure => {
            let mut e = a.exposure();
            changed |= slider(ui, "Exposure", &mut e.exposure, -20.0..=20.0);
            changed |= slider(ui, "Offset", &mut e.offset, -0.5..=0.5);
            changed |= ui.add(egui::Slider::new(&mut e.gamma, 0.01..=9.99).logarithmic(true).text("Gamma")).changed();
            if changed {
                a.exposure_settings = Some(e);
            }
        }
        AdjustmentKind::GradientMap => {
            let mut g = a.gradient_map();
            let mut edit = |ui: &mut egui::Ui, label: &str, c: &mut AdjustmentColor| {
                let mut rgb = [(c.red * 255.0).round() as u8, (c.green * 255.0).round() as u8, (c.blue * 255.0).round() as u8];
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        *c = AdjustmentColor { red: rgb[0] as f64 / 255.0, green: rgb[1] as f64 / 255.0, blue: rgb[2] as f64 / 255.0 };
                        changed = true;
                    }
                });
            };
            edit(ui, "Shadows", &mut g.shadows);
            edit(ui, "Highlights", &mut g.highlights);
            changed |= ui.checkbox(&mut g.reversed, "Reverse").changed();
            if changed {
                a.gradient_map_settings = Some(g);
            }
        }
        AdjustmentKind::Grain => {
            let mut g = a.grain();
            changed |= slider(ui, "Amount", &mut g.amount, 0.0..=100.0);
            changed |= slider(ui, "Size", &mut g.size, 0.5..=20.0);
            changed |= slider(ui, "Roughness", &mut g.roughness, 0.0..=100.0);
            if changed {
                a.grain_settings = Some(g);
            }
        }
        AdjustmentKind::BlackWhite => {
            let mut b = a.black_white();
            for (label, v) in [
                ("Reds", &mut b.reds),
                ("Yellows", &mut b.yellows),
                ("Greens", &mut b.greens),
                ("Cyans", &mut b.cyans),
                ("Blues", &mut b.blues),
                ("Magentas", &mut b.magentas),
            ] {
                changed |= slider(ui, label, v, -200.0..=300.0);
            }
            changed |= ui.checkbox(&mut b.tint, "Tint").changed();
            if b.tint {
                changed |= slider(ui, "Tint hue", &mut b.tint_hue, 0.0..=360.0);
                changed |= slider(ui, "Tint saturation", &mut b.tint_saturation, 0.0..=100.0);
            }
            if changed {
                a.black_white_settings = Some(b);
            }
        }
        AdjustmentKind::ColorBalance => {
            let mut c = a.color_balance();
            ui.horizontal(|ui| {
                ui.selectable_value(tone, 0, "Shadows");
                ui.selectable_value(tone, 1, "Midtones");
                ui.selectable_value(tone, 2, "Highlights");
            });
            let values = match *tone {
                0 => [&mut c.shadow_cyan_red, &mut c.shadow_magenta_green, &mut c.shadow_yellow_blue],
                2 => [&mut c.highlight_cyan_red, &mut c.highlight_magenta_green, &mut c.highlight_yellow_blue],
                _ => [&mut c.mid_cyan_red, &mut c.mid_magenta_green, &mut c.mid_yellow_blue],
            };
            for (label, v) in ["Cyan – Red", "Magenta – Green", "Yellow – Blue"].into_iter().zip(values) {
                changed |= slider(ui, label, v, -100.0..=100.0);
            }
            changed |= ui.checkbox(&mut c.preserve_luminosity, "Preserve luminosity").changed();
            if changed {
                a.color_balance_settings = Some(c);
            }
        }
        AdjustmentKind::GaussianBlur => {
            let mut r = a.gaussian_radius();
            if ui.add(egui::Slider::new(&mut r, 0.1..=250.0).logarithmic(true).text("Radius")).changed() {
                a.blur_radius = Some(r);
                changed = true;
            }
        }
        AdjustmentKind::MotionBlur => {
            let (mut angle, mut distance) = a.motion();
            changed |= slider(ui, "Angle", &mut angle, -90.0..=90.0);
            changed |= ui.add(egui::Slider::new(&mut distance, 1.0..=2000.0).logarithmic(true).text("Distance")).changed();
            if changed {
                a.motion_angle = Some(angle);
                a.motion_distance = Some(distance);
            }
        }
        AdjustmentKind::AddNoise => {
            let mut amount = a.noise_amount.unwrap_or(10.0);
            let mut gaussian = a.noise_gaussian.unwrap_or(false);
            let mut mono = a.noise_monochromatic.unwrap_or(false);
            changed |= ui.add(egui::Slider::new(&mut amount, 0.1..=400.0).logarithmic(true).text("Amount %")).changed();
            changed |= ui.checkbox(&mut gaussian, "Gaussian").changed();
            changed |= ui.checkbox(&mut mono, "Monochromatic").changed();
            if changed {
                a.noise_amount = Some(amount);
                a.noise_gaussian = Some(gaussian);
                a.noise_monochromatic = Some(mono);
            }
        }
        AdjustmentKind::Invert => {
            ui.label("Invert has no settings.");
        }
    }
    changed
}

/// A 256 × 256 curve editor for the selected channel.
fn curve_widget(ui: &mut egui::Ui, c: &mut crate::adjust::CurvesSettings) -> bool {
    let side = ui.available_width().min(240.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(28));
    for i in 1..4 {
        let f = i as f32 / 4.0;
        let x = rect.min.x + f * side;
        let y = rect.min.y + f * side;
        painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], Line::new(1.0, Color32::from_gray(55)));
        painter.line_segment([Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)], Line::new(1.0, Color32::from_gray(55)));
    }
    let channel = c.channel.index();
    let to_screen = |p: &CurvePoint| Pos2::new(rect.min.x + (p.x / 255.0) as f32 * side, rect.max.y - (p.y / 255.0) as f32 * side);
    let to_value = |s: Pos2| ((((s.x - rect.min.x) / side) * 255.0).clamp(0.0, 255.0) as f64, (((rect.max.y - s.y) / side) * 255.0).clamp(0.0, 255.0) as f64);
    let color = [Color32::from_gray(220), Color32::from_rgb(240, 90, 90), Color32::from_rgb(90, 220, 90), Color32::from_rgb(100, 140, 255)][channel];
    painter.line_segment([rect.left_bottom(), rect.right_top()], Line::new(1.0, Color32::from_gray(70)));
    let points: Vec<Pos2> = (0..=64)
        .map(|i| {
            let x = i as f64 / 64.0 * 255.0;
            to_screen(&CurvePoint { x, y: c.value(x, channel) })
        })
        .collect();
    painter.add(egui::Shape::line(points, Line::new(1.5, color)));

    let drag_id = response.id.with("drag");
    let mut changed = false;
    let pts = &mut c.channels[channel];
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(i) = pts.iter().position(|p| to_screen(p).distance(pos) < 8.0) {
                if i != 0 && i != pts.len() - 1 {
                    pts.remove(i);
                    changed = true;
                }
            }
        }
    } else if response.drag_started() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let hit = pts.iter().position(|p| to_screen(p).distance(pos) < 8.0);
            let index = match hit {
                Some(i) => Some(i),
                None if pts.len() < 32 => {
                    let (x, y) = to_value(pos);
                    let at = pts.iter().position(|p| p.x > x).unwrap_or(pts.len() - 1);
                    if at > 0 && x > pts[at - 1].x + 1.0 && x < pts[at].x - 1.0 {
                        pts.insert(at, CurvePoint { x: x.round(), y: y.round() });
                        changed = true;
                        Some(at)
                    } else {
                        None
                    }
                }
                None => None,
            };
            ui.data_mut(|d| d.insert_temp(drag_id, index));
        }
    }
    if response.dragged() {
        let index: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();
        if let (Some(i), Some(pos)) = (index, response.interact_pointer_pos()) {
            if i < pts.len() {
                let (mut x, y) = to_value(pos);
                if i == 0 {
                    x = 0.0;
                } else if i == pts.len() - 1 {
                    x = 255.0;
                } else {
                    x = x.clamp(pts[i - 1].x + 1.0, pts[i + 1].x - 1.0);
                }
                pts[i] = CurvePoint { x: x.round(), y: y.round() };
                changed = true;
            }
        }
    }
    for p in pts.iter() {
        painter.circle_filled(to_screen(p), 4.0, color);
    }
    changed
}
