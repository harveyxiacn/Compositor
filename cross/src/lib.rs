//! Compositor for Windows and Linux.

pub mod adjust;
pub mod app;
pub mod blend;
pub mod brush;
pub mod canvas;
pub mod geom;
pub mod history;
pub mod model;
pub mod ops;
pub mod panels;
pub mod project;
pub mod raster;
pub mod render;
pub mod selection;

#[cfg(test)]
mod tests;
