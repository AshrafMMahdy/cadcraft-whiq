//! WHIQ: `render_preview`, the platform's Preview tab renderer: vector SVG of a space (model or a
//! layout, a layout viewport's model view), the whole extents or a window, in tiles for big
//! drawings, with layers switched on or off; or a PNG to look at. Every primitive carries its
//! layer (`<g data-layer>` runs, draw order kept), so a viewer can also hide layers itself.
//! Nothing is capped: a drawing too big for one answer comes back tile by tile
//! (`continue_from`), each tile holding only what it shows.

use std::fmt::Write as _;

use cadcraft_engine::doc::{Drawing, Entity, EntityKind, Handle, Space};
use cadcraft_engine::geom::{Bounds2, Vec2};
use cadcraft_engine::render::{DisplayList, Kind};
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

    /// The block expansions a build of `window` asks for (the arithmetic over the block table of
    /// [`cadcraft_engine::render::count_space`], before anything is built): in model space those of
    /// the objects that reach into the window, for a layout those of the whole layout.
    pub fn expansions(&self, window: &Bounds2) -> u64 {
        let d = self.drawing();
        match &self.target.space {
            Space::Model => {
                let reach = window.expand(window.width().max(window.height()) * 1e-6);
                cadcraft_engine::render::count_expansions(d, d.model.iter().map(|e| e.as_ref()).filter(|e| self.reaches(e, &reach)), false)
            }
            space => cadcraft_engine::render::count_space(d, space, false),
        }
    }

    /// What every answer that draws says about block contents: how many expansions the build
    /// asked for (`blocks_expanded_total`), and when that is past the limit what was not drawn and
    /// the ways through ([`Prepared::blocks_note_for`]).
    pub fn blocks_note(&self, window: &Bounds2) -> Option<Value> {
        self.blocks_note_for(self.expansions(window))
    }

    /// [`Prepared::blocks_note`] for a build that asked for `total` expansions.
    pub fn blocks_note_for(&self, total: u64) -> Option<Value> {
        let cap = self.block_cap;
        let mut note = json!({"blocks_expanded_total": total});
        let over = total.saturating_sub(cap as u64);
        if over > 0 {
            let layout = !matches!(self.target.space, Space::Model);
            note["blocks_truncated"] = json!(true);
            note["blocks_expanded"] = json!(cap);
            note["blocks_not_drawn"] = json!(over);
            note["warning"] = json!(format!(
                "block contents past {cap} expanded entities were not drawn: {over} of the {total} the drawing needs are missing from this picture"
            ));
            note["ways_through"] = json!(ways_through(cap, total, if layout { Through::Layout } else { Through::Model }));
        }
        Some(note)
    }

    /// Whether model-space `e` reaches into `reach` (rays and infinite lines always do).
    fn reaches(&self, e: &Entity, reach: &Bounds2) -> bool {
        matches!(e.kind, EntityKind::Ray(_) | EntityKind::XLine(_)) || {
            let b = cadcraft_engine::doc::extent_bounds(self.drawing(), e);
            b.is_empty() || b.intersects(reach)
        }
    }

    /// The display list of `window` drawn `width_px` wide: curves as fine as an eighth of a pixel
    /// (room to zoom in on the SVG), and in model space only the objects that reach into the
    /// window (infinite lines always).
    pub fn list_for(&self, window: &Bounds2, width_px: f64, ctx: &Ctx) -> Result<DisplayList, ToolError> {
        let d = self.drawing();
        let size = window.width().max(window.height());
        let opts = self.options(window, width_px, ctx);
        ctx.stage("building the display list", 0);
        let list = match &self.target.space {
            Space::Model => {
                let store = &d.model;
                let reach = window.expand(size * 1e-6);
                let mut picked = Vec::new();
                for (k, e) in store.iter().enumerate() {
                    if k % 65_536 == 0 {
                        ctx.check()?;
                    }
                    if self.reaches(e, &reach) {
                        picked.push(e.as_ref());
                    }
                }
                if picked.len() == store.len() {
                    cadcraft_engine::render::build(d, &Space::Model, &opts)
                } else {
                    cadcraft_engine::render::build_entities(d, picked, &opts)
                }
            }
            space => cadcraft_engine::render::build(d, space, &opts),
        };
        ctx.check()?;
        Ok(list)
    }

    /// What [`Prepared::list_for`] returns, handed to `each` in pieces drawn one after the other:
    /// model space in slices of [`SLICE_ENTITIES`] entities in their stored draw order (so
    /// overlaps and wipeouts draw as in the whole list), each built, handed over and dropped
    /// before the next; a layout, whose viewports and sheet need the whole build, in one piece.
    /// For a drawing too big to hold the display list of at once.
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
        if !matches!(self.target.space, Space::Model) {
            let list = self.list_for(window, width_px, ctx)?;
            each(&list)?;
            return Ok(list.expanded);
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

/// The ways to get the whole drawing when a build needs `total` block expansions and the limit is
/// `cap` per build, in words for the agent. Each is something the tools do today.
pub fn ways_through(cap: usize, total: u64, kind: Through) -> Vec<String> {
    let windows = total.div_ceil((cap as u64).max(1)).max(2);
    let mut ways = Vec::new();
    match kind {
        Through::Model => ways.push(format!(
            "draw it in windows: only objects whose extent reaches the window are expanded and the limit is per build, so a window (or tile) that needs {cap} or fewer is drawn whole. This drawing needs {total}, so use at least {windows} windows: render_preview with tiles {{cols, rows}} and output writes every tile to its own file (each tile's answer says its own blocks_expanded_total), or export svg / png with window [[x0, y0], [x1, y1]] once per region"
        )),
        Through::Layout => ways.push(format!(
            "a layout is built whole, so a window does not reduce it. Draw the model space its viewports show instead: render_preview with viewport: <handle> (read_drawing section \"layouts\" lists them) and tiles or a window, one build per tile; each needs {cap} or fewer to be whole. The layout needs {total} in one build"
        )),
        Through::Pdf => ways.push(format!(
            "a PDF sheet is one build of the whole space and its window only places the drawing on the page, it does not leave out the block references outside it. For the regions of the drawing write SVG or PNG: export svg / png with window [[x0, y0], [x1, y1]] per region, or render_preview with tiles and output. The sheet needs {total} in one build; {cap} is the most a build draws"
        )),
    }
    ways.push(
        "layers_off / layers_on / only_layers leave out the layers you do not need (block references on a switched-off layer are not expanded), e.g. the trees on one pass and the rest on another".to_string(),
    );
    ways.push(format!(
        "one file of the whole drawing needs {total} expansions in one build and {cap} is the most one build draws, so the whole drawing comes as several files (windows or layer passes), not one"
    ));
    ways
}

/// What a PDF export's answer says about block contents: the largest sheet's expansions asked for
/// and, when any sheet passed the limit, which sheets, how much was not drawn and the ways through.
pub fn pdf_blocks_note(blocks: &[cadcraft_io::SheetBlocks]) -> Value {
    let cap = cadcraft_engine::render::MAX_BLOCK_EXPANSION;
    let most = blocks.iter().map(|b| b.total).max().unwrap_or(0);
    let mut note = json!({"blocks_expanded_total": most});
    let cut: Vec<&cadcraft_io::SheetBlocks> = blocks.iter().filter(|b| b.truncated()).collect();
    if !cut.is_empty() {
        let over: u64 = cut.iter().map(|b| b.total - cap as u64).sum();
        note["blocks_truncated"] = json!(true);
        note["blocks_expanded"] = json!(cap);
        note["blocks_not_drawn"] = json!(over);
        note["blocks_truncated_sheets"] = json!(cut.iter().map(|b| b.sheet.clone()).collect::<Vec<_>>());
        note["warning"] = json!(format!("block contents past {cap} expanded entities were not drawn on the sheets listed: {over} expansions are missing"));
        note["ways_through"] = json!(ways_through(cap, most, Through::Pdf));
    }
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
    let view = cadcraft_engine::render::raster::View::of_window(window, Some(width), height);
    let (w, h) = (view.width, view.height);
    let list = p.list_for(window, f64::from(w) / 8.0, ctx)?;
    let mut opts = cadcraft_engine::render::raster::RasterOptions::default();
    if !dark {
        opts.background = cadcraft_engine::doc::color::Rgb(255, 255, 255);
    }
    let bytes = cadcraft_engine::render::raster::render_png(&list, &view, &opts)
        .ok_or_else(|| ToolError::new(code::FAILED, "the picture could not be drawn", "try a smaller width or a window"))?;
    Ok((bytes, w, h, list.expanded))
}
