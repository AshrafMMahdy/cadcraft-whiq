//! WHIQ: quantity take-off (`read_drawing` section `takeoff`) and the drawing's scale hints.
//!
//! Per layer and in total, in DRAWING UNITS (never converted): open lengths, closed perimeters
//! and areas (by DXF type; HATCH areas and perimeters listed but left out of each `total`, as a
//! hatch usually fills an outline already counted), block reference counts by block name
//! (MINSERT rows × columns), and circle, text and dimension counts. The figures are those the
//! platform's former `read_dxf` take-off gave, with block contents measured too (read inside
//! references by default; `expand_blocks: false` counts references only, as `read_dxf` did).
//!
//! Scale hints ($DIMSCALE, $LTSCALE, $CANNOSCALE and "1:100"-style texts) are hints only: a
//! drawing's model space is full size in its own units.

use std::collections::BTreeMap;

use cadcraft_engine::doc::{Drawing, Entity, EntityKind, EntityStore, Space};
use cadcraft_engine::geom::{Bounds2, PolyVertex, Polyline, TAU, Vec2, Vec3};
use serde_json::{Map, Value, json};

use super::contract::ToolError;
use super::docs::Ctx;
use super::read::{Filter, Xf, spaces, text_items};

/// One take-off bucket (a layer, or the totals).
#[derive(Default)]
pub struct Bucket {
    lengths: BTreeMap<String, f64>,
    perimeters: BTreeMap<String, f64>,
    areas: BTreeMap<String, f64>,
    blocks: BTreeMap<String, u64>,
    circles: u64,
    text: u64,
    dimensions: u64,
    closed: u64,
    open: u64,
}

fn r3(x: f64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    let v = (x * 1000.0).round() / 1000.0;
    if v == 0.0 { 0.0 } else { v }
}

fn rec(m: &BTreeMap<String, f64>) -> Value {
    let mut o = Map::new();
    let mut total = 0.0;
    for (k, v) in m {
        if k != "HATCH" {
            total += v;
        }
        o.insert(k.clone(), json!(r3(*v)));
    }
    if !m.is_empty() {
        o.insert("total".into(), json!(r3(total)));
    }
    Value::Object(o)
}

impl Bucket {
    fn to_value(&self) -> Value {
        json!({
            "lengths": rec(&self.lengths), "perimeters": rec(&self.perimeters), "areas": rec(&self.areas),
            "counts": {"blocks": self.blocks, "CIRCLE": self.circles, "text": self.text, "dimensions": self.dimensions,
                "closed_shapes": self.closed, "open_shapes": self.open},
        })
    }
}

/// What one object measures.
enum Measure {
    Open { length: f64 },
    Closed { perimeter: f64, area: f64 },
    None,
}

/// A vertex chain closed by its flag, or by its first and last vertex meeting (the duplicate
/// closing vertex dropped), as `read_dxf` decided.
fn close_chain(mut v: Vec<PolyVertex>, flagged: bool) -> (Vec<PolyVertex>, bool) {
    if v.len() < 2 {
        return (v, false);
    }
    let span = v.iter().fold(1.0_f64, |s, p| s.max(p.p.x.abs()).max(p.p.y.abs()));
    let (a, b) = (v[0].p, v[v.len() - 1].p);
    if a.dist(b) <= 1e-9 * span && v.len() > 2 {
        v.pop();
        return (v, true);
    }
    (v, flagged)
}

fn ccw_sweep(a0: f64, a1: f64) -> f64 {
    let mut s = (a1 - a0) % TAU;
    if s <= 1e-12 {
        s += TAU;
    }
    s
}

/// Area of nested loops by the hatch style: 0 parity (islands subtracted), 1 outer (outer
/// minus the first islands), 2 ignore (outer only).
fn hatch_area(loops: &[Polyline], style: u8) -> (f64, f64) {
    let areas: Vec<f64> = loops.iter().map(|l| l.area().abs()).collect();
    let polys: Vec<Vec<Vec2>> = loops.iter().map(|l| l.tessellate(1e-6_f64.max(l.bounds().width().max(l.bounds().height()) * 1e-7))).collect();
    let mut area = 0.0;
    let mut perimeter = 0.0;
    for (i, l) in loops.iter().enumerate() {
        perimeter += l.len();
        let a = areas.get(i).copied().unwrap_or(0.0);
        let probe = polys.get(i).and_then(|p| p.first().copied());
        let depth = probe.map_or(0, |q| {
            polys
                .iter()
                .enumerate()
                .filter(|(j, o)| *j != i && areas.get(*j).copied().unwrap_or(0.0) > a && cadcraft_engine::geom::point_in_polygon(o, q))
                .count()
        });
        match style {
            2 => {
                if depth == 0 {
                    area += a;
                }
            }
            1 => {
                if depth == 0 {
                    area += a;
                } else if depth == 1 {
                    area -= a;
                }
            }
            _ => area += if depth % 2 == 0 { a } else { -a },
        }
    }
    (area.max(0.0), perimeter)
}

/// The measure of one object in its own coordinates, and a note when it is approximated.
fn measure(e: &Entity) -> (Measure, Option<&'static str>) {
    match &e.kind {
        EntityKind::Line(l) => {
            let d = l.b - l.a;
            (Measure::Open { length: (d.x * d.x + d.y * d.y + d.z * d.z).sqrt() }, None)
        }
        EntityKind::LwPolyline(p) => {
            let verts: Vec<PolyVertex> = p.vertices.iter().filter(|v| v.p.x.is_finite() && v.p.y.is_finite()).cloned().collect();
            if verts.len() < 2 {
                return (Measure::None, None);
            }
            let (v, closed) = close_chain(verts, p.closed);
            let g = Polyline { vertices: v, closed };
            if closed { (Measure::Closed { perimeter: g.len(), area: g.area().abs() }, None) } else { (Measure::Open { length: g.len() }, None) }
        }
        EntityKind::Polyline3d(p) => {
            let mut pts: Vec<Vec3> = p.points.iter().filter(|q| q.x.is_finite() && q.y.is_finite()).copied().collect();
            if pts.len() < 2 {
                return (Measure::None, None);
            }
            let span = pts.iter().fold(1.0_f64, |s, q| s.max(q.x.abs()).max(q.y.abs()));
            let mut closed = p.closed;
            if pts.len() > 2 && pts[0].xy().dist(pts[pts.len() - 1].xy()) <= 1e-9 * span {
                pts.pop();
                closed = true;
            }
            let n = pts.len();
            let segs = if closed { n } else { n - 1 };
            let mut len = 0.0;
            let mut twice = 0.0;
            for i in 0..segs {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let d = b - a;
                len += (d.x * d.x + d.y * d.y + d.z * d.z).sqrt();
                if closed {
                    twice += a.x * b.y - a.y * b.x;
                }
            }
            if closed { (Measure::Closed { perimeter: len, area: (twice / 2.0).abs() }, None) } else { (Measure::Open { length: len }, None) }
        }
        EntityKind::Circle(c) => (Measure::Closed { perimeter: TAU * c.radius, area: std::f64::consts::PI * c.radius * c.radius }, None),
        EntityKind::Arc(a) => (Measure::Open { length: a.radius * ccw_sweep(a.start, a.end) }, None),
        EntityKind::Ellipse(el) => {
            let g = cadcraft_engine::geom::Ellipse { center: el.center.xy(), major: el.major.xy(), ratio: el.ratio, start: el.start, end: el.end };
            let a = el.major.xy().len();
            if g.is_full() {
                (Measure::Closed { perimeter: g.length(), area: std::f64::consts::PI * a * a * el.ratio.abs() }, None)
            } else {
                (Measure::Open { length: g.length() }, None)
            }
        }
        EntityKind::Spline(s) => {
            let b = Bounds2::from_points(s.control.iter().copied());
            let tol = (b.width().max(b.height()) * 1e-7).max(1e-9);
            let pts = s.tessellate(tol);
            if pts.len() < 2 {
                return (Measure::None, None);
            }
            let (v, closed) = close_chain(pts.iter().map(|p| PolyVertex::new(*p)).collect(), s.closed);
            let g = Polyline { vertices: v, closed };
            let note = Some("SPLINE lengths/areas from the curve sampled to 1e-7 of its size");
            if closed { (Measure::Closed { perimeter: g.len(), area: g.area().abs() }, note) } else { (Measure::Open { length: g.len() }, note) }
        }
        _ => (Measure::None, None),
    }
}

/// The take-off of the spaces asked for.
pub struct Takeoff {
    pub totals: Bucket,
    pub by_layer: BTreeMap<String, (String, Bucket)>,
    pub notes: Vec<String>,
    pub references_read: u64,
    pub unparsed_hatches: u64,
}

struct Walk<'a> {
    d: &'a Drawing,
    flt: &'a Filter,
    crossing: bool,
    blocks: Option<Vec<String>>,
    out: Takeoff,
    approx: std::collections::BTreeSet<&'static str>,
    nonuniform: bool,
    n: u64,
}

impl Walk<'_> {
    fn bucket(&mut self, layer: &str) -> &mut Bucket {
        &mut self.out.by_layer.entry(layer.to_ascii_lowercase()).or_insert_with(|| (layer.to_string(), Bucket::default())).1
    }
    fn both(&mut self, layer: &str, f: impl Fn(&mut Bucket)) {
        f(&mut self.out.totals);
        f(self.bucket(layer));
    }
    fn in_window(&self, e: &Entity, xf: &Xf) -> bool {
        let Some(w) = &self.flt.window else { return true };
        let b = cadcraft_engine::doc::extent_bounds(self.d, e);
        if b.is_empty() {
            return false;
        }
        let bb =
            Bounds2::from_points([xf.apply(b.min), xf.apply(b.max), xf.apply(Vec2::new(b.min.x, b.max.y)), xf.apply(Vec2::new(b.max.x, b.min.y))]);
        if self.crossing { w.intersects(&bb) } else { w.contains_box(&bb) }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        store: &EntityStore,
        xf: Xf,
        mult: f64,
        parent_layer: &str,
        depth: usize,
        via: &mut Vec<String>,
        expand: bool,
        ctx: &Ctx,
    ) -> Result<(), ToolError> {
        let d = self.d;
        for e in store.iter() {
            self.n += 1;
            if self.n.is_multiple_of(4096) {
                ctx.check()?;
            }
            let layer = if depth > 0 && e.common.layer == "0" { parent_layer.to_string() } else { e.common.layer.clone() };
            let layer_ok = self.flt.layers.as_ref().is_none_or(|ls| ls.iter().any(|x| x.eq_ignore_ascii_case(&layer)));
            match &e.kind {
                EntityKind::Insert(i) => {
                    let named = self.blocks.as_ref().is_none_or(|bs| bs.iter().any(|b| b.eq_ignore_ascii_case(&i.block)));
                    if !named {
                        continue;
                    }
                    let copies = f64::from(i.cols.max(1)) * f64::from(i.rows.max(1));
                    if layer_ok && self.in_window(e, &xf) {
                        let n = (copies * mult).round() as u64;
                        let name = i.block.clone();
                        self.both(&layer, |b| *b.blocks.entry(name.clone()).or_default() += n);
                    }
                    if expand
                        && depth < cadcraft_engine::doc::MAX_BLOCK_DEPTH
                        && !via.iter().any(|b| b.eq_ignore_ascii_case(&i.block))
                        && let Some(blk) = d.block(&i.block)
                    {
                        self.out.references_read += 1;
                        let inner = xf.then_insert(i.insert, i.scale, i.rotation, blk.base);
                        if !inner.uniform() {
                            self.nonuniform = true;
                        }
                        via.push(blk.name.clone());
                        self.walk(&blk.entities, inner, mult * copies, &layer, depth + 1, via, expand, ctx)?;
                        via.pop();
                    }
                }
                _ if !layer_ok || !self.in_window(e, &xf) => {}
                EntityKind::Text(_) | EntityKind::MText(_) => {
                    let n = mult.round() as u64;
                    self.both(&layer, |b| b.text += n);
                }
                EntityKind::Dimension(_) => {
                    let n = mult.round() as u64;
                    self.both(&layer, |b| b.dimensions += n);
                }
                EntityKind::Hatch(h) => {
                    let loops: Vec<Polyline> = h.loops.iter().map(|l| Polyline { vertices: l.vertices.clone(), closed: true }).collect();
                    if loops.is_empty() {
                        self.out.unparsed_hatches += 1;
                        continue;
                    }
                    let (a, p) = hatch_area(&loops, h.style);
                    let (fa, fl) = (xf.det().abs() * mult, xf.det().abs().sqrt() * mult);
                    self.both(&layer, |b| {
                        *b.areas.entry("HATCH".into()).or_default() += a * fa;
                        *b.perimeters.entry("HATCH".into()).or_default() += p * fl;
                    });
                }
                _ => {
                    let (m, note) = measure(e);
                    if let Some(n) = note {
                        self.approx.insert(n);
                    }
                    let (fa, fl) = (xf.det().abs(), xf.det().abs().sqrt());
                    let key = e.kind.dxf_name().to_string();
                    let circle = matches!(e.kind, EntityKind::Circle(_));
                    let count = mult.round() as u64;
                    match m {
                        Measure::Open { length } => self.both(&layer, |b| {
                            *b.lengths.entry(key.clone()).or_default() += length * fl * mult;
                            b.open += count;
                        }),
                        Measure::Closed { perimeter, area } => self.both(&layer, |b| {
                            *b.perimeters.entry(key.clone()).or_default() += perimeter * fl * mult;
                            *b.areas.entry(key.clone()).or_default() += area * fa * mult;
                            b.closed += count;
                            if circle {
                                b.circles += count;
                            }
                        }),
                        Measure::None => {}
                    }
                }
            }
        }
        Ok(())
    }
}

/// The take-off of `sp`.
pub fn takeoff(
    d: &Drawing,
    sp: &[(String, Space)],
    flt: &Filter,
    crossing: bool,
    blocks: Option<Vec<String>>,
    ctx: &Ctx,
) -> Result<Takeoff, ToolError> {
    let mut w = Walk {
        d,
        flt,
        crossing,
        blocks,
        out: Takeoff { totals: Bucket::default(), by_layer: BTreeMap::new(), notes: Vec::new(), references_read: 0, unparsed_hatches: 0 },
        approx: std::collections::BTreeSet::new(),
        nonuniform: false,
        n: 0,
    };
    for (_, space) in sp {
        if let Some(store) = d.space(space) {
            w.walk(store, Xf::ID, 1.0, "0", 0, &mut Vec::new(), flt.expand_blocks, ctx)?;
        }
    }
    let mut out = w.out;
    out.notes.extend(w.approx.iter().map(|n| format!("Approximated — {n}.")));
    if w.nonuniform {
        out.notes.push("Some block references scale unevenly (x ≠ y): lengths inside them are scaled by the square root of the area factor (exact for straight lines along the axes only).".into());
    }
    Ok(out)
}

impl Takeoff {
    pub fn totals_value(&self) -> Value {
        self.totals.to_value()
    }
    pub fn layer_values(&self) -> Vec<Value> {
        self.by_layer
            .values()
            .map(|(name, b)| {
                let mut v = b.to_value();
                v["layer"] = json!(name);
                v
            })
            .collect()
    }
}

/// `1:100`, `1 : 50`, `1:12.5` and `1/4"=1'-0"` inside a text (the `read_dxf` patterns).
pub fn scale_matches(s: &str) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let digit = |i: usize| c.get(i).is_some_and(char::is_ascii_digit);
    let word = |i: usize| c.get(i).is_some_and(|x| x.is_alphanumeric() || *x == '_');
    let skip_ws = |mut i: usize| {
        while c.get(i).is_some_and(|x| x.is_whitespace()) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    while i < c.len() {
        // 1 : N[.N]
        if c[i] == '1' && (i == 0 || !word(i - 1)) {
            let j = skip_ws(i + 1);
            if c.get(j) == Some(&':') {
                let mut k = skip_ws(j + 1);
                if digit(k) {
                    while digit(k) {
                        k += 1;
                    }
                    if matches!(c.get(k), Some('.') | Some(',')) && digit(k + 1) {
                        k += 1;
                        while digit(k) {
                            k += 1;
                        }
                    }
                    if !word(k) {
                        out.push(c[i..k].iter().filter(|x| !x.is_whitespace()).collect());
                        i = k;
                        continue;
                    }
                }
            }
        }
        // N/N" = 1'-0"
        if digit(i) && (i == 0 || !word(i - 1)) {
            let mut k = i;
            while digit(k) {
                k += 1;
            }
            if c.get(k) == Some(&'/') && digit(k + 1) {
                k += 1;
                while digit(k) {
                    k += 1;
                }
                let mut m = k;
                if c.get(m) == Some(&'"') {
                    m = skip_ws(m + 1);
                    if c.get(m) == Some(&'=') {
                        m = skip_ws(m + 1);
                        if c.get(m) == Some(&'1') && c.get(m + 1) == Some(&'\'') {
                            m = skip_ws(m + 2);
                            if c.get(m) == Some(&'-') {
                                m = skip_ws(m + 1);
                            }
                            if c.get(m) == Some(&'0') && c.get(m + 1) == Some(&'"') {
                                m += 2;
                                out.push(c[i..m].iter().filter(|x| !x.is_whitespace()).collect());
                                i = m;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// The scale hints: header variables and the first 20 "1:100"-style texts (every space, every
/// layer, attributes and texts inside blocks included — a title block's scale lives there).
pub fn scale_hint(d: &Drawing, ctx: &Ctx) -> Result<Value, ToolError> {
    let h = &d.header;
    let num = |k: &str| h.get(k).map(|_| r3(h.f64(k, 1.0)));
    let canno = h.get("CANNOSCALE").and_then(cadcraft_engine::doc::HVal::as_str).map(str::to_string);
    let mut matches = Vec::new();
    for (name, space) in spaces(d) {
        let Some(store) = d.space(&space) else { continue };
        super::read::walk(d, store, &name, true, ctx, &mut |v| {
            for (_, text, _, _, _, _, _) in text_items(v.e) {
                for m in scale_matches(&text) {
                    if matches.len() < 20 {
                        matches.push(json!({"match": m, "text": text.chars().take(120).collect::<String>(), "layer": v.layer, "space": v.space}));
                    }
                }
            }
            if let EntityKind::Dimension(dm) = &v.e.kind
                && !dm.text.trim().is_empty()
            {
                for m in scale_matches(&cadcraft_engine::fonts::plain_mtext(&dm.text)) {
                    if matches.len() < 20 {
                        matches.push(json!({"match": m, "text": dm.text.chars().take(120).collect::<String>(), "layer": v.layer, "space": v.space}));
                    }
                }
            }
            matches.len() < 20
        })?;
        if matches.len() >= 20 {
            break;
        }
    }
    Ok(json!({
        "dimscale": num("DIMSCALE"), "ltscale": num("LTSCALE"), "cannoscale": canno, "text_matches": matches,
        "note": "Hints only — $DIMSCALE/$LTSCALE are annotation/linetype multipliers, not a proven paper scale; a \"1:100\" in text is what the drawing CLAIMS. Model-space quantities are already full size in drawing units.",
    }))
}

/// The one-line summary and measures of an entity (in the coordinates `xf` maps to).
pub fn entity_line(e: &Entity, xf: &Xf) -> (String, Map<String, Value>) {
    let (m, _) = measure(e);
    let (fa, fl) = (xf.det().abs(), xf.det().abs().sqrt());
    let mut o = Map::new();
    let ty = e.kind.dxf_name();
    let s = match (&e.kind, &m) {
        (EntityKind::Circle(c), _) => format!("CIRCLE r={} area {}", r3(c.radius * fl), r3(std::f64::consts::PI * c.radius * c.radius * fa)),
        (EntityKind::Text(_) | EntityKind::MText(_), _) => {
            let t = text_items(e).into_iter().next().map(|x| x.1).unwrap_or_default();
            o.insert("text".into(), json!(t.chars().take(300).collect::<String>()));
            format!("{ty} \"{}\"", t.chars().take(80).collect::<String>())
        }
        (EntityKind::Insert(i), _) => {
            let n = u64::from(i.cols.max(1)) * u64::from(i.rows.max(1));
            format!(
                "INSERT {} rot {}° scale {}×{}{}",
                i.block,
                r3(i.rotation.to_degrees()),
                r3(i.scale.x),
                r3(i.scale.y),
                if n > 1 { format!(" ×{n} (MINSERT)") } else { String::new() }
            )
        }
        (EntityKind::Dimension(dm), _) => format!("DIMENSION measured {}", r3(dm.measurement)),
        (EntityKind::Hatch(h), _) => {
            let loops: Vec<Polyline> = h.loops.iter().map(|l| Polyline { vertices: l.vertices.clone(), closed: true }).collect();
            let (a, p) = hatch_area(&loops, h.style);
            o.insert("area".into(), json!(r3(a * fa)));
            o.insert("perimeter".into(), json!(r3(p * fl)));
            o.insert("closed".into(), json!(true));
            format!("HATCH {}{}, {} path(s), area {}", h.pattern, if h.solid { " solid" } else { "" }, h.loops.len(), r3(a * fa))
        }
        (_, Measure::Open { length }) => format!("{ty} open, length {}", r3(length * fl)),
        (_, Measure::Closed { perimeter, area }) => format!("{ty} closed, perimeter {}, area {}", r3(perimeter * fl), r3(area * fa)),
        _ => ty.to_string(),
    };
    match m {
        Measure::Open { length } => {
            o.insert("length".into(), json!(r3(length * fl)));
            o.insert("closed".into(), json!(false));
        }
        Measure::Closed { perimeter, area } => {
            o.insert("perimeter".into(), json!(r3(perimeter * fl)));
            o.insert("area".into(), json!(r3(area * fa)));
            o.insert("closed".into(), json!(true));
        }
        Measure::None => {}
    }
    (s, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_texts_are_found() {
        assert_eq!(scale_matches("SCALE 1:100"), vec!["1:100"]);
        assert_eq!(scale_matches("1 : 50 and 1:12.5"), vec!["1:50", "1:12.5"]);
        assert_eq!(scale_matches("1/4\" = 1'-0\""), vec!["1/4\"=1'-0\""]);
        assert!(scale_matches("11:30 am").is_empty());
        assert!(scale_matches("A1:B2").is_empty());
    }

    #[test]
    fn chains_close_by_flag_or_meeting_ends() {
        let v = |pts: &[(f64, f64)]| pts.iter().map(|(x, y)| PolyVertex::new(Vec2::new(*x, *y))).collect::<Vec<_>>();
        let (c, closed) = close_chain(v(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]), false);
        assert!(closed && c.len() == 3);
        assert!(!close_chain(v(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]), false).1);
    }

    #[test]
    fn hatch_islands_are_subtracted() {
        let sq = |s: f64, o: f64| Polyline::from_points(&[Vec2::new(o, o), Vec2::new(o + s, o), Vec2::new(o + s, o + s), Vec2::new(o, o + s)], true);
        let loops = [sq(10.0, 0.0), sq(2.0, 4.0)];
        assert!((hatch_area(&loops, 0).0 - 96.0).abs() < 1e-9);
        assert!((hatch_area(&loops, 2).0 - 100.0).abs() < 1e-9);
        assert!((hatch_area(&loops, 0).1 - 48.0).abs() < 1e-9);
    }
}
