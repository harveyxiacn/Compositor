//! Undo and redo: whole-document snapshots, cheap because pixels are shared until changed.
//! A brush stroke still copies the layer it paints, so the history is also kept under a memory budget.

use crate::model::Document;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Pixel memory the undo history may hold on top of the open document.
const BUDGET_BYTES: usize = 2 << 30;

pub struct History {
    undo: Vec<(String, Document)>,
    redo: Vec<(String, Document)>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: 60 }
    }
}

/// Every pixel buffer a document holds, by identity, with its size in bytes.
fn assets(doc: &Document, into: &mut HashMap<usize, usize>) {
    for l in &doc.layers {
        if let Some(p) = &l.pixels {
            let bytes = p.image.data.len() + p.mips.iter().map(|m| m.data.len()).sum::<usize>();
            into.insert(Arc::as_ptr(p) as usize, bytes);
        }
        if let Some(m) = &l.mask {
            let bytes = m.asset.image.data.len() + m.asset.mips.iter().map(|m| m.data.len()).sum::<usize>();
            into.insert(Arc::as_ptr(&m.asset) as usize, bytes);
        }
    }
}

impl History {
    /// Records the document as it is before an edit called `label`.
    pub fn record(&mut self, label: &str, before: &Document) {
        self.undo.push((label.to_string(), before.clone()));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.trim(before);
    }
    /// Drops the oldest steps while the pixels only the history holds exceed the budget.
    fn trim(&mut self, current: &Document) {
        while self.undo.len() > 1 {
            let mut live = HashMap::new();
            assets(current, &mut live);
            let live: HashSet<usize> = live.into_keys().collect();
            let mut held = HashMap::new();
            for (_, doc) in &self.undo {
                assets(doc, &mut held);
            }
            let bytes: usize = held.iter().filter(|(k, _)| !live.contains(k)).map(|(_, v)| *v).sum();
            if bytes <= BUDGET_BYTES {
                break;
            }
            self.undo.remove(0);
        }
    }
    pub fn undo(&mut self, current: &mut Document) -> Option<String> {
        let (label, doc) = self.undo.pop()?;
        let now = std::mem::replace(current, doc);
        self.redo.push((label.clone(), now));
        Some(label)
    }
    pub fn redo(&mut self, current: &mut Document) -> Option<String> {
        let (label, doc) = self.redo.pop()?;
        let now = std::mem::replace(current, doc);
        self.undo.push((label.clone(), now));
        Some(label)
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(l, _)| l.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|(l, _)| l.as_str())
    }
    pub fn len(&self) -> usize {
        self.undo.len()
    }
    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }
}
