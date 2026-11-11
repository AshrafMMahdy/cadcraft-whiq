//! What the count falls by when every reference to a block is drawn as a box, or a layer is switched off,
//! worked out from the references one recording walk met, by arithmetic: no walk per name and no walk up
//! through the ancestors of anything.
//!
//! The recorded expansions are the nodes of a graph; every reference raises the depth by one, so the
//! nodes are in order by depth. Three numbers per node, each made once:
//!
//! * `down(n)`: what one copy of the expansion costs, from the leaves up;
//! * `up(n)`: how many copies of it are drawn in all, from the top level down.
//!
//! A name (a block, or a layer) cuts the references that carry it; a cut reference costs what its box
//! costs (one unit, or nothing for a layer). The removal is counted at the DEEPEST cut of every path:
//! a cut reference takes off `up(holder) x (what it costs - what it is replaced by - copies x what the same
//! name already removes inside one copy of its child)`, and the cut references below it take off the rest
//! (the sum telescopes, so nothing is counted twice). What the name removes inside one copy of a child is a
//! memoised walk that goes only into expansions that hold, somewhere below, a reference of that name (a
//! per-expansion set of names, built once), so a name costs its own references and the part of the graph
//! that leads from one of them to another of the same name, never the drawing.
//!
//! The arithmetic is `u128` and saturates; a figure that did overflow is [`WIDE_SATURATED`] and taints
//! everything computed from it ("not known"), never a wrong number.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::count::{Node, Recorded, Target, WIDE_SATURATED as SAT, wadd as add};

fn mul(a: u128, b: u128) -> u128 {
    if a == 0 || b == 0 {
        0
    } else if a == SAT || b == SAT {
        SAT
    } else {
        a.checked_mul(b).unwrap_or(SAT)
    }
}

/// `a - b` for `b <= a`; not known when either is not.
fn sub(a: u128, b: u128) -> u128 {
    if a == SAT || b == SAT { SAT } else { a.saturating_sub(b) }
}

/// The references to one expansion (or box) from one expansion, on one layer, summed.
struct Item {
    parent: Option<u32>,
    target: Target,
    layer: u32,
    /// The block a block reference refers to (its address); 0 for a dimension or a box.
    block: usize,
    /// Copies drawn in all (cells of the arrays) and the number of references summed.
    copies: u128,
    refs: u128,
}

/// A name that cuts references.
#[derive(Clone, Copy)]
enum Name {
    Layer(u32),
    Block(usize),
}

impl Name {
    fn cuts(self, f: &Item) -> bool {
        match self {
            Name::Layer(l) => f.layer == l,
            Name::Block(b) => f.block == b,
        }
    }
}

pub(crate) struct Derived {
    /// The total of the recorded references, in wide arithmetic.
    pub total: u128,
    /// For each block the unboxed references refer to (its spelling): what the count falls by when all its
    /// references are drawn as boxes.
    pub blocks: HashMap<String, u128>,
    /// For each layer the references sit on (lower case): what the count falls by when it is switched off.
    pub layers: HashMap<String, u128>,
    /// For each block drawn as boxes: the outlines drawn.
    pub outlines: HashMap<String, u128>,
}

struct Graph<'a> {
    nodes: &'a [Node],
    items: Vec<Item>,
    /// The items held by each node.
    held: Vec<Vec<u32>>,
    down: Vec<u128>,
    up: Vec<u128>,
    /// The names (bits) found at or below each node; an empty row: none.
    rows: Vec<Vec<u64>>,
}

impl Graph<'_> {
    fn cost(&self, f: &Item) -> u128 {
        match f.target {
            Target::Node(c) => mul(f.copies, self.down.get(c as usize).copied().unwrap_or(0)),
            Target::Box { .. } => f.copies,
        }
    }

    /// What the name takes off one copy of expansion `n` (its cut references' whole cost less what they
    /// are replaced by, the cut ones below a cut one counted inside it).
    fn within(&self, name: Name, bit: Option<usize>, n: u32, memo: &mut HashMap<u32, u128>) -> u128 {
        if let Some(v) = memo.get(&n) {
            return *v;
        }
        let mut sum = 0u128;
        for f in self.held.get(n as usize).into_iter().flatten().filter_map(|&i| self.items.get(i as usize)) {
            if name.cuts(f) {
                sum = add(sum, sub(self.cost(f), self.replaced(name, f)));
            } else if let (Target::Node(c), Some(b)) = (f.target, bit)
                && self.present(c, b)
            {
                sum = add(sum, mul(f.copies, self.within(name, bit, c, memo)));
            }
        }
        memo.insert(n, sum);
        sum
    }

    /// What the cut reference `f` costs instead: a box is one unit (per reference); a layer switched off, nothing.
    fn replaced(&self, name: Name, f: &Item) -> u128 {
        match name {
            Name::Block(_) => f.refs,
            Name::Layer(_) => 0,
        }
    }

    fn present(&self, node: u32, bit: usize) -> bool {
        self.rows.get(node as usize).and_then(|r| r.get(bit / 64)).is_some_and(|w| w >> (bit % 64) & 1 == 1)
    }

    fn copies_of_holder(&self, f: &Item) -> u128 {
        match f.parent {
            None => 1,
            Some(p) => self.up.get(p as usize).copied().unwrap_or(0),
        }
    }

    /// What the count falls by when the name's references are cut.
    fn removal(&self, name: Name, bit: Option<usize>, cut: &[u32], memo: &mut HashMap<u32, u128>) -> u128 {
        memo.clear();
        let mut total = 0u128;
        for f in cut.iter().filter_map(|&i| self.items.get(i as usize)) {
            let inner = match (f.target, bit) {
                (Target::Node(c), Some(b)) if self.present(c, b) => self.within(name, bit, c, memo),
                _ => 0,
            };
            let own = sub(sub(self.cost(f), self.replaced(name, f)), mul(f.copies, inner));
            total = add(total, mul(self.copies_of_holder(f), own));
        }
        total
    }
}

/// The tables from the recorded references. Stops early when `cancel` is set (checked per node and per name):
/// what comes back then is partial and the caller discards it, as it does a stopped count.
pub(crate) fn derive(rec: &Recorded, cancel: Option<&AtomicBool>) -> Derived {
    let stopped = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let none = || Derived { total: 0, blocks: HashMap::new(), layers: HashMap::new(), outlines: HashMap::new() };
    let nodes = rec.nodes.as_slice();
    // The references, summed per (holder, what it draws, layer).
    let mut at: HashMap<(u32, Target, u32), usize> = HashMap::new();
    let mut items: Vec<Item> = Vec::new();
    let lists = std::iter::once((None, rec.top.as_slice()))
        .chain(nodes.iter().enumerate().map(|(i, n)| (Some(u32::try_from(i).unwrap_or(u32::MAX)), n.sites.as_slice())));
    for (parent, sites) in lists {
        if stopped() {
            return none();
        }
        for s in sites {
            let slot = *at.entry((parent.unwrap_or(u32::MAX), s.target, s.layer)).or_insert_with(|| {
                let block = match s.target {
                    Target::Node(c) => nodes.get(c as usize).filter(|n| !n.dim).map_or(0, |n| n.block),
                    Target::Box { .. } => 0,
                };
                items.push(Item { parent, target: s.target, layer: s.layer, block, copies: 0, refs: 0 });
                items.len() - 1
            });
            if let Some(f) = items.get_mut(slot) {
                f.copies = add(f.copies, u128::from(s.copies));
                f.refs = add(f.refs, 1);
            }
        }
    }
    drop(at);
    let mut order: Vec<u32> = (0..nodes.len()).map(|i| u32::try_from(i).unwrap_or(u32::MAX)).collect();
    order.sort_by_key(|&i| nodes.get(i as usize).map_or(0, |n| n.depth));
    let mut held: Vec<Vec<u32>> = vec![Vec::new(); nodes.len()];
    for (i, f) in items.iter().enumerate() {
        if let Some(h) = f.parent.and_then(|p| held.get_mut(p as usize)) {
            h.push(u32::try_from(i).unwrap_or(u32::MAX));
        }
    }
    let mut g = Graph { nodes, items, held, down: vec![0; nodes.len()], up: vec![0; nodes.len()], rows: vec![Vec::new(); nodes.len()] };
    // down(n): bottom up.
    for &n in order.iter().rev() {
        if stopped() {
            return none();
        }
        let mut d = u128::from(nodes.get(n as usize).map_or(0, |x| x.base));
        for f in g.held.get(n as usize).into_iter().flatten().filter_map(|&i| g.items.get(i as usize)) {
            d = add(d, g.cost(f));
        }
        if let Some(slot) = g.down.get_mut(n as usize) {
            *slot = d;
        }
    }
    // up(n): top down.
    let mut up = vec![0u128; nodes.len()];
    for f in g.items.iter().filter(|f| f.parent.is_none()) {
        if let Target::Node(c) = f.target
            && let Some(u) = up.get_mut(c as usize)
        {
            *u = add(*u, f.copies);
        }
    }
    for &n in &order {
        if stopped() {
            return none();
        }
        let mine = up.get(n as usize).copied().unwrap_or(0);
        for f in g.held.get(n as usize).into_iter().flatten().filter_map(|&i| g.items.get(i as usize)) {
            if let Target::Node(c) = f.target
                && let Some(u) = up.get_mut(c as usize)
            {
                *u = add(*u, mul(mine, f.copies));
            }
        }
    }
    g.up = up;
    let total = g.items.iter().filter(|f| f.parent.is_none()).fold(0u128, |t, f| add(t, g.cost(f)));
    // The items of each name; only a name held by two expansions or more can have a cut under a cut.
    let mut by_layer: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut by_block: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut by_boxed: HashMap<usize, Vec<u32>> = HashMap::new();
    for (i, f) in g.items.iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        by_layer.entry(f.layer).or_default().push(i);
        if f.block != 0 {
            by_block.entry(f.block).or_default().push(i);
        }
        if let Target::Box { block, .. } = f.target
            && block != 0
        {
            by_boxed.entry(block).or_default().push(i);
        }
    }
    let holders = |list: &[u32]| {
        let first = list.first().and_then(|&i| g.items.get(i as usize)).map(|f| f.parent);
        list.iter().filter_map(|&i| g.items.get(i as usize)).any(|f| Some(f.parent) != first)
    };
    let mut layer_bit: HashMap<u32, usize> = HashMap::new();
    let mut block_bit: HashMap<usize, usize> = HashMap::new();
    let mut bits = 0usize;
    for (l, list) in &by_layer {
        if holders(list) {
            layer_bit.insert(*l, bits);
            bits += 1;
        }
    }
    for (b, list) in &by_block {
        if holders(list) {
            block_bit.insert(*b, bits);
            bits += 1;
        }
    }
    if bits > 0 {
        let words = bits.div_ceil(64);
        for &n in order.iter().rev() {
            if stopped() {
                return none();
            }
            let mut row = vec![0u64; words];
            let set = |row: &mut Vec<u64>, b: usize| {
                if let Some(w) = row.get_mut(b / 64) {
                    *w |= 1 << (b % 64);
                }
            };
            for f in g.held.get(n as usize).into_iter().flatten().filter_map(|&i| g.items.get(i as usize)) {
                if let Some(&b) = layer_bit.get(&f.layer) {
                    set(&mut row, b);
                }
                if let Some(&b) = block_bit.get(&f.block) {
                    set(&mut row, b);
                }
                if let Target::Node(c) = f.target
                    && let Some(child) = g.rows.get(c as usize).filter(|r| !r.is_empty())
                {
                    for (w, x) in row.iter_mut().zip(child) {
                        *w |= x;
                    }
                }
            }
            if row.iter().any(|w| *w != 0) && let Some(slot) = g.rows.get_mut(n as usize) {
                *slot = row;
            }
        }
    }
    let mut memo: HashMap<u32, u128> = HashMap::new();
    let mut layers = HashMap::new();
    for (l, cut) in &by_layer {
        if stopped() {
            return none();
        }
        let name = rec.layers.get(*l as usize).cloned().unwrap_or_default();
        layers.insert(name, g.removal(Name::Layer(*l), layer_bit.get(l).copied(), cut, &mut memo));
    }
    let mut blocks = HashMap::new();
    for (b, cut) in &by_block {
        if stopped() {
            return none();
        }
        let name = rec.blocks.get(b).cloned().unwrap_or_default();
        blocks.insert(name, g.removal(Name::Block(*b), block_bit.get(b).copied(), cut, &mut memo));
    }
    let mut outlines = HashMap::new();
    for (b, list) in &by_boxed {
        if stopped() {
            return none();
        }
        let mut sum = 0u128;
        for f in list.iter().filter_map(|&i| g.items.get(i as usize)) {
            if matches!(f.target, Target::Box { outline: true, .. }) {
                sum = add(sum, mul(g.copies_of_holder(f), f.refs));
            }
        }
        outlines.insert(rec.blocks.get(b).cloned().unwrap_or_default(), sum);
    }
    let _ = g.nodes;
    Derived { total, blocks, layers, outlines }
}
