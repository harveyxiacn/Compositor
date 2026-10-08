use crate::adjust::{Adjustment, AdjustmentKind};
use crate::geom::Point;
use crate::model::{BlendMode, Document, Layer, LayerTransform};
use crate::raster::{PixelAsset, Rgba};
use crate::render::{render, Target};
use std::sync::Arc;

fn solid(doc: &mut Document, color: [u8; 4]) -> uuid::Uuid {
    let mut l = Layer::blank("Fill", doc.width, doc.height);
    l.pixels = Some(Arc::new(PixelAsset::new(Rgba::filled(doc.width, doc.height, color))));
    let id = l.id;
    doc.layers.push(l);
    id
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("compositor-test-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn normal_layer_covers_background() {
    let mut doc = Document::new(4, 4, Some([255, 255, 255, 255]));
    solid(&mut doc, [255, 0, 0, 255]);
    let out = render(&doc, Target::full(&doc));
    assert_eq!(out.pixel(1, 1), [255, 0, 0, 255]);
}

#[test]
fn opacity_and_folder_opacity_multiply() {
    let mut doc = Document::new(2, 2, Some([0, 0, 0, 255]));
    let folder = crate::ops::add_folder(&mut doc);
    doc.layer_mut(folder).unwrap().opacity = 0.5;
    let id = solid(&mut doc, [255, 255, 255, 255]);
    let l = doc.layer_mut(id).unwrap();
    l.parent = Some(folder);
    l.opacity = 0.5;
    let out = render(&doc, Target::full(&doc));
    let v = out.pixel(0, 0)[0] as i32;
    assert!((v - 64).abs() <= 1, "got {v}");
}

#[test]
fn multiply_blend() {
    let mut doc = Document::new(2, 2, Some([128, 128, 128, 255]));
    let id = solid(&mut doc, [128, 255, 0, 255]);
    doc.layer_mut(id).unwrap().blend = BlendMode::Multiply;
    let out = render(&doc, Target::full(&doc));
    let p = out.pixel(0, 0);
    assert!((p[0] as i32 - 64).abs() <= 1 && (p[1] as i32 - 128).abs() <= 1 && p[2] == 0, "{p:?}");
}

#[test]
fn invert_adjustment_layer() {
    let mut doc = Document::new(2, 2, Some([10, 20, 30, 255]));
    crate::ops::add_adjustment(&mut doc, AdjustmentKind::Invert, [0; 3], [255; 3]);
    let out = render(&doc, Target::full(&doc));
    assert_eq!(out.pixel(0, 0), [245, 235, 225, 255]);
}

#[test]
fn levels_adjustment_runs_c_kernel() {
    let mut doc = Document::new(2, 2, Some([100, 100, 100, 255]));
    let id = crate::ops::add_adjustment(&mut doc, AdjustmentKind::Levels, [0; 3], [255; 3]);
    let a = doc.layer_mut(id).unwrap().adjustment.as_mut().unwrap();
    a.levels.ranges[0].white = 200.0;
    let out = render(&doc, Target::full(&doc));
    let v = out.pixel(0, 0)[0] as i32;
    assert!((v - 128).abs() <= 1, "got {v}");
}

#[test]
fn clipping_stack_uses_base_alpha() {
    let mut doc = Document::new(4, 1, None);
    doc.layers.clear();
    let mut base = Layer::blank("Base", 4, 1);
    let mut px = Rgba::new(4, 1);
    px.data[0..8].copy_from_slice(&[255, 255, 255, 255, 255, 255, 255, 255]);
    base.pixels = Some(Arc::new(PixelAsset::new(px)));
    let base_id = base.id;
    doc.layers.push(base);
    let clipped = solid(&mut doc, [0, 0, 255, 255]);
    doc.layer_mut(clipped).unwrap().mask_source = Some(base_id);
    let out = render(&doc, Target::full(&doc));
    assert_eq!(out.pixel(0, 0), [0, 0, 255, 255]);
    assert_eq!(out.pixel(3, 0), [0, 0, 0, 0]);
}

#[test]
fn transform_scales_and_flips() {
    let mut doc = Document::new(4, 2, None);
    doc.layers.clear();
    let mut l = Layer::blank("L", 2, 1);
    let mut px = Rgba::new(2, 1);
    px.data.copy_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
    l.pixels = Some(Arc::new(PixelAsset::new(px)));
    l.transform = LayerTransform::new(Point::new(0.0, 0.0), 4.0, 2.0);
    l.transform.flip_x = true;
    l.transform.sampling = crate::model::Sampling::Nearest;
    doc.layers.push(l);
    let out = render(&doc, Target::full(&doc));
    assert_eq!(out.pixel(0, 0), [0, 0, 255, 255]);
    assert_eq!(out.pixel(3, 1), [255, 0, 0, 255]);
}

#[test]
fn project_round_trip_keeps_unknown_fields() {
    let mut doc = Document::new(8, 6, Some([1, 2, 3, 255]));
    let id = crate::ops::add_adjustment(&mut doc, AdjustmentKind::Hsv, [0; 3], [255; 3]);
    doc.layers[0].extra.insert("effects".into(), serde_json::json!({"stroke": {"size": 3}}));
    crate::ops::add_mask(&mut doc, id, false);
    let dir = temp_dir("roundtrip");
    let path = dir.join("Test.comp");
    crate::project::save(&doc, &path).unwrap();
    // Saving over an existing project replaces it.
    crate::project::save(&doc, &path).unwrap();
    let back = crate::project::load(&path).unwrap();
    assert_eq!(back.width, 8);
    assert_eq!(back.layers.len(), 2);
    assert_eq!(back.layers[0].pixels.as_ref().unwrap().image.pixel(2, 2), [1, 2, 3, 255]);
    assert_eq!(back.layers[0].extra["effects"]["stroke"]["size"], 3);
    assert_eq!(back.layers[1].adjustment.as_ref().unwrap().kind, AdjustmentKind::Hsv);
    assert!(back.layers[1].mask.is_some());
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], 11);
    assert!(manifest["layers"][0]["transform"]["origin"].is_array());
    let id_str = manifest["layers"][0]["id"].as_str().unwrap();
    assert_eq!(id_str, id_str.to_uppercase());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn reads_swift_style_hsv_dictionary() {
    let json = r#"{"kind":"Hue/Saturation","hue":0,"saturation":0,"lightness":0,"colorize":false,
        "levels":{"channel":"RGB","ranges":[{"black":0,"gamma":1,"white":255,"outputBlack":0,"outputWhite":255},{"black":0,"gamma":1,"white":255,"outputBlack":0,"outputWhite":255},{"black":0,"gamma":1,"white":255,"outputBlack":0,"outputWhite":255},{"black":0,"gamma":1,"white":255,"outputBlack":0,"outputWhite":255}]},
        "curves":{"channel":"RGB","channels":[[{"x":0,"y":0},{"x":255,"y":255}],[{"x":0,"y":0},{"x":255,"y":255}],[{"x":0,"y":0},{"x":255,"y":255}],[{"x":0,"y":0},{"x":255,"y":255}]]},
        "hsvSettings":{"range":"Reds","colorize":false,"invertRange":false,
            "adjustments":["Reds",{"hue":30,"saturation":0,"lightness":0}],
            "bands":["Reds",{"falloffStart":315,"rangeStart":345,"rangeEnd":15,"falloffEnd":45}]}}"#;
    let a: Adjustment = serde_json::from_str(json).unwrap();
    let hsv = a.hsv();
    assert_eq!(hsv.range, crate::adjust::ColorRange::Reds);
    assert_eq!(hsv.current().hue, 30.0);
    let back = serde_json::to_value(&a).unwrap();
    assert!(back["hsvSettings"]["adjustments"].is_array());
}

#[test]
fn brush_paints_inside_selection_only() {
    let mut doc = Document::new(20, 10, Some([0, 0, 0, 255]));
    let id = doc.layers[0].id;
    doc.selection = crate::selection::shape_selection(
        20,
        10,
        crate::selection::Shape::Rect,
        crate::geom::Rect::new(0.0, 0.0, 10.0, 10.0),
        None,
        crate::selection::Combine::Replace,
    );
    let settings = crate::brush::BrushSettings { size: 40.0, hardness: 1.0, opacity: 1.0 };
    let Ok(mut s) = crate::brush::Stroke::begin(&mut doc, id, false, settings, [255, 255, 255], false) else { panic!() };
    s.add_point(&mut doc, Point::new(10.0, 5.0));
    let px = &doc.layers[0].pixels.as_ref().unwrap().image;
    assert_eq!(px.pixel(5, 5), [255, 255, 255, 255]);
    assert_eq!(px.pixel(15, 5), [0, 0, 0, 255]);
}

#[test]
fn view_render_matches_full_render() {
    let mut doc = Document::new(64, 64, Some([200, 100, 50, 255]));
    crate::ops::add_adjustment(&mut doc, AdjustmentKind::GaussianBlur, [0; 3], [255; 3]);
    let full = render(&doc, Target::full(&doc));
    let part = crate::render::render_view(&doc, crate::geom::Rect::new(16.0, 16.0, 16.0, 16.0), 1.0, (16, 16), (16, 16));
    assert_eq!(part.pixel(0, 0), full.pixel(16, 16));
}

/// The manifest example from docs/writing-comp-files.md, with its Curves layer, loads and renders.
#[test]
fn reads_documented_example_project() {
    let dir = temp_dir("documented");
    let root = dir.join("Example.comp");
    std::fs::create_dir_all(root.join("images")).unwrap();
    let id = "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F";
    let png = crate::raster::encode_png_rgba(&Rgba::filled(16, 9, [100, 100, 100, 255])).unwrap();
    std::fs::write(root.join("images").join(format!("{id}.png")), png).unwrap();
    let manifest = r#"{
      "format": "com.compositor.project", "version": 11, "colorSpace": "sRGB",
      "documentID": "0C5E7A91-3B2D-4F6A-8E1C-9D0B7A6F5E4D", "width": 16, "height": 9, "resolution": 72,
      "activeLayerID": "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F",
      "layers": [
        { "id": "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F", "name": "Background",
          "imageFile": "6F1D3C2A-0B7E-4E8A-9C4D-2A1B3C4D5E6F.png", "isVisible": true, "isGroup": false,
          "opacity": 1, "blendMode": "Normal",
          "transform": { "origin": [0, 0], "size": [16, 9], "rotation": 0, "flipX": false, "flipY": false, "sampling": "High quality" } },
        { "id": "A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D", "name": "Warm Grade", "isVisible": true, "isGroup": false,
          "opacity": 1, "blendMode": "Normal",
          "transform": { "origin": [0, 0], "size": [16, 9], "rotation": 0, "flipX": false, "flipY": false, "sampling": "High quality" },
          "adjustment": { "kind": "Curves", "hue": 0, "saturation": 0, "lightness": 0, "colorize": false,
            "levels": { "channel": "RGB", "ranges": [
              { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 },
              { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 },
              { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 },
              { "black": 0, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255 } ] },
            "curves": { "channel": "RGB", "channels": [
              [ { "x": 0, "y": 0 }, { "x": 255, "y": 255 } ],
              [ { "x": 0, "y": 0 }, { "x": 120, "y": 147 }, { "x": 255, "y": 255 } ],
              [ { "x": 0, "y": 0 }, { "x": 100, "y": 114 }, { "x": 255, "y": 255 } ],
              [ { "x": 0, "y": 0 }, { "x": 115, "y": 97 }, { "x": 255, "y": 238 } ] ] } } }
      ]
    }"#;
    std::fs::write(root.join("manifest.json"), manifest).unwrap();
    let doc = crate::project::load(&root).unwrap();
    let out = render(&doc, Target::full(&doc));
    let p = out.pixel(8, 4);
    // Warmer: red up, blue down.
    assert!(p[0] > 110 && p[2] < 95, "{p:?}");
    std::fs::remove_dir_all(dir).ok();
}

mod ui {
    use crate::app::{App, Tool};
    use crate::geom::Point;
    use eframe::egui::{self, Event, Pos2, RawInput, Rect};

    struct Harness {
        ctx: egui::Context,
        app: App,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            let app = App::with_context(&ctx, vec![]);
            Self { ctx, app, time: 0.0 }
        }
        fn frame(&mut self, events: Vec<Event>) {
            self.time += 1.0 / 60.0;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let app = &mut self.app;
            let mut output = self.ctx.run_ui(input, |ui| app.frame(ui));
            output.textures_delta.clear();
        }
        fn screen(&self, d: Point) -> Pos2 {
            let p = &self.app.projects[self.app.current];
            let r = self.app.canvas_rect;
            Pos2::new(r.min.x + p.view.offset.x + d.x as f32 * p.view.zoom, r.min.y + p.view.offset.y + d.y as f32 * p.view.zoom)
        }
        fn drag(&mut self, from: Point, to: Point) {
            let (a, b) = (self.screen(from), self.screen(to));
            let button = |pos, pressed| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            self.frame(vec![Event::PointerMoved(a)]);
            self.frame(vec![button(a, true)]);
            for i in 1..=4 {
                let t = i as f32 / 4.0;
                self.frame(vec![Event::PointerMoved(a + (b - a) * t)]);
            }
            self.frame(vec![button(b, false)]);
            self.frame(vec![]);
        }
    }

    #[test]
    fn tools_work_through_the_window() {
        let mut h = Harness::new();
        h.frame(vec![]);
        h.app.new_document(400, 300, 72.0, true);
        h.frame(vec![]);
        h.frame(vec![]);
        assert!(h.app.canvas_rect.width() > 300.0);

        // Brush.
        h.app.tool = Tool::Brush;
        h.app.fg = [255, 0, 0];
        h.drag(Point::new(150.0, 150.0), Point::new(250.0, 150.0));
        {
            let p = &h.app.projects[0];
            assert_eq!(p.history.undo_label(), Some("Brush"));
            let px = p.doc.layers[0].pixels.as_ref().unwrap().image.pixel(200, 150);
            assert_eq!(px, [255, 0, 0, 255]);
        }

        // Rectangle marquee.
        h.app.tool = Tool::Marquee;
        h.drag(Point::new(50.0, 50.0), Point::new(150.0, 120.0));
        {
            let sel = h.app.projects[0].doc.selection.as_ref().expect("a selection");
            assert!((sel.bounds.x0 - 50).abs() <= 2 && (sel.bounds.x1 - 150).abs() <= 2, "{:?}", sel.bounds);
        }

        // Move the layer.
        h.app.tool = Tool::Move;
        h.drag(Point::new(200.0, 150.0), Point::new(240.0, 170.0));
        {
            let t = h.app.projects[0].doc.layers[0].transform;
            assert_eq!((t.origin.x, t.origin.y), (40.0, 20.0));
        }

        // Adjustment layers and their panels draw without trouble.
        for kind in crate::adjust::AdjustmentKind::ALL {
            h.app.add_adjustment(kind);
            h.frame(vec![]);
        }
        assert_eq!(h.app.projects[0].doc.layers.len(), 13);

        // Undo walks back through it all.
        for _ in 0..15 {
            h.app.projects[0].undo();
        }
        h.frame(vec![]);
        assert_eq!(h.app.projects[0].doc.layers.len(), 1);
        assert!(h.app.projects[0].doc.layers[0].pixels.as_ref().unwrap().image.pixel(200, 150) == [255, 255, 255, 255]);
    }
}

/// `cargo test --release -- --ignored --nocapture perf`
#[test]
#[ignore]
fn perf_large_document() {
    let (w, h) = (6000, 4000);
    let mut doc = Document::new(w, h, Some([200, 180, 160, 255]));
    for i in 0..4 {
        let mut l = Layer::blank("Photo", w, h);
        let mut px = Rgba::new(3000, 2000);
        for (k, p) in px.data.chunks_exact_mut(4).enumerate() {
            let v = (k % 251) as u8;
            p.copy_from_slice(&[v, 255 - v, (v / 2) + 60, 255]);
        }
        l.pixels = Some(Arc::new(PixelAsset::new(px)));
        l.transform = LayerTransform::new(Point::new(300.0 * i as f64, 200.0 * i as f64), 4000.0, 2667.0);
        l.transform.rotation = 7.0 * i as f64;
        l.opacity = 0.8;
        l.blend = if i % 2 == 0 { BlendMode::Normal } else { BlendMode::Overlay };
        doc.layers.push(l);
    }
    let id = doc.layers[2].id;
    crate::ops::add_mask(&mut doc, id, true);
    crate::ops::add_adjustment(&mut doc, AdjustmentKind::Curves, [0; 3], [255; 3]);
    let hid = crate::ops::add_adjustment(&mut doc, AdjustmentKind::Hsv, [0; 3], [255; 3]);
    let hsv = doc.layer_mut(hid).unwrap().adjustment.as_mut().unwrap();
    hsv.hue = 30.0;
    let t = std::time::Instant::now();
    let n = 5;
    for _ in 0..n {
        let _ = crate::render::render_view(&doc, crate::geom::Rect::new(0.0, 0.0, 6000.0, 3375.0), 2560.0 / 6000.0, (0, 0), (2560, 1440));
    }
    println!("view 2560x1440 (zoomed out): {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
    let t = std::time::Instant::now();
    for _ in 0..n {
        let _ = crate::render::render_view(&doc, crate::geom::Rect::new(1000.0, 1000.0, 2560.0, 1440.0), 1.0, (1000, 1000), (2560, 1440));
    }
    println!("view 2560x1440 (100%): {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
    let t = std::time::Instant::now();
    let _ = render(&doc, Target::full(&doc));
    println!("full 6000x4000 render: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let t = std::time::Instant::now();
    let layer = doc.layers.iter().find(|l| l.name == "Photo").unwrap().id;
    let Ok(mut s) = crate::brush::Stroke::begin(&mut doc, layer, false, crate::brush::BrushSettings { size: 200.0, hardness: 0.5, opacity: 1.0 }, [0, 0, 0], false) else { panic!() };
    println!("stroke begin (copy 3000x2000): {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let t = std::time::Instant::now();
    for i in 0..50 {
        s.add_point(&mut doc, Point::new(1000.0 + i as f64 * 20.0, 1500.0));
    }
    println!("50 brush moves: {:.1} ms each", t.elapsed().as_secs_f64() * 1000.0 / 50.0);
}

#[test]
fn history_stays_within_its_memory_budget() {
    // 64 MB layer: 40 strokes would hold 2.5 GB of copies without the budget.
    let mut doc = Document::new(4096, 4096, Some([0, 0, 0, 255]));
    let id = doc.layers[0].id;
    let mut history = crate::history::History::default();
    for i in 0..40 {
        let before = doc.clone();
        let Ok(mut s) = crate::brush::Stroke::begin(&mut doc, id, false, Default::default(), [255, 255, 255], false) else { panic!() };
        s.add_point(&mut doc, Point::new(10.0 + i as f64, 10.0));
        history.record("Brush", &before);
    }
    assert!(history.len() < 40 && history.len() >= 20, "{}", history.len());
}
