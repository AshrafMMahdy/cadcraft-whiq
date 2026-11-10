//! The block expansions a build will ask for, counted without building: arithmetic over the block
//! table. [`Builder::expand`](super::Builder) takes one unit for every block copy (each MINSERT
//! cell) and one for every entity drawn inside it, nested references included; the count here is
//! that number, so `count == DisplayList::expanded` of a build with no limit.
//!
//! The walk follows the build's own rules for what is drawn at all (hidden objects, hidden
//! objects' layers, a viewport's frozen layers, annotative objects not shown at the scale,
//! [`MAX_BLOCK_DEPTH`]), so a reference that is not drawn costs nothing. Each block is counted
//! once per (depth, layer of the reference that draws it) and remembered, so the work is
//! proportional to the block table, not to the expanded size, and a block that refers to itself
//! stops at the depth limit like the build does.

use std::collections::HashMap;

use cadcraft_doc::{Drawing, Entity, EntityKind, MAX_BLOCK_DEPTH};

/// What counting needs to know of the build it stands for.
pub(crate) struct Counter<'a> {
    d: &'a Drawing,
    /// Plotting skips layers marked "do not plot" ([`Options`](super::Options) of `build_plot`).
    plotting: bool,
    /// A viewport's frozen layers (model space seen through it).
    frozen: &'a [String],
    /// Cost of one copy of a block's contents: (the block, depth of the reference,
    /// the layer layer-0 contents take) -> expansions.
    memo: HashMap<(usize, usize, Option<String>), u64>,
}

impl<'a> Counter<'a> {
    pub(crate) fn new(d: &'a Drawing, plotting: bool, frozen: &'a [String]) -> Self {
        Counter { d, plotting, frozen, memo: HashMap::new() }
    }

    /// The expansions drawing the top-level object `e` asks for. `anno` says how an annotative
    /// object draws here (see `entity_parts`): hidden ones cost nothing.
    pub(crate) fn top(&mut self, e: &Entity, anno_hidden: bool) -> u64 {
        if anno_hidden {
            return 0;
        }
        self.entity(e, 0, None)
    }

    /// Whether `e` is drawn at all inside a reference on `block_layer` (see `resolve_common`).
    fn drawn(&self, e: &Entity, block_layer: Option<&str>) -> bool {
        if self.d.is_hidden_object(e.handle) || !e.common.visible {
            return false;
        }
        let name = if e.common.layer == "0" { block_layer.unwrap_or("0") } else { e.common.layer.as_str() };
        self.d.layer(name).is_none_or(|l| l.visible() && (!self.plotting || l.plot)) && !self.frozen.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    /// The layer layer-0 objects inside the reference `e` take (see `sub_ctx`).
    fn inner_layer(e: &Entity, block_layer: Option<&str>) -> Option<String> {
        if e.common.layer == "0" { block_layer.map(str::to_string) } else { Some(e.common.layer.clone()) }
    }

    fn entity(&mut self, e: &Entity, depth: usize, block_layer: Option<&str>) -> u64 {
        if !self.drawn(e, block_layer) {
            return 0;
        }
        match &e.kind {
            EntityKind::Insert(ins) => self.insert(e, ins, depth, block_layer),
            EntityKind::MLeader(m) => m.block.as_ref().map_or(0, |ins| self.insert(e, ins, depth, block_layer)),
            EntityKind::Dimension(dm) => {
                let d = self.d;
                let Some(blk) = dm.block.as_ref().and_then(|n| d.block(n)).filter(|_| depth < MAX_BLOCK_DEPTH) else { return 0 };
                let inner = Self::inner_layer(e, block_layer);
                let mut n = 0u64;
                for be in blk.entities.iter() {
                    n = n.saturating_add(1).saturating_add(self.entity(be, depth + 1, inner.as_deref()));
                }
                n
            }
            _ => 0,
        }
    }

    fn insert(&mut self, e: &Entity, ins: &cadcraft_doc::Insert, depth: usize, block_layer: Option<&str>) -> u64 {
        if depth >= MAX_BLOCK_DEPTH {
            return 0;
        }
        let d = self.d;
        let Some(blk) = d.block(&ins.block) else { return 0 };
        let copies = u64::from(ins.cols.clamp(1, 10_000)) * u64::from(ins.rows.clamp(1, 10_000));
        let inner = Self::inner_layer(e, block_layer);
        let key = (std::sync::Arc::as_ptr(blk) as usize, depth, inner.clone());
        let one = match self.memo.get(&key) {
            Some(n) => *n,
            None => {
                // The copy itself, then each entity (AttDefs are skipped) and what it expands.
                let mut n = 1u64;
                for be in blk.entities.iter().filter(|be| !matches!(be.kind, EntityKind::AttDef(_))) {
                    n = n.saturating_add(1).saturating_add(self.entity(be, depth + 1, inner.as_deref()));
                }
                self.memo.insert(key, n);
                n
            }
        };
        copies.saturating_mul(one)
    }
}
