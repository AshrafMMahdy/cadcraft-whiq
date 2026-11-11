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
//! top-level object, the layers the drawn references sit on, the blocks they refer to, and arrays
//! drawn short by the 1..=10,000 clamp on a MINSERT's columns and rows. A walk that records the
//! references keeps indices only (`savings.rs` turns them into what each layer and block saves).
//!
//! A reference to a block drawn as a box ([`BoxBlocks`](super::BoxBlocks), plus the blocks of a
//! [`Pass`] being tried) costs one unit however big the block is, as the build takes one for it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use cadcraft_doc::{AnnoDraw, Drawing, Entity, EntityKind, Handle, MAX_BLOCK_DEPTH};

use super::BoxBlocks;

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
    /// The layers (as the build resolves them, names compared ignoring case: the key is the
    /// lower-case name, the value the first spelling met) the drawn references sit on, that are in
    /// the layer table.
    pub layers: BTreeMap<String, String>,
    /// A drawn reference sits on a layer that is not in the layer table (no switch can name it).
    pub off_table: bool,
    pub clamped: BTreeMap<u64, ClampedArray>,
    /// Only from a pass that records ([`Pass::record`]): for every block the drawn references refer to
    /// and that is not drawn as a box (its own spelling), what the count falls by when every reference
    /// to it is drawn as a box; for every layer the drawn references sit on (lower case), what it falls
    /// by when the layer is switched off; and for every block drawn as a box, the outlines drawn for it.
    /// All from the one recording pass, by arithmetic over how the block copies nest, never by a walk
    /// per name. In wide arithmetic: a figure that overflowed even that is [`WIDE_SATURATED`] ("not known").
    pub savings: HashMap<String, u128>,
    pub layer_savings: HashMap<String, u128>,
    pub outlines: HashMap<String, u128>,
    /// The total of the references the recording pass met, in the same wide arithmetic (what is left after
    /// a saving is this minus the saving: exact where the `u64` total has overflowed).
    pub wide_total: u128,
}

/// What one counting walk assumes besides the build's own rules: layers counted as switched off and
/// more blocks counted as drawn boxes.
#[derive(Clone, Copy, Default)]
pub(crate) struct Pass<'a> {
    pub off: &'a [String],
    pub extra_boxes: &'a [String],
    /// Note, for every block and layer, what drawing the block as boxes or switching the layer off
    /// would save, and the outlines drawn ([`Stats::savings`]).
    pub record: bool,
}

impl Stats {
    pub(crate) fn absorb(&mut self, other: Stats) {
        if let Some(l) = other.largest
            && self.largest.as_ref().is_none_or(|m| l.count > m.count)
        {
            self.largest = Some(l);
        }
        for (k, v) in other.layers {
            self.layers.entry(k).or_insert(v);
        }
        self.off_table |= other.off_table;
        for (k, v) in other.clamped {
            self.clamped.entry(k).or_insert(v);
        }
        for (mine, theirs) in
            [(&mut self.savings, other.savings), (&mut self.layer_savings, other.layer_savings), (&mut self.outlines, other.outlines)]
        {
            for (k, v) in theirs {
                let sum = mine.entry(k).or_insert(0);
                *sum = wadd(*sum, v);
            }
        }
        self.wide_total = wadd(self.wide_total, other.wide_total);
    }
}

/// One expansion of a block's contents in the walk: the block (or dimension block), the depth of the
/// reference that draws it and the layer layer-0 contents take (interned, lower case; `None` when the
/// block holds no reference on layer 0, so nothing in it follows the layer: one expansion, not one per
/// layer). Depth grows by one along every edge, so the nodes are in order by depth.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct NodeKey {
    dim: bool,
    block: usize,
    depth: usize,
    layer: Option<u32>,
}

/// What a reference draws: the expansion of a block's contents, or one box.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    /// The recorded expansion (index into [`Recorded::nodes`]).
    Node(u32),
    /// A reference drawn as a box: the block and whether an outline is drawn.
    Box { block: usize, outline: bool },
}

/// A reference met while counting (drawn, within the depth limit), by indices only: no string is kept
/// per record.
#[derive(Clone, Copy)]
pub(crate) struct Site {
    /// The layer the reference is drawn on (interned, lower case: [`Recorded::layers`]).
    pub layer: u32,
    pub target: Target,
    /// Copies drawn (cells of an array).
    pub copies: u64,
}

/// One expansion recorded: the references in the contents of one copy of the block.
pub(crate) struct Node {
    pub dim: bool,
    /// The block (its address; names are in [`Recorded::blocks`]).
    pub block: usize,
    pub depth: u32,
    /// What one copy costs besides its references: the copy itself (a block reference) and one for
    /// every entity of it.
    pub base: u64,
    pub sites: Vec<Site>,
}

/// Everything a recording walk met, to be turned into savings by arithmetic (`savings.rs`).
pub(crate) struct Recorded {
    pub top: Vec<Site>,
    pub nodes: Vec<Node>,
    /// The layers by id, lower case.
    pub layers: Vec<String>,
    /// The block names by address (the drawing's own spelling).
    pub blocks: HashMap<usize, String>,
}


/// A layer of the table as the walk uses it.
pub(crate) struct LayerInfo {
    visible: bool,
    plot: bool,
    name: String,
}

/// The drawing's layers by lower-case name, made once per census (`Drawing::layer` searches the table
/// linearly; a drawing can have thousands of layers). The first layer of a name wins, as
/// `Drawing::layer` finds it.
pub(crate) struct LayerTable(HashMap<String, LayerInfo>);

impl LayerTable {
    pub(crate) fn new(d: &Drawing) -> LayerTable {
        let mut m = HashMap::new();
        for l in &d.layers {
            m.entry(l.name.to_ascii_lowercase()).or_insert_with(|| LayerInfo { visible: l.visible(), plot: l.plot, name: l.name.clone() });
        }
        LayerTable(m)
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
    /// The blocks the picture draws as boxes, and further blocks counted as boxes (what a count
    /// "if this block were a box" asks).
    boxes: Option<&'a BoxBlocks>,
    extra_boxes: HashSet<&'a str>,
    table: &'a LayerTable,
    layers: HashMap<String, Option<&'a LayerInfo>>,
    /// Record the references met, to work out [`Stats::savings`].
    record: bool,
    /// The references met in the contents being counted now (the top level when none is open).
    sites: Vec<Site>,
    /// The expansions recorded, and the layers and block names their records refer to by index.
    nodes: Vec<Node>,
    layer_ids: HashMap<String, u32>,
    layer_lower: HashMap<String, u32>,
    layer_names: Vec<String>,
    block_names: HashMap<usize, String>,
    /// Whether a block holds a reference on layer 0 (its contents then follow the reference's layer).
    follows_layer: HashMap<(usize, bool), bool>,
    /// The build's own stop flag: a stopped count is a partial number the caller must discard.
    cancel: Option<&'a AtomicBool>,
    /// Cost of one copy of a block's contents (a copy is one unit and each entity, AttDefs not; a
    /// dimension's block takes no unit for the copy and every entity), with the index of its recorded
    /// expansion when recording.
    memo: HashMap<NodeKey, (u64, u32)>,
    pub(crate) stats: Stats,
}

impl<'a> Counter<'a> {
    pub(crate) fn new(
        d: &'a Drawing,
        plotting: bool,
        frozen: &'a [String],
        pass: Pass<'a>,
        boxes: Option<&'a BoxBlocks>,
        table: &'a LayerTable,
        cancel: Option<&'a AtomicBool>,
    ) -> Self {
        Counter {
            d,
            plotting,
            frozen,
            off: pass.off,
            boxes,
            extra_boxes: pass.extra_boxes.iter().map(String::as_str).collect(),
            table,
            record: pass.record,
            layers: HashMap::new(),
            sites: Vec::new(),
            nodes: Vec::new(),
            layer_ids: HashMap::new(),
            layer_lower: HashMap::new(),
            layer_names: Vec::new(),
            block_names: HashMap::new(),
            follows_layer: HashMap::new(),
            cancel,
            memo: HashMap::new(),
            stats: Stats::default(),
        }
    }

    /// Whether references to the block `name` (as the drawing spells it) are drawn as boxes in this walk.
    fn boxed(&self, name: &str) -> bool {
        self.boxes.is_some_and(|b| b.contains(name)) || self.extra_boxes.contains(name)
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

    /// The notes of the walk. A walk that recorded the references also works out [`Stats::savings`]
    /// (see `savings.rs`): from how often each expansion is made and what one copy costs, what drawing
    /// all the references to a block as boxes, or switching a layer off, takes off the count.
    pub(crate) fn into_stats(mut self) -> Stats {
        if self.record {
            let rec = Recorded { top: self.sites, nodes: self.nodes, layers: self.layer_names, blocks: self.block_names };
            let got = super::savings::derive(&rec, self.cancel);
            self.stats.savings = got.blocks;
            self.stats.layer_savings = got.layers;
            self.stats.outlines = got.outlines;
            self.stats.wide_total = got.total;
        }
        self.stats
    }

    /// The recorded expansions and the references recorded in all (tests of what a recording keeps).
    #[cfg(test)]
    pub(crate) fn recorded_sizes(&self) -> (usize, usize) {
        (self.nodes.len(), self.sites.len() + self.nodes.iter().map(|n| n.sites.len()).sum::<usize>())
    }

    /// The layer `e` is drawn on inside a reference on `block_layer` (see `resolve_common`).
    fn layer_of<'e>(e: &'e Entity, block_layer: Option<&'e str>) -> &'e str {
        if e.common.layer == "0" { block_layer.unwrap_or("0") } else { e.common.layer.as_str() }
    }

    /// Whether `e` is drawn at all inside a reference on `block_layer` (see `resolve_common`).
    fn drawn(&mut self, e: &Entity, block_layer: Option<&str>) -> bool {
        if self.d.is_hidden_object(e.handle) || !e.common.visible {
            return false;
        }
        let name = Self::layer_of(e, block_layer);
        let plotting = self.plotting;
        self.layer_info(name).is_none_or(|l| l.visible && (!plotting || l.plot))
            && !self.frozen.iter().chain(self.off).any(|f| f.eq_ignore_ascii_case(name))
    }

    /// What the walk needs of the layer `name` (the layer table is searched once per name, not once per
    /// object: a drawing can have thousands of layers).
    fn layer_info(&mut self, name: &str) -> Option<&'a LayerInfo> {
        if let Some(found) = self.layers.get(name) {
            return *found;
        }
        let found = self.table.0.get(&name.to_ascii_lowercase());
        self.layers.insert(name.to_string(), found);
        found
    }

    /// The id of the layer `name` (names compare ignoring case); a name is stored once however often,
    /// and in however many spellings, it is met.
    fn layer_id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.layer_ids.get(name) {
            return id;
        }
        let lower = name.to_ascii_lowercase();
        let id = match self.layer_lower.get(&lower) {
            Some(&id) => id,
            None => {
                let id = u32::try_from(self.layer_names.len()).unwrap_or(u32::MAX);
                self.layer_names.push(lower.clone());
                self.layer_lower.insert(lower, id);
                id
            }
        };
        self.layer_ids.insert(name.to_string(), id);
        id
    }

    /// The layer layer-0 objects inside the reference `e` take (see `sub_ctx`).
    fn inner_layer<'e>(e: &'e Entity, block_layer: Option<&'e str>) -> Option<&'e str> {
        if e.common.layer == "0" { block_layer } else { Some(e.common.layer.as_str()) }
    }

    /// The key of the expansion of `blk` at `depth` inside a reference whose contents take `inner` for
    /// layer 0. A block with no reference on layer 0 does not depend on it (its contents are the same on
    /// every layer), so it has one key.
    fn key_of(&mut self, blk: &std::sync::Arc<cadcraft_doc::Block>, dim: bool, depth: usize, inner: Option<&str>) -> NodeKey {
        let block = std::sync::Arc::as_ptr(blk) as usize;
        let follows = match self.follows_layer.get(&(block, dim)) {
            Some(f) => *f,
            None => {
                let f = blk.entities.iter().any(|be| {
                    be.common.layer == "0"
                        && match &be.kind {
                            EntityKind::Insert(_) => true,
                            EntityKind::MLeader(m) => m.block.is_some(),
                            EntityKind::Dimension(dm) => dm.block.is_some(),
                            _ => false,
                        }
                });
                self.follows_layer.insert((block, dim), f);
                f
            }
        };
        let layer = if follows { inner.map(|l| self.layer_id(l)) } else { None };
        NodeKey { dim, block, depth, layer }
    }

    fn note_layer(&mut self, name: &str) {
        let Some(found) = self.layer_info(name).map(|l| l.name.clone()) else {
            self.stats.off_table = true;
            return;
        };
        self.stats.layers.entry(found.to_ascii_lowercase()).or_insert(found);
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

    /// The cost of one copy of `blk`'s contents (and, when recording, its expansion's index): taken from
    /// the memo, else counted (the copy itself when `dim` is false, then every entity: a block
    /// reference skips AttDefs, a dimension's block counts them) and remembered.
    fn contents(&mut self, blk: &std::sync::Arc<cadcraft_doc::Block>, key: NodeKey, inner: Option<&str>) -> (u64, u32) {
        if let Some(&found) = self.memo.get(&key) {
            return found;
        }
        let outer = self.record.then(|| std::mem::take(&mut self.sites));
        let mut base = u64::from(!key.dim);
        let mut n = base;
        for be in blk.entities.iter().filter(|be| key.dim || !matches!(be.kind, EntityKind::AttDef(_))) {
            base = base.saturating_add(1);
            n = n.saturating_add(1).saturating_add(self.entity(be, key.depth + 1, inner));
        }
        let mut index = u32::MAX;
        if let Some(outer) = outer {
            let sites = std::mem::replace(&mut self.sites, outer);
            index = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
            self.nodes.push(Node { dim: key.dim, block: key.block, depth: u32::try_from(key.depth).unwrap_or(u32::MAX), base, sites });
            self.block_names.entry(key.block).or_insert_with(|| blk.name.clone());
        }
        self.memo.insert(key, (n, index));
        (n, index)
    }

    /// A dimension drawn from its block: every entity of it (no unit for the copy).
    fn dimension(&mut self, e: &Entity, dm: &cadcraft_doc::Dimension, depth: usize, block_layer: Option<&str>) -> u64 {
        let d = self.d;
        let Some(blk) = dm.block.as_ref().and_then(|n| d.block(n)).filter(|_| depth < MAX_BLOCK_DEPTH) else { return 0 };
        self.note_layer(Self::layer_of(e, block_layer));
        let inner = Self::inner_layer(e, block_layer);
        let key = self.key_of(blk, true, depth, inner);
        let (n, index) = self.contents(blk, key, inner);
        if self.record {
            let layer = self.layer_id(Self::layer_of(e, block_layer));
            self.sites.push(Site { layer, target: Target::Node(index), copies: 1 });
        }
        n
    }

    fn insert(&mut self, e: &Entity, ins: &cadcraft_doc::Insert, depth: usize, block_layer: Option<&str>) -> u64 {
        if depth >= MAX_BLOCK_DEPTH {
            return 0;
        }
        let d = self.d;
        let Some(blk) = d.block(&ins.block) else { return 0 };
        self.note_layer(Self::layer_of(e, block_layer));
        if self.boxed(&blk.name) {
            // One outline for the whole reference (an array included): one unit, contents not walked.
            if self.record {
                // It is still a reference on its layer; for a block the picture draws as boxes the
                // site says whether an outline is drawn (a block with nothing in it draws none).
                let block = std::sync::Arc::as_ptr(blk) as usize;
                self.block_names.entry(block).or_insert_with(|| blk.name.clone());
                let drawn = self.boxes.is_some_and(|b| b.contains(&blk.name)).then(|| self.boxes.is_some_and(|b| b.outline(&blk.name, ins, blk.base.xy(), depth).is_some()));
                // A block that is boxed only in a trial (`extra_boxes`) is not drawn as an outline.
                let target = Target::Box { block: if drawn.is_some() { block } else { 0 }, outline: drawn.unwrap_or(false) };
                let layer = self.layer_id(Self::layer_of(e, block_layer));
                self.sites.push(Site { layer, target, copies: 1 });
            }
            return 1;
        }
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
        let key = self.key_of(blk, false, depth, inner);
        let (one, index) = self.contents(blk, key, inner);
        if self.record {
            let layer = self.layer_id(Self::layer_of(e, block_layer));
            self.sites.push(Site { layer, target: Target::Node(index), copies });
        }
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

/// The wide counts of the savings arithmetic: `u128`, saturating; [`WIDE_SATURATED`] is "not even this wide".
pub(crate) const WIDE_SATURATED: u128 = u128::MAX;

/// `a + b` in the wide counts.
pub(crate) fn wadd(a: u128, b: u128) -> u128 {
    if a == WIDE_SATURATED || b == WIDE_SATURATED { WIDE_SATURATED } else { a.checked_add(b).unwrap_or(WIDE_SATURATED) }
}
