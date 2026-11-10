//! A layout (paper space) built in pieces, for an export written piece by piece.
//!
//! [`build_layout_slices`] hands over, one after the other, display lists that drawn in turn are
//! the list [`build`](super::build) makes of the whole layout: the sheet's own objects and each
//! viewport's border and model-space contents, in the stored draw order, the contents clipped to
//! the viewport as the whole build clips them. Each piece is built, handed over and dropped before
//! the next, so only one piece is in memory.
//!
//! What has to be carried from one piece to the next so the pieces together are the whole list:
//! the block-expansion count (the limit is the layout's, spent in draw order), and the pattern
//! cells left for plot-style fills ([`pen::apply_budgeted`]; the sheet's objects share one, each
//! viewport's contents their own, as in the whole build).

use super::display::{DisplayList, VIEWPORT_CONTENT};
use super::draw::entity;
use super::pipeline::{Options, style_table, top_ctx, with_point_view};
#[cfg(not(target_arch = "wasm32"))]
use super::pipeline::PARALLEL_CHUNK;
use super::viewports::{Clip, MAX_VIEWPORTS, View, append_clipped, model_seen, viewport_clip, viewport_rect, viewport_shown};
use super::{Builder, Ctx, Drawing, Entity, EntityKind, Mat3, Space, paper, pen, plotstyle};

/// Build the layout `name` in pieces of at most `slice` objects (the sheet's own objects, or the
/// model-space objects seen through one viewport) and hand each to `each`, which may stop the
/// build with an error. Returns the block contents expanded in all (as [`DisplayList::expanded`]
/// of the whole build). For screen exports (plots, with their layers and text, are built whole).
pub fn build_layout_slices<E>(
    d: &Drawing,
    name: &str,
    opts: &Options,
    slice: usize,
    each: &mut dyn FnMut(&DisplayList) -> Result<(), E>,
) -> Result<usize, E> {
    let space = Space::Paper(name.to_string());
    let opts = &with_point_view(d, &space, opts);
    let styler = style_table(d, &space, opts, false).map(|t| plotstyle::Styler::new(d, t));
    let styles = styler.as_ref();
    let slice = slice.max(1);
    let Some(store) = d.space(&space) else { return Ok(0) };
    // A layout is drawn in paper units, so its pens become geometry as each piece is made.
    let sheet_mm = paper::sheet(d, name).map(|s| s.unit_mm);
    let mut sheet_cells = pen::MAX_PATTERN_CELLS;
    let mut expanded = 0usize;

    // The sheet's own objects waiting to be built (viewport borders are among them).
    let mut pending: Vec<&Entity> = Vec::new();
    let mut flush = |pending: &mut Vec<&Entity>, expanded: &mut usize, each: &mut dyn FnMut(&DisplayList) -> Result<(), E>| -> Result<(), E> {
        if pending.is_empty() {
            return Ok(());
        }
        let list = DisplayList { expanded: *expanded, ..DisplayList::default() };
        let mut b = Builder { list, opts, plotting: false, styles, named: None, shared_budget: None, transparency: 0 };
        for e in pending.drain(..) {
            entity(&mut b, &top_ctx(d, Mat3::IDENTITY, e.handle, &[], &[]), e);
        }
        if let Some(mm) = sheet_mm {
            pen::apply_budgeted(&mut b.list, mm, &mut sheet_cells);
        }
        *expanded = b.list.expanded;
        each(&b.list)
    };

    let mut viewports = 0usize;
    for e in store.iter() {
        let EntityKind::Viewport(vp) = &e.kind else {
            pending.push(e);
            if pending.len() >= slice {
                flush(&mut pending, &mut expanded, &mut *each)?;
            }
            continue;
        };
        if viewports >= MAX_VIEWPORTS {
            continue;
        }
        viewports += 1;
        if !viewport_shown(d, e, vp) {
            continue;
        }
        let poly = viewport_clip(d, e.handle, vp);
        // The border (a clipped viewport's boundary draws itself).
        if poly.is_none() {
            pending.push(e);
        }
        if opts.skip_viewport == Some(e.handle) {
            continue;
        }
        flush(&mut pending, &mut expanded, &mut *each)?;
        let unit_mm = paper::unit_mm_of(d, e.handle);
        let one = Piece { d, opts, styles, vp, unit_mm, slice };
        match &poly {
            Some(p) => one.contents(p.bounds(), &Clip::Poly(p), &mut expanded, each)?,
            None => {
                if let Some(rect) = viewport_rect(vp) {
                    one.contents(rect, &Clip::Rect(rect), &mut expanded, each)?;
                }
            }
        }
    }
    flush(&mut pending, &mut expanded, &mut *each)?;
    Ok(expanded)
}

/// One viewport's contents to build.
struct Piece<'a> {
    d: &'a Drawing,
    opts: &'a Options,
    styles: Option<&'a plotstyle::Styler>,
    vp: &'a cadcraft_doc::Viewport,
    unit_mm: f64,
    slice: usize,
}

impl Piece<'_> {
    /// Model space seen through the viewport, `slice` objects at a time, clipped to `clip` (see
    /// `viewports::contents`, which builds the same as one list).
    fn contents<E>(&self, area: super::Bounds2, clip: &Clip, expanded: &mut usize, each: &mut dyn FnMut(&DisplayList) -> Result<(), E>) -> Result<(), E> {
        let (d, vp) = (self.d, self.vp);
        if *expanded >= self.opts.max_block_expansion || !vp.on {
            return Ok(());
        }
        let Some(View { s, xf, win }) = View::of(vp, area) else { return Ok(()) };
        let lt_factor = if d.header.i64("PSLTSCALE", 1) != 0 { 1.0 / s } else { 1.0 };
        let ctx = || Ctx { lt_factor, ..top_ctx(d, xf, VIEWPORT_CONTENT, &vp.frozen_layers, &vp.layer_colors) };
        let seen: Vec<&Entity> = d.model.iter().map(|e| e.as_ref()).filter(|me| model_seen(d, &win, me)).collect();
        let mut cells = pen::MAX_PATTERN_CELLS;
        for chunk in seen.chunks(self.slice) {
            let budget = std::sync::atomic::AtomicUsize::new(*expanded);
            let mut sub = Builder {
                list: DisplayList::default(),
                opts: self.opts,
                plotting: false,
                styles: self.styles,
                named: None,
                shared_budget: Some(&budget),
                transparency: 0,
            };
            let mut built = false;
            #[cfg(not(target_arch = "wasm32"))]
            if self.styles.is_none() && chunk.len() >= 2 * PARALLEL_CHUNK {
                use rayon::prelude::*;
                let opts = self.opts;
                let parts: Vec<DisplayList> = chunk
                    .par_chunks(PARALLEL_CHUNK)
                    .map(|part| {
                        let mut b =
                            Builder { list: DisplayList::default(), opts, plotting: false, styles: None, named: None, shared_budget: Some(&budget), transparency: 0 };
                        let c = ctx();
                        for me in part {
                            entity(&mut b, &c, me);
                        }
                        b.list
                    })
                    .collect();
                // Past the limit the parts raced for the budget: build in draw order instead (see
                // `viewports::contents`).
                if budget.load(std::sync::atomic::Ordering::Relaxed) <= self.opts.max_block_expansion {
                    for part in parts {
                        super::display::append(&mut sub.list, part);
                    }
                    built = true;
                } else {
                    budget.store(*expanded, std::sync::atomic::Ordering::Relaxed);
                }
            }
            if !built {
                let c = ctx();
                for me in chunk {
                    entity(&mut sub, &c, me);
                }
            }
            pen::apply_budgeted(&mut sub.list, self.unit_mm, &mut cells);
            *expanded = expanded.saturating_add(sub.list.expanded);
            let mut out = DisplayList::default();
            append_clipped(&mut out, &sub.list, clip);
            each(&out)?;
        }
        Ok(())
    }
}
