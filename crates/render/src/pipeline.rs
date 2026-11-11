use super::count::{COUNT_SATURATED, ClampedArray, LargestRef, Pass, Stats};
#[cfg(not(target_arch = "wasm32"))]
use super::display::append;
use super::display::{DisplayList, Kind, NO_LAYER, PlotText, VIEWPORT_CONTENT};
use super::draw::entity;
use super::viewports::{MAX_VIEWPORTS, viewport};
use super::{Bounds2, Builder, Color, Ctx, Drawing, Entity, EntityKind, Handle, Lineweight, Mat3, Space, Vec2, paper, pen, plotstyle, point};

/// Build options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Chord tolerance in world units (about half a pixel at the current zoom).
    pub tolerance: f64,
    /// Two pixels in world units: shorter dashes are continuous and filled font text uses
    /// screen-size detail. Zero keeps full detail (plots and image/vector exports).
    pub min_dash: f64,
    /// Draw text (off for very coarse previews).
    pub text: bool,
    /// Highlighted (selected) handles are not special here; the canvas overlays them.
    pub fill: bool,
    pub lineweights: bool,
    /// Height of the visible area in world units of the space being built. Relative point
    /// sizes (`PDSIZE` <= 0) are a percentage of it; 0 = the height of the space's extents.
    pub view_height: f64,
    /// Plot style table to draw with. When `None`, [`build`] of a layout whose page setup has
    /// "Display plot styles" on uses the layout's table; [`build_plot`] uses none.
    pub plot_style_table: Option<std::sync::Arc<cadcraft_doc::PlotStyleTable>>,
    /// The area being looked at (world units of the space being built), for on-screen builds:
    /// hatch patterns are only generated inside it. `None` = everywhere (plots, exports, picks).
    pub view: Option<Bounds2>,
    /// Draw objects with their transparency ([`DPrim::transparency`]). Off by default: the screen
    /// follows TRANSPARENCYDISPLAY, plots the page setup's "Plot transparency".
    pub transparency: bool,
    /// A layout viewport whose model-space contents are left out (its border is still drawn):
    /// the canvas draws the active viewport's contents itself, see [`viewport_content`].
    pub skip_viewport: Option<Handle>,
    /// Set from another thread to stop the build early (the canvas cancels a background
    /// rebuild the view no longer needs). A cancelled build returns a partial list, which the
    /// caller must discard; see [`Options::cancelled`].
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Block contents expanded before the rest are not drawn: [`MAX_BLOCK_EXPANSION`] (only
    /// tests give another value). A build that went past it has [`DisplayList::expanded`] above
    /// this ([`Options::blocks_truncated`]) and is incomplete: a caller that must not show an
    /// incomplete picture counts first ([`census_space`], [`census_entities`]) and discards it.
    pub max_block_expansion: usize,
    /// Blocks drawn as one outline box per reference instead of their contents (see
    /// [`BoxBlocks`]). The block extents in it were taken from the drawing the build is given.
    pub blocks_as_boxes: Option<std::sync::Arc<super::BoxBlocks>>,
}

impl Options {
    /// Whether [`Options::cancel`] has been set.
    pub fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Whether a list built with these options left block contents out because of the cap
    /// (`expanded` counts every expansion asked for, drawn or not).
    pub fn blocks_truncated(&self, expanded: usize) -> bool {
        expanded > self.max_block_expansion
    }
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tolerance: 0.001,
            min_dash: 0.0,
            text: true,
            fill: true,
            lineweights: false,
            view_height: 0.0,
            plot_style_table: None,
            view: None,
            skip_viewport: None,
            transparency: false,
            cancel: None,
            max_block_expansion: super::MAX_BLOCK_EXPANSION,
            blocks_as_boxes: None,
        }
    }
}

/// Build the display list for a space. For a layout (paper space) this includes the model-space
/// geometry seen through each viewport, clipped to the viewport, and [`DisplayList::sheet`].
pub fn build(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    build_space(d, space, opts, false)
}

/// Like [`build`] but for plotting: layers marked "do not plot" (such as `Defpoints`) are left out.
pub fn build_plot(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    build_space(d, space, opts, true)
}

pub(crate) fn top_ctx<'a>(d: &'a Drawing, xf: Mat3, top: Handle, frozen: &'a [String], vp_colors: &'a [(String, Color)]) -> Ctx<'a> {
    Ctx {
        d,
        xf,
        block_color: Color::Index(7),
        block_layer: None,
        block_lw: Lineweight::Default,
        block_ltype: "Continuous".into(),
        depth: 0,
        top,
        frozen,
        vp_colors,
        lt_factor: 1.0,
        block_pstyle: None,
        block_transparency: 0,
    }
}

fn build_space(d: &Drawing, space: &Space, opts: &Options, plotting: bool) -> DisplayList {
    let opts = &with_point_view(d, space, opts);
    let styler = style_table(d, space, opts, plotting).map(|t| plotstyle::Styler::new(d, t));
    let mut b = Builder { list: DisplayList::default(), opts, plotting, styles: styler.as_ref(), named: None, shared_budget: None, transparency: 0 };
    if let Space::Paper(name) = space {
        b.list.sheet = paper::sheet(d, name);
    }
    if let Some(store) = d.space(space) {
        let paper = matches!(space, Space::Paper(_));
        #[cfg(not(target_arch = "wasm32"))]
        if !paper && !plotting && b.styles.is_none() && store.len() >= 2 * PARALLEL_CHUNK {
            let ents: Vec<&Entity> = store.iter().map(|e| e.as_ref()).collect();
            build_parallel(&mut b, d, &ents);
            return b.list;
        }
        let mut viewports = 0usize;
        for e in store.iter() {
            if paper && let EntityKind::Viewport(vp) = &e.kind {
                if viewports < MAX_VIEWPORTS {
                    viewports += 1;
                    viewport(&mut b, d, e, vp);
                    if plotting {
                        take_viewport_layers(&mut b.list);
                    }
                }
                continue;
            }
            entity(&mut b, &top_ctx(d, Mat3::IDENTITY, e.handle, &[], &[]), e);
        }
    }
    // A layout is drawn in paper units, so its pens become geometry now.
    if let Some(sheet) = b.list.sheet {
        pen::apply(&mut b.list, sheet.unit_mm);
    }
    b.list
}

/// Top-level entities per task when a display list is built on all cores.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const PARALLEL_CHUNK: usize = 256;

/// Build model space for the screen on all cores: the entities are split into chunks built
/// side by side, then joined in drawing order, so the list is the same as a one-thread build.
/// The canvas rebuilds the whole list at every 2x zoom step (in the background since zooming
/// stopped blocking the UI) and after every edit; one core made both slow on real drawings. Plots, layouts and plot styles keep the one-thread
/// path (they run once, and their pens and per-primitive layers are built along the way).
#[cfg(not(target_arch = "wasm32"))]
fn build_parallel(b: &mut Builder, d: &Drawing, ents: &[&Entity]) {
    use rayon::prelude::*;
    // The block-expansion count starts where the builder's is (0, or the earlier slices' in
    // [`build_slice`]).
    let budget = std::sync::atomic::AtomicUsize::new(b.list.expanded);
    let (opts, plotting) = (b.opts, b.plotting);
    let parts: Vec<DisplayList> = ents
        .par_chunks(PARALLEL_CHUNK)
        .map(|chunk| {
            let mut part =
                Builder { list: DisplayList::default(), opts, plotting, styles: None, named: None, shared_budget: Some(&budget), transparency: 0 };
            for e in chunk {
                entity(&mut part, &top_ctx(d, Mat3::IDENTITY, e.handle, &[], &[]), e);
            }
            part.list
        })
        .collect();
    // Chunks racing for the budget drop block contents by thread timing once it runs out. Then
    // the parallel result is thrown away and the entities are built in draw order on this thread,
    // where the first contents in draw order take the budget: the same list every run, and the
    // one a one-thread build makes. Below the cap nothing is dropped, so the chunks' list is
    // already that list.
    if budget.load(std::sync::atomic::Ordering::Relaxed) > opts.max_block_expansion {
        for e in ents {
            entity(b, &top_ctx(d, Mat3::IDENTITY, e.handle, &[], &[]), e);
        }
        return;
    }
    // One allocation per array for the joined list.
    b.list.prims.reserve(parts.iter().map(|p| p.prims.len()).sum());
    b.list.verts.reserve(parts.iter().map(|p| p.verts.len()).sum());
    b.list.tris.reserve(parts.iter().map(|p| p.tris.len()).sum());
    for part in parts {
        append(&mut b.list, part);
    }
}

/// The plot style table a build draws with: the one in `opts`, else (on screen) the layout's
/// when its page setup has "Display plot styles" on.
pub(crate) fn style_table(d: &Drawing, space: &Space, opts: &Options, plotting: bool) -> Option<std::sync::Arc<cadcraft_doc::PlotStyleTable>> {
    opts.plot_style_table.clone().or_else(|| match space {
        Space::Paper(n) if !plotting => d
            .layout(n)
            .filter(|l| l.page.show_plot_styles)
            .and_then(|l| cadcraft_doc::plot_style_table(d, &l.page.plot_style_table))
            .map(std::sync::Arc::new),
        _ => None,
    })
}

/// `opts` with a view height for relative point sizes: the extents height when none is given
/// and points draw as figures sized relative to the view.
pub(crate) fn with_point_view(d: &Drawing, space: &Space, opts: &Options) -> Options {
    let mut o = opts.clone();
    let relative = d.header.f64("PDSIZE", 0.0) <= 0.0 && point::sized(d.header.i64("PDMODE", 0));
    if !(o.view_height.is_finite() && o.view_height > 0.0) && relative {
        let ext = d.extents(space);
        o.view_height = if ext.is_empty() { 0.0 } else { ext.height().max(ext.width() * 1e-3) };
    }
    o
}

/// Build the display list for a set of loose entities (previews, rubber bands).
pub fn build_entities<'a, I: IntoIterator<Item = &'a Entity>>(d: &Drawing, ents: I, opts: &Options) -> DisplayList {
    let ents: Vec<&Entity> = ents.into_iter().collect();
    // Only previews holding points need the model extents for relative point sizes.
    let with_view;
    let opts = if ents.iter().any(|e| matches!(e.kind, EntityKind::Point(_))) {
        with_view = with_point_view(d, &Space::Model, opts);
        &with_view
    } else {
        opts
    };
    let mut b = Builder { list: DisplayList::default(), opts, plotting: false, styles: None, named: None, shared_budget: None, transparency: 0 };
    for e in ents {
        entity(&mut b, &top_ctx(d, Mat3::IDENTITY, e.handle, &[], &[]), e);
    }
    b.list
}

/// Build one slice of model-space entities for an export written slice by slice. The entities
/// are in stored draw order and the slices follow each other, so the lists drawn one after the
/// other are the list [`build`] makes of all of them at once (same primitives, same order); a big
/// slice is built on all cores like [`build`] does. `expanded` is the block-expansion count of the
/// earlier slices ([`DisplayList::expanded`] of the previous one), so [`MAX_BLOCK_EXPANSION`] caps
/// the whole drawing, not each slice.
pub fn build_slice(d: &Drawing, ents: &[&Entity], opts: &Options, expanded: usize) -> DisplayList {
    let opts = &with_point_view(d, &Space::Model, opts);
    let styler = style_table(d, &Space::Model, opts, false).map(|t| plotstyle::Styler::new(d, t));
    let list = DisplayList { expanded, ..DisplayList::default() };
    let mut b = Builder { list, opts, plotting: false, styles: styler.as_ref(), named: None, shared_budget: None, transparency: 0 };
    #[cfg(not(target_arch = "wasm32"))]
    if ents.len() >= 2 * PARALLEL_CHUNK && b.styles.is_none() {
        build_parallel(&mut b, d, ents);
        return b.list;
    }
    for e in ents {
        entity(&mut b, &top_ctx(d, Mat3::IDENTITY, e.handle, &[], &[]), e);
    }
    b.list
}

/// A layer whose switching off makes the count smaller (see [`BlockCensus::layers_off`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerOff {
    pub layer: String,
    /// The expansions asked for with that layer off ([`COUNT_SATURATED`] when still more than can
    /// be counted).
    pub total_if_off: u64,
}

/// A block that, drawn as outline boxes, makes the count smaller (see [`BlockCensus::heaviest`]).
#[derive(Clone, Debug, PartialEq)]
pub struct BlockSaving {
    /// The block's name.
    pub block: String,
    /// The expansions asked for with every reference to this block (and nothing else changed)
    /// drawn as one box ([`COUNT_SATURATED`](super::COUNT_SATURATED) when still more than can be
    /// counted).
    pub total_if_boxed: u64,
}

/// The references to a block that a picture draws as boxes.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxedRefs {
    /// The block's name as the drawing spells it.
    pub block: String,
    /// The outlines drawn for it: every reference to it that is drawn, nested ones once per copy
    /// of the block holding them, an array once ([`COUNT_SATURATED`](super::COUNT_SATURATED) when
    /// more than can be counted).
    pub references: u64,
}

/// What counting the block expansions of a build finds, from the block table without building:
/// the number, and what an answer needs when the build is too big or draws an array short.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockCensus {
    /// The expansions the build asks for ([`COUNT_SATURATED`], `u64::MAX`, when more than can be
    /// counted: nested references multiply past 1.8e19).
    pub total: u64,
    /// The largest single top-level object: the build expands one whole, so windows cannot split it.
    pub largest: Option<LargestRef>,
    /// The layers the drawn references (block references, multileader blocks, dimension blocks)
    /// sit on. The build takes one expansion for every entity of a block before it looks at the
    /// entity's layer, so only the layer the *references* are on can reduce a count, not the
    /// layers of their contents.
    pub reference_layers: Vec<String>,
    /// A drawn reference sits on a layer missing from the layer table: no layer switch reaches it,
    /// so the reference layers do not say where every reference is.
    pub references_off_table: bool,
    /// Only when `total` is past [`Options::max_block_expansion`]: each reference layer that
    /// makes the count smaller when switched off, the smallest count first.
    pub layers_off: Vec<LayerOff>,
    /// Arrays with more columns or rows than the 10,000 the build draws.
    pub clamped: Vec<ClampedArray>,
    /// Only when `total` is past [`Options::max_block_expansion`]: the blocks that make the count
    /// smallest when drawn as boxes, each with what is left if that block alone is a box, the
    /// biggest saving first, down to the one at which boxing all of them fits (the list ends
    /// there; when boxing every block still does not fit, it holds every block that saves any).
    pub heaviest: Vec<BlockSaving>,
    /// With `heaviest` not empty: the count with every block in it drawn as a box.
    pub total_if_listed_boxed: u64,
    /// The blocks the picture is asked to draw as boxes, with the references of each that are drawn.
    pub boxed: Vec<BoxedRefs>,
}

/// Count with `run` (the walk of the build's top-level objects under a [`Pass`]: the count and
/// what it noted), then, when that is past the limit, once more per reference layer (switched off)
/// and per block (drawn as a box), and the references drawn as boxes when blocks are asked for.
fn census_of(opts: &Options, run: impl Fn(Pass) -> (u64, Stats)) -> BlockCensus {
    let (total, stats) = run(Pass::default());
    let mut c = BlockCensus {
        total,
        largest: stats.largest,
        reference_layers: stats.layers.into_values().collect(),
        references_off_table: stats.off_table,
        layers_off: Vec::new(),
        clamped: stats.clamped.into_values().collect(),
        heaviest: Vec::new(),
        total_if_listed_boxed: 0,
        boxed: Vec::new(),
    };
    let cap = opts.max_block_expansion as u64;
    if total > cap {
        for l in &c.reference_layers {
            if opts.cancelled() {
                break;
            }
            let after = run(Pass { off: std::slice::from_ref(l), ..Pass::default() }).0;
            if after < total {
                c.layers_off.push(LayerOff { layer: l.clone(), total_if_off: after });
            }
        }
        c.layers_off.sort_by(|a, b| a.total_if_off.cmp(&b.total_if_off).then_with(|| a.layer.cmp(&b.layer)));
        // Each block alone drawn as a box, the biggest saving first. What a block saves comes from
        // how often its copies are expanded (one pass that records them, for all blocks); a block that
        // is part of itself, or sums that overflowed, are counted by a walk of their own.
        let noted = run(Pass { record: true, ..Pass::default() }).1;
        let (blocks, savings, cyclic, savings_unknown) = (noted.blocks, noted.savings, noted.cyclic, noted.savings_unknown);
        let mut each: Vec<BlockSaving> = Vec::new();
        for name in blocks.into_values() {
            if opts.cancelled() {
                break;
            }
            let after = if !savings_unknown && total != COUNT_SATURATED && !cyclic.contains(&name) {
                total.saturating_sub(savings.get(&name).copied().unwrap_or(0))
            } else {
                run(Pass { extra_boxes: std::slice::from_ref(&name), ..Pass::default() }).0
            };
            if after < total {
                each.push(BlockSaving { block: name, total_if_boxed: after });
            }
        }
        each.sort_by(|a, b| a.total_if_boxed.cmp(&b.total_if_boxed).then_with(|| a.block.cmp(&b.block)));
        // Boxing more blocks never costs more (a box is one unit, a copy at least one), so the
        // list ends at the first prefix whose blocks together fit, found by halving; when even all
        // of them do not fit, every one is listed.
        let names: Vec<String> = each.iter().map(|b| b.block.clone()).collect();
        let together = |k: usize| run(Pass { extra_boxes: names.get(..k).unwrap_or(&names), ..Pass::default() }).0;
        let (mut lo, mut hi) = (0usize, names.len());
        let mut listed = names.len();
        let mut total_listed = if names.is_empty() { total } else { together(names.len()) };
        if total_listed <= cap {
            while lo < hi && !opts.cancelled() {
                let mid = lo + (hi - lo) / 2;
                let t = together(mid + 1);
                if t <= cap {
                    hi = mid;
                    listed = mid + 1;
                    total_listed = t;
                } else {
                    lo = mid + 1;
                }
            }
        }
        each.truncate(listed);
        c.heaviest = each;
        c.total_if_listed_boxed = total_listed;
    }
    if let Some(boxes) = opts.blocks_as_boxes.as_deref() {
        for name in boxes.names() {
            if opts.cancelled() {
                break;
            }
            let references = run(Pass { tally: Some(name), ..Pass::default() }).0;
            c.boxed.push(BoxedRefs { block: name.to_string(), references });
        }
    }
    c
}

/// The block expansions building the loose entities `ents` asks for ([`DisplayList::expanded`] of
/// a build with no limit), counted from the block table without building: what [`build_entities`]
/// and [`build_slice`] (`plotting` false) or the plot's top-level objects (`plotting` true) will
/// take from [`Options::max_block_expansion`], with the largest object, the layers the references
/// are on and the arrays drawn short. Stops early, with a count to discard, when `opts.cancel` is
/// set.
pub fn census_entities(d: &Drawing, ents: &[&Entity], plotting: bool, opts: &Options) -> BlockCensus {
    census_of(opts, |pass| {
        let mut counter = super::count::Counter::new(d, plotting, &[], pass, opts.blocks_as_boxes.as_deref(), opts.cancel.as_deref());
        let n = ents.iter().map(|e| counter.top(e, d.anno_draw(e))).fold(0, u64::saturating_add);
        (n, counter.into_stats())
    })
}

/// [`count_expansions`] with the notes: for [`build`] (`plotting` false) or [`build_plot`]
/// (`plotting` true) of `space`, the entities' own and, for a layout, each viewport's model-space
/// contents. Counted from the block table without building.
pub fn census_space(d: &Drawing, space: &Space, plotting: bool, opts: &Options) -> BlockCensus {
    census_of(opts, |pass| {
        let Some(store) = d.space(space) else { return (0, Stats::default()) };
        let cancel = opts.cancel.as_deref();
        let boxes = opts.blocks_as_boxes.as_deref();
        let mut counter = super::count::Counter::new(d, plotting, &[], pass, boxes, cancel);
        let mut stats = Stats::default();
        let mut n = 0u64;
        if !matches!(space, Space::Paper(_)) {
            n = store.iter().map(|e| counter.top(e, d.anno_draw(e))).fold(0, u64::saturating_add);
        } else {
            let mut viewports = 0usize;
            for e in store.iter() {
                let more = if let EntityKind::Viewport(vp) = &e.kind {
                    if viewports >= MAX_VIEWPORTS {
                        continue;
                    }
                    viewports += 1;
                    let (more, s) = super::viewports::count_viewport(d, e, vp, plotting, pass, boxes, cancel);
                    stats.absorb(s);
                    more
                } else {
                    counter.top(e, d.anno_draw(e))
                };
                n = n.saturating_add(more);
            }
        }
        stats.absorb(counter.into_stats());
        (n, stats)
    })
}

/// The block expansions of [`census_entities`] (the number only).
pub fn count_expansions<'a, I: IntoIterator<Item = &'a Entity>>(d: &Drawing, ents: I, plotting: bool) -> u64 {
    let ents: Vec<&Entity> = ents.into_iter().collect();
    census_entities(d, &ents, plotting, &Options::default()).total
}

/// The block expansions of [`census_space`] (the number only). Saturates at `u64::MAX`.
pub fn count_space(d: &Drawing, space: &Space, plotting: bool) -> u64 {
    census_space(d, space, plotting, &Options::default()).total
}

/// After a viewport is appended to a plot: the layers its contents carry in their handles
/// (see [`entity`]) go to [`DisplayList::prim_layers`], and the handles back to
/// [`VIEWPORT_CONTENT`]. The viewport's border was tagged when it was drawn.
///
/// Text glyphs seen through the viewport (marked in the handles' high half) keep their outline
/// primitive when it came through the clip whole; a glyph cut by the viewport edge stays
/// geometry.
fn take_viewport_layers(list: &mut DisplayList) {
    let start = list.prim_layers.len();
    for i in start..list.prims.len() {
        let Some(p) = list.prims.get(i).copied() else { break };
        list.prim_layers.push(u32::try_from(p.handle.0 & 0xFFFF_FFFF).unwrap_or(NO_LAYER));
        let bounds = Bounds2::from_points(list.points(&p).iter().copied());
        if let Some(t) = (p.handle.0 >> 32).checked_sub(1).and_then(|k| list.texts.get_mut(k as usize)) {
            let same = |a: Vec2, b: Vec2| a.dist(b) <= 1e-9 * (1.0 + a.len());
            let whole = t.prim == PlotText::NONE
                && p.kind == Kind::Tris
                && p.len == t.verts
                && same(bounds.min, t.bounds.min)
                && same(bounds.max, t.bounds.max);
            // A second piece (or a cut one) means the clip changed the glyph.
            t.prim = if whole { u32::try_from(i).unwrap_or(PlotText::NONE) } else { PlotText::CLIPPED };
        }
        if let Some(p) = list.prims.get_mut(i) {
            p.handle = VIEWPORT_CONTENT;
        }
    }
}
