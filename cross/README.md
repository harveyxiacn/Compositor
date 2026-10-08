# Compositor for Windows and Linux

A cross-platform build of Compositor, written in Rust with [egui](https://github.com/emilk/egui). It opens and saves the same `.comp` projects as the macOS app, so a project can move between a Mac, a Windows PC and a Linux machine.

The macOS app (Swift, SwiftUI, AppKit, Metal, Core Image) stays as it is in `../Compositor`. This version shares its file format and its C pixel kernels (`../Compositor/Rendering/*.c`, compiled in by `build.rs`), and reimplements the rest.

## Status

This is the first stage of the port: the core editing workflow. What works:

- **Projects**: open and save `.comp` projects (format version 11, readable by the Mac app); multiple projects in tabs; unsaved-changes prompts; drag and drop.
- **Files**: open PNG, JPEG, WebP, TIFF, BMP and GIF as a new document or place them as layers; export PNG and JPEG with the document's resolution.
- **Layers**: pixel layers and folders, visibility, opacity (a folder's opacity dims what is inside), all 24 blend modes, reordering, grouping and ungrouping, duplicate, delete, rename (double-click), Merge Down, Flatten.
- **Masks**: layer and folder masks, including masks unlinked and moved apart from their layer; add (reveal/hide, or from the selection), paint, invert, enable or disable, link or unlink, delete. Clipping masks (the clipped layers blend with their base the way the Mac app's clipping stacks do).
- **Adjustment layers**: Hue/Saturation (including per-range and Colorize), Levels, Curves, Exposure, Gradient Map, Grain, Black & White, Color Balance, Invert, Gaussian Blur, Motion Blur and Add Noise, with editors for each.
- **Transform**: move, scale, rotate and flip with handles or exact values, snapping to the canvas and guides, arrow-key nudging, Nearest / Smooth / High quality sampling. Folders move as a whole.
- **Selections**: rectangle and ellipse marquee with add, subtract and intersect; Select All, Deselect, Inverse, Load Layer Transparency; Fill and Clear.
- **Painting**: brush and eraser with size, hardness and opacity on pixels or masks, limited to the selection; Shift-click for straight lines; eyedropper.
- **View**: zoom and pan (scroll, Ctrl+scroll, Space-drag, middle-drag), Fit on Screen, Actual Pixels; guides saved in the project are shown.
- Undo and redo for every edit.

Kept in the file but not yet drawn or edited here: layer effects (stroke, shadows, glows, color overlay), live text editing, shape editing, and the alignment guide/grid tools. These layers still show their pixels, and their settings are written back unchanged when the project is saved. Not yet ported: Camera Raw, PSD/PSB/RAW/HEIC/SVG import, Content-Aware Fill and healing, Clone Stamp, Magic Wand / Select Subject, lasso tools, Crop / Canvas Size / Image Size, the Type and Shape tools, and the command palette.

## Running

Download a build from the repository's Actions artifacts or Releases, or build it yourself (below).

- **Windows**: unzip and run `compositor.exe`. Windows 10 or 11, 64-bit.
- **Linux**: extract the archive and run `./compositor`, or `./install.sh` to add it to your applications menu (`~/.local`). Needs a desktop with Vulkan or OpenGL drivers.

The canvas draws through wgpu (Direct3D 12 / Vulkan). If the window stays blank or fails to open on older drivers, start it with `COMPOSITOR_RENDERER=glow` to use OpenGL instead.

`.comp` projects are folders. Open one with **File › Open Project Folder…** (Ctrl+Shift+O), by choosing its `manifest.json` in **File › Open…**, by dropping the folder on the window, or by passing its path on the command line.

### Command line

`compositor-cli` renders projects without opening a window:

```sh
compositor-cli export Poster.comp poster.png
compositor-cli export Poster.comp poster.jpg --quality 85
compositor-cli info Poster.comp
```

## Shortcuts

| | |
|---|---|
| V, M, Shift+M, B, E, I, H, Z | Move, Rectangle Marquee, Ellipse Marquee, Brush, Eraser, Eyedropper, Hand, Zoom |
| [ and ] | Brush size |
| X / D | Swap / default colors |
| Space (hold) | Pan |
| Ctrl+N, Ctrl+O, Ctrl+Shift+O | New, Open, Open Project Folder |
| Ctrl+S, Ctrl+Shift+S, Ctrl+Shift+E | Save, Save As, Export PNG |
| Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y | Undo, Redo |
| Ctrl+A, Ctrl+D, Ctrl+Shift+I | Select All, Deselect, Inverse |
| Shift+F5, Delete | Fill with foreground, Clear |
| Ctrl+Shift+N, Ctrl+J, Ctrl+E | New Layer, Duplicate, Merge Down |
| Ctrl+G, Ctrl+Shift+G, Ctrl+Alt+G | Group, Ungroup, Clipping Mask |
| Ctrl+0, Ctrl+1, Ctrl+= / Ctrl+- | Fit, Actual Pixels, Zoom In / Out |

## Building

Install Rust (stable) from [rustup.rs](https://rustup.rs).

- **Windows**: Visual Studio 2022 or later with the "Desktop development with C++" workload (for the MSVC compiler and linker).
- **Linux**: a C compiler, plus the windowing headers, e.g. on Debian/Ubuntu `sudo apt install build-essential libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev`.

```sh
cd cross
cargo run --release          # the app
cargo test                   # unit tests, including a headless run of the window
cargo build --release        # target/release/compositor and compositor-cli
```

`.github/workflows/cross-platform.yml` builds and tests both platforms on every push and uploads the packaged builds as artifacts; pushing a tag named `cross-v*` publishes them as a GitHub release.

## Layout

| File | |
|---|---|
| `src/model.rs` | Document, layers, transforms, blend modes, masks |
| `src/project.rs` | `.comp` reading and writing, PNG/JPEG export |
| `src/render.rs` | The compositor (pass-through folders, clipping stacks, adjustments) |
| `src/blend.rs` | Blend modes |
| `src/adjust.rs` | Adjustment settings and the C kernels they call |
| `src/raster.rs` | Pixel buffers and their halvings for zoomed-out drawing |
| `src/brush.rs`, `src/selection.rs`, `src/ops.rs` | Painting, selections, layer operations |
| `src/app.rs`, `src/canvas.rs`, `src/panels.rs` | The window |
| `src/bin/compositor-cli.rs` | The command-line tool |

The project format is described in [`../docs/project-format.md`](../docs/project-format.md); a change to what is saved needs the same version bump there and in the Mac app.
