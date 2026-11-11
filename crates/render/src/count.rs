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
//! drawn short by the 1..=10,000 clamp on a MINSERT's columns and rows.
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
    /// The blocks the drawn references (not those drawn as boxes) refer to: the lower-case name
    /// is the key, the block's own spelling the value.
    pub blocks: BTreeMap<String, String>,
    /// Only from a pass that records ([`Pass::record`]): for each block (its own spelling) what the
    /// count falls by when every reference to it is drawn as a box, and for each layer (lower case)
    /// what it falls by when that layer is switched off, worked out from how often each block copy is
    /// expanded without a walk per block or layer. `None`: that figure overflowed (the walk of its own
    /// is the way to it). When nothing was recorded `savings_unknown` is set.
    pub savings: HashMap<String, Option<u64>>,
    pub layer_savings: HashMap<String, Option<u64>>,
    pub savings_unknown: bool,
}

/// What one counting walk assumes besides the build's own rules: layers counted as switched off,
/// more blocks counted as drawn boxes, and, to tally rather than count, the one block whose
/// boxes are counted.
#[derive(Clone, Copy, Default)]
pub(crate) struct Pass<'a> {
    pub off: &'a [String],
    pub extra_boxes: &'a [String],
    /// Count only the references to this block that are drawn as boxes (nothing else costs).
    pub tally: Option<&'a str>,
    /// Note, for every block, what drawing its references as boxes would save ([`Stats::savings`]).
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
        for (k, v) in other.blocks {
            self.blocks.entry(k).or_insert(v);
        }
        for (mine, theirs) in [(&mut self.savings, other.savings), (&mut self.layer_savings, other.layer_savings)] {
            for (k, v) in theirs {
                let sum = mine.entry(k).or_insert(Some(0));
                *sum = add(*sum, v);
            }
        }
        self.savings_unknown |= other.savings_unknown;
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

/// One expansion of a block's contents in the walk: the block (or dimension block), the depth of the
/// reference that draws it and the layer layer-0 contents take. Depth grows by one along every edge,
/// so the nodes are in order by depth.
#[derive(Clone, PartialEq, Eq, Hash)]
struct NodeKey {
    dim: bool,
    block: usize,
    depth: usize,
    layer: Option<String>,
}

/// A reference met while counting (drawn, within the depth limit): what it draws and what one copy costs.
struct Site {
    /// The block a block reference refers to (its own spelling); `None` for a dimension's block.
    block: Option<String>,
    /// The layer the reference is drawn on (lower case).
    layer: String,
    /// The expansion the reference makes (`None`: a box, which expands nothing).
    child: Option<NodeKey>,
    /// Copies drawn (cells of an array).
    copies: u64,
    /// What one copy costs.
    one: u64,
}

/// A layer of the table as the walk uses it.
struct LayerInfo {
    visible: bool,
    plot: bool,
    name: String,
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
    /// Tally the boxes drawn for this block instead of counting expansions.
    tally: Option<&'a str>,
    layers: HashMap<String, Option<LayerInfo>>,
    /// Record the references met, to work out [`Stats::savings`].
    record: bool,
    /// The references met in the contents being counted now (the top level when none is open).
    sites: Vec<Site>,
    /// The references of each block expansion counted.
    nodes: HashMap<NodeKey, (String, Vec<Site>)>,
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
    pub(crate) fn new(
        d: &'a Drawing,
        plotting: bool,
        frozen: &'a [String],
        pass: Pass<'a>,
        boxes: Option<&'a BoxBlocks>,
        cancel: Option<&'a AtomicBool>,
    ) -> Self {
        Counter {
            d,
            plotting,
            frozen,
            off: pass.off,
            boxes,
            extra_boxes: pass.extra_boxes.iter().map(String::as_str).collect(),
            tally: pass.tally,
            record: pass.record,
            layers: HashMap::new(),
            sites: Vec::new(),
            nodes: HashMap::new(),
            cancel,
            memo: Costs::default(),
            dim_memo: Costs::default(),
            stats: Stats::default(),
        }
    }

    /// The units one block copy or one entity of it costs: one, but none when tallying boxes.
    fn unit(&self) -> u64 {
        u64::from(self.tally.is_none())
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

    /// The notes of the walk. A walk that recorded the references also works out [`Stats::savings`]:
    /// how often each block copy is expanded (the top-level references once, a copy's references
    /// once per expansion of the copy, nodes in order of depth) gives, for a block that is not part
    /// of itself, what drawing all its references as boxes takes off the count: each reference of it
    /// costs `copies x one copy` and a box costs one.
    pub(crate) fn into_stats(mut self) -> Stats {
        if !self.record {
            self.stats.savings_unknown = true;
            return self.stats;
        }
        let (blocks, layers) = savings(&self.sites, &self.nodes);
        self.stats.savings = blocks;
        self.stats.layer_savings = layers;
        self.stats
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
    fn layer_info(&mut self, name: &str) -> Option<&LayerInfo> {
        if !self.layers.contains_key(name) {
            let info = self.d.layer(name).map(|l| LayerInfo { visible: l.visible(), plot: l.plot, name: l.name.clone() });
            self.layers.insert(name.to_string(), info);
        }
        self.layers.get(name).and_then(Option::as_ref)
    }

    /// The layer layer-0 objects inside the reference `e` take (see `sub_ctx`).
    fn inner_layer<'e>(e: &'e Entity, block_layer: Option<&'e str>) -> Option<&'e str> {
        if e.common.layer == "0" { block_layer } else { Some(e.common.layer.as_str()) }
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

    /// A dimension drawn from its block: every entity of it (no unit for the copy).
    fn dimension(&mut self, e: &Entity, dm: &cadcraft_doc::Dimension, depth: usize, block_layer: Option<&str>) -> u64 {
        let d = self.d;
        let Some(blk) = dm.block.as_ref().and_then(|n| d.block(n)).filter(|_| depth < MAX_BLOCK_DEPTH) else { return 0 };
        self.note_layer(Self::layer_of(e, block_layer));
        let inner = Self::inner_layer(e, block_layer);
        let key = (std::sync::Arc::as_ptr(blk) as usize, depth);
        let node = self.record.then(|| NodeKey { dim: true, block: key.0, depth, layer: inner.map(str::to_string) });
        let n = match self.dim_memo.get(key, inner) {
            Some(n) => n,
            None => {
                let outer = node.as_ref().map(|_| std::mem::take(&mut self.sites));
                let mut n = 0u64;
                for be in blk.entities.iter() {
                    n = n.saturating_add(self.unit()).saturating_add(self.entity(be, depth + 1, inner));
                }
                if let (Some(k), Some(outer)) = (&node, outer) {
                    let mine = std::mem::replace(&mut self.sites, outer);
                    self.nodes.insert(k.clone(), (blk.name.clone(), mine));
                }
                self.dim_memo.put(key, inner, n);
                n
            }
        };
        if let Some(child) = node {
            self.sites.push(Site { block: None, layer: Self::layer_of(e, block_layer).to_ascii_lowercase(), child: Some(child), copies: 1, one: n });
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
            if self.record {
                // A box costs one piece and expands nothing; it is still a reference on its layer.
                self.sites.push(Site { block: None, layer: Self::layer_of(e, block_layer).to_ascii_lowercase(), child: None, copies: 1, one: 1 });
            }
            // One outline for the whole reference (an array included): one unit, contents not walked.
            return match self.tally {
                None => 1,
                // Only the outlines that are drawn (a block with nothing in it draws none).
                Some(t) if t == blk.name => u64::from(self.boxes.is_some_and(|b| b.outline(&blk.name, ins, blk.base.xy(), depth).is_some())),
                Some(_) => 0,
            };
        }
        self.stats.blocks.entry(blk.name.to_ascii_lowercase()).or_insert_with(|| blk.name.clone());
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
        let node = self.record.then(|| NodeKey { dim: false, block: key.0, depth, layer: inner.map(str::to_string) });
        let one = match self.memo.get(key, inner) {
            Some(n) => n,
            None => {
                // The copy itself, then each entity (AttDefs are skipped) and what it expands.
                let outer = node.as_ref().map(|_| std::mem::take(&mut self.sites));
                let mut n = self.unit();
                for be in blk.entities.iter().filter(|be| !matches!(be.kind, EntityKind::AttDef(_))) {
                    n = n.saturating_add(self.unit()).saturating_add(self.entity(be, depth + 1, inner));
                }
                if let (Some(k), Some(outer)) = (&node, outer) {
                    let mine = std::mem::replace(&mut self.sites, outer);
                    self.nodes.insert(k.clone(), (blk.name.clone(), mine));
                }
                self.memo.put(key, inner, n);
                n
            }
        };
        if let Some(child) = node {
            self.sites.push(Site {
                block: Some(blk.name.clone()),
                layer: Self::layer_of(e, block_layer).to_ascii_lowercase(),
                child: Some(child),
                copies,
                one,
            });
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

fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a?.checked_add(b?)
}

fn mul(a: Option<u64>, b: u64) -> Option<u64> {
    a?.checked_mul(b)
}

/// A reference of the walk with its place in the graph of block expansions.
struct Flat<'a> {
    /// The expansion that holds it (`None`: a top-level object).
    parent: Option<usize>,
    child: Option<usize>,
    block: Option<&'a str>,
    layer: &'a str,
    copies: u64,
    one: u64,
}

/// What the count falls by when all the references to a block are drawn as boxes, for every block met,
/// and when all the references on a layer are switched off, for every layer met, from how often each
/// block copy is expanded, with no walk per block or layer. `top` are the references of the top-level
/// objects, `nodes` those inside each block expansion.
///
/// Expansions are nodes of a graph whose edges (the references) raise the depth by one, so by depth every
/// node comes after all that hold a reference to it. `mult[n]` is how often the node is expanded. A
/// cut (a block's references, a layer's references) takes off, for each of its references, the
/// expansions of the holding node times `copies x one copy - 1` (a box costs one; a layer switched off
/// costs nothing). That is exact when no cut reference sits inside an expansion that another of them
/// replaces; when one does (a block holding itself, a layer's reference holding another on the layer,
/// as layer 0 contents do) the expansions that pass through a cut reference are taken away first
/// (`through`, found over the nodes below the cut only), so the cost of a cut is that of the part of the
/// graph below it, not of the drawing.
fn savings(top: &[Site], nodes: &HashMap<NodeKey, (String, Vec<Site>)>) -> (HashMap<String, Option<u64>>, HashMap<String, Option<u64>>) {
    let mut keys: Vec<&NodeKey> = nodes.keys().collect();
    keys.sort_by_key(|k| k.depth);
    let idx: HashMap<&NodeKey, usize> = keys.iter().enumerate().map(|(i, k)| (*k, i)).collect();
    let mut flat: Vec<Flat> = Vec::new();
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    let mut block_of: Vec<&str> = Vec::with_capacity(keys.len());
    let mut lists: Vec<(Option<usize>, &[Site])> = vec![(None, top)];
    for (i, k) in keys.iter().enumerate() {
        let (block, sites) = nodes.get(*k).map(|(b, v)| (b.as_str(), v.as_slice())).unwrap_or(("", &[]));
        block_of.push(block);
        lists.push((Some(i), sites));
    }
    for (parent, sites) in lists {
        for s in sites {
            let child = match &s.child {
                Some(k) => match idx.get(k) {
                    Some(&c) => Some(c),
                    None => continue,
                },
                None => None,
            };
            if child.is_some()
                && let Some(o) = parent.and_then(|p| out.get_mut(p))
            {
                o.push(flat.len());
            }
            flat.push(Flat { parent, child, block: s.block.as_deref(), layer: &s.layer, copies: s.copies, one: s.one });
        }
    }
    // How often each expansion happens.
    let mut mult: Vec<Option<u64>> = vec![Some(0); keys.len()];
    for f in flat.iter().filter(|f| f.parent.is_none()) {
        if let Some(m) = f.child.and_then(|c| mult.get_mut(c)) {
            *m = add(*m, Some(f.copies));
        }
    }
    for i in 0..keys.len() {
        let here = mult.get(i).copied().flatten();
        for &fi in out.get(i).map(Vec::as_slice).unwrap_or_default() {
            if let Some(f) = flat.get(fi)
                && let Some(m) = f.child.and_then(|c| mult.get_mut(c))
            {
                *m = add(*m, mul(here, f.copies));
            }
        }
    }
    let times = |f: &Flat| -> Option<u64> {
        match f.parent {
            None => Some(1),
            Some(p) => mult.get(p).copied().flatten(),
        }
    };
    let cost = |f: &Flat, layer: bool| -> Option<u64> {
        if f.one == COUNT_SATURATED {
            return None;
        }
        f.copies.checked_mul(f.one).map(|c| if layer { c } else { c.saturating_sub(1) })
    };
    // The saving of the cut `which` (indices into `flat`): its references switched off (`layer`) or
    // drawn as boxes. `nested`: a cut reference may sit below another of the cut.
    let saving = |which: &[usize], layer: bool, nested: bool, is_cut: &dyn Fn(&Flat) -> bool| -> Option<u64> {
        let mut through: HashMap<usize, Option<u64>> = HashMap::new();
        if nested {
            // The expansions below the cut and how often each is reached through a cut reference.
            let mut cone: std::collections::BTreeSet<usize> = which.iter().filter_map(|&i| flat.get(i)).filter_map(|f| f.child).collect();
            let mut stack: Vec<usize> = cone.iter().copied().collect();
            while let Some(n) = stack.pop() {
                for &fi in out.get(n).map(Vec::as_slice).unwrap_or_default() {
                    if let Some(c) = flat.get(fi).and_then(|f| f.child)
                        && cone.insert(c)
                    {
                        stack.push(c);
                    }
                }
            }
            for f in which.iter().filter_map(|&i| flat.get(i)) {
                if let Some(c) = f.child {
                    let t = through.entry(c).or_insert(Some(0));
                    *t = add(*t, mul(times(f), f.copies));
                }
            }
            for &n in &cone {
                let here = through.get(&n).copied().flatten();
                for &fi in out.get(n).map(Vec::as_slice).unwrap_or_default() {
                    if let Some(f) = flat.get(fi)
                        && !is_cut(f)
                        && let Some(c) = f.child
                    {
                        let t = through.entry(c).or_insert(Some(0));
                        *t = add(*t, mul(here, f.copies));
                    }
                }
            }
        }
        let mut sum = Some(0u64);
        for f in which.iter().filter_map(|&i| flat.get(i)) {
            let held = match f.parent {
                Some(p) => through.get(&p).copied().unwrap_or(Some(0)),
                None => Some(0),
            };
            let once = times(f).zip(held).and_then(|(a, h)| a.checked_sub(h));
            sum = add(sum, once.zip(cost(f, layer)).and_then(|(o, c)| o.checked_mul(c)));
        }
        sum
    };
    let mut by_block: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_layer: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in flat.iter().enumerate() {
        if let Some(b) = f.block {
            by_block.entry(b).or_default().push(i);
        }
        by_layer.entry(f.layer).or_default().push(i);
    }
    // The blocks that are part of themselves, however deep (the only ones whose references nest).
    let mut ids: HashMap<&str, usize> = HashMap::new();
    let mut edges: Vec<Vec<usize>> = Vec::new();
    for f in &flat {
        if let (Some(p), Some(to)) = (f.parent, f.block) {
            let from = block_of.get(p).copied().unwrap_or("");
            let mut intern = |n| match ids.get(n) {
                Some(i) => *i,
                None => {
                    edges.push(Vec::new());
                    ids.insert(n, edges.len() - 1);
                    edges.len() - 1
                }
            };
            let a = intern(from);
            let b = intern(to);
            if let Some(e) = edges.get_mut(a) {
                e.push(b);
            }
        }
    }
    let on_cycle: std::collections::HashSet<usize> = on_a_cycle(&edges).into_iter().collect();
    let mut blocks = HashMap::new();
    for (b, which) in &by_block {
        let nested = ids.get(b).is_some_and(|i| on_cycle.contains(i));
        blocks.insert(b.to_string(), saving(which, false, nested, &|f: &Flat| f.block == Some(*b)));
    }
    let mut layers = HashMap::new();
    for (l, which) in &by_layer {
        layers.insert(l.to_string(), saving(which, true, true, &|f: &Flat| f.layer == *l));
    }
    (blocks, layers)
}

/// The vertices of the graph `edges` (adjacency lists) that lie on a cycle (a self edge or a
/// strongly connected component of two or more), by Tarjan's algorithm without recursion.
fn on_a_cycle(edges: &[Vec<usize>]) -> Vec<usize> {
    let n = edges.len();
    let (mut index, mut low) = (vec![usize::MAX; n], vec![0usize; n]);
    let (mut on_stack, mut stack) = (vec![false; n], Vec::new());
    let (mut next, mut out) = (0usize, Vec::new());
    for root in 0..n {
        if index.get(root).copied() != Some(usize::MAX) {
            continue;
        }
        // (vertex, next edge to look at)
        let mut work = vec![(root, 0usize)];
        while let Some(&(v, i)) = work.last() {
            if i == 0 {
                if let (Some(a), Some(b)) = (index.get_mut(v), low.get_mut(v)) {
                    *a = next;
                    *b = next;
                }
                next += 1;
                stack.push(v);
                if let Some(o) = on_stack.get_mut(v) {
                    *o = true;
                }
            }
            let target = edges.get(v).and_then(|e| e.get(i)).copied();
            if let Some(last) = work.last_mut() {
                last.1 += 1;
            }
            match target {
                Some(w) if index.get(w).copied() == Some(usize::MAX) => work.push((w, 0)),
                Some(w) => {
                    if on_stack.get(w).copied() == Some(true) {
                        let lw = index.get(w).copied().unwrap_or(usize::MAX);
                        if let Some(l) = low.get_mut(v) {
                            *l = (*l).min(lw);
                        }
                    }
                }
                None => {
                    work.pop();
                    let lv = low.get(v).copied().unwrap_or(0);
                    if let Some(&(parent, _)) = work.last()
                        && let Some(l) = low.get_mut(parent)
                    {
                        *l = (*l).min(lv);
                    }
                    if lv == index.get(v).copied().unwrap_or(usize::MAX) {
                        let mut comp = Vec::new();
                        while let Some(w) = stack.pop() {
                            if let Some(o) = on_stack.get_mut(w) {
                                *o = false;
                            }
                            comp.push(w);
                            if w == v {
                                break;
                            }
                        }
                        let self_edge = comp.len() == 1 && edges.get(v).is_some_and(|e| e.contains(&v));
                        if comp.len() > 1 || self_edge {
                            out.extend(comp);
                        }
                    }
                }
            }
        }
    }
    out
}
