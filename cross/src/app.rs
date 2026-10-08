//! The window: project tabs, menus, shortcuts, dialogs and file handling. The canvas and the panels
//! live in `canvas.rs` and `panels.rs`.

use crate::adjust::AdjustmentKind;
use crate::brush::{edit_region, BrushSettings, RegionOp, Stroke};
use crate::geom::Point;
use crate::history::History;
use crate::model::{Document, LayerTransform};
use crate::ops;
use crate::project::{self, ExportFormat};
use crate::selection::{Combine, Selection, Shape};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Move,
    Marquee,
    Ellipse,
    Brush,
    Eraser,
    Eyedropper,
    Hand,
    Zoom,
}

impl Tool {
    pub const ALL: [Tool; 8] = [Tool::Move, Tool::Marquee, Tool::Ellipse, Tool::Brush, Tool::Eraser, Tool::Eyedropper, Tool::Hand, Tool::Zoom];
    pub fn icon(self) -> &'static str {
        match self {
            Tool::Move => "✥",
            Tool::Marquee => "⬚",
            Tool::Ellipse => "◯",
            Tool::Brush => "🖌",
            Tool::Eraser => "⌫",
            Tool::Eyedropper => "💧",
            Tool::Hand => "✋",
            Tool::Zoom => "🔍",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Move => "Move / Transform (V)",
            Tool::Marquee => "Rectangle Marquee (M)",
            Tool::Ellipse => "Ellipse Marquee (Shift+M)",
            Tool::Brush => "Brush (B)",
            Tool::Eraser => "Eraser (E)",
            Tool::Eyedropper => "Eyedropper (I)",
            Tool::Hand => "Hand (H, or hold Space)",
            Tool::Zoom => "Zoom (Z; Alt-click zooms out)",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DragMode {
    Move,
    Resize(usize),
    Rotate,
}

pub struct TransformDrag {
    /// Each moving layer with its transform and mask placement when the drag began.
    pub layers: Vec<(Uuid, LayerTransform, Option<LayerTransform>)>,
    /// The box being edited (the layer's own transform, or the box around a folder's contents).
    pub original: LayerTransform,
    pub start: Point,
    pub mode: DragMode,
    pub moved: bool,
}

pub enum Interaction {
    None,
    Pan,
    Transform(TransformDrag),
    Paint(Stroke),
    Marquee { start: Point, current: Point, mode: Combine, shape: Shape },
}

pub struct View {
    /// Screen points per document pixel.
    pub zoom: f32,
    /// Where document (0, 0) sits, relative to the canvas area's top-left, in points.
    pub offset: egui::Vec2,
    pub needs_fit: bool,
}

#[derive(Default)]
pub struct CanvasCache {
    pub texture: Option<egui::TextureHandle>,
    /// The view the texture shows: offset and size in output pixels, and the scale.
    pub geometry: Option<(i64, i64, usize, usize, u64)>,
    /// The document revision the texture shows.
    pub revision: u64,
}

pub struct Project {
    pub doc: Document,
    pub path: Option<PathBuf>,
    pub history: History,
    pub dirty: bool,
    pub revision: u64,
    pub view: View,
    pub collapsed: HashSet<Uuid>,
    /// Painting and filling target the active layer's mask rather than its pixels.
    pub on_mask: bool,
    pub renaming: Option<(Uuid, String)>,
    pub canvas: CanvasCache,
    pub interaction: Interaction,
    /// The edit a slider drag is coalescing into one undo step.
    pub edit_label: Option<String>,
    pub thumbs: HashMap<(Uuid, bool), (usize, egui::TextureHandle)>,
    pub last_paint: Option<Point>,
    /// Revisions `from..to` changed only this document area (a brush stroke), so the canvas can
    /// redraw just that part.
    pub damage: Option<(u64, u64, crate::geom::Rect)>,
}

impl Project {
    pub fn new(doc: Document, path: Option<PathBuf>) -> Self {
        Self {
            doc,
            path,
            history: History::default(),
            dirty: false,
            revision: 1,
            view: View { zoom: 1.0, offset: egui::Vec2::ZERO, needs_fit: true },
            collapsed: HashSet::new(),
            on_mask: false,
            renaming: None,
            canvas: CanvasCache::default(),
            interaction: Interaction::None,
            edit_label: None,
            thumbs: HashMap::new(),
            last_paint: None,
            damage: None,
        }
    }
    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }
    /// Records the document before an edit, as one undo step.
    pub fn checkpoint(&mut self, label: &str) {
        self.history.record(label, &self.doc);
        self.edit_label = None;
        self.dirty = true;
    }
    /// Starts (or continues) an edit that several frames of a slider drag share.
    pub fn begin_edit(&mut self, label: &str) {
        if self.edit_label.as_deref() != Some(label) {
            self.history.record(label, &self.doc);
            self.edit_label = Some(label.to_string());
        }
        self.dirty = true;
    }
    pub fn changed(&mut self) {
        self.revision += 1;
        self.dirty = true;
    }
    /// A change confined to `area` of the document.
    pub fn touch(&mut self, area: crate::geom::Rect) {
        let base = self.revision;
        self.revision += 1;
        self.dirty = true;
        self.damage = match self.damage {
            Some((from, to, r)) if to == base => Some((from, self.revision, r.union(&area))),
            _ => Some((base, self.revision, area)),
        };
    }
    pub fn undo(&mut self) -> Option<String> {
        let label = self.history.undo(&mut self.doc)?;
        self.after_history();
        Some(label)
    }
    pub fn redo(&mut self) -> Option<String> {
        let label = self.history.redo(&mut self.doc)?;
        self.after_history();
        Some(label)
    }
    fn after_history(&mut self) {
        self.revision += 1;
        self.dirty = true;
        self.edit_label = None;
        self.interaction = Interaction::None;
        if self.doc.active_layer().is_none_or(|l| l.mask.is_none()) {
            self.on_mask = false;
        }
    }
}

pub struct NewDocDialog {
    pub width: u32,
    pub height: u32,
    pub resolution: f64,
    pub white: bool,
}

pub struct JpegDialog {
    pub quality: u8,
    pub matte: [u8; 3],
}

pub enum Pending {
    CloseTab(usize),
    Quit,
}

pub struct App {
    pub projects: Vec<Project>,
    pub current: usize,
    pub tool: Tool,
    pub brush: BrushSettings,
    pub eraser: BrushSettings,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub status: Option<(String, f64)>,
    pub new_doc: Option<NewDocDialog>,
    pub jpeg: Option<JpegDialog>,
    pub pending: Option<Pending>,
    pub quit_confirmed: bool,
    pub checker: Option<egui::TextureHandle>,
    pub balance_tone: usize,
    pub show_about: bool,
    pub last_dir: Option<PathBuf>,
    /// The canvas area on screen, from the last frame.
    pub canvas_rect: egui::Rect,
    /// The document point under the pointer, for the status bar.
    pub cursor_doc: Option<Point>,
}

const IMAGE_EXTENSIONS: [&str; 9] = ["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif", "PNG"];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        Self::with_context(&cc.egui_ctx, paths)
    }

    pub fn with_context(ctx: &egui::Context, paths: Vec<PathBuf>) -> Self {
        install_fonts(ctx);
        ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            projects: Vec::new(),
            current: 0,
            tool: Tool::Brush,
            brush: BrushSettings::default(),
            eraser: BrushSettings { size: 60.0, hardness: 0.6, opacity: 1.0 },
            fg: [0, 0, 0],
            bg: [255, 255, 255],
            status: None,
            new_doc: None,
            jpeg: None,
            pending: None,
            quit_confirmed: false,
            checker: None,
            balance_tone: 1,
            show_about: false,
            last_dir: None,
            canvas_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
            cursor_doc: None,
        };
        for p in paths {
            app.open_path(&p);
        }
        app
    }

    pub fn project(&mut self) -> Option<&mut Project> {
        self.projects.get_mut(self.current)
    }
    pub fn project_ref(&self) -> Option<&Project> {
        self.projects.get(self.current)
    }
    pub fn say(&mut self, ctx: &egui::Context, message: impl Into<String>) {
        self.status = Some((message.into(), ctx.input(|i| i.time)));
    }
    fn error(&mut self, ctx: &egui::Context, what: &str, e: impl std::fmt::Display) {
        let message = format!("{what}: {e}");
        self.say(ctx, message.clone());
        rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title("Compositor").set_description(message).show();
    }

    // MARK: Files

    pub fn new_document(&mut self, w: usize, h: usize, resolution: f64, white: bool) {
        let mut doc = Document::new(w, h, white.then_some([255, 255, 255, 255]));
        doc.resolution = resolution;
        if !white {
            doc.layers[0].name = "Layer 1".into();
        }
        self.projects.push(Project::new(doc, None));
        self.current = self.projects.len() - 1;
    }

    pub fn open_path(&mut self, path: &Path) {
        if project::is_project(path) {
            let root = project::project_root(path);
            if let Some(i) = self.projects.iter().position(|p| p.path.as_deref() == Some(root.as_path())) {
                self.current = i;
                return;
            }
            match project::load(&root) {
                Ok(doc) => {
                    self.projects.push(Project::new(doc, Some(root.clone())));
                    self.current = self.projects.len() - 1;
                    self.last_dir = root.parent().map(Path::to_path_buf);
                }
                Err(e) => {
                    rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Compositor")
                        .set_description(format!("Could not open {}: {e:#}", root.display()))
                        .show();
                }
            }
            return;
        }
        match project::import_image(path) {
            Ok((image, name)) => {
                let mut doc = Document::new(image.w, image.h, None);
                doc.layers[0].name = name;
                doc.layers[0].pixels = Some(std::sync::Arc::new(crate::raster::PixelAsset::new(image)));
                self.projects.push(Project::new(doc, None));
                self.current = self.projects.len() - 1;
                self.last_dir = path.parent().map(Path::to_path_buf);
            }
            Err(e) => {
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Compositor")
                    .set_description(format!("Could not open {}: {e:#}", path.display()))
                    .show();
            }
        }
    }

    fn dialog(&self) -> rfd::FileDialog {
        let d = rfd::FileDialog::new();
        match &self.last_dir {
            Some(dir) => d.set_directory(dir),
            None => d,
        }
    }

    pub fn open_dialog(&mut self) {
        let picked = self
            .dialog()
            .set_title("Open — choose an image, or a project's manifest.json")
            .add_filter("Images and projects", &["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif", "json"])
            .pick_files();
        for p in picked.unwrap_or_default() {
            self.open_path(&p);
        }
    }

    pub fn open_project_dialog(&mut self) {
        if let Some(dir) = self.dialog().set_title("Open a .comp project folder").pick_folder() {
            if project::is_project(&dir) {
                self.open_path(&dir);
            } else {
                rfd::MessageDialog::new()
                    .set_title("Compositor")
                    .set_description(format!("{} is not a Compositor project (it has no manifest.json).", dir.display()))
                    .show();
            }
        }
    }

    pub fn place_dialog(&mut self, ctx: &egui::Context) {
        if self.project().is_none() {
            return;
        }
        let picked = self.dialog().set_title("Place images as layers").add_filter("Images", &IMAGE_EXTENSIONS).pick_files();
        for p in picked.unwrap_or_default() {
            self.place_path(ctx, &p);
        }
    }

    pub fn place_path(&mut self, ctx: &egui::Context, path: &Path) {
        match project::import_image(path) {
            Ok((image, name)) => {
                if let Some(p) = self.project() {
                    p.checkpoint("Place Image");
                    ops::place_image(&mut p.doc, image, &name);
                    p.changed();
                }
            }
            Err(e) => self.error(ctx, "Could not place the image", format!("{e:#}")),
        }
    }

    /// Saves to the project's folder, asking for one the first time. Returns whether it saved.
    pub fn save(&mut self, ctx: &egui::Context, save_as: bool) -> bool {
        let Some(p) = self.projects.get(self.current) else { return false };
        let target = match (&p.path, save_as) {
            (Some(path), false) => Some(path.clone()),
            _ => {
                let name = format!("{}.comp", p.title().trim_end_matches(".comp"));
                self.dialog().set_title("Save project").set_file_name(name).save_file().map(|mut path| {
                    if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("comp")) {
                        let mut s = path.into_os_string();
                        s.push(".comp");
                        path = PathBuf::from(s);
                    }
                    path
                })
            }
        };
        let Some(path) = target else { return false };
        let p = &mut self.projects[self.current];
        match project::save(&p.doc, &path) {
            Ok(()) => {
                p.path = Some(path.clone());
                p.dirty = false;
                self.last_dir = path.parent().map(Path::to_path_buf);
                self.say(ctx, format!("Saved {}", path.display()));
                true
            }
            Err(e) => {
                self.error(ctx, "Could not save", format!("{e:#}"));
                false
            }
        }
    }

    pub fn export(&mut self, ctx: &egui::Context, format: ExportFormat) {
        let Some(p) = self.project_ref() else { return };
        let (ext, label) = match format {
            ExportFormat::Png => ("png", "PNG image"),
            ExportFormat::Jpeg { .. } => ("jpg", "JPEG image"),
        };
        let name = format!("{}.{ext}", p.title().trim_end_matches(".comp"));
        let Some(path) = self.dialog().set_title("Export").set_file_name(name).add_filter(label, &[ext]).save_file() else { return };
        let doc = self.projects[self.current].doc.clone();
        match project::export(&doc, &path, format) {
            Ok(()) => self.say(ctx, format!("Exported {}", path.display())),
            Err(e) => self.error(ctx, "Could not export", format!("{e:#}")),
        }
    }

    pub fn close_tab(&mut self, index: usize, force: bool) {
        if index >= self.projects.len() {
            return;
        }
        if self.projects[index].dirty && !force {
            self.current = index;
            self.pending = Some(Pending::CloseTab(index));
            return;
        }
        self.projects.remove(index);
        if self.current >= self.projects.len() {
            self.current = self.projects.len().saturating_sub(1);
        }
    }

    // MARK: Edits

    /// Runs an edit on the current project as one undo step, refreshing the canvas.
    pub fn edit(&mut self, label: &str, f: impl FnOnce(&mut Document)) {
        if let Some(p) = self.project() {
            p.checkpoint(label);
            f(&mut p.doc);
            p.changed();
        }
    }

    pub fn active_id(&self) -> Option<Uuid> {
        self.project_ref().and_then(|p| p.doc.active)
    }

    pub fn fill_or_clear(&mut self, ctx: &egui::Context, fill: bool) {
        let fg = self.fg;
        let Some(p) = self.project() else { return };
        let Some(id) = p.doc.active else { return };
        let on_mask = p.on_mask;
        let before = p.doc.clone();
        let op = if fill { RegionOp::Fill(fg) } else { RegionOp::Clear };
        match edit_region(&mut p.doc, id, on_mask, op) {
            Ok(()) => {
                p.history.record(if fill { "Fill" } else { "Clear" }, &before);
                p.thumbs.remove(&(id, on_mask));
                p.changed();
            }
            Err(msg) => {
                p.doc = before;
                self.say(ctx, msg);
            }
        }
    }

    pub fn select_all(&mut self) {
        if let Some(p) = self.project() {
            p.checkpoint("Select All");
            p.doc.selection = Some(Selection::all(p.doc.width, p.doc.height));
            p.changed();
        }
    }

    pub fn deselect(&mut self) {
        if let Some(p) = self.project() {
            if p.doc.selection.is_some() {
                p.checkpoint("Deselect");
                p.doc.selection = None;
                p.changed();
            }
        }
    }

    pub fn invert_selection(&mut self) {
        if let Some(p) = self.project() {
            p.checkpoint("Inverse");
            p.doc.selection = match &p.doc.selection {
                Some(s) => s.inverted(),
                None => Some(Selection::all(p.doc.width, p.doc.height)),
            };
            p.changed();
        }
    }

    pub fn add_adjustment(&mut self, kind: AdjustmentKind) {
        let (fg, bg) = (self.fg, self.bg);
        self.edit(&format!("New {} Adjustment", kind.label()), |d| {
            ops::add_adjustment(d, kind, fg, bg);
        });
    }

    pub fn merge_down(&mut self, ctx: &egui::Context) {
        let Some(id) = self.active_id() else { return };
        let Some(p) = self.project() else { return };
        let before = p.doc.clone();
        match ops::merge_down(&mut p.doc, id) {
            Ok(()) => {
                p.history.record("Merge Down", &before);
                p.changed();
            }
            Err(msg) => {
                p.doc = before;
                self.say(ctx, msg);
            }
        }
    }

    pub fn toggle_clipping(&mut self, ctx: &egui::Context) {
        let Some(id) = self.active_id() else { return };
        let Some(p) = self.project() else { return };
        let clipped = p.doc.layer(id).is_some_and(|l| l.mask_source.is_some());
        let before = p.doc.clone();
        let result = if clipped {
            p.doc.layer_mut(id).unwrap().mask_source = None;
            Ok(())
        } else {
            ops::create_clipping(&mut p.doc, id)
        };
        match result {
            Ok(()) => {
                p.history.record(if clipped { "Release Clipping Mask" } else { "Create Clipping Mask" }, &before);
                p.changed();
            }
            Err(msg) => {
                p.doc = before;
                self.say(ctx, msg);
            }
        }
    }

    pub fn zoom_by(&mut self, factor: f32, anchor: Option<egui::Pos2>, canvas: egui::Rect) {
        if let Some(p) = self.project() {
            let anchor = anchor.unwrap_or(canvas.center());
            let old = p.view.zoom;
            let new = (old * factor).clamp(0.01, 64.0);
            // Keep the document point under the anchor where it is.
            let local = anchor - canvas.min;
            p.view.offset = local - (local - p.view.offset) * (new / old);
            p.view.zoom = new;
        }
    }

    // MARK: Frame

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let pressed = |ctx: &egui::Context, m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let cmd_alt = Modifiers::COMMAND | Modifiers::ALT;
        if pressed(ctx, cmd_shift, Key::N) {
            if self.project_ref().is_some() {
                self.edit("New Layer", |d| {
                    ops::add_layer(d);
                });
            }
        } else if pressed(ctx, cmd, Key::N) {
            self.new_doc = Some(NewDocDialog { width: 1920, height: 1080, resolution: 72.0, white: true });
        }
        if pressed(ctx, cmd_shift, Key::O) {
            self.open_project_dialog();
        } else if pressed(ctx, cmd, Key::O) {
            self.open_dialog();
        }
        if pressed(ctx, cmd_shift, Key::S) {
            self.save(ctx, true);
        } else if pressed(ctx, cmd, Key::S) {
            self.save(ctx, false);
        }
        if pressed(ctx, cmd, Key::W) {
            let i = self.current;
            self.close_tab(i, false);
        }
        if pressed(ctx, cmd_shift, Key::E) {
            self.export(ctx, ExportFormat::Png);
        }
        if typing {
            return;
        }
        if pressed(ctx, cmd_shift, Key::Z) || pressed(ctx, cmd, Key::Y) {
            if let Some(p) = self.project() {
                if let Some(label) = p.redo() {
                    self.say(ctx, format!("Redo {label}"));
                }
            }
        } else if pressed(ctx, cmd, Key::Z) {
            if let Some(p) = self.project() {
                if let Some(label) = p.undo() {
                    self.say(ctx, format!("Undo {label}"));
                }
            }
        }
        if pressed(ctx, cmd, Key::A) {
            self.select_all();
        }
        if pressed(ctx, cmd, Key::D) {
            self.deselect();
        }
        if pressed(ctx, cmd_shift, Key::I) {
            self.invert_selection();
        }
        if pressed(ctx, cmd, Key::J) {
            if let Some(id) = self.active_id() {
                self.edit("Duplicate Layer", |d| {
                    ops::duplicate_layer(d, id);
                });
            }
        }
        if pressed(ctx, cmd_shift, Key::G) {
            if let Some(id) = self.active_id() {
                self.edit("Ungroup", |d| ops::ungroup(d, id));
            }
        } else if pressed(ctx, cmd_alt, Key::G) {
            self.toggle_clipping(ctx);
        } else if pressed(ctx, cmd, Key::G) {
            if let Some(id) = self.active_id() {
                self.edit("Group Layer", |d| {
                    ops::group_layer(d, id);
                });
            }
        }
        if pressed(ctx, cmd, Key::E) {
            self.merge_down(ctx);
        }
        if pressed(ctx, Modifiers::SHIFT, Key::F5) {
            self.fill_or_clear(ctx, true);
        }
        if pressed(ctx, Modifiers::NONE, Key::Delete) || pressed(ctx, Modifiers::NONE, Key::Backspace) {
            self.fill_or_clear(ctx, false);
        }
        let canvas = self.canvas_rect;
        if pressed(ctx, cmd, Key::Plus) || pressed(ctx, cmd, Key::Equals) {
            self.zoom_by(1.25, None, canvas);
        }
        if pressed(ctx, cmd, Key::Minus) {
            self.zoom_by(0.8, None, canvas);
        }
        if pressed(ctx, cmd, Key::Num0) {
            if let Some(p) = self.project() {
                p.view.needs_fit = true;
            }
        }
        if pressed(ctx, cmd, Key::Num1) {
            self.actual_pixels(ctx);
        }
        // Tools and colors take single keys.
        let keys = [
            (Key::V, Tool::Move),
            (Key::B, Tool::Brush),
            (Key::E, Tool::Eraser),
            (Key::I, Tool::Eyedropper),
            (Key::H, Tool::Hand),
            (Key::Z, Tool::Zoom),
        ];
        for (key, tool) in keys {
            if pressed(ctx, Modifiers::NONE, key) {
                self.tool = tool;
            }
        }
        if pressed(ctx, Modifiers::SHIFT, Key::M) {
            self.tool = Tool::Ellipse;
        } else if pressed(ctx, Modifiers::NONE, Key::M) {
            self.tool = Tool::Marquee;
        }
        if pressed(ctx, Modifiers::NONE, Key::X) {
            std::mem::swap(&mut self.fg, &mut self.bg);
        }
        if pressed(ctx, Modifiers::NONE, Key::D) {
            self.fg = [0, 0, 0];
            self.bg = [255, 255, 255];
        }
        let brush = if self.tool == Tool::Eraser { &mut self.eraser } else { &mut self.brush };
        if pressed(ctx, Modifiers::NONE, Key::OpenBracket) {
            brush.size = (brush.size / 1.15).max(1.0).round();
        }
        if pressed(ctx, Modifiers::NONE, Key::CloseBracket) {
            brush.size = (brush.size * 1.15).min(5000.0).round().max(brush.size + 1.0);
        }
        if self.tool == Tool::Move {
            let step = if ctx.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
            for (key, dx, dy) in [(Key::ArrowLeft, -step, 0.0), (Key::ArrowRight, step, 0.0), (Key::ArrowUp, 0.0, -step), (Key::ArrowDown, 0.0, step)] {
                let hit = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key) || i.consume_key(Modifiers::SHIFT, key));
                if hit {
                    if let Some(id) = self.active_id() {
                        self.edit("Nudge", |d| ops::translate(d, id, dx, dy));
                    }
                }
            }
        }
    }

    fn menus(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let has_doc = self.project_ref().is_some();
        let active = self.project_ref().and_then(|p| p.doc.active_layer().cloned());
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New…").shortcut_text("Ctrl+N")).clicked() {
                    self.new_doc = Some(NewDocDialog { width: 1920, height: 1080, resolution: 72.0, white: true });
                    ui.close();
                }
                if ui.add(egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                    ui.close();
                    self.open_dialog();
                }
                if ui.add(egui::Button::new("Open Project Folder (.comp)…").shortcut_text("Ctrl+Shift+O")).clicked() {
                    ui.close();
                    self.open_project_dialog();
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Place Image as Layer…")).clicked() {
                    ui.close();
                    self.place_dialog(ctx);
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    ui.close();
                    self.save(ctx, false);
                }
                if ui.add_enabled(has_doc, egui::Button::new("Save As…").shortcut_text("Ctrl+Shift+S")).clicked() {
                    ui.close();
                    self.save(ctx, true);
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Export PNG…").shortcut_text("Ctrl+Shift+E")).clicked() {
                    ui.close();
                    self.export(ctx, ExportFormat::Png);
                }
                if ui.add_enabled(has_doc, egui::Button::new("Export JPEG…")).clicked() {
                    ui.close();
                    self.jpeg = Some(JpegDialog { quality: 85, matte: [255, 255, 255] });
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Close").shortcut_text("Ctrl+W")).clicked() {
                    ui.close();
                    let i = self.current;
                    self.close_tab(i, false);
                }
                if ui.button("Quit").clicked() {
                    ui.close();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let (undo, redo) = self
                    .project_ref()
                    .map(|p| (p.history.undo_label().map(str::to_string), p.history.redo_label().map(str::to_string)))
                    .unwrap_or((None, None));
                let undo_text = undo.as_ref().map_or("Undo".to_string(), |l| format!("Undo {l}"));
                if ui.add_enabled(undo.is_some(), egui::Button::new(undo_text).shortcut_text("Ctrl+Z")).clicked() {
                    if let Some(p) = self.project() {
                        p.undo();
                    }
                    ui.close();
                }
                let redo_text = redo.as_ref().map_or("Redo".to_string(), |l| format!("Redo {l}"));
                if ui.add_enabled(redo.is_some(), egui::Button::new(redo_text).shortcut_text("Ctrl+Shift+Z")).clicked() {
                    if let Some(p) = self.project() {
                        p.redo();
                    }
                    ui.close();
                }
                ui.separator();
                if ui.add_enabled(active.is_some(), egui::Button::new("Fill with Foreground").shortcut_text("Shift+F5")).clicked() {
                    ui.close();
                    self.fill_or_clear(ctx, true);
                }
                if ui.add_enabled(active.is_some(), egui::Button::new("Clear").shortcut_text("Delete")).clicked() {
                    ui.close();
                    self.fill_or_clear(ctx, false);
                }
            });
            ui.menu_button("Image", |ui| {
                if ui.add_enabled(has_doc, egui::Button::new("Flatten Image")).clicked() {
                    ui.close();
                    self.edit("Flatten Image", ops::flatten);
                }
                ui.separator();
                if let Some(p) = self.project() {
                    let mut resolution = p.doc.resolution;
                    ui.horizontal(|ui| {
                        ui.label("Resolution");
                        if ui.add(egui::DragValue::new(&mut resolution).range(1.0..=9600.0).suffix(" ppi")).changed() {
                            p.begin_edit("Resolution");
                            p.doc.resolution = resolution;
                        }
                    });
                    ui.label(format!("Canvas: {} × {} px", p.doc.width, p.doc.height));
                }
            });
            ui.menu_button("Layer", |ui| {
                if ui.add_enabled(has_doc, egui::Button::new("New Layer").shortcut_text("Ctrl+Shift+N")).clicked() {
                    ui.close();
                    self.edit("New Layer", |d| {
                        ops::add_layer(d);
                    });
                }
                if ui.add_enabled(has_doc, egui::Button::new("New Folder")).clicked() {
                    ui.close();
                    self.edit("New Folder", |d| {
                        ops::add_folder(d);
                    });
                }
                ui.menu_button("New Adjustment Layer", |ui| {
                    for kind in AdjustmentKind::ALL {
                        if ui.add_enabled(has_doc, egui::Button::new(kind.label())).clicked() {
                            ui.close();
                            self.add_adjustment(kind);
                        }
                    }
                });
                ui.separator();
                let id = active.as_ref().map(|l| l.id);
                if ui.add_enabled(id.is_some(), egui::Button::new("Duplicate Layer").shortcut_text("Ctrl+J")).clicked() {
                    ui.close();
                    self.edit("Duplicate Layer", |d| {
                        ops::duplicate_layer(d, id.unwrap());
                    });
                }
                if ui.add_enabled(id.is_some(), egui::Button::new("Delete Layer")).clicked() {
                    ui.close();
                    self.edit("Delete Layer", |d| ops::delete_layer(d, id.unwrap()));
                }
                ui.separator();
                if ui.add_enabled(id.is_some(), egui::Button::new("Group into Folder").shortcut_text("Ctrl+G")).clicked() {
                    ui.close();
                    self.edit("Group Layer", |d| {
                        ops::group_layer(d, id.unwrap());
                    });
                }
                let is_group = active.as_ref().is_some_and(|l| l.is_group);
                if ui.add_enabled(is_group, egui::Button::new("Ungroup Folder").shortcut_text("Ctrl+Shift+G")).clicked() {
                    ui.close();
                    self.edit("Ungroup", |d| ops::ungroup(d, id.unwrap()));
                }
                let in_folder = active.as_ref().is_some_and(|l| l.parent.is_some());
                if ui.add_enabled(in_folder, egui::Button::new("Move Out of Folder")).clicked() {
                    ui.close();
                    self.edit("Move Out of Folder", |d| ops::release_from_folder(d, id.unwrap()));
                }
                ui.separator();
                let clipped = active.as_ref().is_some_and(|l| l.mask_source.is_some());
                let label = if clipped { "Release Clipping Mask" } else { "Create Clipping Mask" };
                if ui.add_enabled(id.is_some(), egui::Button::new(label).shortcut_text("Ctrl+Alt+G")).clicked() {
                    ui.close();
                    self.toggle_clipping(ctx);
                }
                ui.separator();
                let has_mask = active.as_ref().is_some_and(|l| l.mask.is_some());
                if ui.add_enabled(id.is_some() && !has_mask, egui::Button::new("Add Mask (Reveal)")).clicked() {
                    ui.close();
                    self.edit("Add Layer Mask", |d| ops::add_mask(d, id.unwrap(), true));
                }
                if ui.add_enabled(id.is_some() && !has_mask, egui::Button::new("Add Mask (Hide)")).clicked() {
                    ui.close();
                    self.edit("Add Layer Mask", |d| ops::add_mask(d, id.unwrap(), false));
                }
                if ui.add_enabled(has_mask, egui::Button::new("Invert Mask")).clicked() {
                    ui.close();
                    self.edit("Invert Mask", |d| ops::invert_mask(d, id.unwrap()));
                }
                if ui.add_enabled(has_mask, egui::Button::new("Delete Mask")).clicked() {
                    ui.close();
                    self.edit("Delete Mask", |d| d.layer_mut(id.unwrap()).unwrap().mask = None);
                    if let Some(p) = self.project() {
                        p.on_mask = false;
                    }
                }
                ui.separator();
                if ui.add_enabled(id.is_some(), egui::Button::new("Merge Down").shortcut_text("Ctrl+E")).clicked() {
                    ui.close();
                    self.merge_down(ctx);
                }
                ui.separator();
                for (label, horizontal) in [("Flip Horizontal", true), ("Flip Vertical", false)] {
                    if ui.add_enabled(id.is_some(), egui::Button::new(label)).clicked() {
                        ui.close();
                        self.edit(label, |d| {
                            let mut ids = d.descendants(id.unwrap());
                            ids.push(id.unwrap());
                            let (w, h) = (d.width as f64, d.height as f64);
                            let pivot = d.layer(id.unwrap()).map(|l| l.transform.center()).unwrap_or(Point::new(w / 2.0, h / 2.0));
                            for l in d.layers.iter_mut().filter(|l| ids.contains(&l.id)) {
                                let old = l.transform;
                                let mut t = old;
                                let c = t.center();
                                if horizontal {
                                    t.flip_x = !t.flip_x;
                                    t.origin.x = 2.0 * pivot.x - c.x - t.size.0 / 2.0;
                                    t.rotation = -t.rotation;
                                } else {
                                    t.flip_y = !t.flip_y;
                                    t.origin.y = 2.0 * pivot.y - c.y - t.size.1 / 2.0;
                                    t.rotation = -t.rotation;
                                }
                                ops::set_transform(l, &old, l.mask.as_ref().and_then(|m| m.placement), t);
                            }
                        });
                    }
                }
            });
            ui.menu_button("Select", |ui| {
                if ui.add_enabled(has_doc, egui::Button::new("All").shortcut_text("Ctrl+A")).clicked() {
                    ui.close();
                    self.select_all();
                }
                if ui.add_enabled(has_doc, egui::Button::new("Deselect").shortcut_text("Ctrl+D")).clicked() {
                    ui.close();
                    self.deselect();
                }
                if ui.add_enabled(has_doc, egui::Button::new("Inverse").shortcut_text("Ctrl+Shift+I")).clicked() {
                    ui.close();
                    self.invert_selection();
                }
                if ui.add_enabled(active.is_some(), egui::Button::new("Load Layer Transparency")).clicked() {
                    ui.close();
                    if let Some(p) = self.project() {
                        let id = p.doc.active.unwrap();
                        let sel = ops::transparency_selection(&p.doc, id);
                        p.checkpoint("Load Selection");
                        p.doc.selection = sel;
                        p.changed();
                    }
                }
            });
            ui.menu_button("View", |ui| {
                if ui.add_enabled(has_doc, egui::Button::new("Fit on Screen").shortcut_text("Ctrl+0")).clicked() {
                    ui.close();
                    if let Some(p) = self.project() {
                        p.view.needs_fit = true;
                    }
                }
                if ui.add_enabled(has_doc, egui::Button::new("Actual Pixels (100%)").shortcut_text("Ctrl+1")).clicked() {
                    ui.close();
                    self.actual_pixels(ctx);
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About Compositor").clicked() {
                    ui.close();
                    self.show_about = true;
                }
            });
        });
    }

    pub fn actual_pixels(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        let canvas = self.canvas_rect;
        if let Some(p) = self.project() {
            // Centered, one document pixel per screen pixel.
            p.view.zoom = 1.0 / ppp;
            p.view.needs_fit = false;
            let size = egui::vec2(p.doc.width as f32, p.doc.height as f32) * p.view.zoom;
            p.view.offset = (canvas.size() - size) / 2.0;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(d) = &mut self.new_doc {
            let mut open = true;
            let mut create = false;
            egui::Window::new("New Document")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    egui::Grid::new("new-doc").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Width");
                        ui.add(egui::DragValue::new(&mut d.width).range(1..=30_000).suffix(" px"));
                        ui.end_row();
                        ui.label("Height");
                        ui.add(egui::DragValue::new(&mut d.height).range(1..=30_000).suffix(" px"));
                        ui.end_row();
                        ui.label("Resolution");
                        ui.add(egui::DragValue::new(&mut d.resolution).range(1.0..=9600.0).suffix(" ppi"));
                        ui.end_row();
                        ui.label("Background");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.white, true, "White");
                            ui.radio_value(&mut d.white, false, "Transparent");
                        });
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        for (label, w, h) in [("1080p", 1920, 1080), ("4K", 3840, 2160), ("A4 300ppi", 2480, 3508), ("Square", 2048, 2048)] {
                            if ui.small_button(label).clicked() {
                                d.width = w;
                                d.height = h;
                            }
                        }
                    });
                    ui.add_space(6.0);
                    if (d.width as u64 * d.height as u64) > 100_000_000 {
                        ui.colored_label(egui::Color32::LIGHT_RED, "At most 100 megapixels.");
                    } else if ui.button("Create").clicked() {
                        create = true;
                    }
                });
            if create {
                let (w, h, r, white) = (d.width as usize, d.height as usize, d.resolution, d.white);
                self.new_doc = None;
                self.new_document(w, h, r, white);
            } else if !open {
                self.new_doc = None;
            }
        }
        if let Some(j) = &mut self.jpeg {
            let mut open = true;
            let mut go = false;
            egui::Window::new("Export JPEG")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.add(egui::Slider::new(&mut j.quality, 1..=100).text("Quality"));
                    ui.horizontal(|ui| {
                        ui.label("Transparent areas become");
                        ui.color_edit_button_srgb(&mut j.matte);
                    });
                    if ui.button("Export…").clicked() {
                        go = true;
                    }
                });
            if go {
                let format = ExportFormat::Jpeg { quality: j.quality, background: j.matte };
                self.jpeg = None;
                self.export(ctx, format);
            } else if !open {
                self.jpeg = None;
            }
        }
        if let Some(pending) = &self.pending {
            let names: Vec<String> = match pending {
                Pending::CloseTab(i) => self.projects.get(*i).map(|p| vec![p.title()]).unwrap_or_default(),
                Pending::Quit => self.projects.iter().filter(|p| p.dirty).map(|p| p.title()).collect(),
            };
            let mut choice = None;
            egui::Window::new("Unsaved Changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Save changes to {}?", names.join(", ")));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            choice = Some(0);
                        }
                        if ui.button("Don't Save").clicked() {
                            choice = Some(1);
                        }
                        if ui.button("Cancel").clicked() {
                            choice = Some(2);
                        }
                    });
                });
            match (choice, pending) {
                (Some(0), Pending::CloseTab(i)) => {
                    let i = *i;
                    self.pending = None;
                    self.current = i;
                    if self.save(ctx, false) {
                        self.close_tab(i, true);
                    }
                }
                (Some(1), Pending::CloseTab(i)) => {
                    let i = *i;
                    self.pending = None;
                    self.close_tab(i, true);
                }
                (Some(0), Pending::Quit) => {
                    self.pending = None;
                    let mut all_saved = true;
                    for i in 0..self.projects.len() {
                        if self.projects[i].dirty {
                            self.current = i;
                            all_saved &= self.save(ctx, false);
                        }
                    }
                    if all_saved {
                        self.quit_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                (Some(1), Pending::Quit) => {
                    self.pending = None;
                    self.quit_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                (Some(_), _) => self.pending = None,
                _ => {}
            }
        }
        if self.show_about {
            egui::Window::new("About Compositor")
                .collapsible(false)
                .resizable(false)
                .open(&mut self.show_about)
                .show(ctx, |ui| {
                    ui.heading("Compositor");
                    ui.label(format!("Version {} for Windows and Linux", env!("CARGO_PKG_VERSION")));
                    ui.label("A layered image editor that opens and saves the macOS app's .comp projects.");
                    ui.hyperlink_to("github.com/robbietilton/Compositor", "https://github.com/robbietilton/Compositor");
                });
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut close = None;
            for i in 0..self.projects.len() {
                let p = &self.projects[i];
                let title = format!("{}{}", p.title(), if p.dirty { " •" } else { "" });
                if ui.selectable_label(i == self.current, title).clicked() {
                    self.current = i;
                }
                if ui.small_button("×").on_hover_text("Close").clicked() {
                    close = Some(i);
                }
                ui.separator();
            }
            if let Some(i) = close {
                self.close_tab(i, false);
            }
        });
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for path in dropped {
            if project::is_project(&path) || self.project_ref().is_none() {
                self.open_path(&path);
            } else {
                self.place_path(ctx, &path);
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

impl App {
    /// One frame of the whole window.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed && self.projects.iter().any(|p| p.dirty) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        self.handle_drops(ctx);
        self.shortcuts(ctx);

        let title = match self.project_ref() {
            Some(p) => format!("{}{} — Compositor", p.title(), if p.dirty { " •" } else { "" }),
            None => "Compositor".to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("menu").show(ui, |ui| {
            self.menus(ctx, ui);
        });
        egui::Panel::top("options").show(ui, |ui| {
            self.options_bar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            self.status_bar(ctx, ui);
        });
        egui::Panel::left("tools").resizable(false).exact_size(44.0).show(ui, |ui| {
            self.toolbar(ui);
        });
        egui::Panel::right("panels").default_size(300.0).min_size(240.0).show(ui, |ui| {
            self.side_panels(ctx, ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::default().fill(egui::Color32::from_gray(38))).show(ui, |ui| {
            if self.projects.len() > 1 {
                self.tabs(ui);
            }
            self.canvas_ui(ctx, ui);
        });
        self.dialogs(ctx);

        // A slider let go ends the edit it was coalescing into one undo step.
        let idle = ctx.input(|i| !i.pointer.any_down()) && !ctx.egui_wants_keyboard_input();
        if idle {
            if let Some(p) = self.project() {
                if p.edit_label.is_some() {
                    p.edit_label = None;
                    p.revision += 1;
                }
            }
        }
    }
}

/// Adds a system CJK font as a fallback so layer names and file names in Chinese, Japanese or
/// Korean show instead of boxes.
fn install_fonts(ctx: &egui::Context) {
    let candidates = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttf",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    ];
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("cjk".into());
    }
    ctx.set_fonts(fonts);
}
