//! WHIQ: `render_preview`, the platform's Preview tab renderer: vector SVG of a space (model or a
//! layout, a layout viewport's model view), the whole extents or a window, in tiles for big
//! drawings, with layers switched on or off; or a PNG to look at. Every primitive carries its
//! layer (`<g data-layer>` runs, draw order kept), so a viewer can also hide layers itself.
//! A drawing too big for one answer comes back tile by tile (`continue_from`), each tile holding
//! only what it shows. One picture draws at most [`MAX_BLOCK_EXPANSION`](cadcraft_engine::render::MAX_BLOCK_EXPANSION)
//! block pieces: a picture that would need more is not made (no picture, no file) and the answer
//! is a refusal with the ways to a complete one ([`refusal`]).

use std::fmt::Write as _;

use cadcraft_engine::doc::{Drawing, Entity, EntityKind, Handle, Space};
use cadcraft_engine::geom::{Bounds2, Vec2};
use cadcraft_engine::render::{BlockCensus, BoxBlocks, COUNT_SATURATED, DisplayList, Kind};
use serde_json::{Value, json};

use super::contract::{ToolError, code};
use super::docs::Ctx;
use super::files::WriteErr;
use super::read::window_arg;

/// The layer switches of a call: `layers_off`, `layers_on`, `only_layers` (names, any case).
pub struct LayerSwitch {
    pub off: Vec<String>,
    pub on: Vec<String>,
    pub only: Option<Vec<String>>,
}

fn names(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

impl LayerSwitch {
    pub fn from_args(a: &Value) -> LayerSwitch {
        let only = names(a.get("only_layers"));
        LayerSwitch { off: names(a.get("layers_off")), on: names(a.get("layers_on")), only: (!only.is_empty()).then_some(only) }
    }
    pub fn is_empty(&self) -> bool {
        self.off.is_empty() && self.on.is_empty() && self.only.is_none()
    }
    /// The drawing with the switches applied (a copy; the open drawing is not changed). Names
    /// that are no layer are returned.
    pub fn apply(&self, d: &Drawing) -> (Drawing, Vec<String>) {
        let mut c = d.clone();
        let mut unknown = Vec::new();
        for n in self.off.iter().chain(self.on.iter()).chain(self.only.iter().flatten()) {
            if !c.layers.iter().any(|l| l.name.eq_ignore_ascii_case(n)) {
                unknown.push(n.clone());
            }
        }
        for l in &mut c.layers {
            if let Some(only) = &self.only {
                let keep = only.iter().any(|n| n.eq_ignore_ascii_case(&l.name));
                l.on = keep;
                if keep {
                    l.frozen = false;
                }
            }
            if self.on.iter().any(|n| n.eq_ignore_ascii_case(&l.name)) {
                l.on = true;
                l.frozen = false;
            }
            if self.off.iter().any(|n| n.eq_ignore_ascii_case(&l.name)) {
                l.on = false;
            }
        }
        (c, unknown)
    }
}

/// Which space a call shows: `space` (default model; a layout name) or `viewport` (a layout
/// viewport's handle: its model view, its frozen layers off).
pub struct Target {
    pub space: Space,
    pub name: String,
    /// The model area a viewport shows.
    pub window: Option<Bounds2>,
    pub frozen: Vec<String>,
}

pub fn target(d: &Drawing, a: &Value) -> Result<Target, ToolError> {
    if let Some(vp) = a.get("viewport").filter(|v| !v.is_null()) {
        let h = match vp {
            Value::String(s) => Handle::parse_hex(s.trim()),
            Value::Number(n) => n.as_u64().map(Handle),
            _ => None,
        }
        .ok_or_else(|| {
            ToolError::new(
                code::INVALID_ARGUMENT,
                "`viewport` must be a viewport handle (hex)",
                "read_drawing section \"layouts\" lists each layout's viewports",
            )
        })?;
        for l in &d.layouts {
            if let Some(e) = l.entities.get(h)
                && let EntityKind::Viewport(v) = &e.kind
            {
                let half_h = v.view_height / 2.0;
                let aspect = if v.height.abs() > 1e-12 { v.width / v.height } else { 1.0 };
                let half_w = half_h * aspect;
                let w = Bounds2::new(
                    Vec2::new(v.view_center.x - half_w, v.view_center.y - half_h),
                    Vec2::new(v.view_center.x + half_w, v.view_center.y + half_h),
                );
                return Ok(Target {
                    space: Space::Model,
                    name: format!("{} viewport {}", l.name, e.handle.hex()),
                    window: Some(w),
                    frozen: v.frozen_layers.clone(),
                });
            }
        }
        return Err(ToolError::new(
            code::NOT_FOUND,
            format!("no layout viewport has the handle {}", h.hex()),
            "read_drawing section \"layouts\" lists each layout's viewports",
        ));
    }
    let asked = a.get("space").and_then(Value::as_str).unwrap_or("model").trim().to_string();
    if asked.is_empty() || ["model", "modelspace", "model space", "ms"].contains(&asked.to_ascii_lowercase().as_str()) {
        return Ok(Target { space: Space::Model, name: "Model".into(), window: None, frozen: Vec::new() });
    }
    match d.layouts.iter().find(|l| l.name.eq_ignore_ascii_case(&asked)) {
        Some(l) => Ok(Target { space: Space::Paper(l.name.clone()), name: l.name.clone(), window: None, frozen: Vec::new() }),
        None => {
            let names: Vec<&str> = std::iter::once("Model").chain(d.layouts.iter().map(|l| l.name.as_str())).collect();
            Err(ToolError::new(code::NOT_FOUND, format!("there is no layout `{asked}`"), "use \"model\" or a layout name from `layouts`")
                .with("layouts", json!(names))
                .with("closest", json!(super::contract::closest(&asked, names.iter().copied(), 3))))
        }
    }
}

/// The layer each top-level object of `space` is on (prims carry their top-level handle).
fn layer_of(d: &Drawing, space: &Space) -> std::collections::HashMap<Handle, String> {
    let mut m = std::collections::HashMap::new();
    for s in [Space::Model, space.clone()] {
        if let Some(store) = d.space(&s) {
            for e in store.iter() {
                m.entry(e.handle).or_insert_with(|| e.common.layer.clone());
            }
        }
    }
    m
}

fn prim_bounds(list: &DisplayList, p: &cadcraft_engine::render::DPrim) -> Bounds2 {
    Bounds2::from_points(list.points(p).iter().copied())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Decimal places that keep about 1/20000 of the window's size.
fn decimals(size: f64) -> usize {
    if !(size.is_finite() && size > 0.0) {
        return 6;
    }
    let d = (-(size / 20_000.0).log10()).ceil();
    d.clamp(0.0, 9.0) as usize
}

/// Model-space entities per display-list slice when an SVG is written to a file (a batch: it
/// bounds the memory of one slice and does not change the output).
pub const SLICE_ENTITIES: usize = 16_384;

/// An SVG written in pieces: it sends what it has made to `out` in blocks, so the SVG of a huge
/// drawing is never one string in memory.
pub struct SvgWriter<'a, W: std::io::Write> {
    out: W,
    /// Text made and not yet sent to `out`.
    buf: String,
    layers: &'a std::collections::HashMap<Handle, String>,
    window: Bounds2,
    vw: f64,
    vh: f64,
    dp: usize,
    x0: f64,
    y1: f64,
    bg: cadcraft_engine::doc::color::Rgb,
    /// The layer of the open `<g>` (layer runs continue from one display list to the next).
    open: Option<String>,
    prims: usize,
    bytes: usize,
}

/// Text made before it is sent to the sink.
const SVG_FLUSH: usize = 1 << 16;

impl<'a, W: std::io::Write> SvgWriter<'a, W> {
    /// Start the SVG of `window` (the header depends on the window and width only, never on the
    /// primitives, so it can be written before any of them exists).
    pub fn new(out: W, layers: &'a std::collections::HashMap<Handle, String>, window: &Bounds2, width_px: f64, dark: bool) -> Self {
        let (vw, vh) = (window.width().max(1e-9), window.height().max(1e-9));
        let dp = decimals(vw.max(vh));
        let (x0, y1) = (window.min.x, window.max.y);
        let bg = if dark { cadcraft_engine::doc::color::Rgb(33, 40, 48) } else { cadcraft_engine::doc::color::Rgb(255, 255, 255) };
        let mut buf = String::new();
        let _ = write!(
            buf,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {vw:.dp$} {vh:.dp$}" width="{:.0}" height="{:.0}" data-x0="{x0}" data-y1="{y1}"><rect width="100%" height="100%" fill="{}"/>"#,
            width_px,
            width_px * vh / vw,
            bg.hex()
        );
        SvgWriter { out, buf, layers, window: *window, vw, vh, dp, x0, y1, bg, open: None, prims: 0, bytes: 0 }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.write_all(self.buf.as_bytes())?;
        self.bytes += self.buf.len();
        self.buf.clear();
        Ok(())
    }

    /// Write the primitives of `list` (only those that reach into the window), after the ones
    /// written before.
    pub fn add(&mut self, list: &DisplayList, ctx: &Ctx) -> Result<(), WriteErr> {
        let (vw, vh, dp, bg, window) = (self.vw, self.vh, self.dp, self.bg, self.window);
        let (x0, y1) = (self.x0, self.y1);
        let tx = |x: f64| x - x0;
        let ty = |y: f64| y1 - y;
        for (k, p) in list.prims.iter().enumerate() {
            if k % 8192 == 0 {
                ctx.check()?;
            }
            let pts = list.points(p);
            if !matches!(p.kind, Kind::Infinite { .. }) && !prim_bounds(list, p).intersects(&window) {
                continue;
            }
            let layer = self.layers.get(&p.handle).map(String::as_str).unwrap_or("");
            let s = &mut self.buf;
            if self.open.as_deref() != Some(layer) {
                if self.open.is_some() {
                    s.push_str("</g>");
                }
                let _ = write!(s, r#"<g data-layer="{}">"#, esc(layer));
                self.open = Some(layer.to_string());
            }
            self.prims += 1;
            let c = p.display_rgb(bg).hex();
            let op = if p.kind == Kind::Mask || p.transparency == 0 {
                String::new()
            } else {
                format!(r#" opacity="{:.2}""#, f64::from(p.alpha()) / 255.0)
            };
            let sw = if p.lw > 0.35 { 2.0 } else { 1.0 };
            match p.kind {
                Kind::Polyline => {
                    let _ = write!(
                        s,
                        r#"<polyline fill="none" stroke="{c}"{op} stroke-width="{sw}" vector-effect="non-scaling-stroke" stroke-linecap="round" stroke-linejoin="round" points=""#
                    );
                    for q in pts {
                        let _ = write!(s, "{:.dp$},{:.dp$} ", tx(q.x), ty(q.y));
                    }
                    s.push_str(r#""/>"#);
                }
                Kind::Tris | Kind::Mask => {
                    let c = if p.kind == Kind::Mask { bg.hex() } else { c };
                    let _ = write!(s, r#"<path fill="{c}"{op} d=""#);
                    for t in pts.chunks(3) {
                        if let [a, b, cc] = t {
                            let _ = write!(
                                s,
                                "M{:.dp$} {:.dp$}L{:.dp$} {:.dp$}L{:.dp$} {:.dp$}Z",
                                tx(a.x),
                                ty(a.y),
                                tx(b.x),
                                ty(b.y),
                                tx(cc.x),
                                ty(cc.y)
                            );
                        }
                    }
                    s.push_str(r#""/>"#);
                }
                Kind::Point => {
                    if let Some(q) = pts.first() {
                        let _ = write!(s, r#"<circle cx="{:.dp$}" cy="{:.dp$}" r="{:.dp$}" fill="{c}"{op}/>"#, tx(q.x), ty(q.y), vw.max(vh) / 1500.0);
                    }
                }
                Kind::Infinite { ray } => {
                    if let (Some(bp), Some(dir)) = (pts.first(), pts.get(1)) {
                        let far = (vw + vh) * 2.0 + bp.dist(window.center());
                        let a = if ray { *bp } else { *bp - *dir * far };
                        let e = *bp + *dir * far;
                        let _ = write!(
                            s,
                            r#"<line x1="{:.dp$}" y1="{:.dp$}" x2="{:.dp$}" y2="{:.dp$}" stroke="{c}"{op} stroke-width="1" vector-effect="non-scaling-stroke"/>"#,
                            tx(a.x),
                            ty(a.y),
                            tx(e.x),
                            ty(e.y)
                        );
                    }
                }
            }
            if self.buf.len() >= SVG_FLUSH {
                self.flush()?;
            }
        }
        Ok(())
    }

    /// Close the SVG and send the rest: the sink, the primitives written and the bytes.
    pub fn finish(mut self) -> Result<(W, usize, usize), WriteErr> {
        if self.open.is_some() {
            self.buf.push_str("</g>");
        }
        self.buf.push_str("</svg>");
        self.flush()?;
        self.out.flush()?;
        Ok((self.out, self.prims, self.bytes))
    }
}

/// One SVG of `window` from the display list: only the primitives that reach into it.
pub fn svg_tile(
    list: &DisplayList,
    layers: &std::collections::HashMap<Handle, String>,
    window: &Bounds2,
    width_px: f64,
    dark: bool,
    ctx: &Ctx,
) -> Result<(String, usize), ToolError> {
    let fail = |e: WriteErr| match e {
        WriteErr::Tool(e) => e,
        WriteErr::Io(e) => ToolError::new(code::FAILED, e.to_string(), "retry"),
    };
    let mut w = SvgWriter::new(Vec::new(), layers, window, width_px, dark);
    w.add(list, ctx).map_err(fail)?;
    let (bytes, n, _) = w.finish().map_err(fail)?;
    let svg = String::from_utf8(bytes).map_err(|e| ToolError::new(code::FAILED, e.to_string(), "retry"))?;
    Ok((svg, n))
}

/// Everything a preview is built from: the drawing (with the layer switches), the area shown
/// and the layer of each top-level object. Display lists are built per window
/// ([`Prepared::list_for`]), so a tile of a huge drawing holds only what it shows.
pub struct Prepared<'a> {
    base: &'a Drawing,
    switched: Option<Drawing>,
    pub layers: std::collections::HashMap<Handle, String>,
    pub window: Bounds2,
    pub extents: Bounds2,
    pub target: Target,
    pub unknown_layers: Vec<String>,
    /// Each layer and whether it is shown (after the switches).
    pub layer_states: Vec<(String, bool)>,
    lineweights: bool,
    /// The most block pieces one picture draws: a picture that needs more is not made (a test
    /// lowers it).
    pub block_cap: usize,
    /// The answer budget (`max_result_bytes`): a refusal lists as much as fits in it.
    pub budget: usize,
    /// The blocks drawn as outline boxes (`blocks_as_boxes`, as the drawing spells them) and
    /// their extents.
    pub boxed_names: Vec<String>,
    boxes: Option<std::sync::Arc<BoxBlocks>>,
    /// What counting a layout's block expansions found: the whole layout is built for every tile,
    /// so it is counted once per call.
    layout_census: std::cell::OnceCell<BlockCensus>,
    /// What the last count before drawing said (for the message when the build disagrees).
    counted: std::cell::Cell<Option<u64>>,
}

impl Prepared<'_> {
    pub fn drawing(&self) -> &Drawing {
        self.switched.as_ref().unwrap_or(self.base)
    }

    /// The build options of a display list of `window` drawn `width_px` wide: curves as fine as an
    /// eighth of a pixel (room to zoom in on the SVG).
    fn options(&self, window: &Bounds2, width_px: f64, ctx: &Ctx) -> cadcraft_engine::render::Options {
        let size = window.width().max(window.height());
        cadcraft_engine::render::Options {
            transparency: true,
            lineweights: self.lineweights,
            tolerance: (size / (width_px.max(16.0) * 8.0)).max(1e-9),
            view_height: window.height(),
            cancel: Some(ctx.cancel.clone()),
            max_block_expansion: self.block_cap,
            blocks_as_boxes: self.boxes.clone(),
            ..cadcraft_engine::render::Options::default()
        }
    }

    /// What a build of `window` asks for of block contents, counted from the block table (the
    /// arithmetic of [`cadcraft_engine::render::census_entities`]), with the largest object, the
    /// layers the references are on and the arrays drawn short: in model space for the objects that
    /// reach into the window (`picked` when the caller already has them), for a layout the whole
    /// layout (once per call). Stops with an error when the call is cancelled.
    pub fn census(&self, window: &Bounds2, picked: Option<&[&Entity]>, ctx: &Ctx) -> Result<BlockCensus, ToolError> {
        let d = self.drawing();
        let opts = self.options(window, 1600.0, ctx);
        let c = match &self.target.space {
            Space::Model => match picked {
                Some(p) => cadcraft_engine::render::census_entities(d, p, false, &opts),
                None => cadcraft_engine::render::census_entities(d, &self.pick(window, ctx)?, false, &opts),
            },
            space => self.layout_census.get_or_init(|| cadcraft_engine::render::census_space(d, space, false, &opts)).clone(),
        };
        ctx.check()?;
        self.counted.set(Some(c.total));
        // Test hook: stop right after the count, so the drawing is what is stopped.
        #[cfg(test)]
        if STOP_AFTER_COUNT.with(std::cell::Cell::get) {
            ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(c)
    }

    /// The block pieces a build of `window` asks for (the number of [`Prepared::census`]).
    pub fn expansions(&self, window: &Bounds2, ctx: &Ctx) -> Result<u64, ToolError> {
        Ok(self.census(window, None, ctx)?.total)
    }

    /// The refusal of a picture whose count `census` is past the limit, decided before anything is
    /// drawn (the count is what the build expands: tests `the_up_front_count_is_what_an_unlimited_build_expands`
    /// and the annotative ones prove it, so a picture the build would draw whole is never refused).
    /// `None` when the picture fits.
    pub fn refusal(&self, census: &BlockCensus) -> Option<ToolError> {
        (census.total > self.block_cap as u64).then(|| refusal(self.block_cap, census.total, self.through(), census, &self.boxed_names, self.budget))
    }

    fn through(&self) -> Through {
        if matches!(self.target.space, Space::Model) { Through::Model } else { Through::Layout }
    }

    /// The error for a build that went past the limit although the count said it fits: no picture is
    /// made, and the agent is told what to try next.
    fn over_error(&self, built: usize) -> ToolError {
        mismatch_error(self.block_cap, self.counted.get(), built as u64, "picture")
    }

    /// What every answer that drew a picture says about block contents: the block pieces the build
    /// took (`built`, the build's own count: `DisplayList::expanded`), the blocks drawn as boxes
    /// with the references of each, and arrays drawn short.
    pub fn blocks_note(&self, built: usize, census: &BlockCensus) -> Value {
        let mut note = json!({"blocks_expanded_total": built});
        boxed_fields(&mut note, census);
        clamped_fields(&mut note, census);
        note
    }

    /// Whether model-space `e` reaches into `reach` (rays and infinite lines always do).
    fn reaches(&self, e: &Entity, reach: &Bounds2) -> bool {
        matches!(e.kind, EntityKind::Ray(_) | EntityKind::XLine(_)) || {
            let b = cadcraft_engine::doc::extent_bounds(self.drawing(), e);
            b.is_empty() || b.intersects(reach)
        }
    }

    /// The model-space objects a build of `window` takes: those that reach into it (infinite lines
    /// always). Empty for a layout, which is built whole.
    pub fn pick(&self, window: &Bounds2, ctx: &Ctx) -> Result<Vec<&Entity>, ToolError> {
        let mut picked = Vec::new();
        if !matches!(self.target.space, Space::Model) {
            return Ok(picked);
        }
        let reach = window.expand(window.width().max(window.height()) * 1e-6);
        for (k, e) in self.drawing().model.iter().enumerate() {
            if k % 65_536 == 0 {
                ctx.check()?;
            }
            if self.reaches(e, &reach) {
                picked.push(e.as_ref());
            }
        }
        Ok(picked)
    }

    /// The display list of `window` drawn `width_px` wide: curves as fine as an eighth of a pixel
    /// (room to zoom in on the SVG), and in model space only the objects that reach into the
    /// window (infinite lines always).
    pub fn list_for(&self, window: &Bounds2, width_px: f64, ctx: &Ctx) -> Result<DisplayList, ToolError> {
        let picked = self.pick(window, ctx)?;
        self.list_of(&picked, window, width_px, ctx)
    }

    /// [`Prepared::list_for`] of the objects `picked` already took ([`Prepared::pick`]).
    pub fn list_of(&self, picked: &[&Entity], window: &Bounds2, width_px: f64, ctx: &Ctx) -> Result<DisplayList, ToolError> {
        let d = self.drawing();
        let opts = self.options(window, width_px, ctx);
        ctx.stage("building the display list", 0);
        let list = match &self.target.space {
            Space::Model if picked.len() == d.model.len() => cadcraft_engine::render::build(d, &Space::Model, &opts),
            Space::Model => cadcraft_engine::render::build_entities(d, picked.iter().copied(), &opts),
            space => cadcraft_engine::render::build(d, space, &opts),
        };
        ctx.check()?;
        if list.expanded > self.block_cap {
            return Err(self.over_error(list.expanded));
        }
        Ok(list)
    }

    /// What [`Prepared::list_for`] returns, handed to `each` in pieces drawn one after the other:
    /// model space in slices of [`SLICE_ENTITIES`] entities in their stored draw order (so
    /// overlaps and wipeouts draw as in the whole list), each built, handed over and dropped
    /// before the next; a layout the same: its own objects and each viewport's model-space
    /// contents in slices in draw order (`render::build_layout_slices`). For a drawing too big to
    /// hold the display list of at once.
    pub fn each_list(
        &self,
        window: &Bounds2,
        width_px: f64,
        ctx: &Ctx,
        mut each: impl FnMut(&DisplayList) -> Result<(), WriteErr>,
    ) -> Result<usize, WriteErr> {
        self.each_list_sliced(window, width_px, ctx, SLICE_ENTITIES, &mut each)
    }

    fn each_list_sliced(
        &self,
        window: &Bounds2,
        width_px: f64,
        ctx: &Ctx,
        slice: usize,
        each: &mut dyn FnMut(&DisplayList) -> Result<(), WriteErr>,
    ) -> Result<usize, WriteErr> {
        if let Space::Paper(name) = &self.target.space {
            // A layout: the sheet's own objects and each viewport's model-space contents in slices
            // in draw order, each viewport's clip applied as the whole build does.
            let opts = self.options(window, width_px, ctx);
            ctx.stage("building the display list", 0);
            ctx.check()?;
            let expanded = cadcraft_engine::render::build_layout_slices(self.drawing(), name, &opts, slice, &mut |list: &DisplayList| {
                ctx.check()?;
                each(list)
            })?;
            ctx.check()?;
            if expanded > self.block_cap {
                return Err(WriteErr::Tool(self.over_error(expanded)));
            }
            return Ok(expanded);
        }
        let d = self.drawing();
        let opts = self.options(window, width_px, ctx);
        let reach = window.expand(window.width().max(window.height()) * 1e-6);
        ctx.stage("building the display list", 0);
        let mut picked: Vec<&Entity> = Vec::new();
        // Block contents expanded so far: the cap on them is the drawing's, not a slice's.
        let mut expanded = 0usize;
        let mut build = |picked: &mut Vec<&Entity>| -> Result<(), WriteErr> {
            ctx.check()?;
            let list = cadcraft_engine::render::build_slice(d, picked, &opts, expanded);
            expanded = list.expanded;
            picked.clear();
            ctx.check()?;
            if expanded > self.block_cap {
                return Err(WriteErr::Tool(self.over_error(expanded)));
            }
            each(&list)
        };
        for (k, e) in d.model.iter().enumerate() {
            if k % 65_536 == 0 {
                ctx.check()?;
            }
            if self.reaches(e, &reach) {
                picked.push(e.as_ref());
                if picked.len() >= slice {
                    build(&mut picked)?;
                }
            }
        }
        if !picked.is_empty() {
            build(&mut picked)?;
        }
        Ok(expanded)
    }

    /// The SVG of `window` written to `out` as it is made: the same bytes as
    /// [`svg_tile`] of [`Prepared::list_for`], without the whole display list or the whole text in
    /// memory. Returns the bytes written and the block contents expanded ([`Prepared::blocks_note`]).
    pub fn write_svg<W: std::io::Write>(&self, out: W, window: &Bounds2, width_px: f64, ctx: &Ctx) -> Result<(usize, usize), WriteErr> {
        self.write_svg_sliced(out, window, width_px, ctx, SLICE_ENTITIES)
    }

    pub fn write_svg_sliced<W: std::io::Write>(
        &self,
        out: W,
        window: &Bounds2,
        width_px: f64,
        ctx: &Ctx,
        slice: usize,
    ) -> Result<(usize, usize), WriteErr> {
        let mut w = SvgWriter::new(out, &self.layers, window, width_px, false);
        let expanded = self.each_list_sliced(window, width_px, ctx, slice, &mut |list| w.add(list, ctx))?;
        Ok((w.finish()?.2, expanded))
    }
}

/// The most block pieces one picture draws: [`MAX_BLOCK_EXPANSION`](cadcraft_engine::render::MAX_BLOCK_EXPANSION).
/// (Tests lower it for the calls they make on their own thread with [`lower_limit`], so the
/// behaviour past it can be run without building millions of objects.)
pub fn block_limit() -> usize {
    #[cfg(test)]
    if let Some(n) = TEST_LIMIT.with(std::cell::Cell::get) {
        return n;
    }
    cadcraft_engine::render::MAX_BLOCK_EXPANSION
}

#[cfg(test)]
thread_local! {
    pub static STOP_AFTER_COUNT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    static TEST_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Lowers the limit on this thread until the guard is dropped.
#[cfg(test)]
pub struct LowerLimit;

#[cfg(test)]
impl Drop for LowerLimit {
    fn drop(&mut self) {
        TEST_LIMIT.with(|c| c.set(None));
    }
}

#[cfg(test)]
pub fn lower_limit(n: usize) -> LowerLimit {
    TEST_LIMIT.with(|c| c.set(Some(n)));
    LowerLimit
}

/// Which picture needs more than the block-piece limit, for the ways to a complete one.
#[derive(Clone, Copy, PartialEq)]
pub enum Through {
    /// SVG or PNG of model space (or a viewport's model view).
    Model,
    /// SVG or PNG of a layout.
    Layout,
    /// A PDF sheet.
    Pdf,
}

/// What the count says when it overflowed (never a number: `u64::MAX` is not a count).
const UNCOUNTABLE: &str = "more than can be counted (nested references multiply past 1.8e19)";

/// The largest whole number a JSON client reads exactly (2^53).
const JSON_EXACT: u64 = 1 << 53;

/// A count as it goes in an answer: a number up to 2^53, a decimal string above it (JSON clients
/// lose precision there) and, when the count overflowed, the words [`UNCOUNTABLE`].
fn count_value(n: u64) -> Value {
    if n == COUNT_SATURATED {
        json!(UNCOUNTABLE)
    } else if n > JSON_EXACT {
        json!(n.to_string())
    } else {
        json!(n)
    }
}

/// A count in a sentence.
fn count_text(n: u64) -> String {
    if n == COUNT_SATURATED { UNCOUNTABLE.to_string() } else { n.to_string() }
}

/// How many entries of the layer table and of the heaviest list a refusal shows: as many as fit the
/// answer budget (`max_result_bytes`, or the call's `max_bytes`) measured on the refusal as it is
/// serialised, the heaviest first, at least one of each. An entry appears in the words and in a field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub blocks: usize,
    pub layers: usize,
}

impl Shown {
    /// Every entry of the census.
    pub fn all(census: &BlockCensus) -> Shown {
        Shown { blocks: census.heaviest.len(), layers: census.layers_off.len() }
    }
}

/// The bytes of an error as it is answered.
fn serialised(e: &ToolError) -> usize {
    e.to_value().to_string().len()
}

/// The refusal `build` makes with as many entries as fit `budget`, found by measuring the serialised refusal
/// (the fixed text, the names already drawn as boxes, every entry), never by an estimate: the lists grow
/// together, then each alone, by halving (a measurement is one build of the refusal, the cost of its entries).
/// `build(shown, names)`: `names` says whether the blocks already drawn as boxes are listed by name (they are
/// when the refusal still fits with them; otherwise they are counted). At least one entry of each list is
/// always given (the agent needs a name to act on), so a budget below the fixed text and one entry of each is
/// exceeded by exactly that much.
fn fitted(blocks: usize, layers: usize, has_names: bool, budget: usize, build: impl Fn(Shown, bool) -> ToolError) -> ToolError {
    let first = Shown { blocks: blocks.min(1), layers: layers.min(1) };
    let names = has_names && serialised(&build(first, true)) <= budget;
    let fits = |s: Shown| serialised(&build(s, names)) <= budget;
    // Largest k of `lo..=hi` for which `at(k)` fits (`lo` is taken as fitting).
    let most = |lo: usize, hi: usize, at: &dyn Fn(usize) -> Shown| {
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if fits(at(mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    };
    let k = most(1, blocks.max(layers).max(1), &|k| Shown { blocks: blocks.min(k), layers: layers.min(k) });
    let mut best = Shown { blocks: blocks.min(k), layers: layers.min(k) };
    let layers_kept = best.layers;
    best.blocks = most(best.blocks, blocks, &|k| Shown { blocks: k, layers: layers_kept });
    let blocks_kept = best.blocks;
    best.layers = most(best.layers, layers, &|k| Shown { blocks: blocks_kept, layers: k });
    build(best, names)
}

/// The words for the blocks the call already draws as boxes: their names, or how many when the names do not fit.
fn already_drawn(boxed: &[String], names: bool) -> String {
    if boxed.is_empty() {
        String::new()
    } else if names {
        format!(" (already drawn as boxes in this call: {})", boxed.join(", "))
    } else {
        format!(" (already drawn as boxes in this call: {} blocks, the ones named in this call's blocks_as_boxes)", boxed.len())
    }
}

/// Whether `left` pieces make a complete picture one picture can draw: a way that leaves nothing is an empty
/// picture, not a way through.
fn fits_one(left: u64, cap: usize) -> bool {
    left > 0 && left <= cap as u64
}

/// "saves N" / "saves more than can be counted" for a figure.
fn saves(n: u64) -> Value {
    count_value(n)
}

/// The fields a refusal carries about what makes the picture big: the one object that needs the
/// most, the layers the references are on and what switching each off leaves, and the heaviest
/// blocks with what drawing each as outline boxes leaves. The lists are shown as far as `shown` says;
/// `..._more` is how many were left out (box the listed ones and call again to list the next).
fn refusal_fields(note: &mut Value, cap: usize, census: &BlockCensus, shown: Shown) {
    if let Some(l) = &census.largest {
        note["largest_object"] = json!({
            "handle": l.handle.hex(),
            "kind": l.kind,
            "block": l.block,
            "blocks_expanded_total": count_value(l.count),
            "fits_one_picture": fits_one(l.count, cap),
        });
    }
    let layer_names: Vec<&String> = census.reference_layers.iter().take(shown.layers.max(1)).collect();
    note["reference_layers"] = json!(layer_names);
    if census.reference_layers.len() > layer_names.len() {
        note["reference_layers_more"] = json!(census.reference_layers.len() - layer_names.len());
    }
    if !census.layers_off.is_empty() {
        note["layers_that_reduce"] = json!(
            census
                .layers_off
                .iter()
                .take(shown.layers)
                .map(|l| {
                    let mut v = json!({"layer": l.layer, "saves": saves(l.saving)});
                    match l.total_if_off {
                        Some(t) => {
                            v["blocks_expanded_total_if_off"] = count_value(t);
                            v["fits_one_picture"] = json!(fits_one(t, cap));
                        }
                        None => v["left_cannot_be_counted"] = json!(true),
                    }
                    v
                })
                .collect::<Vec<_>>()
        );
        if census.layers_off.len() > shown.layers {
            note["layers_that_reduce_more"] = json!(census.layers_off.len() - shown.layers);
        }
    }
    if !census.heaviest.is_empty() {
        note["heaviest_blocks"] = json!(
            census
                .heaviest
                .iter()
                .take(shown.blocks)
                .map(|b| {
                    let mut v = json!({"block": b.block, "saves": saves(b.saving)});
                    match b.total_if_boxed {
                        Some(t) => {
                            v["blocks_expanded_total_if_boxed"] = count_value(t);
                            v["fits_one_picture"] = json!(fits_one(t, cap));
                        }
                        None => v["left_cannot_be_counted"] = json!(true),
                    }
                    v
                })
                .collect::<Vec<_>>()
        );
        if census.heaviest.len() > shown.blocks {
            note["heaviest_blocks_more"] = json!(census.heaviest.len() - shown.blocks);
        }
        note["heaviest_blocks_all_boxed"] = json!({
            "blocks": census.heaviest.len(),
            "blocks_expanded_total": count_value(census.total_if_listed_boxed),
            "fits_one_picture": fits_one(census.total_if_listed_boxed, cap)
        });
    }
}

/// The blocks drawn as boxes in a picture and the references of each (said in every answer that
/// drew with `blocks_as_boxes`).
fn boxed_fields(note: &mut Value, census: &BlockCensus) {
    if census.boxed.is_empty() {
        return;
    }
    note["blocks_drawn_as_boxes"] =
        json!(census.boxed.iter().map(|b| json!({"block": b.block, "outlines_drawn": count_value(b.outlines)})).collect::<Vec<_>>());
    note["blocks_drawn_as_boxes_note"] = json!(
        "every reference to these blocks is drawn as ONE outline of its extents (the block's extents under the reference's transform; an array of references is one outline of the whole array) on the reference's own layer and colour, instead of everything the reference draws: its contents AND its attribute texts are not drawn. outlines_drawn is how many outlines were drawn (a block with nothing visible in it draws none). The outline covers the block as the drawing's extents measure it: layers that are off or frozen in the drawing are left out, but layers frozen only in one layout viewport, and layers marked do-not-plot in a PDF, are not subtracted (the extents are taken once per block), so there the outline can be larger than what the block would draw, never smaller. The drawing itself is not changed"
    );
}

/// Arrays drawn short by the 10,000 columns / rows the build draws of a MINSERT (said in every
/// answer that draws).
fn clamped_fields(note: &mut Value, census: &BlockCensus) {
    if census.clamped.is_empty() {
        return;
    }
    note["arrays_clamped"] = json!(
        census
            .clamped
            .iter()
            .map(|a| json!({
                "handle": a.handle.hex(),
                "block": a.block,
                "cols": a.cols,
                "rows": a.rows,
                "drawn_cols": a.drawn_cols,
                "drawn_rows": a.drawn_rows,
            }))
            .collect::<Vec<_>>()
    );
    note["arrays_clamped_note"] = json!(format!(
        "{} block array{} drawn short: a block array (MINSERT) is drawn with at most 10,000 columns and 10,000 rows and the rest of its copies are not in this picture (arrays_clamped lists them; reading, measuring and extents still see the whole array). No way to draw more of such an array exists today",
        census.clamped.len(),
        if census.clamped.len() == 1 { " is" } else { "s are" }
    ));
}

/// The answer when a build reached `built` block pieces, past the `cap` one picture draws, although the
/// count made before drawing (`counted`) said it fits: the count may be wrong for this drawing. No
/// picture is made and no file is written; the way on is the same as for a refusal.
pub fn mismatch_error(cap: usize, counted: Option<u64>, built: u64, what: &str) -> ToolError {
    let said = counted.map_or("fits".to_string(), |n| format!("fits (it counted {n})"));
    ToolError::new(
        code::TOO_LARGE,
        format!(
            "this {what} still came out over the {cap} block-piece limit (the build reached {built}) although the count made before drawing said it {said}, so the count may be wrong for this drawing: no picture was made and no file was written"
        ),
        "switch off more layers (layers_off / only_layers) and / or draw more blocks as boxes (blocks_as_boxes: [the names of the blocks with the most references]; read_drawing section \"blocks\" lists the block names) and call again",
    )
    .with("blocks_expanded_limit", json!(cap))
    .with("blocks_expanded_counted", counted.map_or(Value::Null, count_value))
    .with("blocks_expanded_built", count_value(built))
    .with("picture_made", json!(false))
}

/// The refusal of a picture that needs `total` block pieces when one picture draws at most `cap`:
/// no picture was made and no file was written. What happened, why and how to go on, with the
/// tables that say what each choice leaves (`boxed` are the blocks the call already draws as boxes;
/// `budget` is the answer budget the whole refusal is kept within: the entries shown are as many as
/// fit it when measured, see [`fitted`]).
pub fn refusal(cap: usize, total: u64, kind: Through, census: &BlockCensus, boxed: &[String], budget: usize) -> ToolError {
    fitted(census.heaviest.len(), census.layers_off.len(), !boxed.is_empty(), budget, |shown, names| {
        refusal_shown(cap, total, kind, census, &already_drawn(boxed, names), shown)
    })
}

fn refusal_shown(cap: usize, total: u64, kind: Through, census: &BlockCensus, already: &str, shown: Shown) -> ToolError {
    let ways = ways(cap, total, kind, census, already, shown);
    let reason = format!(
        "this picture needs {} block pieces (every copy of a block and every object in it, nested blocks counted) and one picture draws at most {cap}: no picture was made and no file was written",
        count_text(total)
    );
    let next = format!("make the picture complete by one of these, all of which leave the drawing as it is: {}", numbered(&ways));
    let mut e = ToolError::new(code::TOO_LARGE, reason, next)
        .with("blocks_expanded_total", count_value(total))
        .with("blocks_expanded_limit", json!(cap))
        .with("picture_made", json!(false));
    let mut fields = json!({});
    refusal_fields(&mut fields, cap, census, shown);
    clamped_fields(&mut fields, census);
    if let Value::Object(o) = fields {
        for (k, v) in o {
            e = e.with(&k, v);
        }
    }
    e
}

fn numbered(ways: &[String]) -> String {
    ways.iter().enumerate().map(|(i, w)| format!("({}) {}.", i + 1, w.trim_end_matches('.'))).collect::<Vec<_>>().join(" ")
}

/// The ways to a complete picture when it needs `total` block pieces and one picture draws at most
/// `cap`, in words for the agent. Only routes that leave the drawing as it is; each is proved by a
/// test, and what does not exist is said. Windows are advised only when the largest single object
/// fits: a build expands an object whole in every window that touches it.
pub fn ways_through(cap: usize, total: u64, kind: Through, census: &BlockCensus, boxed: &[String], shown: Shown) -> Vec<String> {
    ways(cap, total, kind, census, &already_drawn(boxed, true), shown)
}

fn ways(cap: usize, total: u64, kind: Through, census: &BlockCensus, already: &str, shown: Shown) -> Vec<String> {
    let mut ways = Vec::new();
    let too_big = census.largest.as_ref().filter(|l| l.count > cap as u64);
    let single = census.reference_layers.len() == 1 && !census.references_off_table;
    // Layers.
    if census.layers_off.is_empty() {
        if single {
            ways.push(format!(
                "every block reference is on one layer ({}), so layers_off / only_layers cannot split the drawing into passes: switching that layer off leaves none of them",
                census.reference_layers.first().map_or("", String::as_str)
            ));
        }
    } else {
        let list = census
            .layers_off
            .iter()
            .take(shown.layers)
            .map(|l| match l.total_if_off {
                Some(0) => format!("{} leaves 0, an empty picture (no way through)", l.layer),
                Some(t) => format!("{} leaves {}{}", l.layer, count_text(t), if fits_one(t, cap) { ", which fits one picture" } else { "" }),
                None => format!("{} saves {} (what would be left cannot be counted)", l.layer, count_text(l.saving)),
            })
            .collect::<Vec<_>>()
            .join("; ");
        let more = census.layers_off.len().saturating_sub(shown.layers);
        let more = if more == 0 {
            String::new()
        } else {
            format!(
                "; {more} more layers are not listed here (layers_that_reduce_more): switch off the ones listed and call again to list the next ones"
            )
        };
        ways.push(format!(
            "switch off the layer the block references are on (layers_off / only_layers); the references on it are then not in the picture, and the block pieces with it off are: {list}{more}. Switching off the layers of the references' contents does not reduce the count (the build takes one piece for each object of a block before it looks at the object's layer)"
        ));
        if single {
            ways.push(format!(
                "every block reference is on one layer ({}), so layers cannot split the drawing into passes: switching it off leaves none of them",
                census.reference_layers.first().map_or("", String::as_str)
            ));
        }
    }
    // Blocks drawn as boxes.
    if census.heaviest.is_empty() {
        ways.push(if census.blocks_referenced == 0 {
            format!(
                "blocks_as_boxes cannot shorten this: the picture refers to no block that is still drawn with its contents{already} (a dimension's block is drawn from its definition and is not drawn as a box)"
            )
        } else {
            format!(
                "blocks_as_boxes cannot shorten this: the {} block(s) the picture refers to that are still drawn with their contents{already} each cost one piece per copy whether drawn or boxed (they hold nothing that costs more)",
                census.blocks_referenced
            )
        });
    } else {
        let list = census
            .heaviest
            .iter()
            .take(shown.blocks)
            .map(|b| match b.total_if_boxed {
                Some(t) => format!("{} leaves {}{}", b.block, count_text(t), if fits_one(t, cap) { ", which fits one picture" } else { "" }),
                None => format!("{} saves {} (what would be left cannot be counted)", b.block, count_text(b.saving)),
            })
            .collect::<Vec<_>>()
            .join("; ");
        let n = census.heaviest.len();
        let listed = shown.blocks.min(n);
        let more = n - listed;
        let (more_text, every) = if more == 0 {
            (String::new(), format!("all {n} listed blocks"))
        } else {
            (
                format!(
                    "; {more} more are not listed here (heaviest_blocks_more): draw the listed blocks as boxes and call again, and the next ones are listed"
                ),
                format!("all {n} (the {listed} listed and {more} more)"),
            )
        };
        let all = if fits_one(census.total_if_listed_boxed, cap) {
            format!("Drawing {every} as boxes leaves {}, which fits one picture", count_text(census.total_if_listed_boxed))
        } else {
            format!(
                "Drawing {every} as boxes still leaves {}, more than {cap}: boxes alone do not give a complete picture of this drawing, so combine them with layers off or windows",
                count_text(census.total_if_listed_boxed)
            )
        };
        let pass = "Pass the block names from the top of the list down as far as is needed (blocks_as_boxes: [those names])";
        ways.push(format!(
            "draw named blocks as outline boxes (blocks_as_boxes: [block names]{already}): every reference to a named block, at any nesting depth, is drawn as one outline of its extents (a rotated reference gives a rotated rectangle, an array of references one outline for the whole array) on the reference's own layer and colour, instead of everything the reference draws (its contents and its attribute texts), and counts one piece. The heaviest blocks, the biggest saving first, with what is left if that block alone is drawn as boxes: {list}{more_text}. {all}. {pass}"
        ));
    }
    // Windows, tiles and the viewport route.
    if let Some(l) = too_big {
        // A dimension's block is drawn from its definition and never as a box: only a layer reduces it.
        let reduces = if l.kind == "dimension" {
            "only switching off the layer it is on can reduce it: blocks_as_boxes does not apply to a dimension"
        } else {
            "only layers or blocks_as_boxes can reduce it"
        };
        ways.push(format!(
            "windows and tiles cannot complete this: the {} with handle {} (block {}) alone needs {} block pieces, more than the {cap} one picture draws, and a build expands every object that reaches its window whole, so every window or tile that touches it hits the limit again ({reduces}){}",
            l.kind,
            l.handle.hex(),
            l.block,
            count_text(l.count),
            if kind == Through::Pdf { "; a PDF sheet's window only places the drawing on the page, it does not leave out the block references outside it" } else { "" }
        ));
    } else {
        let windows = total.div_ceil((cap as u64).max(1)).max(2);
        let how_many = if total == COUNT_SATURATED {
            "this drawing needs more than can be counted, so no number of windows is given".to_string()
        } else {
            format!("this drawing needs {total}, so use at least {windows} windows")
        };
        match kind {
            Through::Model => ways.push(format!(
                "draw it in windows: only objects whose extent reaches the window are expanded and the limit is per picture, so a window (or tile) that needs {cap} or fewer is drawn whole. No single object needs more than {cap}, and {how_many}: render_preview with tiles {{cols, rows}} and output writes every tile that fits to its own file (each tile's answer says its own blocks_expanded_total; a tile over the limit is refused on its own), or export svg / png with window [[x0, y0], [x1, y1]] once per region"
            )),
            Through::Layout => ways.push(format!(
                "a layout is built whole, so a window does not reduce it. Draw the model space its viewports show instead: render_preview with viewport: <handle> (read_drawing section \"layouts\" lists them) and tiles or a window, one picture per tile; each needs {cap} or fewer to be whole. The layout needs {} in one picture",
                count_text(total)
            )),
            Through::Pdf => ways.push(format!(
                "a PDF sheet is one picture of the whole space and its window only places the drawing on the page, it does not leave out the block references outside it. For the regions of the drawing write SVG or PNG: export svg / png with window [[x0, y0], [x1, y1]] per region, or render_preview with tiles and output. {how_many}; the sheet needs {} in one picture and {cap} is the most a picture draws",
                count_text(total)
            )),
        }
    }
    ways.push(
        "reading is not affected by the limit: read_drawing (entities with expand_blocks, takeoff, text, dimensions), search_drawing, measuring and the extents do not make a picture, so everything in the drawing can still be read, searched and measured".to_string(),
    );
    let fits_somewhere = census.layers_off.iter().any(|l| l.total_if_off.is_some_and(|t| fits_one(t, cap)))
        || (!census.heaviest.is_empty() && fits_one(census.total_if_listed_boxed, cap))
        || (too_big.is_none() && kind == Through::Model);
    let unknown = census.layers_off.iter().filter(|l| l.total_if_off.is_none()).count()
        + census.heaviest.iter().filter(|b| b.total_if_boxed.is_none()).count();
    if !fits_somewhere && unknown > 0 {
        // Never "none of these" while a layer or block exists whose remainder could not be counted.
        ways.push(format!(
            "for {unknown} of the layers and blocks above what would be left cannot be counted (it is past what can be worked out), so it is not known whether switching one of them off, or drawing it as boxes, gives a complete picture: try them, and the answer of the next call gives the figures with that change made"
        ));
    } else if !fits_somewhere {
        let combine = if !census.layers_off.is_empty() && !census.heaviest.is_empty() {
            " They can be combined (for example layers_off together with blocks_as_boxes)."
        } else {
            ""
        };
        ways.push(format!(
            "none of these alone gives a complete picture of this drawing in one picture: each way above leaves more than {cap}, leaves nothing, or cannot split it.{combine} The answer of the next call gives the new figures"
        ));
    }
    ways
}

/// What a PDF export's refusal says: which sheets are over the limit (with their page numbers),
/// each one's figures, and the ways to a complete file. One file holds every sheet, so one sheet
/// over the limit means no file for any.
pub fn pdf_refusal(blocks: &[cadcraft_io::SheetBlocks], boxed: &[String], budget: usize) -> ToolError {
    let over: Vec<&cadcraft_io::SheetBlocks> = blocks.iter().filter(|b| b.over_limit()).collect();
    // A sheet the count said fits but whose build went over: the count may be wrong.
    if let Some(b) = over.iter().find(|b| b.census.total <= b.limit as u64) {
        return mismatch_error(b.limit, Some(b.census.total), b.total, &format!("PDF sheet {} ({})", b.number, b.sheet));
    }
    let Some(cap) = over.first().map(|b| b.limit) else {
        return ToolError::new(
            code::INTERNAL,
            "a PDF was refused although no sheet is over the limit",
            "retry the export; report this if it repeats",
        );
    };
    let list = over.iter().map(|b| format!("sheet {} ({}) needs {}", b.number, b.sheet, count_text(b.total))).collect::<Vec<_>>().join("; ");
    let reason = format!(
        "{} of the {} sheet{} of this PDF {} more block pieces (every copy of a block and every object in it, nested blocks counted) than the {cap} one sheet draws: {list}. A PDF is one file and a sheet is never drawn incomplete, so no PDF was written",
        over.len(),
        blocks.len(),
        if blocks.len() == 1 { "" } else { "s" },
        if over.len() == 1 { "needs" } else { "need" }
    );
    let sum = over
        .iter()
        .map(|b| b.total)
        .fold(0u64, |a, n| if a == COUNT_SATURATED || n == COUNT_SATURATED { COUNT_SATURATED } else { a.saturating_add(n) });
    let Some(worst) = over.iter().max_by_key(|b| b.total) else {
        return ToolError::new(
            code::INTERNAL,
            "a PDF was refused although no sheet is over the limit",
            "retry the export; report this if it repeats",
        );
    };
    let blocks_max = over.iter().map(|b| b.census.heaviest.len()).max().unwrap_or(0);
    let layers_max = over.iter().map(|b| b.census.layers_off.len()).max().unwrap_or(0);
    fitted(blocks_max, layers_max, !boxed.is_empty(), budget, |shown, names| {
        let already = already_drawn(boxed, names);
        let ways = ways(cap, worst.total, Through::Pdf, &worst.census, &already, shown);
        let next = format!(
            "make every sheet over the limit complete by one of these, all of which leave the drawing as it is (the figures are those of sheet {}, the one that needs the most; sheets_over_limit gives each sheet's own): {}",
            worst.number,
            numbered(&ways)
        );
        let mut e = ToolError::new(code::TOO_LARGE, reason.clone(), next)
            .with("blocks_expanded_total", count_value(sum))
            .with("blocks_expanded_limit", json!(cap))
            .with("picture_made", json!(false))
            .with("sheet_numbers_over_limit", json!(over.iter().map(|b| b.number).collect::<Vec<_>>()));
        let entries: Vec<Value> = over
            .iter()
            .map(|b| {
                let mut v = json!({"sheet_number": b.number, "sheet": b.sheet, "blocks_expanded_total": count_value(b.total)});
                refusal_fields(&mut v, b.limit, &b.census, shown);
                v
            })
            .collect();
        e = e.with("sheets_over_limit", json!(entries));
        e.with(
            "sheets_within_limit",
            json!(
                blocks
                    .iter()
                    .filter(|b| !b.over_limit())
                    .map(|b| json!({"sheet_number": b.number, "sheet": b.sheet, "blocks_expanded_total": count_value(b.total)}))
                    .collect::<Vec<_>>()
            ),
        )
    })
}

/// What a PDF export's answer says about block contents when the file was made, sheet by sheet:
/// each sheet's block pieces, the sums, the blocks drawn as boxes and arrays drawn short.
pub fn pdf_blocks_note(blocks: &[cadcraft_io::SheetBlocks]) -> Value {
    let sum = |f: &dyn Fn(&cadcraft_io::SheetBlocks) -> u64| {
        blocks.iter().map(f).fold(0u64, |a, n| if a == COUNT_SATURATED || n == COUNT_SATURATED { COUNT_SATURATED } else { a.saturating_add(n) })
    };
    let mut note = json!({"blocks_expanded_total": count_value(sum(&|b| b.total))});
    note["sheets"] = json!(
        blocks
            .iter()
            .map(|b| {
                let mut e = json!({"sheet": b.sheet, "sheet_number": b.number, "blocks_expanded_total": count_value(b.total)});
                boxed_fields(&mut e, &b.census);
                clamped_fields(&mut e, &b.census);
                e
            })
            .collect::<Vec<_>>()
    );
    // The blocks drawn as boxes and the references of each, over every sheet.
    let mut boxed: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for b in blocks.iter().flat_map(|b| b.census.boxed.iter()) {
        let n = boxed.entry(b.block.clone()).or_insert(0);
        *n = if *n == COUNT_SATURATED || b.outlines == COUNT_SATURATED { COUNT_SATURATED } else { n.saturating_add(b.outlines) };
    }
    let total = BlockCensus {
        boxed: boxed.into_iter().map(|(block, references)| cadcraft_engine::render::BoxedRefs { block, outlines: references }).collect(),
        ..BlockCensus::default()
    };
    boxed_fields(&mut note, &total);
    // Arrays drawn short on any sheet.
    let mut seen = std::collections::BTreeSet::new();
    let clamped: Vec<_> = blocks.iter().flat_map(|b| b.census.clamped.iter()).filter(|a| seen.insert(a.handle.0)).cloned().collect();
    clamped_fields(&mut note, &BlockCensus { clamped, ..BlockCensus::default() });
    note
}

/// The blocks a call asks to draw as boxes (`blocks_as_boxes`: a list of block names), as the drawing spells
/// them, each once. Anything else is refused: not a list of strings (what was given, the form, an
/// example), a name that is no block of the drawing (it, the nearest block names, where the full list is);
/// each says to call again.
pub fn boxed_blocks(d: &Drawing, a: &Value) -> Result<Vec<String>, ToolError> {
    let asked = cadcraft_engine::doc::boxed_names(a.get("blocks_as_boxes")).map_err(|e| {
        ToolError::new(
            code::INVALID_ARGUMENT,
            format!("{e}; nothing was drawn and no file was written"),
            "give blocks_as_boxes as a list of block names, e.g. blocks_as_boxes: [\"Tree\", \"Chair\"] (read_drawing section \"blocks\" lists the names), and call again",
        )
    })?;
    let mut unknown: Vec<String> = Vec::new();
    for n in &asked {
        if d.block(n).is_none() && !unknown.contains(n) {
            unknown.push(n.clone());
        }
    }
    if !unknown.is_empty() {
        let known: Vec<&str> = d.blocks.values().map(|b| b.name.as_str()).collect();
        let closest: serde_json::Map<String, Value> =
            unknown.iter().map(|n| (n.clone(), json!(super::contract::closest(n, known.iter().copied(), 3)))).collect();
        return Err(ToolError::new(
            code::NOT_FOUND,
            format!("{}; nothing was drawn and no file was written", cadcraft_engine::doc::unknown_blocks_message(d, &unknown)),
            "read_drawing section \"blocks\" lists every block name (anonymous: true for the ones the program names itself; `closest` gives the nearest names): call again with blocks_as_boxes names taken from that list, in any case",
        )
        .with("unknown_blocks", json!(unknown))
        .with("closest", Value::Object(closest)));
    }
    cadcraft_engine::doc::checked_boxed_names(d, a.get("blocks_as_boxes"))
        .map_err(|e| ToolError::new(code::NOT_FOUND, e, "call again with names from read_drawing section \"blocks\""))
}

/// The answer budget of a call: its `max_bytes` when given, else the server's `max_result_bytes`.
pub fn answer_budget(a: &Value, ctx: &Ctx) -> usize {
    a.get("max_bytes").and_then(Value::as_u64).map_or(ctx.settings.max_result_bytes, |b| usize::try_from(b).unwrap_or(usize::MAX))
}

pub fn prepare<'a>(d: &'a Drawing, a: &Value, ctx: &Ctx) -> Result<Prepared<'a>, ToolError> {
    let target = target(d, a)?;
    let boxed_names = boxed_blocks(d, a)?;
    let mut sw = LayerSwitch::from_args(a);
    sw.off.extend(target.frozen.iter().cloned());
    let (switched, unknown) = if sw.is_empty() {
        (None, Vec::new())
    } else {
        let (c, u) = sw.apply(d);
        (Some(c), u)
    };
    let dd = switched.as_ref().unwrap_or(d);
    ctx.stage("measuring the extents", 0);
    let extents = dd.extents(&target.space);
    ctx.check()?;
    let window = match window_arg(a.get("window"))?.or(target.window) {
        Some(w) => w,
        None if extents.is_empty() => Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)),
        None => {
            let m = extents.width().max(extents.height()) * 0.02;
            extents.expand(m.max(1e-9))
        }
    };
    if !(window.width() > 0.0 && window.height() > 0.0) || [window.min.x, window.min.y, window.max.x, window.max.y].iter().any(|c| !c.is_finite()) {
        return Err(ToolError::new(code::INVALID_ARGUMENT, "the window has no area", "give two different corners [[x0, y0], [x1, y1]]"));
    }
    let layers = layer_of(dd, &target.space);
    let boxes = if boxed_names.is_empty() {
        None
    } else {
        Some(std::sync::Arc::new(BoxBlocks::new(dd, &boxed_names).map_err(|e| {
            ToolError::new(code::NOT_FOUND, e, "read_drawing section \"blocks\" lists the block names: call again with names taken from it")
        })?))
    };
    let layer_states = dd.layers.iter().map(|l| (l.name.clone(), l.on && !l.frozen)).collect();
    let lineweights = a.get("lineweights").and_then(Value::as_bool).unwrap_or(true);
    Ok(Prepared {
        base: d,
        switched,
        layers,
        window,
        extents,
        target,
        unknown_layers: unknown,
        layer_states,
        lineweights,
        block_cap: block_limit(),
        budget: answer_budget(a, ctx),
        boxed_names,
        boxes,
        layout_census: std::cell::OnceCell::new(),
        counted: std::cell::Cell::new(None),
    })
}

/// The tiles of `window`: `tiles: {cols, rows}` (or `[cols, rows]`, or a number for both),
/// only `tile: [col, row]` (0-based, from the lower left) when given.
pub fn tiles(window: &Bounds2, a: &Value) -> Result<Vec<([u32; 2], Bounds2)>, ToolError> {
    let (cols, rows) = match a.get("tiles") {
        None | Some(Value::Null) => (1, 1),
        Some(Value::Number(n)) => {
            let k = n.as_u64().unwrap_or(1);
            (k, k)
        }
        Some(Value::Array(v)) => (v.first().and_then(Value::as_u64).unwrap_or(1), v.get(1).and_then(Value::as_u64).unwrap_or(1)),
        Some(Value::Object(o)) => (o.get("cols").and_then(Value::as_u64).unwrap_or(1), o.get("rows").and_then(Value::as_u64).unwrap_or(1)),
        Some(_) => (0, 0),
    };
    if cols == 0 || rows == 0 || cols.saturating_mul(rows) > 1_000_000 {
        return Err(ToolError::new(
            code::INVALID_ARGUMENT,
            "`tiles` must be {cols, rows} with whole numbers from 1",
            "e.g. tiles: {cols: 4, rows: 3}",
        ));
    }
    let (cols, rows) = (u32::try_from(cols).unwrap_or(1), u32::try_from(rows).unwrap_or(1));
    let only = a.get("tile").and_then(Value::as_array).map(|t| (t.first().and_then(Value::as_u64), t.get(1).and_then(Value::as_u64)));
    let (tw, th) = (window.width() / f64::from(cols), window.height() / f64::from(rows));
    let mut out = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            if let Some((oc, or)) = only
                && (oc != Some(u64::from(c)) || or != Some(u64::from(r)))
            {
                continue;
            }
            let min = Vec2::new(window.min.x + tw * f64::from(c), window.min.y + th * f64::from(r));
            out.push(([c, r], Bounds2::new(min, Vec2::new(min.x + tw, min.y + th))));
        }
    }
    if out.is_empty() {
        return Err(ToolError::new(
            code::OUT_OF_RANGE,
            format!("`tile` is outside the {cols} x {rows} grid"),
            "tiles count from [0, 0] at the lower left",
        ));
    }
    Ok(out)
}

fn r6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

pub fn win_value(w: &Bounds2) -> Value {
    json!([[r6(w.min.x), r6(w.min.y)], [r6(w.max.x), r6(w.max.y)]])
}

/// PNG of the window (a picture for a visual check), as base64.
/// The PNG of `window` with its size and the block contents expanded ([`Prepared::blocks_note`]).
pub fn png(
    p: &Prepared<'_>,
    window: &Bounds2,
    width: u32,
    height: Option<u32>,
    dark: bool,
    ctx: &Ctx,
) -> Result<(Vec<u8>, u32, u32, usize), ToolError> {
    let picked = p.pick(window, ctx)?;
    png_of(p, &picked, window, width, height, dark, ctx)
}

/// [`png`] of the objects `picked` already took ([`Prepared::pick`]).
pub fn png_of(
    p: &Prepared<'_>,
    picked: &[&Entity],
    window: &Bounds2,
    width: u32,
    height: Option<u32>,
    dark: bool,
    ctx: &Ctx,
) -> Result<(Vec<u8>, u32, u32, usize), ToolError> {
    let view = cadcraft_engine::render::raster::View::of_window(window, Some(width), height);
    let (w, h) = (view.width, view.height);
    let list = p.list_of(picked, window, f64::from(w) / 8.0, ctx)?;
    let mut opts = cadcraft_engine::render::raster::RasterOptions::default();
    if !dark {
        opts.background = cadcraft_engine::doc::color::Rgb(255, 255, 255);
    }
    let bytes = cadcraft_engine::render::raster::render_png(&list, &view, &opts)
        .ok_or_else(|| ToolError::new(code::FAILED, "the picture could not be drawn", "try a smaller width or a window"))?;
    Ok((bytes, w, h, list.expanded))
}
