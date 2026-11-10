//! The block expansions a build will ask for, counted without building: arithmetic over the block
//! table. [`Builder::expand`](super::Builder) takes one unit for every block copy (each MINSERT
//! cell) and one for every entity drawn inside it, nested references included; the count here is
//! that number, so `count == DisplayList::expanded` of a build with no limit.
//!
//! The walk follows the build's own rules for what is drawn at all (hidden objects, hidden
//! objects' layers, a viewport's frozen layers, annotative objects not shown at the scale,
//! [`MAX_BLOCK_DEPTH`]), so a reference that is not drawn costs nothing. Each block is counted
//! once per (depth, layer of the reference that draws it) and remembered (block references and
//! dimension blocks in a map each: their costs differ), so the work is proportional to the block
//! table, not to the expanded size, and a block that refers to itself stops at the depth limit like
//! the build does. A top-level annotative object counts as the build draws it: hidden costs
//! nothing, and a scaled dimension is drawn from its definition (`anno_scaled` drops its block),
//! so it costs nothing either; scaled multileaders and references keep their block.
//!
//! The same walk notes what the answers to an agent need when a build is too big: the largest
//! top-level object, the layers the drawn references sit on, and arrays drawn short by the
//! 1..=10,000 clamp on a MINSERT's columns and rows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use cadcraft_doc::{AnnoDraw, Drawing, Entity, EntityKind, Handle, MAX_BLOCK_DEPTH};

/// The most columns or rows of a MINSERT the build draws (`blocks::insert` clamps to 1..=this).
pub(crate) const MAX_ARRAY_SIDE: u32 = 10_000;

/// The count when it overflowed `u64`: "more than can be counted". No real count is this exactly.
pub const COUNT_SATURATED: u64 = u64::MAX;

/// The largest top-level object of a count: the objects of a build are expanded whole, so no
/// window can split what one of them needs.
#[derive(Clone, Debug, PartialEq)]
pub struct LargestRef {
    pub handle: Handle,
    /// "block reference", "array of block references (MINSERT)", "dimension" or "multileader".
    pub kind: &'static str,
    /// The block it expands.
    pub block: String,
    /// The expansions this one object asks for ([`COUNT_SATURATED`] when more than can be counted).
    pub count: u64,
}

/// A MINSERT with more than 10,000 columns or rows: the build draws the first 10,000 of each.
#[derive(Clone, Debug, PartialEq)]
pub struct ClampedArray {
    pub handle: Handle,
    pub block: String,
    pub cols: u32,
    pub rows: u32,
    pub drawn_cols: u32,
    pub drawn_rows: u32,
}

/// What a walk notes besides the count.
#[derive(Default)]
pub(crate) struct Stats {
    pub largest: Option<LargestRef>,
    /// The layers (as the build resolves them) the drawn references sit on.
    pub layers: BTreeSet<String>,
    pub clamped: BTreeMap<u64, ClampedArray>,
}

impl Stats {
    pub(crate) fn absorb(&mut self, other: Stats) {
        if let Some(l) = other.largest
            && self.largest.as_ref().is_none_or(|m| l.count > m.count)
        {
            self.largest = Some(l);
        }
        self.layers.extend(other.layers);
        for (k, v) in other.clamped {
            self.clamped.entry(k).or_insert(v);
        }
    }
}

/// Costs by (block, depth) and then the layer layer-0 contents take (`None`: no layer yet), looked
/// up by `&str`, so a visit that finds its cost clones nothing.
#[derive(Default)]
struct Costs {
    by_key: HashMap<(usize, usize), ByLayer>,
}

#[derive(Default)]
struct ByLayer {
    none: Option<u64>,
    named: HashMap<String, u64>,
}

impl Costs {
    fn get(&self, key: (usize, usize), layer: Option<&str>) -> Option<u64> {
        let b = self.by_key.get(&key)?;
        match layer {
            None => b.none,
            Some(l) => b.named.get(l).copied(),
        }
    }
    fn put(&mut self, key: (usize, usize), layer: Option<&str>, n: u64) {
        let b = self.by_key.entry(key).or_default();
        match layer {
            None => b.none = Some(n),
            Some(l) => {
                b.named.insert(l.to_string(), n);
            }
        }
    }
}

/// What counting needs to know of the build it stands for.
pub(crate) struct Counter<'a> {
    d: &'a Drawing,
    /// Plotting skips layers marked "do not plot" ([`Options`](super::Options) of `build_plot`).
    plotting: bool,
    /// A viewport's frozen layers (model space seen through it).
    frozen: &'a [String],
    /// Layers counted as switched off (what a count "if this layer were off" asks).
    off: &'a [String],
    /// The build's own stop flag: a stopped count is a partial number the caller must discard.
    cancel: Option<&'a AtomicBool>,
    /// Cost of one copy of a block's contents: (the block, depth of the reference, the layer
    /// layer-0 contents take) -> expansions. A copy is one unit and each entity (not AttDefs).
    memo: Costs,
    /// The same for a dimension's block: no unit for the copy, every entity counts (AttDefs too).
    dim_memo: Costs,
    pub(crate) stats: Stats,
}

impl<'a> Counter<'a> {
    pub(crate) fn new(d: &'a Drawing, plotting: bool, frozen: &'a [String], off: &'a [String], cancel: Option<&'a AtomicBool>) -> Self {
        Counter { d, plotting, frozen, off, cancel, memo: Costs::default(), dim_memo: Costs::default(), stats: Stats::default() }
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// The expansions drawing the top-level object `e` asks for. `draw` says how an annotative
    /// object draws here (see `entity_parts`): hidden ones cost nothing, and a scaled dimension
    /// is drawn from its definition (`anno_scaled` sets its block to `None`), so it costs nothing.
    pub(crate) fn top(&mut self, e: &Entity, draw: AnnoDraw) -> u64 {
        match draw {
            AnnoDraw::Hidden => return 0,
            AnnoDraw::Scaled(_) if matches!(e.kind, EntityKind::Dimension(_)) => return 0,
            _ => {}
        }
        let n = self.entity(e, 0, None);
        if n > 0 && self.stats.largest.as_ref().is_none_or(|m| n > m.count) {
            self.stats.largest = reference_of(e).map(|(kind, block)| LargestRef { handle: e.handle, kind, block, count: n });
        }
        n
    }

    /// The layer `e` is drawn on inside a reference on `block_layer` (see `resolve_common`).
    fn layer_of<'e>(e: &'e Entity, block_layer: Option<&'e str>) -> &'e str {
        if e.common.layer == "0" { block_layer.unwrap_or("0") } else { e.common.layer.as_str() }
    }

    /// Whether `e` is drawn at all inside a reference on `block_layer` (see `resolve_common`).
    fn drawn(&self, e: &Entity, block_layer: Option<&str>) -> bool {
        if self.d.is_hidden_object(e.handle) || !e.common.visible {
            return false;
        }
        let name = Self::layer_of(e, block_layer);
        self.d.layer(name).is_none_or(|l| l.visible() && (!self.plotting || l.plot))
            && !self.frozen.iter().chain(self.off).any(|f| f.eq_ignore_ascii_case(name))
    }

    /// The layer layer-0 objects inside the reference `e` take (see `sub_ctx`).
    fn inner_layer<'e>(e: &'e Entity, block_layer: Option<&'e str>) -> Option<&'e str> {
        if e.common.layer == "0" { block_layer } else { Some(e.common.layer.as_str()) }
    }

    fn note_layer(&mut self, name: &str) {
        if !self.stats.layers.contains(name) {
            self.stats.layers.insert(name.to_string());
        }
    }

    fn entity(&mut self, e: &Entity, depth: usize, block_layer: Option<&str>) -> u64 {
        if self.cancelled() || !self.drawn(e, block_layer) {
            return 0;
        }
        match &e.kind {
            EntityKind::Insert(ins) => self.insert(e, ins, depth, block_layer),
            EntityKind::MLeader(m) => m.block.as_ref().map_or(0, |ins| self.insert(e, ins, depth, block_layer)),
            EntityKind::Dimension(dm) => self.dimension(e, dm, depth, block_layer),
            _ => 0,
        }
    }

    /// A dimension drawn from its block: every entity of it (no unit for the copy).
    fn dimension(&mut self, e: &Entity, dm: &cadcraft_doc::Dimension, depth: usize, block_layer: Option<&str>) -> u64 {
        let d = self.d;
        let Some(blk) = dm.block.as_ref().and_then(|n| d.block(n)).filter(|_| depth < MAX_BLOCK_DEPTH) else { return 0 };
        self.note_layer(Self::layer_of(e, block_layer));
        let inner = Self::inner_layer(e, block_layer);
        let key = (std::sync::Arc::as_ptr(blk) as usize, depth);
        if let Some(n) = self.dim_memo.get(key, inner) {
            return n;
        }
        let mut n = 0u64;
        for be in blk.entities.iter() {
            n = n.saturating_add(1).saturating_add(self.entity(be, depth + 1, inner));
        }
        self.dim_memo.put(key, inner, n);
        n
    }

    fn insert(&mut self, e: &Entity, ins: &cadcraft_doc::Insert, depth: usize, block_layer: Option<&str>) -> u64 {
        if depth >= MAX_BLOCK_DEPTH {
            return 0;
        }
        let d = self.d;
        let Some(blk) = d.block(&ins.block) else { return 0 };
        self.note_layer(Self::layer_of(e, block_layer));
        if ins.cols > MAX_ARRAY_SIDE || ins.rows > MAX_ARRAY_SIDE {
            self.stats.clamped.entry(e.handle.0).or_insert_with(|| ClampedArray {
                handle: e.handle,
                block: ins.block.clone(),
                cols: ins.cols,
                rows: ins.rows,
                drawn_cols: ins.cols.clamp(1, MAX_ARRAY_SIDE),
                drawn_rows: ins.rows.clamp(1, MAX_ARRAY_SIDE),
            });
        }
        let copies = u64::from(ins.cols.clamp(1, MAX_ARRAY_SIDE)) * u64::from(ins.rows.clamp(1, MAX_ARRAY_SIDE));
        let inner = Self::inner_layer(e, block_layer);
        let key = (std::sync::Arc::as_ptr(blk) as usize, depth);
        let one = match self.memo.get(key, inner) {
            Some(n) => n,
            None => {
                // The copy itself, then each entity (AttDefs are skipped) and what it expands.
                let mut n = 1u64;
                for be in blk.entities.iter().filter(|be| !matches!(be.kind, EntityKind::AttDef(_))) {
                    n = n.saturating_add(1).saturating_add(self.entity(be, depth + 1, inner));
                }
                self.memo.put(key, inner, n);
                n
            }
        };
        copies.saturating_mul(one)
    }
}

/// What kind of reference `e` is and the block it expands, for the largest-object note.
fn reference_of(e: &Entity) -> Option<(&'static str, String)> {
    match &e.kind {
        EntityKind::Insert(i) if u64::from(i.cols.clamp(1, MAX_ARRAY_SIDE)) * u64::from(i.rows.clamp(1, MAX_ARRAY_SIDE)) > 1 => {
            Some(("array of block references (MINSERT)", i.block.clone()))
        }
        EntityKind::Insert(i) => Some(("block reference", i.block.clone())),
        EntityKind::MLeader(m) => m.block.as_ref().map(|i| ("multileader", i.block.clone())),
        EntityKind::Dimension(dm) => dm.block.clone().map(|b| ("dimension", b)),
        _ => None,
    }
}
