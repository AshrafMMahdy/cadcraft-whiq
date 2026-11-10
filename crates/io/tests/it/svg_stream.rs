//! `svg::write_list` writes the same bytes as the string writer it replaced, to any writer, in
//! blocks, header first.

use std::io::Write;

use cadcraft_color::Rgb;
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DPrim, DisplayList, Kind};

use std::fmt::Write as _;

/// ` opacity="..."` for a transparent primitive (none for an opaque one or a wipeout's mask).
fn opacity(p: &DPrim) -> String {
    if p.kind == Kind::Mask || p.transparency == 0 {
        return String::new();
    }
    format!(r#" opacity="{:.2}""#, f64::from(p.alpha()) / 255.0)
}

/// The writer as it was before it wrote to a writer (copied verbatim, renamed): the bytes it
/// made are the ones the streaming writer must make.
fn reference_export_list(list: &DisplayList, window: Option<Bounds2>) -> String {
    let window = window.filter(|w| !w.is_empty());
    let b = window.unwrap_or(list.bounds);
    // A single point (or a zero-size window) has no extent: show a unit box centred on it.
    let point = !b.is_empty() && b.width().max(b.height()) <= 1e-9;
    let (w, h) = if b.is_empty() {
        (1.0, 1.0)
    } else if point {
        (0.0, 0.0)
    } else {
        (b.width().max(1e-9), b.height().max(1e-9))
    };
    let m = if point {
        0.5
    } else if window.is_some() {
        0.0
    } else {
        (w.max(h)) * 0.02
    };
    let (x0, y1) = if b.is_empty() { (0.0, 1.0) } else { (b.min.x - m, b.max.y + m) };
    let vw = w + 2.0 * m;
    let vh = h + 2.0 * m;
    let stroke = vw.max(vh) / 1500.0;
    // Decimals: at least 6, more when the stroke would otherwise lose its leading digits.
    let dp = (3.0 - stroke.log10().floor()).clamp(6.0, 15.0) as usize;
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {vw:.dp$} {vh:.dp$}" width="{:.0}" height="{:.0}"><rect width="100%" height="100%" fill="white"/>"#,
        1600.0,
        1600.0 * vh / vw
    );
    let white = Rgb(255, 255, 255);
    let tx = |x: f64| x - x0;
    let ty = |y: f64| y1 - y;
    for p in &list.prims {
        let c = p.display_rgb(white).hex();
        let op = opacity(p);
        let pts = list.points(p);
        match p.kind {
            Kind::Polyline => {
                let _ = write!(
                    s,
                    r#"<polyline fill="none" stroke="{c}"{op} stroke-width="{stroke:.dp$}" stroke-linecap="round" stroke-linejoin="round" points=""#
                );
                for q in pts {
                    let _ = write!(s, "{:.dp$},{:.dp$} ", tx(q.x), ty(q.y));
                }
                s.push_str(r#""/>"#);
            }
            Kind::Tris | Kind::Mask => {
                // A wipeout's mask hides what lies under it with the white background.
                let c = if p.kind == Kind::Mask { white.hex() } else { c };
                let _ = write!(s, r#"<path fill="{c}"{op} d=""#);
                for t in pts.chunks(3) {
                    if let [a, b2, cc] = t {
                        let _ =
                            write!(s, "M{:.dp$} {:.dp$}L{:.dp$} {:.dp$}L{:.dp$} {:.dp$}Z", tx(a.x), ty(a.y), tx(b2.x), ty(b2.y), tx(cc.x), ty(cc.y));
                    }
                }
                s.push_str(r#""/>"#);
            }
            Kind::Point => {
                if let Some(q) = pts.first() {
                    let _ = write!(s, r#"<circle cx="{:.dp$}" cy="{:.dp$}" r="{:.dp$}" fill="{c}"{op}/>"#, tx(q.x), ty(q.y), stroke);
                }
            }
            Kind::Infinite { ray } => {
                if let (Some(bp), Some(dir)) = (pts.first(), pts.get(1)) {
                    // A window can lie far from the base point: reach past it.
                    let far = (vw + vh) * 2.0 + if window.is_some() { bp.dist(b.center()) } else { 0.0 };
                    let a = if ray { *bp } else { *bp - *dir * far };
                    let e = *bp + *dir * far;
                    let _ = write!(
                        s,
                        r#"<line x1="{:.dp$}" y1="{:.dp$}" x2="{:.dp$}" y2="{:.dp$}" stroke="{c}"{op} stroke-width="{stroke:.dp$}"/>"#,
                        tx(a.x),
                        ty(a.y),
                        tx(e.x),
                        ty(e.y)
                    );
                }
            }
        }
    }
    s.push_str("</svg>");
    s
}

/// A list with every primitive kind the writer handles: opaque and transparent polylines (one
/// wide), filled triangles (opaque and transparent), a wipeout mask, points, a ray and an
/// infinite line, colour 7 and others, over enough polylines to cross the writer's flush size.
fn every_kind(n: usize) -> DisplayList {
    let mut l = DisplayList::default();
    let handle = cadcraft_doc::Handle(1);
    let prim = |l: &mut DisplayList, kind: Kind, rgb: Rgb, aci7: bool, lw: f32, transparency: u8, pts: &[Vec2]| {
        let tris = matches!(kind, Kind::Tris | Kind::Mask);
        let start = if tris { l.tris.len() } else { l.verts.len() } as u32;
        for q in pts {
            l.bounds.add(*q);
        }
        if tris { l.tris.extend_from_slice(pts) } else { l.verts.extend_from_slice(pts) }
        l.prims.push(DPrim { handle, color: rgb, aci7, lw, transparency, kind, start, len: pts.len() as u32 });
    };
    let v = Vec2::new;
    for k in 0..n {
        let x = k as f64 * 0.37;
        prim(&mut l, Kind::Polyline, Rgb(255, 0, 0), false, 0.0, 0, &[v(x, 0.0), v(x + 1.0, 2.5), v(x + 2.0, 0.125)]);
    }
    prim(&mut l, Kind::Polyline, Rgb(255, 255, 255), true, 0.5, 40, &[v(0.0, 0.0), v(5.5, 5.5)]);
    prim(&mut l, Kind::Tris, Rgb(0, 0, 255), false, 0.0, 0, &[v(0.0, 0.0), v(4.0, 0.0), v(0.0, 3.0), v(5.0, 5.0), v(6.0, 5.0), v(5.0, 6.0)]);
    prim(&mut l, Kind::Tris, Rgb(0, 128, 0), false, 0.0, 70, &[v(1.0, 1.0), v(2.0, 1.0), v(1.0, 2.0)]);
    prim(&mut l, Kind::Mask, Rgb(255, 255, 255), false, 0.0, 30, &[v(2.0, 2.0), v(3.0, 2.0), v(2.0, 3.0)]);
    prim(&mut l, Kind::Point, Rgb(10, 20, 30), false, 0.0, 0, &[v(3.25, 3.75)]);
    prim(&mut l, Kind::Point, Rgb(10, 20, 30), false, 0.0, 50, &[v(1.5, 1.5)]);
    prim(&mut l, Kind::Infinite { ray: true }, Rgb(9, 9, 9), false, 0.0, 0, &[v(1.0, 1.0), v(0.6, 0.8)]);
    prim(&mut l, Kind::Infinite { ray: false }, Rgb(9, 9, 9), false, 0.0, 20, &[v(2.0, 1.0), v(0.0, 1.0)]);
    l
}

/// A writer that keeps what it is given and each write's size.
#[derive(Default)]
struct Blocks {
    bytes: Vec<u8>,
    writes: Vec<usize>,
}

impl Write for Blocks {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        self.writes.push(buf.len());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_streaming_writer_makes_the_bytes_of_the_string_writer() {
    let windows = [None, Some(Bounds2::new(Vec2::new(1.0, 0.5), Vec2::new(4.0, 4.0))), Some(Bounds2::new(Vec2::new(2.0, 2.0), Vec2::new(2.0, 2.0)))];
    for n in [0, 3, 20_000] {
        let list = every_kind(n);
        for window in windows {
            let reference = reference_export_list(&list, window);
            let mut sink = Blocks::default();
            cadcraft_io::svg::write_list(&mut sink, &list, window).unwrap();
            assert!(sink.bytes == reference.as_bytes(), "{n} polylines, window {window:?}");
            assert!(cadcraft_io::svg::export_list(&list, window) == reference, "{n} polylines, window {window:?}");
            if n == 20_000 {
                // Sent in blocks, not as one string: the header alone goes first.
                assert!(sink.writes.len() > 4 && sink.writes.iter().all(|w| *w < reference.len() / 2), "{:?}", sink.writes.len());
                assert!(sink.bytes.starts_with(b"<svg xmlns"));
            }
        }
    }
    // An empty list has a header and no primitives.
    let empty = DisplayList::default();
    assert!(cadcraft_io::svg::export_list(&empty, None) == reference_export_list(&empty, None));
}

/// A writer that fails is an error to the caller (a full disk), not a panic or a short file said
/// to be whole.
#[test]
fn a_failing_writer_is_an_error() {
    struct Full(usize);
    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 < buf.len() {
                return Err(std::io::Error::other("disk full"));
            }
            self.0 -= buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let list = every_kind(20_000);
    let err = cadcraft_io::svg::write_list(Full(100_000), &list, None).unwrap_err();
    assert_eq!(err.to_string(), "disk full");
}
