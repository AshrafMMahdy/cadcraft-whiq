//! Block references drawn as one outline box instead of their contents ("blocks as boxes").
//!
//! A picture that names blocks in [`Options::blocks_as_boxes`](super::Options) draws every
//! reference to one of them, at any nesting depth, as the outline of the reference's extents (the
//! block's extents under the reference's transform, so a rotated reference gives a rotated
//! rectangle; a MINSERT array is one outline of the whole array) on the reference's own layer and
//! colour, and takes one block-expansion unit for it. The block's contents are not walked.

use cadcraft_doc::{BlockBoundsCache, Drawing, Insert, MAX_BLOCK_DEPTH};
use cadcraft_geom::{Bounds2, Mat3, Vec2};

use super::count::MAX_ARRAY_SIDE;

/// The blocks a picture draws as boxes, with the extents of each (in its own coordinates, for a
/// reference at every nesting depth: the depth limit cuts nested contents short, so the extents
/// depend on it). Made once per picture from the drawing the picture is built from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxBlocks {
    entries: Vec<Entry>,
    /// The blocks' own spellings -> their place in `entries`.
    index: std::collections::HashMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    /// The block's name as the drawing spells it.
    name: String,
    /// `depth_bounds[d]`: the contents' box for a reference at depth `d`.
    depth_bounds: Vec<Bounds2>,
}

impl BoxBlocks {
    /// The boxes for `names` (compared with the drawing's block names ignoring case; a name that
    /// is no block of `d` is left out, and a name given twice counts once).
    pub fn new(d: &Drawing, names: &[String]) -> BoxBlocks {
        let mut entries: Vec<Entry> = Vec::new();
        let mut cache = BlockBoundsCache::default();
        for n in names {
            let Some(blk) = d.block(n) else { continue };
            if entries.iter().any(|e| e.name.eq_ignore_ascii_case(&blk.name)) {
                continue;
            }
            let depth_bounds = (0..MAX_BLOCK_DEPTH).map(|depth| cache.contents(d, &blk.name, depth)).collect();
            entries.push(Entry { name: blk.name.clone(), depth_bounds });
        }
        let index = entries.iter().enumerate().map(|(i, e)| (e.name.clone(), i)).collect();
        BoxBlocks { entries, index }
    }

    /// The blocks drawn as boxes, by name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether references to the block `name`, as the drawing spells it, are drawn as boxes. (A
    /// reference's own spelling may differ in case: resolve it with `Drawing::block` first.)
    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// The outline of the reference `ins` to the boxed block `name` seen at nesting `depth`, in
    /// the coordinates of the space the reference sits in: a closed polygon (the first point
    /// repeated), or `None` when the block has no extents.
    pub(crate) fn outline(&self, name: &str, ins: &Insert, base: Vec2, depth: usize) -> Option<Vec<Vec2>> {
        let inner = self.entries.get(*self.index.get(name)?)?.depth_bounds.get(depth)?;
        outline(ins, base, inner)
    }
}

/// The outline of the whole of `ins` (every copy of an array) when one copy's contents fill
/// `inner` in block coordinates and the block's base point is `base`.
///
/// A copy is `inner` under the reference's scale, rotation and insertion point; copy (c, r) is
/// moved by the column and row spacing turned by the rotation. Seen in the frame turned back by
/// the rotation every copy is an axis-aligned rectangle, so the array is the box of the four
/// corner copies there, turned forward again: a rotated reference gives a rotated rectangle.
fn outline(ins: &Insert, base: Vec2, inner: &Bounds2) -> Option<Vec<Vec2>> {
    if inner.is_empty() {
        return None;
    }
    let cols = ins.cols.clamp(1, MAX_ARRAY_SIDE);
    let rows = ins.rows.clamp(1, MAX_ARRAY_SIDE);
    let m = ins.transform(base);
    let back = Mat3::rotate(-ins.rotation);
    let mut seen = Bounds2::EMPTY;
    for (c, r) in [(0, 0), (cols - 1, 0), (0, rows - 1), (cols - 1, rows - 1)] {
        let off = Vec2::new(ins.col_spacing * f64::from(c), ins.row_spacing * f64::from(r)).rotate(ins.rotation);
        for corner in inner.corners() {
            seen.add(back.apply(m.apply(corner) + off));
        }
    }
    if seen.is_empty() {
        return None;
    }
    let turn = Mat3::rotate(ins.rotation);
    let mut pts: Vec<Vec2> = seen.corners().iter().map(|c| turn.apply(*c)).collect();
    if !pts.iter().all(|p| p.is_finite()) {
        return None;
    }
    let first = *pts.first()?;
    pts.push(first);
    Some(pts)
}
