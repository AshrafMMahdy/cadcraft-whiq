//! WHIQ: `read_drawing` and `search_drawing` — what an agent reads from a DWG or DXF: the
//! summary (units, extents, layouts, layers, blocks, entity counts), every text and dimension
//! value with its layer, space and position, the title blocks' attributes, entities, layers,
//! blocks, layouts and xrefs. Lists are windowed (`limit`, `continue_from`), so a huge drawing is
//! never dumped whole; filters narrow them (`space`, `layer`, `window`, `contains`, `type`).
//!
//! Texts inside block references are read too (with the block path and the reference's
//! transform applied), because construction drawings keep notes, tags and title blocks in blocks.

use std::collections::BTreeMap;

use cadcraft_engine::doc::{Drawing, Entity, EntityKind, EntityStore, Space};
use cadcraft_engine::geom::{Bounds2, Vec2, Vec3};
use serde_json::{Map, Value, json};

use super::contract::{ToolError, code};
use super::docs::{Ctx, Doc};

/// A 2-D similarity-ish transform (block references: rotate, scale, move).
#[derive(Clone, Copy, Debug)]
pub struct Xf {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    t: Vec2,
    /// Rotation added to angles (radians) and the scale applied to heights.
    pub rot: f64,
    pub scale: f64,
}

impl Xf {
    pub const ID: Xf = Xf { a: 1.0, b: 0.0, c: 0.0, d: 1.0, t: Vec2 { x: 0.0, y: 0.0 }, rot: 0.0, scale: 1.0 };
    pub fn apply(&self, p: Vec2) -> Vec2 {
        Vec2::new(self.a * p.x + self.b * p.y + self.t.x, self.c * p.x + self.d * p.y + self.t.y)
    }
    /// The determinant: areas scale by its magnitude, lengths by its square root.
    pub fn det(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    /// Whether lengths scale alike in every direction (no non-uniform block scaling).
    pub fn uniform(&self) -> bool {
        let (sx, sy) = ((self.a * self.a + self.c * self.c).sqrt(), (self.b * self.b + self.d * self.d).sqrt());
        (sx - sy).abs() <= 1e-9 * sx.max(sy).max(1e-300)
    }
    /// The transform of a block reference (insert, scale, rotation, block base) inside `self`.
    pub fn then_insert(&self, at: Vec3, scale: Vec3, rotation: f64, base: Vec3) -> Xf {
        let (s, c) = rotation.sin_cos();
        let (sx, sy) = (scale.x, scale.y);
        // inner(p) = at + R * S * (p - base)
        let (ia, ib, ic, id) = (c * sx, -s * sy, s * sx, c * sy);
        let it = Vec2::new(at.x - (ia * base.x + ib * base.y), at.y - (ic * base.x + id * base.y));
        let a = self.a * ia + self.b * ic;
        let b = self.a * ib + self.b * id;
        let cc = self.c * ia + self.d * ic;
        let d = self.c * ib + self.d * id;
        let t = self.apply(it);
        Xf { a, b, c: cc, d, t, rot: self.rot + rotation, scale: self.scale * sy.abs().max(1e-300) }
    }
}

/// One entity met while walking a space, with where it sits.
pub struct Visit<'a> {
    pub e: &'a Entity,
    pub space: &'a str,
    pub xf: Xf,
    /// Block names from the space down to this entity (empty: in the space itself).
    pub via: &'a [String],
    /// The outermost block reference this entity is reached through.
    pub top_insert: Option<cadcraft_engine::doc::Handle>,
    /// The layer it shows on (an entity on layer 0 inside a block takes the reference's).
    pub layer: &'a str,
}

/// The spaces of a drawing: "Model" then each layout by tab order.
pub fn spaces(d: &Drawing) -> Vec<(String, Space)> {
    let mut v = vec![("Model".to_string(), Space::Model)];
    let mut ls: Vec<_> = d.layouts.iter().collect();
    ls.sort_by_key(|l| l.tab_order);
    v.extend(ls.into_iter().map(|l| (l.name.clone(), Space::Paper(l.name.clone()))));
    v
}

/// The spaces a `space` argument names: "all" (default), "model", "paper" (every layout), a
/// layout name, or a list of them.
pub fn spaces_named(d: &Drawing, arg: Option<&Value>) -> Result<Vec<(String, Space)>, ToolError> {
    let all = spaces(d);
    let names: Vec<String> = match arg {
        None | Some(Value::Null) => return Ok(all),
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(_) => {
            return Err(ToolError::new(
                code::INVALID_ARGUMENT,
                "`space` must be a name or a list of names",
                "use \"model\", \"paper\", \"all\" or a layout name",
            ));
        }
    };
    let mut out: Vec<(String, Space)> = Vec::new();
    for n in names {
        let low = n.to_ascii_lowercase();
        let pick: Vec<(String, Space)> = match low.as_str() {
            "all" | "*" => all.clone(),
            "model" | "modelspace" | "model space" | "ms" => all.iter().filter(|(_, s)| !s.is_paper()).cloned().collect(),
            "paper" | "layouts" | "paperspace" | "paper space" | "ps" => all.iter().filter(|(_, s)| s.is_paper()).cloned().collect(),
            _ => all.iter().filter(|(name, _)| name.eq_ignore_ascii_case(&n)).cloned().collect(),
        };
        if pick.is_empty() {
            let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
            return Err(ToolError::new(
                code::NOT_FOUND,
                format!("there is no space or layout `{n}`"),
                "use \"model\", \"paper\", \"all\" or one of the layouts in `closest`",
            )
            .with("closest", json!(super::contract::closest(&n, names.iter().copied(), 5)))
            .with("layouts", json!(names)));
        }
        for p in pick {
            if !out.iter().any(|(x, _)| *x == p.0) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// Walk `store` and, when `expand`, the blocks it references (bounded depth), calling `f`.
/// `f` returns false to stop.
pub fn walk<'a>(
    d: &'a Drawing,
    store: &'a EntityStore,
    space: &'a str,
    expand: bool,
    ctx: &Ctx,
    f: &mut dyn FnMut(&Visit<'_>) -> bool,
) -> Result<(), ToolError> {
    let mut n = 0u64;
    walk_in(d, store, space, Xf::ID, &mut Vec::new(), None, "0", expand, ctx, &mut n, f).map(|_| ())
}

#[allow(clippy::too_many_arguments)]
fn walk_in<'a>(
    d: &'a Drawing,
    store: &'a EntityStore,
    space: &'a str,
    xf: Xf,
    via: &mut Vec<String>,
    top: Option<cadcraft_engine::doc::Handle>,
    parent_layer: &str,
    expand: bool,
    ctx: &Ctx,
    n: &mut u64,
    f: &mut dyn FnMut(&Visit<'_>) -> bool,
) -> Result<bool, ToolError> {
    for e in store.iter() {
        *n += 1;
        if (*n).is_multiple_of(4096) {
            ctx.check()?;
        }
        let layer = if !via.is_empty() && e.common.layer == "0" { parent_layer.to_string() } else { e.common.layer.clone() };
        let v = Visit { e, space, xf, via, top_insert: top, layer: &layer };
        if !f(&v) {
            return Ok(false);
        }
        if expand
            && let EntityKind::Insert(i) = &e.kind
            && via.len() < cadcraft_engine::doc::MAX_BLOCK_DEPTH
            && !via.iter().any(|b| b.eq_ignore_ascii_case(&i.block))
            && let Some(b) = d.block(&i.block)
        {
            let inner = xf.then_insert(i.insert, i.scale, i.rotation, b.base);
            via.push(b.name.clone());
            let go = walk_in(d, &b.entities, space, inner, via, top.or(Some(e.handle)), &layer, expand, ctx, n, f)?;
            via.pop();
            if !go {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn pt(p: Vec2) -> Value {
    let r = |x: f64| if x.is_finite() { (x * 1e6).round() / 1e6 } else { 0.0 };
    json!([r(p.x), r(p.y)])
}

fn round(x: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    if x.is_finite() { (x * m).round() / m } else { 0.0 }
}

/// Filters every list section takes.
pub struct Filter {
    pub layers: Option<Vec<String>>,
    pub window: Option<Bounds2>,
    pub contains: Option<String>,
    pub exact: bool,
    pub types: Option<Vec<String>>,
    pub expand_blocks: bool,
}

pub fn window_arg(v: Option<&Value>) -> Result<Option<Bounds2>, ToolError> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(None) };
    let num = |x: &Value| x.as_f64().filter(|f| f.is_finite());
    let corners: Option<(Vec2, Vec2)> = match v.as_array().map(Vec::as_slice) {
        Some([a, b]) if a.is_array() => {
            let p = |q: &Value| -> Option<Vec2> { Some(Vec2::new(num(q.get(0)?)?, num(q.get(1)?)?)) };
            p(a).zip(p(b))
        }
        Some([x0, y0, x1, y1]) => num(x0).zip(num(y0)).zip(num(x1).zip(num(y1))).map(|((a, b), (c, d))| (Vec2::new(a, b), Vec2::new(c, d))),
        _ => None,
    };
    match corners {
        Some((a, b)) => Ok(Some(Bounds2::new(a, b))),
        None => Err(ToolError::new(
            code::INVALID_ARGUMENT,
            "`window` must be [[x0, y0], [x1, y1]] (or [x0, y0, x1, y1])",
            "give two corners in drawing units of the space read",
        )),
    }
}

fn list_arg(v: Option<&Value>) -> Option<Vec<String>> {
    match v? {
        Value::String(s) => Some(s.split(',').map(|x| x.trim().to_ascii_lowercase()).filter(|x| !x.is_empty()).collect()),
        Value::Array(a) => Some(a.iter().filter_map(Value::as_str).map(|x| x.trim().to_ascii_lowercase()).collect()),
        _ => None,
    }
    .filter(|v: &Vec<String>| !v.is_empty())
}

impl Filter {
    pub fn from_args(a: &Value) -> Result<Filter, ToolError> {
        Ok(Filter {
            layers: list_arg(a.get("layer").or(a.get("layers"))),
            window: window_arg(a.get("window"))?,
            contains: a.get("contains").and_then(Value::as_str).map(str::to_lowercase).filter(|s| !s.is_empty()),
            exact: a.get("match").and_then(Value::as_str).is_some_and(|m| m.eq_ignore_ascii_case("exact")),
            types: list_arg(a.get("type").or(a.get("types"))),
            expand_blocks: a.get("expand_blocks").and_then(Value::as_bool).unwrap_or(true),
        })
    }
    fn layer_ok(&self, l: &str) -> bool {
        self.layers.as_ref().is_none_or(|ls| ls.iter().any(|x| x.eq_ignore_ascii_case(l)))
    }
    fn at_ok(&self, p: Vec2) -> bool {
        self.window.as_ref().is_none_or(|w| w.contains(p))
    }
    fn text_ok(&self, s: &str) -> bool {
        match &self.contains {
            None => true,
            Some(c) if self.exact => s.trim().to_lowercase() == *c,
            Some(c) => s.to_lowercase().contains(c.as_str()),
        }
    }
}

/// `items[from..from+limit]` with the continuation fields every craft app uses.
pub fn page(key: &str, items: Vec<Value>, from: usize, limit: usize) -> Map<String, Value> {
    let total = items.len();
    let part: Vec<Value> = items.into_iter().skip(from).take(limit).collect();
    let next = from.saturating_add(part.len());
    let mut o = Map::new();
    o.insert("total".into(), json!(total));
    o.insert("returned".into(), json!(part.len()));
    o.insert("offset".into(), json!(from));
    o.insert(key.into(), Value::Array(part));
    if next < total {
        o.insert("truncated".into(), json!(true));
        o.insert("continue_from".into(), json!(next));
        o.insert(
            "next_step".into(),
            json!(format!("call again with continue_from: {next} for the next {key} (or narrow with space, layer, window, contains)")),
        );
    }
    o
}

/// Every readable text: TEXT, MTEXT (formatting removed), attributes, attribute definitions,
/// multileaders, table cells and (with `dims`) dimension texts.
pub fn texts(d: &Drawing, sp: &[(String, Space)], flt: &Filter, ctx: &Ctx) -> Result<Vec<Value>, ToolError> {
    let mut out = Vec::new();
    for (name, space) in sp {
        let Some(store) = d.space(space) else { continue };
        walk(d, store, name, flt.expand_blocks, ctx, &mut |v| {
            for (ty, text, at, h, rot, style, tag) in text_items(v.e) {
                let at = v.xf.apply(at);
                if text.trim().is_empty() || !flt.layer_ok(v.layer) || !flt.at_ok(at) || !flt.text_ok(&text) {
                    continue;
                }
                if flt.types.as_ref().is_some_and(|t| !t.iter().any(|x| x.eq_ignore_ascii_case(ty))) {
                    continue;
                }
                // An attribute definition inside a referenced block shows as the reference's attribute.
                if ty == "AttDef" && !v.via.is_empty() {
                    continue;
                }
                let mut o = json!({
                    "handle": v.e.handle.hex(), "type": ty, "text": text.trim(), "layer": v.layer, "space": v.space,
                    "at": pt(at), "height": round(h * v.xf.scale, 6), "rotation": round((rot + v.xf.rot).to_degrees(), 4), "style": style,
                });
                if let Some(t) = tag {
                    o["tag"] = json!(t);
                }
                if !v.via.is_empty() {
                    o["block"] = json!(v.via.join(" > "));
                    o["insert"] = json!(v.top_insert.map(|h| h.hex()));
                }
                out.push(o);
            }
            true
        })?;
    }
    Ok(out)
}

pub type TextItem = (&'static str, String, Vec2, f64, f64, String, Option<String>);

pub fn text_items(e: &Entity) -> Vec<TextItem> {
    match &e.kind {
        EntityKind::Text(t) => vec![("Text", t.value.clone(), t.justify_point(), t.height, t.rotation, t.style.clone(), None)],
        EntityKind::MText(t) => {
            vec![("MText", cadcraft_engine::fonts::plain_mtext(&t.contents), t.insert.xy(), t.height, t.rotation, t.style.clone(), None)]
        }
        EntityKind::AttDef(a) => {
            vec![("AttDef", a.text.value.clone(), a.text.justify_point(), a.text.height, a.text.rotation, a.text.style.clone(), Some(a.tag.clone()))]
        }
        EntityKind::Insert(i) => i
            .attribs
            .iter()
            .filter(|a| !a.invisible)
            .map(|a| {
                ("Attribute", a.text.value.clone(), a.text.justify_point(), a.text.height, a.text.rotation, a.text.style.clone(), Some(a.tag.clone()))
            })
            .collect(),
        EntityKind::MLeader(m) => m
            .text
            .iter()
            .map(|t| ("MLeader", cadcraft_engine::fonts::plain_mtext(&t.contents), t.insert.xy(), t.height, t.rotation, t.style.clone(), None))
            .collect(),
        EntityKind::Table(t) => {
            let mut v = Vec::new();
            let mut y = 0.0;
            for (r, row) in t.cells.iter().enumerate() {
                let mut x = 0.0;
                for (c, cell) in row.iter().enumerate() {
                    if !cell.text.trim().is_empty() {
                        let at = Vec2::new(t.insert.x + x, t.insert.y - y);
                        v.push((
                            "TableCell",
                            cadcraft_engine::fonts::plain_mtext(&cell.text),
                            at,
                            t.text_height,
                            0.0,
                            t.style.clone(),
                            Some(format!("R{}C{}", r + 1, c + 1)),
                        ));
                    }
                    x += t.col_widths.get(c).copied().unwrap_or(0.0);
                }
                y += t.row_heights.get(r).copied().unwrap_or(0.0);
            }
            v
        }
        _ => Vec::new(),
    }
}

fn dim_kind(k: &cadcraft_engine::doc::DimKind) -> &'static str {
    use cadcraft_engine::doc::DimKind as K;
    match k {
        K::Linear { rotation } if rotation.abs() < 1e-9 => "linear (horizontal)",
        K::Linear { rotation } if (rotation.abs() - std::f64::consts::FRAC_PI_2).abs() < 1e-9 => "linear (vertical)",
        K::Linear { .. } => "linear (rotated)",
        K::Aligned => "aligned",
        K::Angular => "angular",
        K::Angular3P => "angular (3 points)",
        K::Diameter => "diameter",
        K::Radius => "radius",
        K::Ordinate { x_type: true } => "ordinate (x)",
        K::Ordinate { .. } => "ordinate (y)",
        K::ArcLength => "arc length",
    }
}

/// Every dimension: kind, the text it shows, the measured value, its layer, space and position.
pub fn dimensions(d: &Drawing, sp: &[(String, Space)], flt: &Filter, ctx: &Ctx) -> Result<Vec<Value>, ToolError> {
    let mut out = Vec::new();
    for (name, space) in sp {
        let Some(store) = d.space(space) else { continue };
        walk(d, store, name, flt.expand_blocks, ctx, &mut |v| {
            let EntityKind::Dimension(dm) = &v.e.kind else { return true };
            let at = v.xf.apply(dm.text_mid.xy());
            if !flt.layer_ok(v.layer) || !flt.at_ok(at) {
                return true;
            }
            let g = cadcraft_engine::render::dimension_in(d, dm);
            let shown = g.value.trim().to_string();
            if !flt.text_ok(&shown) {
                return true;
            }
            use cadcraft_engine::doc::DimKind as K;
            let angular = matches!(dm.kind, K::Angular | K::Angular3P);
            let measured = if angular { g.measurement.to_degrees() } else { g.measurement * v.xf.scale };
            let mut o = json!({
                "handle": v.e.handle.hex(), "kind": dim_kind(&dm.kind), "text": shown,
                "measurement": round(measured, 8), "unit": if angular { "degrees" } else { "drawing units" },
                "overridden": !dm.text.is_empty() && !dm.text.contains("<>"),
                "layer": v.layer, "space": v.space, "at": pt(at), "style": dm.style,
                "points": {"defpt": pt(v.xf.apply(dm.defpt.xy())), "p13": pt(v.xf.apply(dm.p13.xy())), "p14": pt(v.xf.apply(dm.p14.xy()))},
            });
            if !v.via.is_empty() {
                o["block"] = json!(v.via.join(" > "));
            }
            out.push(o);
            true
        })?;
    }
    Ok(out)
}

/// Words a title block's attribute tags and block names carry.
const TB_TAGS: &[&str] = &[
    "TITLE", "DWG", "DRAWING", "SHEET", "SCALE", "DATE", "REV", "DRAWN", "CHECK", "APPROV", "PROJECT", "CLIENT", "JOB", "NUMBER", "NO", "DESIGN",
    "OWNER", "CONTRACT", "PHASE", "STATUS", "SIZE",
];
const TB_NAMES: &[&str] = &["title", "tb", "border", "sheet", "frame", "cartouche", "a0", "a1", "a2", "a3", "a4", "ansi", "arch"];

/// Title blocks: block references with attributes (scored: in a layout, title-like tags and
/// block names), each with all its attribute values and the fixed texts of its block; plus the
/// texts written straight on each layout sheet (a title block drawn without attributes).
pub fn title_blocks(d: &Drawing, sp: &[(String, Space)], ctx: &Ctx) -> Result<Value, ToolError> {
    let mut cands = Vec::new();
    let mut sheet_texts = Vec::new();
    for (name, space) in sp {
        let Some(store) = d.space(space) else { continue };
        for (k, e) in store.iter().enumerate() {
            if k % 4096 == 0 {
                ctx.check()?;
            }
            match &e.kind {
                EntityKind::Insert(i) => {
                    let block = d.block(&i.block);
                    let attdefs = block.map(|b| b.entities.iter().filter(|x| matches!(x.kind, EntityKind::AttDef(_))).count()).unwrap_or(0);
                    if i.attribs.is_empty() && attdefs == 0 {
                        continue;
                    }
                    let mut score = 0i32;
                    if space.is_paper() {
                        score += 3;
                    }
                    let lname = i.block.to_ascii_lowercase();
                    if TB_NAMES.iter().any(|w| lname.contains(w)) {
                        score += 2;
                    }
                    let mut attrs = Map::new();
                    for a in &i.attribs {
                        let tag = a.tag.to_ascii_uppercase();
                        if TB_TAGS.iter().any(|w| tag.contains(w)) {
                            score += 1;
                        }
                        attrs.insert(a.tag.clone(), json!(a.text.value));
                    }
                    if i.attribs.len() >= 4 {
                        score += 1;
                    }
                    let mut fixed = Vec::new();
                    if let Some(b) = block {
                        let xf = Xf::ID.then_insert(i.insert, i.scale, i.rotation, b.base);
                        for x in b.entities.iter() {
                            for (ty, text, at, h, _, _, _) in text_items(x) {
                                if ty != "AttDef" && !text.trim().is_empty() {
                                    fixed.push(json!({"text": text.trim(), "at": pt(xf.apply(at)), "height": round(h * xf.scale, 6)}));
                                }
                            }
                        }
                    }
                    cands.push(json!({
                        "handle": e.handle.hex(), "block": i.block, "space": name, "layer": e.common.layer, "at": pt(i.insert.xy()),
                        "scale": [round(i.scale.x, 6), round(i.scale.y, 6)], "rotation": round(i.rotation.to_degrees(), 4),
                        "score": score, "likely": score >= 4, "attributes": attrs, "block_texts": fixed,
                    }));
                }
                _ if space.is_paper() => {
                    for (ty, text, at, h, _, _, _) in text_items(e) {
                        if !text.trim().is_empty() {
                            sheet_texts.push(json!({"handle": e.handle.hex(), "type": ty, "text": text.trim(), "space": name, "layer": e.common.layer, "at": pt(at), "height": round(h, 6)}));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    cands.sort_by(|a, b| b["score"].as_i64().cmp(&a["score"].as_i64()));
    Ok(json!({"candidates": cands, "sheet_texts": sheet_texts}))
}

/// The entities of the spaces (block contents with `expand_blocks: true`), filtered.
pub fn entities(d: &Drawing, sp: &[(String, Space)], flt: &Filter, geometry: bool, ctx: &Ctx) -> Result<Vec<Value>, ToolError> {
    let mut out = Vec::new();
    let expand = flt.expand_blocks && flt.types.is_some();
    for (name, space) in sp {
        let Some(store) = d.space(space) else { continue };
        walk(d, store, name, expand, ctx, &mut |v| {
            let ty = v.e.kind.type_name();
            if !flt.layer_ok(v.layer)
                || flt.types.as_ref().is_some_and(|t| !t.iter().any(|x| x.eq_ignore_ascii_case(ty) || x.eq_ignore_ascii_case(v.e.kind.dxf_name())))
            {
                return true;
            }
            let b = cadcraft_engine::doc::extent_bounds(d, v.e);
            let bw = if b.is_empty() {
                None
            } else {
                Some(Bounds2::from_points([
                    v.xf.apply(b.min),
                    v.xf.apply(b.max),
                    v.xf.apply(Vec2::new(b.min.x, b.max.y)),
                    v.xf.apply(Vec2::new(b.max.x, b.min.y)),
                ]))
            };
            if let (Some(w), Some(bb)) = (&flt.window, &bw)
                && !w.intersects(bb)
            {
                return true;
            }
            let mut o = json!({"handle": v.e.handle.hex(), "type": ty, "dxf": v.e.kind.dxf_name(), "layer": v.layer, "space": v.space, "color": v.e.common.color.name(),
                "bounds": bw.map(|b| json!([pt(b.min), pt(b.max)]))});
            if let EntityKind::Insert(i) = &v.e.kind {
                o["block"] = json!(i.block);
            }
            if !v.via.is_empty() {
                o["in_block"] = json!(v.via.join(" > "));
            }
            let (line, measures) = super::takeoff::entity_line(v.e, &v.xf);
            o["summary"] = json!(line);
            for (k, m) in measures {
                o[k] = m;
            }
            if geometry {
                o["geometry"] = serde_json::to_value(&v.e.kind).unwrap_or(Value::Null);
            }
            out.push(o);
            true
        })?;
    }
    Ok(out)
}

/// Entity counts per space and type (top level), and per layer over every space and block.
pub fn counts(d: &Drawing) -> (BTreeMap<String, BTreeMap<String, usize>>, BTreeMap<String, usize>) {
    let mut per_space = BTreeMap::new();
    let mut per_layer: BTreeMap<String, usize> = BTreeMap::new();
    for (name, space) in spaces(d) {
        let mut c: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(store) = d.space(&space) {
            for e in store.iter() {
                *c.entry(e.kind.type_name().to_string()).or_default() += 1;
                *per_layer.entry(e.common.layer.to_ascii_lowercase()).or_default() += 1;
            }
        }
        per_space.insert(name, c);
    }
    for b in d.blocks.values() {
        for e in b.entities.iter() {
            *per_layer.entry(e.common.layer.to_ascii_lowercase()).or_default() += 1;
        }
    }
    (per_space, per_layer)
}

const INSUNITS: &[&str] = &[
    "unitless",
    "inches",
    "feet",
    "miles",
    "millimeters",
    "centimeters",
    "meters",
    "kilometers",
    "microinches",
    "mils",
    "yards",
    "angstroms",
    "nanometers",
    "microns",
    "decimeters",
    "decameters",
    "hectometers",
    "gigameters",
    "astronomical units",
    "light years",
    "parsecs",
    "US survey feet",
    "US survey inches",
    "US survey yards",
    "US survey miles",
];
const LUNITS: &[&str] = &["", "scientific", "decimal", "engineering", "architectural", "fractional", "Windows desktop"];

pub fn version_name(code: &str) -> &'static str {
    match code {
        "AC1006" => "R10",
        "AC1009" => "R11/R12",
        "AC1012" => "R13",
        "AC1014" => "R14",
        "AC1015" => "2000",
        "AC1018" => "2004",
        "AC1021" => "2007",
        "AC1024" => "2010",
        "AC1027" => "2013",
        "AC1032" => "2018",
        _ => "unknown",
    }
}

fn ext_value(b: &Bounds2) -> Value {
    if b.is_empty() { Value::Null } else { json!([pt(b.min), pt(b.max)]) }
}

fn insert_counts(d: &Drawing) -> BTreeMap<String, usize> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |store: &EntityStore| {
        for e in store.iter() {
            if let EntityKind::Insert(i) = &e.kind {
                *m.entry(i.block.to_ascii_lowercase()).or_default() += 1;
            }
        }
    };
    for (_, s) in spaces(d) {
        if let Some(st) = d.space(&s) {
            add(st);
        }
    }
    for b in d.blocks.values() {
        add(&b.entities);
    }
    m
}

/// The drawing summary.
pub fn summary(doc: &Doc, ctx: &Ctx) -> Result<Value, ToolError> {
    let d = doc.drawing()?;
    let h = &d.header;
    let ins = usize::try_from(h.i64("INSUNITS", 0)).unwrap_or(0);
    let lu = usize::try_from(h.i64("LUNITS", 2)).unwrap_or(2);
    let (per_space, _) = counts(d);
    let mut texts_n = 0usize;
    let mut dims_n = 0usize;
    let mut layouts = Vec::new();
    for (name, space) in spaces(d) {
        let store = d.space(&space);
        let n = store.map_or(0, EntityStore::len);
        if let Some(st) = store {
            for e in st.iter() {
                match &e.kind {
                    EntityKind::Text(_) | EntityKind::MText(_) | EntityKind::MLeader(_) => texts_n += 1,
                    EntityKind::Insert(i) => texts_n += i.attribs.len(),
                    EntityKind::Dimension(_) => dims_n += 1,
                    _ => {}
                }
            }
        }
        let ext = d.extents(&space);
        let mut l = json!({"name": name, "entities": n, "extents": ext_value(&ext)});
        if let Space::Paper(ln) = &space
            && let Some(lay) = d.layout(ln)
        {
            let vps = lay.entities.iter().filter(|e| matches!(e.kind, EntityKind::Viewport(_))).count();
            l["tab_order"] = json!(lay.tab_order);
            l["paper"] = json!({"name": lay.page.paper, "width_mm": round(lay.page.width_mm, 3), "height_mm": round(lay.page.height_mm, 3), "landscape": lay.page.landscape});
            l["viewports"] = json!(vps);
            l["active"] = json!(d.active_layout.as_deref() == Some(ln.as_str()));
        }
        layouts.push(l);
    }
    ctx.check()?;
    let user_blocks: Vec<&String> = d.blocks.iter().filter(|(k, b)| !k.starts_with('*') && !b.anonymous).map(|(k, _)| k).collect();
    let xrefs: Vec<Value> = d
        .blocks
        .values()
        .filter(|b| b.xref_path.is_some())
        .map(|b| json!({"name": b.name, "path": b.xref_path, "overlay": b.xref_overlay, "status": b.xref_status}))
        .collect();
    let tb = title_blocks(d, &spaces(d), ctx)?;
    let likely: Vec<Value> = tb["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["likely"] == true)
        .map(|c| json!({"block": c["block"], "space": c["space"], "handle": c["handle"]}))
        .collect();
    let version = doc.version.clone().or_else(|| h.get("ACADVER").and_then(cadcraft_engine::doc::HVal::as_str).map(str::to_string));
    Ok(json!({
        "document": doc.handle,
        "name": doc.name(),
        "file": {"format": doc.format, "version": version, "release": version.as_deref().map(version_name), "bytes": doc.source_bytes},
        "units": {
            "insunits": ins, "insunits_name": INSUNITS.get(ins).copied().unwrap_or("unknown"),
            "measurement": if h.i64("MEASUREMENT", 0) == 1 { "metric" } else { "imperial" },
            "lunits": lu, "lunits_name": LUNITS.get(lu).copied().unwrap_or("unknown"), "luprec": h.i64("LUPREC", 4),
            "aunits": h.i64("AUNITS", 0), "dimscale": h.f64("DIMSCALE", 1.0), "ltscale": h.f64("LTSCALE", 1.0),
        },
        "entities": d.entity_count(),
        "counts": per_space,
        "texts": texts_n,
        "dimensions": dims_n,
        "layouts": layouts,
        "layers": {"count": d.layers.len(), "off": d.layers.iter().filter(|l| !l.on).count(), "frozen": d.layers.iter().filter(|l| l.frozen).count(), "names": if d.layers.len() <= 100 { json!(d.layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>()) } else { json!("more than 100: read_drawing section \"layers\" lists them") }},
        "blocks": {"count": user_blocks.len(), "anonymous": d.blocks.len() - user_blocks.len()},
        "xrefs": xrefs,
        "title_blocks": likely,
        "styles": {"text": d.text_styles.len(), "dimension": d.dim_styles.len(), "linetypes": d.linetypes.len()},
        "read_notes": d.read_notes,
        "skipped_records": d.skipped_records,
        "next_step": "read_drawing section text | dimensions | title_block | layers | blocks | layouts | entities for the details; render_preview to look at a space",
    }))
}

pub fn layers(d: &Drawing) -> Vec<Value> {
    let (_, per_layer) = counts(d);
    let mut by_type: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for (_, space) in spaces(d) {
        for e in d.space(&space).into_iter().flat_map(|st| st.iter()) {
            *by_type.entry(e.common.layer.to_ascii_lowercase()).or_default().entry(e.kind.dxf_name().to_string()).or_default() += 1;
        }
    }
    d.layers
        .iter()
        .map(|l| {
            json!({"name": l.name, "on": l.on, "frozen": l.frozen, "locked": l.locked, "plot": l.plot, "color": l.color.name(), "linetype": l.linetype,
                "lineweight": serde_json::to_value(l.lineweight).unwrap_or(Value::Null), "transparency": l.transparency, "description": l.description,
                "entities": per_layer.get(&l.name.to_ascii_lowercase()).copied().unwrap_or(0),
                "by_type": by_type.get(&l.name.to_ascii_lowercase()).cloned().unwrap_or_default()})
        })
        .collect()
}

pub fn blocks(d: &Drawing, anonymous: bool) -> Vec<Value> {
    let refs = insert_counts(d);
    d.blocks
        .values()
        .filter(|b| anonymous || (!b.anonymous && !b.name.starts_with('*')))
        .map(|b| {
            let attdefs: Vec<Value> = b
                .entities
                .iter()
                .filter_map(|e| match &e.kind {
                    EntityKind::AttDef(a) => Some(json!({"tag": a.tag, "prompt": a.prompt, "default": a.text.value, "invisible": a.invisible, "constant": a.constant})),
                    _ => None,
                })
                .collect();
            json!({"name": b.name, "entities": b.entities.len(), "base": pt(b.base.xy()), "references": refs.get(&b.name.to_ascii_lowercase()).copied().unwrap_or(0),
                "attributes": attdefs, "xref_path": b.xref_path, "anonymous": b.anonymous, "description": b.description})
        })
        .collect()
}

pub fn layouts(d: &Drawing) -> Vec<Value> {
    spaces(d)
        .into_iter()
        .map(|(name, space)| {
            let mut v = json!({"name": name, "extents": ext_value(&d.extents(&space)), "entities": d.space(&space).map_or(0, EntityStore::len)});
            if let Space::Paper(ln) = &space
                && let Some(l) = d.layout(ln)
            {
                let vps: Vec<Value> = l
                    .entities
                    .iter()
                    .filter_map(|e| match &e.kind {
                        EntityKind::Viewport(vp) => Some(json!({
                            "handle": e.handle.hex(), "id": vp.id, "center": pt(vp.center.xy()), "width": round(vp.width, 6), "height": round(vp.height, 6),
                            "view_center": pt(vp.view_center), "view_height": round(vp.view_height, 6),
                            "scale": if vp.view_height.abs() > 1e-12 { round(vp.height / vp.view_height, 9) } else { 0.0 },
                            "on": vp.on, "locked": vp.locked, "frozen_layers": vp.frozen_layers, "layer": e.common.layer,
                        })),
                        _ => None,
                    })
                    .collect();
                v["tab_order"] = json!(l.tab_order);
                v["page"] = serde_json::to_value(&l.page).unwrap_or(Value::Null);
                v["viewports"] = json!(vps);
                v["active"] = json!(d.active_layout.as_deref() == Some(ln.as_str()));
            }
            v
        })
        .collect()
}

/// `search_drawing`: every text, attribute (tag or value), dimension text, block name, layer
/// name and layout name that matches, with the entity, its layer, its space and position.
pub fn search(
    d: &Drawing,
    sp: &[(String, Space)],
    query: &str,
    exact: bool,
    kinds: Option<&[String]>,
    flt: &Filter,
    ctx: &Ctx,
) -> Result<Vec<Value>, ToolError> {
    let q = query.to_lowercase();
    let hit = |s: &str| if exact { s.trim().to_lowercase() == q } else { s.to_lowercase().contains(q.as_str()) };
    let want = |k: &str| kinds.is_none_or(|ks| ks.iter().any(|x| x.eq_ignore_ascii_case(k)));
    let mut out = Vec::new();
    if want("text") || want("attribute") || want("tag") || want("dimension") || want("block_reference") {
        for (name, space) in sp {
            let Some(store) = d.space(space) else { continue };
            walk(d, store, name, flt.expand_blocks, ctx, &mut |v| {
                if !flt.layer_ok(v.layer) {
                    return true;
                }
                for (ty, text, at, _, _, _, tag) in text_items(v.e) {
                    let at = v.xf.apply(at);
                    if !flt.at_ok(at) || (ty == "AttDef" && !v.via.is_empty()) {
                        continue;
                    }
                    let is_attr = ty == "Attribute" || ty == "AttDef";
                    let value_hit = hit(&text) && (if is_attr { want("attribute") } else { want("text") });
                    let tag_hit = is_attr && want("tag") && tag.as_deref().is_some_and(&hit);
                    if value_hit || tag_hit {
                        let mut o = json!({"match": if tag_hit && !value_hit { "tag" } else { "value" }, "kind": ty, "text": text.trim(), "handle": v.e.handle.hex(),
                            "layer": v.layer, "space": v.space, "at": pt(at)});
                        if let Some(t) = &tag {
                            o["tag"] = json!(t);
                        }
                        if !v.via.is_empty() {
                            o["block"] = json!(v.via.join(" > "));
                            o["insert"] = json!(v.top_insert.map(|h| h.hex()));
                        }
                        out.push(o);
                    }
                }
                match &v.e.kind {
                    EntityKind::Dimension(dm) if want("dimension") => {
                        let g = cadcraft_engine::render::dimension_in(d, dm);
                        let at = v.xf.apply(dm.text_mid.xy());
                        if hit(&g.value) && flt.at_ok(at) {
                            out.push(json!({"match": "value", "kind": "Dimension", "text": g.value.trim(), "handle": v.e.handle.hex(), "layer": v.layer, "space": v.space, "at": pt(at)}));
                        }
                    }
                    EntityKind::Insert(i) if want("block_reference") && hit(&i.block) => {
                        let at = v.xf.apply(i.insert.xy());
                        if flt.at_ok(at) {
                            out.push(json!({"match": "block name", "kind": "Block Reference", "text": i.block, "handle": v.e.handle.hex(), "layer": v.layer, "space": v.space, "at": pt(at)}));
                        }
                    }
                    _ => {}
                }
                true
            })?;
        }
    }
    if want("layer") {
        for l in d.layers.iter().filter(|l| hit(&l.name)) {
            out.push(json!({"match": "layer name", "kind": "Layer", "text": l.name, "on": l.on, "frozen": l.frozen}));
        }
    }
    if want("block") {
        let refs = insert_counts(d);
        for b in d.blocks.values().filter(|b| !b.anonymous && hit(&b.name)) {
            out.push(json!({"match": "block name", "kind": "Block", "text": b.name, "references": refs.get(&b.name.to_ascii_lowercase()).copied().unwrap_or(0)}));
        }
    }
    if want("layout") {
        for (n, _) in spaces(d).iter().filter(|(n, _)| hit(n)) {
            out.push(json!({"match": "layout name", "kind": "Layout", "text": n}));
        }
    }
    Ok(out)
}

/// Markdown for an agent: a heading and a table of the list's main columns.
pub fn markdown(section: &str, v: &Value) -> String {
    let mut s = String::new();
    let list_key = ["texts", "dimensions", "entities", "layers", "blocks", "layouts", "hits", "candidates"]
        .into_iter()
        .find(|k| v.get(*k).is_some_and(Value::is_array));
    if section == "summary" {
        s.push_str(&format!("# {}\n\n", v["name"].as_str().unwrap_or("Drawing")));
        s.push_str(&format!(
            "- File: {} {} ({})\n- Units: {} ({}, {})\n- Entities: {} · texts {} · dimensions {}\n- Layers: {} · blocks {} · xrefs {}\n",
            v["file"]["format"].as_str().unwrap_or(""),
            v["file"]["version"].as_str().unwrap_or(""),
            v["file"]["release"].as_str().unwrap_or(""),
            v["units"]["insunits_name"].as_str().unwrap_or(""),
            v["units"]["measurement"].as_str().unwrap_or(""),
            v["units"]["lunits_name"].as_str().unwrap_or(""),
            v["entities"],
            v["texts"],
            v["dimensions"],
            v["layers"]["count"],
            v["blocks"]["count"],
            v["xrefs"].as_array().map_or(0, Vec::len),
        ));
        s.push_str("\n| Space | Entities | Viewports | Paper | Extents |\n|---|---:|---:|---|---|\n");
        for l in v["layouts"].as_array().into_iter().flatten() {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                l["name"].as_str().unwrap_or(""),
                l["entities"],
                l.get("viewports").unwrap_or(&json!("")),
                l["paper"]["name"].as_str().unwrap_or(""),
                l["extents"]
            ));
        }
        return s;
    }
    if section == "takeoff" || section == "quantities" || section == "take_off" {
        let unit = v["unit"].as_str().unwrap_or("units");
        s.push_str(&format!("## Take-off (drawing units: {unit}; areas {unit}²; HATCH out of the totals)\n\n| layer | length | perimeter | area | HATCH area | blocks | circles | texts | dims |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|\n"));
        let row = |name: &str, b: &Value| {
            let blocks: u64 = b["counts"]["blocks"].as_object().map_or(0, |m| m.values().filter_map(Value::as_u64).sum());
            format!(
                "| {name} | {} | {} | {} | {} | {blocks} | {} | {} | {} |\n",
                b["lengths"]["total"].as_f64().unwrap_or(0.0),
                b["perimeters"]["total"].as_f64().unwrap_or(0.0),
                b["areas"]["total"].as_f64().unwrap_or(0.0),
                b["areas"]["HATCH"].as_f64().unwrap_or(0.0),
                b["counts"]["CIRCLE"],
                b["counts"]["text"],
                b["counts"]["dimensions"]
            )
        };
        for l in v["by_layer"].as_array().into_iter().flatten() {
            s.push_str(&row(l["layer"].as_str().unwrap_or(""), l));
        }
        s.push_str(&row("**total**", &v["totals"]));
        if let Some(b) = v["totals"]["counts"]["blocks"].as_object().filter(|b| !b.is_empty()) {
            s.push_str("\n| block | references |\n|---|---:|\n");
            for (k, n) in b {
                s.push_str(&format!("| {} | {n} |\n", k.replace('|', "\\|")));
            }
        }
        if let Some(n) = v.get("continue_from") {
            s.push_str(&format!("\nMore layers: call again with continue_from: {n}\n"));
        }
        return s;
    }
    let Some(key) = list_key else { return serde_json::to_string_pretty(v).unwrap_or_default() };
    let cols: &[&str] = match key {
        "texts" => &["text", "type", "tag", "layer", "space", "at", "height", "block"],
        "dimensions" => &["text", "measurement", "kind", "layer", "space", "at"],
        "entities" => &["handle", "type", "layer", "space", "bounds"],
        "layers" => &["name", "on", "frozen", "locked", "color", "linetype", "entities"],
        "blocks" => &["name", "entities", "references", "attributes"],
        "layouts" => &["name", "entities", "viewports", "extents"],
        "hits" => &["text", "match", "kind", "tag", "layer", "space", "at", "block"],
        _ => &["block", "space", "score", "likely", "attributes"],
    };
    s.push_str(&format!("## {section} ({} of {})\n\n|", v[key].as_array().map_or(0, Vec::len), v.get("total").unwrap_or(&json!("?"))));
    for c in cols {
        s.push_str(&format!(" {c} |"));
    }
    s.push_str("\n|");
    for _ in cols {
        s.push_str("---|");
    }
    s.push('\n');
    for row in v[key].as_array().into_iter().flatten() {
        s.push('|');
        for c in cols {
            let cell = match &row[*c] {
                Value::Null => String::new(),
                Value::String(x) => x.replace('|', "\\|").replace('\n', " "),
                Value::Array(a) if *c == "attributes" => a.len().to_string(),
                other => other.to_string().replace('|', "\\|"),
            };
            s.push_str(&format!(" {cell} |"));
        }
        s.push('\n');
    }
    if let Some(n) = v.get("continue_from") {
        s.push_str(&format!("\nMore: call again with continue_from: {n}\n"));
    }
    s
}
