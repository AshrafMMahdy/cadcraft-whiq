//! WHIQ: `render_preview`, the platform's Preview tab renderer: vector SVG of a space (model or a
//! layout, a layout viewport's model view), the whole extents or a window, in tiles for big
//! drawings, with layers switched on or off; or a PNG to look at. Every primitive carries its
//! layer (`<g data-layer>` runs, draw order kept), so a viewer can also hide layers itself.
//! Nothing is capped: a drawing too big for one answer comes back tile by tile
//! (`continue_from`), each tile holding only what it shows.

use std::fmt::Write as _;

use cadcraft_engine::doc::{Drawing, Entity, EntityKind, Handle, Space};
use cadcraft_engine::geom::{Bounds2, Vec2};
use cadcraft_engine::render::{BlockCensus, COUNT_SATURATED, DisplayList, Kind};
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
    /// Block contents expanded before the rest are not drawn (a test lowers it).
    pub block_cap: usize,
    /// What counting a layout's block expansions found: the whole layout is built for every tile,
    /// so it is counted once per call.
    layout_census: std::cell::OnceCell<BlockCensus>,
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
        Ok(c)
    }

    /// The block expansions a build of `window` asks for (the number of [`Prepared::census`]).
    pub fn expansions(&self, window: &Bounds2, ctx: &Ctx) -> Result<u64, ToolError> {
        Ok(self.census(window, None, ctx)?.total)
    }

    /// What every answer that draws says about block contents: the expansions the build took
    /// (`built`, the build's own count: `DisplayList::expanded`), and when that is past the limit
    /// the count of what the drawing asks for, what was not drawn and the ways through; arrays
    /// drawn short are named ([`Prepared::blocks_note_for`]).
    pub fn blocks_note(&self, window: &Bounds2, built: usize, picked: Option<&[&Entity]>, ctx: &Ctx) -> Result<Option<Value>, ToolError> {
        let census = self.census(window, picked, ctx)?;
        Ok(Some(self.blocks_note_for(built, &census)))
    }

    /// [`Prepared::blocks_note`] for a build that took `built` expansions and a count `census`.
    /// The build is the ground truth: `blocks_truncated` is true only when its own count went past
    /// the limit, and a build that drew everything gives its own figure as the total. Past the
    /// limit the build stops, so the total is the count (never below what the build took).
    pub fn blocks_note_for(&self, built: usize, census: &BlockCensus) -> Value {
        let cap = self.block_cap;
        let truncated = built > cap;
        let total = if truncated { census.total.max(built as u64) } else { built as u64 };
        let mut note = json!({"blocks_expanded_total": count_value(total)});
        if truncated {
            let not_drawn = if total == COUNT_SATURATED { total } else { total.saturating_sub(cap as u64) };
            note["blocks_truncated"] = json!(true);
            note["blocks_expanded"] = json!(cap);
            note["blocks_not_drawn"] = count_value(not_drawn);
            note["warning"] = json!(if total == COUNT_SATURATED {
                format!("block contents past {cap} expanded entities were not drawn: the drawing needs {UNCOUNTABLE}")
            } else {
                format!("block contents past {cap} expanded entities were not drawn: {not_drawn} of the {total} the drawing needs are missing from this picture")
            });
            let kind = if matches!(self.target.space, Space::Model) { Through::Model } else { Through::Layout };
            note["ways_through"] = json!(ways_through(cap, total, kind, census));
            truncated_fields(&mut note, cap, census);
        }
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

    pub fn write_svg_sliced<W: std::io::Write>(&self, out: W, window: &Bounds2, width_px: f64, ctx: &Ctx, slice: usize) -> Result<(usize, usize), WriteErr> {
        let mut w = SvgWriter::new(out, &self.layers, window, width_px, false);
        let expanded = self.each_list_sliced(window, width_px, ctx, slice, &mut |list| w.add(list, ctx))?;
        Ok((w.finish()?.2, expanded))
    }
}

/// Which picture ran past the block-expansion limit, for the ways through it.
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

/// The structured fields of a truncated answer: the one object that needs the most and the
/// reference layers that reduce the count.
fn truncated_fields(note: &mut Value, cap: usize, census: &BlockCensus) {
    if let Some(l) = &census.largest {
        note["largest_object"] = json!({
            "handle": l.handle.hex(),
            "kind": l.kind,
            "block": l.block,
            "blocks_expanded": count_value(l.count),
            "fits_one_build": l.count <= cap as u64,
        });
    }
    note["reference_layers"] = json!(census.reference_layers);
    if !census.layers_off.is_empty() {
        note["layers_that_reduce"] = json!(census
            .layers_off
            .iter()
            .map(|l| json!({"layer": l.layer, "blocks_expanded_total_if_off": count_value(l.total_if_off), "fits_one_build": l.total_if_off <= cap as u64}))
            .collect::<Vec<_>>());
    }
}

/// Arrays drawn short by the 10,000 columns / rows the build draws of a MINSERT (said in every
/// answer that draws, truncated or not).
fn clamped_fields(note: &mut Value, census: &BlockCensus) {
    if census.clamped.is_empty() {
        return;
    }
    note["arrays_clamped"] = json!(census
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
        .collect::<Vec<_>>());
    note["arrays_clamped_note"] = json!(format!(
        "{} block array{} drawn short: a block array (MINSERT) is drawn with at most 10,000 columns and 10,000 rows and the rest of its copies are not in this picture (arrays_clamped lists them; reading, measuring and extents still see the whole array). {SEPARATE_REFERENCES}",
        census.clamped.len(),
        if census.clamped.len() == 1 { " is" } else { "s are" }
    ));
}

/// How to turn an array into separate references the build draws all of (and windows can split);
/// proved in the tests (`the_ways_the_answer_names_work`).
const SEPARATE_REFERENCES: &str = "To draw all of an array, make its copies separate references with edit: erase the array (op \"delete\", handles), insert the block once at the array's insertion point with its rotation (op \"block\", action \"insert\", name, at, rotation) and run arrayrect {handles: [that reference], rows, cols, rowSpacing, colSpacing, axisAngle: the array's rotation in degrees, associative: false}; each copy is then an object of its own. This changes the open drawing in memory only until save_drawing (the edit op \"undo\", or close_drawing with discard: true, takes it back)";

/// How to open up one block reference, proved in the tests.
const EXPLODE_ONE: &str = "edit {op: \"command\", command: \"explode\", params: {handles: [\"HANDLE\"]}} replaces a block reference by the block's own entities, so the references nested inside it become objects of the drawing that windows can split (explode again for one that is still too big); it changes the open drawing in memory only until save_drawing (the edit op \"undo\", or close_drawing with discard: true, takes it back). Exploding a MINSERT gives one copy only, so for a MINSERT use the separate-references route";

/// The ways to a complete picture when a build needs `total` block expansions and the limit is
/// `cap` per build, in words for the agent. Each is something the tools do today, and what does
/// not work is said. Windows are advised only when the largest single object fits the limit: a
/// build expands an object whole in every window that touches it.
pub fn ways_through(cap: usize, total: u64, kind: Through, census: &BlockCensus) -> Vec<String> {
    let mut ways = Vec::new();
    let largest = census.largest.as_ref();
    let too_big = largest.filter(|l| l.count > cap as u64);
    if let Some(l) = too_big {
        let (kind_text, h) = (l.kind, l.handle.hex());
        ways.push(format!(
            "windows and tiles cannot complete this: the {kind_text} {h} (block {}) alone needs {} expansions, more than the {cap} one build draws, and a build expands every object that reaches its window whole, so every window or tile that touches it hits the limit again",
            l.block,
            count_text(l.count)
        ));
        if l.count == COUNT_SATURATED {
            ways.push(
                "this reference nests blocks so deeply that it multiplies past what can be counted: opening it one level (explode) leaves references that still multiply past it, so no route draws all of it today. Reading is unaffected (below)".to_string(),
            );
        } else if l.kind.starts_with("array") {
            ways.push(format!(
                "make the copies separate references so windows can split them: {}",
                SEPARATE_REFERENCES.replace("To draw all of an array, make", "make")
            ));
        } else {
            ways.push(format!("open the reference so its parts can be split by windows: {}", EXPLODE_ONE.replace("HANDLE", &h)));
        }
        ways.push(
            "render_preview and export take only model space, a layout or a layout viewport as the space to draw: a block's own definition cannot be drawn as a space of its own".to_string(),
        );
    } else {
        let windows = total.div_ceil((cap as u64).max(1)).max(2);
        let how_many = if total == COUNT_SATURATED {
            "this drawing needs more than can be counted, so no number of windows is given".to_string()
        } else {
            format!("this drawing needs {total}, so use at least {windows} windows")
        };
        match kind {
            Through::Model => ways.push(format!(
                "draw it in windows: only objects whose extent reaches the window are expanded and the limit is per build, so a window (or tile) that needs {cap} or fewer is drawn whole. No single object needs more than {cap}, and {how_many}: render_preview with tiles {{cols, rows}} and output writes every tile to its own file (each tile's answer says its own blocks_expanded_total), or export svg / png with window [[x0, y0], [x1, y1]] once per region"
            )),
            Through::Layout => ways.push(format!(
                "a layout is built whole, so a window does not reduce it. Draw the model space its viewports show instead: render_preview with viewport: <handle> (read_drawing section \"layouts\" lists them) and tiles or a window, one build per tile; each needs {cap} or fewer to be whole. The layout needs {} in one build",
                count_text(total)
            )),
            Through::Pdf => ways.push(format!(
                "a PDF sheet is one build of the whole space and its window only places the drawing on the page, it does not leave out the block references outside it. For the regions of the drawing write SVG or PNG: export svg / png with window [[x0, y0], [x1, y1]] per region, or render_preview with tiles and output. {how_many}; the sheet needs {} in one build and {cap} is the most a build draws",
                count_text(total)
            )),
        }
    }
    if census.layers_off.is_empty() {
        if census.reference_layers.len() == 1 {
            ways.push(format!(
                "every block reference is on one layer ({}), so layers_off / only_layers cannot split the drawing into passes: switching that layer off leaves none of them",
                census.reference_layers.first().map_or("", String::as_str)
            ));
        }
    } else {
        let list = census
            .layers_off
            .iter()
            .map(|l| {
                let fits = if l.total_if_off <= cap as u64 { ", which fits one build" } else { "" };
                format!("{} leaves {}{fits}", l.layer, count_text(l.total_if_off))
            })
            .collect::<Vec<_>>()
            .join("; ");
        ways.push(format!(
            "switch off the layer the block references are on (layers_off / only_layers): with it off the count is {list}. Switching off the layers of the references' contents does not reduce the count (the build takes one expansion for each entity of a block before it looks at the entity's layer)"
        ));
        if census.reference_layers.len() == 1 {
            ways.push(format!(
                "every block reference is on one layer ({}), so layers cannot split the drawing into passes: switching it off leaves none of them",
                census.reference_layers.first().map_or("", String::as_str)
            ));
        }
    }
    ways.push(
        "reading is not affected by the limit: read_drawing (entities with expand_blocks, takeoff, text, dimensions), search_drawing, measuring and the extents do not expand blocks into a picture, so everything in the drawing can still be read, searched and measured".to_string(),
    );
    ways.push(format!(
        "one file of the whole drawing needs {} expansions in one build and {cap} is the most one build draws, so a complete picture comes as several files (windows, layer passes or opened-up references as above), not one",
        count_text(total)
    ));
    ways
}

/// What a PDF export's answer says about block contents, sheet by sheet: each sheet's expansions
/// asked for and not drawn (`sheets`), the sums of them, and when a sheet passed the limit which
/// sheets, the largest object and the ways through. The totals add up: `blocks_not_drawn` is the
/// sum of the sheets' (a sheet's total is the build's own count, or past the limit the count).
pub fn pdf_blocks_note(blocks: &[cadcraft_io::SheetBlocks]) -> Value {
    let sum = |f: &dyn Fn(&cadcraft_io::SheetBlocks) -> u64| blocks.iter().map(f).fold(0u64, |a, n| if a == COUNT_SATURATED || n == COUNT_SATURATED { COUNT_SATURATED } else { a.saturating_add(n) });
    let mut note = json!({"blocks_expanded_total": count_value(sum(&|b| b.total))});
    note["sheets"] = json!(blocks
        .iter()
        .map(|b| {
            let mut e = json!({"sheet": b.sheet, "blocks_expanded_total": count_value(b.total), "blocks_not_drawn": count_value(b.not_drawn())});
            if b.truncated() {
                e["blocks_truncated"] = json!(true);
                truncated_fields(&mut e, b.limit, &b.census);
            }
            clamped_fields(&mut e, &b.census);
            e
        })
        .collect::<Vec<_>>());
    let cut: Vec<&cadcraft_io::SheetBlocks> = blocks.iter().filter(|b| b.truncated()).collect();
    if let Some(worst) = cut.iter().max_by_key(|b| b.total) {
        let cap = worst.limit;
        let over = sum(&|b| b.not_drawn());
        note["blocks_truncated"] = json!(true);
        note["blocks_expanded"] = json!(cap);
        note["blocks_not_drawn"] = count_value(over);
        note["blocks_truncated_sheets"] = json!(cut.iter().map(|b| b.sheet.clone()).collect::<Vec<_>>());
        note["warning"] = json!(format!(
            "block contents past {cap} expanded entities were not drawn on the sheets listed: {} expansions are missing (sheets gives each sheet's own figures)",
            count_text(over)
        ));
        note["ways_through"] = json!(ways_through(cap, worst.total, Through::Pdf, &worst.census));
    }
    // Arrays drawn short on any sheet.
    let mut seen = std::collections::BTreeSet::new();
    let clamped: Vec<_> = blocks.iter().flat_map(|b| b.census.clamped.iter()).filter(|a| seen.insert(a.handle.0)).cloned().collect();
    clamped_fields(&mut note, &BlockCensus { clamped, ..BlockCensus::default() });
    note
}

pub fn prepare<'a>(d: &'a Drawing, a: &Value, ctx: &Ctx) -> Result<Prepared<'a>, ToolError> {
    let target = target(d, a)?;
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
        block_cap: cadcraft_engine::render::MAX_BLOCK_EXPANSION,
        layout_census: std::cell::OnceCell::new(),
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
pub fn png(p: &Prepared<'_>, window: &Bounds2, width: u32, height: Option<u32>, dark: bool, ctx: &Ctx) -> Result<(Vec<u8>, u32, u32, usize), ToolError> {
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
