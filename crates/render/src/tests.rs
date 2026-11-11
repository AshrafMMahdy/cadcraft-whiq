use crate::raster::{RasterOptions, View, render};
use crate::*;
use cadcraft_doc::{Common, Drawing, EntityKind, Hatch, HatchLoop, Space};
use cadcraft_geom::{PolyVertex, Vec3};

fn sample() -> Drawing {
    let mut d = Drawing::new_imperial();
    d.add(&Space::Model, Common::default(), EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(0.0, 0.0, 0.0), b: Vec3::new(10.0, 0.0, 0.0) }))
        .unwrap();
    d.add(
        &Space::Model,
        Common { color: Color::Index(1), ..Default::default() },
        EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(5.0, 5.0, 0.0), radius: 3.0 }),
    )
    .unwrap();
    d
}

#[test]
fn builds_lines_and_circles() {
    let d = sample();
    let l = build(&d, &Space::Model, &Options { tolerance: 0.01, ..Default::default() });
    assert_eq!(l.prims.len(), 2);
    assert!(l.prims[1].len > 20);
    assert_eq!(l.prims[1].color, Rgb(255, 0, 0));
}

#[test]
fn linetype_dashes_line() {
    let mut d = sample();
    d.linetypes.extend(cadcraft_doc::library::standard_linetypes());
    d.layer_mut("0").unwrap().linetype = "DASHED".into();
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.len() > 10);
}

#[test]
fn frozen_layer_hidden() {
    let mut d = sample();
    d.layer_mut("0").unwrap().frozen = true;
    assert!(build(&d, &Space::Model, &Options::default()).prims.is_empty());
}

#[test]
fn text_produces_strokes() {
    let mut d = Drawing::new_imperial();
    d.add(
        &Space::Model,
        Common::default(),
        EntityKind::Text(cadcraft_doc::Text {
            insert: Vec3::ZERO,
            align_pt: None,
            height: 1.0,
            value: "CAD".into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: Default::default(),
            valign: Default::default(),
        }),
    )
    .unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.len() >= 3);
}

#[test]
fn hatch_pattern_and_solid() {
    let mut d = Drawing::new_imperial();
    let sq = vec![
        PolyVertex::new(Vec2::new(0.0, 0.0)),
        PolyVertex::new(Vec2::new(4.0, 0.0)),
        PolyVertex::new(Vec2::new(4.0, 4.0)),
        PolyVertex::new(Vec2::new(0.0, 4.0)),
    ];
    let h = Hatch {
        pattern: "ANSI31".into(),
        solid: false,
        loops: vec![HatchLoop { vertices: sq.clone(), outer: true, sources: Vec::new() }],
        scale: 1.0,
        angle: 0.0,
        associative: false,
        style: 0,
        elevation: 0.0,
        gradient: None,
        origin: Vec2::ZERO,
        background: None,
        pattern_lines: Vec::new(),
    };
    d.add(&Space::Model, Common::default(), EntityKind::Hatch(h.clone())).unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    // 45° lines at 0.125 spacing across a 4x4 square: about 4*sqrt(2)/0.125 ≈ 45 lines.
    assert!(l.prims.len() > 35 && l.prims.len() < 60, "{}", l.prims.len());
    let mut d2 = Drawing::new_imperial();
    d2.add(&Space::Model, Common::default(), EntityKind::Hatch(Hatch { solid: true, pattern: "SOLID".into(), ..h })).unwrap();
    let l2 = build(&d2, &Space::Model, &Options::default());
    assert_eq!(l2.prims.len(), 1);
    assert_eq!(l2.prims[0].kind, Kind::Tris);
}

#[test]
fn raster_draws_pixels() {
    let d = sample();
    let l = build(&d, &Space::Model, &Options::default());
    let v = View::fit(&l.bounds, 200, 200, 0.1);
    let pm = render(&l, &v, &RasterOptions::default()).unwrap();
    let bg = pm.pixels().iter().filter(|p| p.red() == 33).count();
    assert!(bg < 200 * 200);
    // A red pixel exists from the circle.
    assert!(pm.pixels().iter().any(|p| p.red() > 200 && p.green() < 60));
}

#[test]
fn raster_rejects_bad_size() {
    let l = DisplayList::default();
    assert!(render(&l, &View { center: Vec2::ZERO, scale: 1.0, width: 0, height: 10 }, &RasterOptions::default()).is_none());
}

fn layout_with_viewport(vp: cadcraft_doc::Viewport) -> Drawing {
    let mut d = sample(); // line (0,0)-(10,0) and red circle at (5,5) r3
    let paper = Space::Paper("Layout1".into());
    d.add(&paper, Common::default(), EntityKind::Viewport(vp)).unwrap();
    d
}

fn vp(center: Vec2, w: f64, h: f64, view_center: Vec2, view_height: f64, id: u32) -> cadcraft_doc::Viewport {
    cadcraft_doc::Viewport {
        center: center.to3(0.0),
        width: w,
        height: h,
        view_center,
        view_height,
        id,
        locked: false,
        frozen_layers: Vec::new(),
        layer_colors: Vec::new(),
        clip: None,
        on: true,
    }
}

#[test]
fn layout_viewport_shows_model_clipped() {
    // Viewport 4x4 paper units centred at (5,4), showing model window centred at (5,5), 8 high:
    // scale 0.5, so model (5,5) → paper (5,4); the visible model window is (1,1)..(9,9).
    let d = layout_with_viewport(vp(Vec2::new(5.0, 4.0), 4.0, 4.0, Vec2::new(5.0, 5.0), 8.0, 2));
    let l = build(&d, &Space::Paper("Layout1".into()), &Options { tolerance: 0.01, ..Default::default() });
    let sheet = l.sheet.unwrap();
    assert!(sheet.size.x > sheet.size.y, "landscape sheet");
    let rect = Bounds2::new(Vec2::new(3.0, 2.0), Vec2::new(7.0, 6.0)).expand(1e-9);
    let content: Vec<&DPrim> = l.prims.iter().filter(|p| p.handle == VIEWPORT_CONTENT).collect();
    assert!(!content.is_empty(), "model geometry is drawn in the viewport");
    for p in &content {
        for q in l.points(p) {
            assert!(rect.contains(*q), "{q:?} outside the viewport");
        }
    }
    // The circle (red) is visible, scaled by 0.5 about the view centre: radius 1.5 around (5,4).
    let red: Vec<Vec2> = content.iter().filter(|p| p.color == Rgb(255, 0, 0)).flat_map(|p| l.points(p).to_vec()).collect();
    assert!(!red.is_empty());
    assert!(red.iter().all(|q| (q.dist(Vec2::new(5.0, 4.0)) - 1.5).abs() < 0.02));
    // The line y=0 maps to paper y=1.5, below the viewport: clipped away entirely.
    assert!(content.iter().filter(|p| p.color != Rgb(255, 0, 0)).all(|p| l.points(p).iter().all(|q| q.y > 1.9)));
    // The border is drawn with the viewport's own handle.
    assert!(l.prims.iter().any(|p| p.handle != VIEWPORT_CONTENT));
}

#[test]
fn layout_viewport_frozen_layer_and_id1() {
    let mut v = vp(Vec2::new(5.0, 4.0), 20.0, 20.0, Vec2::new(5.0, 5.0), 20.0, 2);
    v.frozen_layers = vec!["0".into()];
    let d = layout_with_viewport(v);
    let l = build(&d, &Space::Paper("Layout1".into()), &Options::default());
    assert!(l.prims.iter().all(|p| p.handle != VIEWPORT_CONTENT), "layer 0 is frozen in this viewport");
    // Viewport id 1 (the paper-space view) draws nothing.
    let d = layout_with_viewport(vp(Vec2::new(5.0, 4.0), 20.0, 20.0, Vec2::new(5.0, 5.0), 20.0, 1));
    assert!(build(&d, &Space::Paper("Layout1".into()), &Options::default()).prims.is_empty());
    // Model space never draws viewport contents; plotting skips non-plot layers.
    let mut d = sample();
    d.layer_mut("0").unwrap().plot = false;
    assert!(build_plot(&d, &Space::Model, &Options::default()).prims.is_empty());
    assert!(!build(&d, &Space::Model, &Options::default()).prims.is_empty());
}

/// The points of each primitive drawn through a viewport.
fn viewport_points(l: &DisplayList) -> Vec<Vec<Vec2>> {
    l.prims.iter().filter(|p| p.handle == VIEWPORT_CONTENT).map(|p| l.points(p).to_vec()).collect()
}

#[test]
fn viewport_contents_can_be_left_out_and_built_for_any_area() {
    let d = layout_with_viewport(vp(Vec2::new(5.0, 4.0), 4.0, 4.0, Vec2::new(5.0, 5.0), 8.0, 2));
    let paper = Space::Paper("Layout1".into());
    let h = d.space(&paper).unwrap().iter().find(|e| matches!(e.kind, EntityKind::Viewport(_))).unwrap().handle;
    let opts = Options { tolerance: 0.01, ..Default::default() };
    // Left out of the layout: only the border remains.
    let skipped = build(&d, &paper, &Options { skip_viewport: Some(h), ..opts.clone() });
    assert!(viewport_points(&skipped).is_empty());
    assert!(skipped.prims.iter().any(|p| p.handle == h));
    // Built for the viewport's own rectangle: what the layout draws there.
    let rect = Bounds2::new(Vec2::new(3.0, 2.0), Vec2::new(7.0, 6.0));
    let full = build(&d, &paper, &opts);
    assert!(!viewport_points(&full).is_empty());
    assert_eq!(viewport_points(&viewport_content(&d, &paper, h, rect, &opts)), viewport_points(&full));
    // Built for a larger area, clipped to it instead of the viewport: the line y=0 (paper y=1.5,
    // below the viewport) is there.
    let wide = viewport_content(&d, &paper, h, rect.expand(2.0), &opts);
    assert!(viewport_points(&wide).iter().any(|pts| pts.iter().all(|q| (q.y - 1.5).abs() < 1e-9)));
    // Not a viewport, or an empty or non-finite area: nothing.
    assert!(viewport_content(&d, &paper, Handle(0xDEAD), rect, &opts).prims.is_empty());
    assert!(viewport_content(&d, &paper, h, Bounds2::EMPTY, &opts).prims.is_empty());
    let nan = Bounds2::new(Vec2::new(f64::NAN, 0.0), Vec2::new(1.0, 1.0));
    assert!(viewport_content(&d, &paper, h, nan, &opts).prims.is_empty());
}

#[test]
fn viewport_contents_built_on_all_cores_match_one_thread() {
    // Enough model objects for the parallel path; plots build on one thread.
    let mut d = layout_with_viewport(vp(Vec2::new(5.0, 4.0), 8.0, 6.0, Vec2::new(30.0, 30.0), 60.0, 2));
    for i in 0..1200 {
        let (x, y) = (f64::from(i % 40) * 1.5, f64::from(i / 40) * 2.0);
        let line = EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(x, y, 0.0), b: Vec3::new(x + 1.0, y + 1.5, 0.0) });
        d.add(&Space::Model, Common::default(), line).unwrap();
    }
    let paper = Space::Paper("Layout1".into());
    let screen = build(&d, &paper, &Options::default());
    let plot = build_plot(&d, &paper, &Options::default());
    let pts = |l: &DisplayList| -> Vec<Vec<Vec2>> {
        let border: Vec<Handle> = d.space(&paper).unwrap().iter().map(|e| e.handle).collect();
        l.prims.iter().filter(|p| !border.contains(&p.handle)).map(|p| l.points(p).to_vec()).collect()
    };
    assert!(pts(&screen).len() > 1000);
    assert_eq!(pts(&screen), pts(&plot));
}

#[test]
fn hostile_viewports_do_not_panic() {
    for v in [
        vp(Vec2::new(f64::NAN, 0.0), 1.0, 1.0, Vec2::ZERO, 1.0, 2),
        vp(Vec2::ZERO, -1.0, 1.0, Vec2::ZERO, 1.0, 2),
        vp(Vec2::ZERO, 1.0, 1.0, Vec2::ZERO, 0.0, 2),
        vp(Vec2::ZERO, 1e308, 1e308, Vec2::new(1e308, -1e308), 1e-308, 2),
        vp(Vec2::ZERO, 1.0, 1.0, Vec2::new(f64::INFINITY, 0.0), 1.0, 2),
    ] {
        let d = layout_with_viewport(v);
        let _ = build(&d, &Space::Paper("Layout1".into()), &Options::default());
    }
}

fn text_doc(style_font: &str, value: &str) -> Drawing {
    let mut d = Drawing::new_imperial();
    d.text_styles.push(cadcraft_doc::TextStyle { name: "TT".into(), font: style_font.into(), ..Default::default() });
    d.add(
        &Space::Model,
        Common::default(),
        EntityKind::Text(cadcraft_doc::Text {
            insert: Vec3::ZERO,
            align_pt: None,
            height: 1.0,
            value: value.into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "TT".into(),
            halign: Default::default(),
            valign: Default::default(),
        }),
    )
    .unwrap();
    d
}

/// The first common system TrueType font, if any is installed (tests skip otherwise).
fn system_font() -> Option<&'static str> {
    ["Arial", "Helvetica", "DejaVuSans", "Verdana", "LiberationSans-Regular"].into_iter().find(|n| cadcraft_fonts::ttf::find(n).is_some())
}

#[test]
fn truetype_text_is_filled_or_outlined() {
    let Some(font) = system_font() else { return };
    let mut d = text_doc(font, "OH");
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.iter().any(|p| p.kind == Kind::Tris), "TEXTFILL=1 fills glyphs");
    // The O's counter is a hole: filled area is well below the glyph box area.
    let area: f64 = l
        .prims
        .iter()
        .filter(|p| p.kind == Kind::Tris)
        .flat_map(|p| l.points(p).chunks(3).map(|t| ((t[1] - t[0]).cross(t[2] - t[0]) / 2.0).abs()).collect::<Vec<_>>())
        .sum();
    assert!(area > 0.1 && area < l.bounds.width() * l.bounds.height() * 0.8, "area {area}");
    assert!((l.bounds.max.y - 1.0).abs() < 0.1, "cap height = text height");
    d.header.set_i64("TEXTFILL", 0);
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.iter().all(|p| p.kind == Kind::Polyline), "TEXTFILL=0 outlines");
    assert!(l.prims.len() >= 3, "O has two contours, H one");
}

#[test]
fn missing_font_falls_back_to_stroke() {
    let d = text_doc("NoSuchFontAnywhere.ttf", "OH");
    let l = build(&d, &Space::Model, &Options::default());
    assert!(!l.prims.is_empty() && l.prims.iter().all(|p| p.kind == Kind::Polyline));
}

#[test]
fn backwards_and_upside_down_mirror_text() {
    let mut d = text_doc("CADCraft Stroke", "ABC");
    let normal = build(&d, &Space::Model, &Options::default()).bounds;
    assert!(normal.min.x >= -1e-9 && normal.min.y >= -1e-9);
    d.text_styles.iter_mut().for_each(|s| s.backwards = true);
    let back = build(&d, &Space::Model, &Options::default()).bounds;
    assert!(back.max.x <= 1e-9 && (back.width() - normal.width()).abs() < 1e-9, "mirrored to the left");
    d.text_styles.iter_mut().for_each(|s| {
        s.backwards = false;
        s.upside_down = true;
    });
    let up = build(&d, &Space::Model, &Options::default()).bounds;
    assert!(up.max.y <= 1e-9 && up.min.y < -0.9, "mirrored below the baseline");
}

#[test]
fn truetype_mtext_and_dimension_text() {
    let Some(font) = system_font() else { return };
    let mut d = Drawing::new_imperial();
    d.text_styles.iter_mut().for_each(|s| s.font = font.into());
    d.add(
        &Space::Model,
        Common::default(),
        EntityKind::MText(cadcraft_doc::MText {
            insert: Vec3::ZERO,
            height: 1.0,
            width: 0.0,
            attach: 1,
            rotation: 0.0,
            style: "Standard".into(),
            contents: "A{\\C1;B}\\P\\S1/2;".into(),
            line_spacing: 1.0,
            line_spacing_exact: false,
            columns: None,
        }),
    )
    .unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.iter().any(|p| p.kind == Kind::Tris && p.color == Rgb(255, 0, 0)), "red B");
    assert!(l.prims.iter().any(|p| p.kind == Kind::Tris && p.color != Rgb(255, 0, 0)));
    let mut d = Drawing::new_imperial();
    d.text_styles.iter_mut().for_each(|s| s.font = font.into());
    d.add(&Space::Model, Common::default(), EntityKind::Dimension(lin_dim())).unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.iter().filter(|p| p.kind == Kind::Tris).count() > 2, "arrows plus filled glyphs");
}

fn lin_dim() -> cadcraft_doc::Dimension {
    cadcraft_doc::Dimension {
        kind: cadcraft_doc::DimKind::Linear { rotation: 0.0 },
        defpt: Vec3::new(0.0, 2.0, 0.0),
        text_mid: Vec3::ZERO,
        p13: Vec3::ZERO,
        p14: Vec3::new(10.0, 0.0, 0.0),
        p15: Vec3::ZERO,
        p16: Vec3::ZERO,
        text: String::new(),
        style: "Standard".into(),
        measurement: 0.0,
        text_rotation: 0.0,
        user_text_pos: false,
        block: None,
        overrides: Default::default(),
        assoc: Vec::new(),
        marks: Default::default(),
    }
}

#[test]
fn dimension_colours_by_role() {
    let mut d = Drawing::new_imperial();
    if let Some(st) = d.dim_styles.first_mut() {
        st.dim_line_color = Color::Index(1);
        st.ext_line_color = Color::Index(3);
        st.text_color = Color::Index(5);
    }
    d.add(&Space::Model, Common::default(), EntityKind::Dimension(lin_dim())).unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    let has = |c: Rgb, k: Kind| l.prims.iter().any(|p| p.color == c && p.kind == k);
    assert!(has(Rgb(255, 0, 0), Kind::Polyline) && has(Rgb(255, 0, 0), Kind::Tris), "dimension line and arrows red");
    assert!(has(Rgb(0, 255, 0), Kind::Polyline), "extension lines green");
    assert!(has(Rgb(0, 0, 255), Kind::Polyline), "text blue");
    // ByBlock (the default) takes the dimension's own colour.
    let mut d = Drawing::new_imperial();
    d.add(&Space::Model, Common { color: Color::Index(6), ..Default::default() }, EntityKind::Dimension(lin_dim())).unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.iter().all(|p| p.color == Rgb(255, 0, 255)));
}

#[test]
fn table_title_is_centred_and_merged() {
    let mut d = Drawing::new_imperial();
    let cell = |t: &str| cadcraft_doc::TableCell { text: t.into(), ..Default::default() };
    let mut title = vec![cell("TITLE"), cell(""), cell("")];
    title[0].merged = Some((1, 3));
    let t = cadcraft_doc::Table {
        insert: Vec3::new(0.0, 0.0, 0.0),
        col_widths: vec![2.0, 2.0, 2.0],
        row_heights: vec![1.0, 1.0],
        cells: vec![title, vec![cell("a"), cell("b"), cell("c")]],
        style: "Standard".into(),
        text_height: 0.2,
        title: true,
        header: false,
        ..Default::default()
    };
    d.add(&Space::Model, Common::default(), EntityKind::Table(t)).unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    // No vertical cell borders cross the merged title row (y in 0..-1) except the outer ones.
    let inner_vertical_in_title = l.prims.iter().filter(|p| p.kind == Kind::Polyline).any(|p| {
        l.points(p).windows(2).any(|w| {
            (w[0].x - w[1].x).abs() < 1e-12
                && (w[0].y - w[1].y).abs() > 0.5
                && w[0].x > 0.1
                && w[0].x < 5.9
                && w[0].y.max(w[1].y) > -0.99
                && w[0].y.min(w[1].y) < -0.01
        })
    });
    assert!(!inner_vertical_in_title);
    // Title text centred on the table width.
    let text: Vec<Vec2> = l
        .prims
        .iter()
        .filter(|p| p.kind == Kind::Polyline)
        .flat_map(|p| l.points(p).to_vec())
        .filter(|q| q.y > -0.95 && q.y < -0.05 && q.x > 0.05 && q.x < 5.95)
        .collect();
    let b = Bounds2::from_points(text);
    assert!((b.center().x - 3.0).abs() < 0.05, "{b:?}");
}

// ---------- hatch level of detail (on-screen builds, issue #544) ----------

fn pattern_hatch(pattern: &str, scale: f64, (x0, y0, x1, y1): (f64, f64, f64, f64)) -> Hatch {
    let sq = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
    Hatch {
        pattern: pattern.into(),
        solid: false,
        loops: vec![HatchLoop { vertices: sq.to_vec(), outer: true, sources: Vec::new() }],
        scale,
        angle: 0.0,
        associative: false,
        style: 0,
        elevation: 0.0,
        gradient: None,
        origin: Vec2::ZERO,
        background: None,
        pattern_lines: Vec::new(),
    }
}

fn drawing_with(h: Hatch) -> Drawing {
    let mut d = Drawing::new_imperial();
    d.add(&Space::Model, Common::default(), EntityKind::Hatch(h)).unwrap();
    d
}

fn loops_of(h: &Hatch) -> Vec<Vec<Vec2>> {
    crate::hatch::fill_loops(h, 0.001)
}

/// On-screen options for a view where one pixel is `px` drawing units.
fn screen(px: f64, view: Option<Bounds2>) -> Options {
    Options { tolerance: px / 2.0, min_dash: px * 2.0, view, ..Default::default() }
}

#[test]
fn dense_hatch_on_screen_is_one_solid_fill() {
    // ANSI31 at scale 0.01: lines 0.00125 apart over a 4x4 square. On a 1000 px view of it a
    // pixel is 0.004, so the lines run together: one solid fill. Plots keep every line.
    let d = drawing_with(pattern_hatch("ANSI31", 0.01, (0.0, 0.0, 4.0, 4.0)));
    let plot = build(&d, &Space::Model, &Options::default());
    assert!(plot.prims.len() > 4000, "plots keep every line: {}", plot.prims.len());
    let l = build(&d, &Space::Model, &screen(0.004, None));
    assert_eq!(l.prims.len(), 1);
    assert_eq!(l.prims[0].kind, Kind::Tris);
}

#[test]
fn sparse_dots_keep_their_look_when_lines_are_close() {
    // Dot rows half a pixel apart but dots five pixels apart along them: not solid colour.
    let fam = |dashes: &[f64]| cadcraft_doc::library::PatternLine { angle: 0.0, origin: (0.0, 0.0), delta: (0.0, 0.5), dashes: dashes.to_vec() };
    let sparse = Hatch { pattern_lines: vec![fam(&[0.0, -5.0])], ..pattern_hatch("MYDOTS", 1.0, (0.0, 0.0, 40.0, 40.0)) };
    assert!(!crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &sparse, 1.0));
    // Dots closer than a pixel along rows closer than a pixel: solid.
    let dense = Hatch { pattern_lines: vec![fam(&[0.0, -0.5])], ..sparse.clone() };
    assert!(crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &dense, 1.0));
    // A family with only gaps draws nothing, so it never reads as solid.
    let blank = Hatch { pattern_lines: vec![fam(&[-0.5])], ..sparse.clone() };
    assert!(!crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &blank, 1.0));
    assert!(crate::hatch::pattern_lines(&cadcraft_doc::Drawing::new_imperial(), &blank, &loops_of(&blank), 1.0, None).is_empty());
    // Full detail is never solid.
    assert!(!crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &dense, 0.0));
}

#[test]
fn scattered_dot_pattern_reads_solid_once_dots_fill_the_pixels() {
    // AR-SAND at scale 3/16 (the library pattern is drawn 16x coarser since #643, so this is the
    // geometry of the old scale 3): three dot families, no single one dense enough on its own at
    // 0.5 per pixel, but together about four dots on every pixel. Zoomed in 4x the dots stand apart.
    let h = pattern_hatch("AR-SAND", 3.0 / 16.0, (0.0, 0.0, 100.0, 100.0));
    assert!(crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &h, 0.5));
    assert!(!crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &h, 0.125));
}

/// Same polylines up to float noise: the view-limited path rotates into the family frame and back,
/// so a start point can differ by ~1e-16 between platforms (#804). Structure must match exactly.
fn assert_lines_close(a: &[Vec<Vec2>], b: &[Vec<Vec2>], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: line count");
    for (i, (ra, rb)) in a.iter().zip(b).enumerate() {
        assert_eq!(ra.len(), rb.len(), "{what}: run {i} length");
        for (pa, pb) in ra.iter().zip(rb) {
            assert!((pa.x - pb.x).abs() < 1e-9 && (pa.y - pb.y).abs() < 1e-9, "{what}: run {i}: {pa:?} vs {pb:?}");
        }
    }
}

#[test]
fn normal_zoom_pattern_is_unchanged() {
    // Lines and dash repeats a few pixels long or more: the level of detail changes nothing.
    for (pattern, px) in [("ANSI31", 0.05), ("ANSI33", 0.05), ("AR-CONC", 0.02), ("AR-SAND", 0.01), ("DOTS", 0.01)] {
        let h = pattern_hatch(pattern, 1.0, (0.0, 0.0, 6.0, 4.0));
        let loops = loops_of(&h);
        let full = crate::hatch::pattern_lines(&cadcraft_doc::Drawing::new_imperial(), &h, &loops, 0.0, None);
        assert!(!full.is_empty(), "{pattern}");
        assert!(!crate::hatch::reads_solid(&cadcraft_doc::Drawing::new_imperial(), &h, px), "{pattern}");
        let same_res = crate::hatch::pattern_lines(&cadcraft_doc::Drawing::new_imperial(), &h, &loops, px, None);
        assert_lines_close(&same_res, &full, pattern);
        // A view covering the whole hatch changes nothing either.
        let all = Bounds2::new(Vec2::new(-1.0, -1.0), Vec2::new(7.0, 5.0));
        let in_view = crate::hatch::pattern_lines(&cadcraft_doc::Drawing::new_imperial(), &h, &loops, px, Some(&all));
        assert_lines_close(&in_view, &full, pattern);
    }
}

#[test]
fn dash_repeat_under_two_pixels_is_one_run() {
    // ANSI33's dashed family repeats every 0.1875: at 0.1 per pixel it reads as a continuous line,
    // drawn as one run per span covering the same length as the full-detail span.
    let h = pattern_hatch("ANSI33", 1.0, (0.0, 0.0, 6.0, 4.0));
    let loops = loops_of(&h);
    let full = crate::hatch::pattern_lines(&cadcraft_doc::Drawing::new_imperial(), &h, &loops, 0.0, None);
    let lod = crate::hatch::pattern_lines(&cadcraft_doc::Drawing::new_imperial(), &h, &loops, 0.1, None);
    assert!(lod.len() < full.len(), "{} vs {}", lod.len(), full.len());
    assert!(lod.iter().all(|r| r.len() == 2));
    // The continuous family: identical runs in both.
    let solid_runs = |v: &[Vec<Vec2>]| v.iter().filter(|r| r.len() == 2).count();
    assert!(solid_runs(&lod) > 0 && solid_runs(&full) > solid_runs(&lod));
}

#[test]
fn hatch_pattern_only_inside_the_view() {
    // A 40x40 ANSI31 hatch looked at through a 2x2 view in a corner, at full pixel detail.
    let d = drawing_with(pattern_hatch("ANSI31", 1.0, (0.0, 0.0, 40.0, 40.0)));
    let full = build(&d, &Space::Model, &Options::default());
    let far = Some(Bounds2::new(Vec2::new(100.0, 100.0), Vec2::new(110.0, 110.0)));
    assert_eq!(build(&d, &Space::Model, &Options { view: far, ..Default::default() }).prims.len(), 0);
    let corner = Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(2.0, 2.0));
    let part = build(&d, &Space::Model, &Options { view: Some(corner), ..Default::default() });
    assert!(!part.prims.is_empty() && part.prims.len() * 10 < full.prims.len(), "{} of {}", part.prims.len(), full.prims.len());
    // Runs are cut in the family's rotated frame: a 2x2 view at 45° reaches 1 unit out, plus one
    // line spacing (0.125 × √2 along an axis) from rounding the line range outwards.
    let near = corner.expand(1.0 + 0.125 * 2f64.sqrt() + 1e-9);
    assert!(part.verts.iter().all(|p| near.contains(*p)));
    // Nothing is lost inside the view: the drawn length within an inner box is the same.
    let inner = Bounds2::new(Vec2::new(0.2, 0.2), Vec2::new(1.8, 1.8));
    let (a, b) = (length_inside(&full, &inner), length_inside(&part, &inner));
    assert!(a > 1.0 && (a - b).abs() < 1e-9, "{a} vs {b}");
}

/// Total length of a display list's polylines inside an axis-aligned box (Liang–Barsky).
fn length_inside(l: &DisplayList, bx: &Bounds2) -> f64 {
    let mut total = 0.0;
    for p in l.prims.iter().filter(|p| p.kind == Kind::Polyline) {
        for w in l.points(p).windows(2) {
            let (p0, d) = (w[0], w[1] - w[0]);
            let (mut t0, mut t1) = (0.0f64, 1.0f64);
            let mut ok = true;
            for (pp, q) in [(-d.x, p0.x - bx.min.x), (d.x, bx.max.x - p0.x), (-d.y, p0.y - bx.min.y), (d.y, bx.max.y - p0.y)] {
                if pp == 0.0 {
                    ok &= q >= 0.0;
                } else if pp < 0.0 {
                    t0 = t0.max(q / pp);
                } else {
                    t1 = t1.min(q / pp);
                }
            }
            if ok && t1 > t0 {
                total += d.len() * (t1 - t0);
            }
        }
    }
    total
}

#[test]
fn big_hatch_seen_through_a_wide_view_is_drawn() {
    // ANSI31 (0.125 apart) over ±1e5 seen through a 3000-unit view: about 34,000 lines in the
    // family's rotated frame, past the 20,000-line guard of a whole-hatch sweep, but still drawn
    // when the sweep is limited to the view.
    let d = drawing_with(pattern_hatch("ANSI31", 1.0, (-1e5, -1e5, 1e5, 1e5)));
    let view = Some(Bounds2::new(Vec2::new(-1500.0, -1500.0), Vec2::new(1500.0, 1500.0)));
    let l = build(&d, &Space::Model, &Options { view, ..Default::default() });
    assert!(l.prims.len() > 20_000, "{}", l.prims.len());
}

#[test]
fn hatch_in_a_squashed_block_keeps_lines_that_show() {
    // A block hatch inserted 100x wider than tall: along x its lines are spread far apart on
    // screen, so it is drawn with lines, not merged into a fill.
    let mut d = Drawing::new_imperial();
    let mut blk = cadcraft_doc::Block::new("B");
    let mut e = cadcraft_doc::Entity::new(cadcraft_doc::Handle(1), EntityKind::Hatch(pattern_hatch("ANSI31", 1.0, (0.0, 0.0, 4.0, 4.0))));
    e.common = Common::default();
    blk.entities.push(e);
    d.blocks.insert("B".into(), std::sync::Arc::new(blk));
    let ins = cadcraft_doc::Insert {
        block: "B".into(),
        insert: Vec3::new(0.0, 0.0, 0.0),
        scale: Vec3::new(100.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    d.add(&Space::Model, Common::default(), EntityKind::Insert(ins)).unwrap();
    // One pixel = 1 unit: 0.125 spacing × 100 along x is far more than a pixel.
    let l = build(&d, &Space::Model, &screen(1.0, None));
    assert!(l.prims.iter().all(|p| p.kind != Kind::Tris), "merged into a fill");
    assert!(l.prims.len() > 10, "{}", l.prims.len());
}

#[test]
fn max_scale_is_the_most_stretched_axis() {
    let m = Mat3::scale(1.0, 0.1);
    assert!((max_scale(&m) - 1.0).abs() < 1e-12);
    let r = Mat3::rotate(0.7).then(Mat3::scale(2.0, 2.0));
    assert!((max_scale(&r) - 2.0).abs() < 1e-12);
    assert!((max_scale(&Mat3::IDENTITY) - 1.0).abs() < 1e-12);
}

// ---------- transparency ----------

fn line_common(c: Common) -> (Common, EntityKind) {
    (c, EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: Vec3::new(10.0, 0.0, 0.0) }))
}

fn with_transparency() -> Options {
    Options { transparency: true, ..Default::default() }
}

fn block_entity(handle: u64, common: Common, kind: EntityKind) -> cadcraft_doc::Entity {
    let mut e = cadcraft_doc::Entity::new(cadcraft_doc::Handle(handle), kind);
    e.common = common;
    e
}

fn insert_of(block: &str) -> EntityKind {
    EntityKind::Insert(cadcraft_doc::Insert {
        block: block.into(),
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    })
}

#[test]
fn transparency_resolves_by_object_layer_and_block() {
    use cadcraft_doc::Transparency::{ByBlock, ByLayer, Percent};
    let mut d = Drawing::new_imperial();
    d.layers.push(cadcraft_doc::Layer { name: "Walls".into(), transparency: 40, ..Default::default() });
    let add = |d: &mut Drawing, c: Common| {
        let (c, k) = line_common(c);
        d.add(&Space::Model, c, k).unwrap();
    };
    add(&mut d, Common { transparency: Percent(25), ..Default::default() });
    add(&mut d, Common { layer: "Walls".into(), transparency: ByLayer, ..Default::default() });
    // ByBlock outside a block: opaque.
    add(&mut d, Common { transparency: ByBlock, ..Default::default() });
    // Block "Inner": a ByBlock line and a ByLayer line on layer 0 (the reference's layer).
    let mut inner = cadcraft_doc::Block::new("Inner");
    let (c, k) = line_common(Common { transparency: ByBlock, ..Default::default() });
    inner.entities.push(block_entity(1, c, k));
    let (c, k) = line_common(Common::default());
    inner.entities.push(block_entity(2, c, k));
    d.blocks.insert("Inner".into(), std::sync::Arc::new(inner));
    // Block "Outer": a ByBlock reference to Inner, on layer Walls.
    let mut outer = cadcraft_doc::Block::new("Outer");
    outer.entities.push(block_entity(3, Common { layer: "Walls".into(), transparency: ByBlock, ..Default::default() }, insert_of("Inner")));
    d.blocks.insert("Outer".into(), std::sync::Arc::new(outer));
    d.add(&Space::Model, Common { transparency: Percent(70), ..Default::default() }, insert_of("Outer")).unwrap();
    let l = build(&d, &Space::Model, &with_transparency());
    let t: Vec<u8> = l.prims.iter().map(|p| p.transparency).collect();
    // Nested: Inner's ByBlock line takes Outer's reference (70, through the ByBlock reference to
    // Inner); its layer-0 line takes the reference's layer, Walls (40).
    assert_eq!(t, vec![25, 40, 0, 70, 40]);
    // Off (the default): everything is opaque, as before.
    assert!(build(&d, &Space::Model, &Options::default()).prims.iter().all(|p| p.transparency == 0 && p.alpha() == 255));
}

#[test]
fn hostile_transparency_values_are_clamped() {
    let mut d = Drawing::new_imperial();
    d.layers.push(cadcraft_doc::Layer { name: "Odd".into(), transparency: 250, ..Default::default() });
    let (c, k) = line_common(Common { transparency: cadcraft_doc::Transparency::Percent(u8::MAX), ..Default::default() });
    d.add(&Space::Model, c, k).unwrap();
    let (c, k) = line_common(Common { layer: "Odd".into(), ..Default::default() });
    d.add(&Space::Model, c, k).unwrap();
    // A ByLayer object on a layer that isn't in the table: opaque.
    let (c, k) = line_common(Common { layer: "Missing".into(), ..Default::default() });
    d.add(&Space::Model, c, k).unwrap();
    let l = build(&d, &Space::Model, &with_transparency());
    assert_eq!(l.prims.iter().map(|p| p.transparency).collect::<Vec<_>>(), vec![90, 90, 0]);
    assert_eq!((alpha(0), alpha(50), alpha(90), alpha(200)), (255, 128, 26, 26));
}

#[test]
fn transparent_solid_hatch_blends_with_the_line_under_it() {
    let mut d = Drawing::new_imperial();
    // The line runs mid-pixel (y = 2.025 is screen row 99.5) so it fills one row exactly.
    d.add(&Space::Model, Common::default(), EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(-2.0, 2.025, 0.0), b: Vec3::new(6.0, 2.025, 0.0) }))
        .unwrap();
    let solid = Hatch { solid: true, pattern: "SOLID".into(), ..pattern_hatch("SOLID", 1.0, (0.0, 0.0, 4.0, 4.0)) };
    d.add(
        &Space::Model,
        Common { color: Color::Index(1), transparency: cadcraft_doc::Transparency::Percent(50), ..Default::default() },
        EntityKind::Hatch(solid),
    )
    .unwrap();
    let pixel = |o: &Options, x: f64, y: f64| {
        let l = build(&d, &Space::Model, o);
        let v = View { center: Vec2::new(2.0, 2.0), scale: 20.0, width: 200, height: 200 };
        let pm = render(&l, &v, &RasterOptions { background: Rgb(0, 0, 0), antialias: false, ..Default::default() }).unwrap();
        let (px, py) = v.to_screen(Vec2::new(x, y));
        let c = pm.pixel(px as u32, py as u32).unwrap();
        (c.red(), c.green(), c.blue())
    };
    let near = |a: (u8, u8, u8), b: (u8, u8, u8)| [(a.0, b.0), (a.1, b.1), (a.2, b.2)].iter().all(|(x, y)| x.abs_diff(*y) <= 2);
    // 50 %: half red over black, and half red over the white line, which shows through.
    let inside = pixel(&with_transparency(), 1.0, 1.0);
    assert!(near(inside, (128, 0, 0)), "{inside:?}");
    let on_line = pixel(&with_transparency(), 1.0, 2.025);
    assert!(near(on_line, (255, 127, 127)), "{on_line:?}");
    // Transparency off: the hatch hides the line, as before.
    assert_eq!(pixel(&Options::default(), 1.0, 2.025), (255, 0, 0));
}

fn attrib(tag: &str) -> cadcraft_doc::Attrib {
    cadcraft_doc::Attrib {
        tag: tag.into(),
        text: cadcraft_doc::Text {
            insert: Vec3::new(1.0, 1.0, 0.0),
            align_pt: None,
            height: 1.0,
            value: "A1".into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: Default::default(),
            valign: Default::default(),
        },
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: Default::default(),
    }
}

/// A block "B" with a line (layer 0, ByLayer) and an attribute definition with `def`'s
/// properties, and a reference to it with `reference`'s, carrying the attribute.
fn attributed(def: Common, reference: Common) -> Drawing {
    let mut d = Drawing::new_imperial();
    let mut blk = cadcraft_doc::Block::new("B");
    let (c, k) = line_common(Common::default());
    blk.entities.push(block_entity(1, c, k));
    blk.entities.push(block_entity(2, def, EntityKind::AttDef(attrib("TAG"))));
    d.blocks.insert("B".into(), std::sync::Arc::new(blk));
    let EntityKind::Insert(mut ins) = insert_of("B") else { panic!("an insert") };
    ins.attribs.push(attrib("TAG"));
    d.add(&Space::Model, reference, EntityKind::Insert(ins)).unwrap();
    d
}

/// The transparency of the block's line and of the attribute's text, from a list built with
/// transparency on (the line is drawn first, then the text).
fn line_and_text(d: &Drawing) -> (u8, Vec<u8>) {
    let l = build(d, &Space::Model, &with_transparency());
    let (first, rest) = l.prims.split_first().unwrap();
    assert!(!rest.is_empty(), "the attribute is drawn");
    (first.transparency, rest.iter().map(|p| p.transparency).collect())
}

#[test]
fn attributes_draw_with_their_own_transparency() {
    use cadcraft_doc::Transparency::{ByBlock, Percent};
    // A reference at 60 % on an opaque layer: layer-0 ByLayer contents (the line and the
    // attribute) take the reference's layer, so both are opaque.
    let d = attributed(Common::default(), Common { transparency: Percent(60), ..Default::default() });
    let (line, text) = line_and_text(&d);
    assert_eq!(line, 0);
    assert!(text.iter().all(|t| *t == 0), "{text:?}");
    // An attribute definition with its own 50 % in an opaque reference.
    let d = attributed(Common { transparency: Percent(50), ..Default::default() }, Common::default());
    let (line, text) = line_and_text(&d);
    assert_eq!(line, 0);
    assert!(text.iter().all(|t| *t == 50), "{text:?}");
    // ByBlock follows the reference.
    let d = attributed(Common { transparency: ByBlock, ..Default::default() }, Common { transparency: Percent(30), ..Default::default() });
    assert!(line_and_text(&d).1.iter().all(|t| *t == 30));
    // Off: everything opaque.
    assert!(build(&d, &Space::Model, &Options::default()).prims.iter().all(|p| p.transparency == 0));
}

#[test]
fn attributes_follow_the_reference_layer_and_have_no_definition() {
    use cadcraft_doc::Transparency::Percent;
    // A layer-0 ByLayer definition in a reference on a 40 % layer: the line and the attribute
    // both take the reference's layer.
    let mut d = attributed(Common::default(), Common { layer: "Glass".into(), ..Default::default() });
    // Adding the reference made the layer.
    d.layer_mut("Glass").unwrap().transparency = 40;
    let (line, text) = line_and_text(&d);
    assert_eq!(line, 40);
    assert!(text.iter().all(|t| *t == 40), "{text:?}");
    // No definition in the block: the attribute follows the reference (30 %), with or without
    // properties of its own.
    for props in [cadcraft_doc::AttribProps::default(), cadcraft_doc::AttribProps { color: Some(Color::Index(3)), ..Default::default() }] {
        let mut d = Drawing::new_imperial();
        let mut blk = cadcraft_doc::Block::new("B");
        let (c, k) = line_common(Common::default());
        blk.entities.push(block_entity(1, c, k));
        d.blocks.insert("B".into(), std::sync::Arc::new(blk));
        let EntityKind::Insert(mut ins) = insert_of("B") else { panic!("an insert") };
        ins.attribs.push(cadcraft_doc::Attrib { props: props.clone(), ..attrib("TAG") });
        d.add(&Space::Model, Common { transparency: Percent(30), ..Default::default() }, EntityKind::Insert(ins)).unwrap();
        let (line, text) = line_and_text(&d);
        assert_eq!(line, 0, "layer-0 ByLayer contents take the reference's opaque layer");
        assert!(text.iter().all(|t| *t == 30), "{props:?}: {text:?}");
    }
}

#[test]
fn mtext_exact_spacing_ignores_taller_lines() {
    // #711: a doubled middle line pushes the next baseline down with At least, not with Exactly.
    let mtext = |contents: &str, line_spacing: f64, line_spacing_exact: bool| cadcraft_doc::MText {
        insert: Vec3::ZERO,
        height: 1.0,
        width: 10.0,
        attach: 1,
        rotation: 0.0,
        style: "Standard".into(),
        contents: contents.into(),
        line_spacing,
        line_spacing_exact,
        columns: None,
    };
    let d = Drawing::new_imperial();
    let height = |contents: &str, ls: f64, exact: bool| layout_mtext_entity(&d, &mtext(contents, ls, exact)).bounds.height();
    // The plain layout and the paragraph layout (`\p`) both honour the style.
    for mixed in ["A\\P{\\H2x;BB}\\PCC", "\\pxi1;A\\P{\\H2x;BB}\\PCC"] {
        assert!((height(mixed, 1.0, false) - (1.0 + 2.0 * 5.0 / 3.0 + 5.0 / 3.0)).abs() < 1e-9, "{mixed}: at least");
        assert!((height(mixed, 1.0, true) - (1.0 + 2.0 * 5.0 / 3.0)).abs() < 1e-9, "{mixed}: exactly");
        assert!((height(mixed, 2.0, true) - (1.0 + 2.0 * 5.0 / 3.0 * 2.0)).abs() < 1e-9, "{mixed}: exactly, factor 2");
    }
    for exact in [false, true] {
        assert!((height("A\\PBB\\PCC", 1.0, exact) - (1.0 + 2.0 * 5.0 / 3.0)).abs() < 1e-9, "uniform lines, exact {exact}");
    }
}

#[test]
fn table_cell_text_height_scales_with_the_table() {
    let height = |table_h: f64, own: Option<f64>| {
        let d = Drawing::new_imperial();
        let st = crate::table_style_of(&d, &cadcraft_doc::Table::default());
        let cell = cadcraft_doc::TableCell { text: "a".into(), text_height: own, ..Default::default() };
        let t = cadcraft_doc::Table {
            col_widths: vec![500.0],
            row_heights: vec![500.0],
            cells: vec![vec![cell]],
            text_height: table_h * st.text_height,
            ..Default::default()
        };
        crate::table_geometry(&d, &t).texts.first().map(|x| x.height)
    };
    let base = height(1.0, None).unwrap();
    assert_eq!(height(1.0, Some(150.0)), Some(150.0));
    assert_eq!(height(2.0, Some(150.0)), Some(300.0));
    assert_eq!(height(2.0, None), Some(base * 2.0));
}

#[test]
fn cancelled_build_stops_early() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let d = sample();
    let set = Some(Arc::new(AtomicBool::new(true)));
    let full = build(&d, &Space::Model, &Options::default());
    assert!(!full.prims.is_empty());
    let opts = Options { cancel: set.clone(), ..Default::default() };
    assert!(opts.cancelled());
    assert!(build(&d, &Space::Model, &opts).prims.is_empty(), "no entity is drawn once cancelled");
    // A hatch sweep stops at its next pattern line.
    let h = pattern_hatch("ANSI31", 1.0, (0.0, 0.0, 6.0, 4.0));
    let loops = loops_of(&h);
    let drawing = cadcraft_doc::Drawing::new_imperial();
    assert!(!crate::hatch::pattern_lines_until(&drawing, &h, &loops, 0.0, None, None).is_empty());
    assert!(crate::hatch::pattern_lines_until(&drawing, &h, &loops, 0.0, None, set.as_deref()).is_empty());
}

/// #1434: a closed polyline with a global width narrower than a pixel loses whole sides at
/// some zooms, while the same polyline with width 0 always shows every side.
#[test]
fn thin_wide_polyline_keeps_every_side_at_every_zoom() {
    let square = |const_width: f64| {
        let mut d = Drawing::new_imperial();
        let vertices = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y))).to_vec();
        let p = cadcraft_doc::LwPolyline { vertices, closed: true, const_width, elevation: 0.0, plinegen: false };
        d.add(&Space::Model, Common::default(), EntityKind::LwPolyline(p)).unwrap();
        d
    };
    // Side midpoints and the direction across each side.
    let sides = [("bottom", Vec2::new(5.0, 0.0)), ("right", Vec2::new(10.0, 5.0)), ("top", Vec2::new(5.0, 10.0)), ("left", Vec2::new(0.0, 5.0))];
    let mut missing = Vec::new();
    for const_width in [0.0, 0.01, 0.05] {
        let d = square(const_width);
        for step in 0..40 {
            let scale = 20.0 + f64::from(step) * 0.37;
            let px = 1.0 / scale;
            let l = build(&d, &Space::Model, &screen(px, None));
            let v = View { center: Vec2::new(5.17, 4.62), scale, width: 400, height: 400 };
            // No anti-aliasing: pixel-centre coverage, as the GPU canvas rasterises (no MSAA).
            let pm = render(&l, &v, &RasterOptions { background: Rgb(0, 0, 0), antialias: false, ..Default::default() }).unwrap();
            for (name, mid) in sides {
                let (sx, sy) = v.to_screen(mid);
                let lit = (-2i32..=2)
                    .flat_map(|dy| (-2i32..=2).map(move |dx| (dx, dy)))
                    .any(|(dx, dy)| pm.pixel((sx as i32 + dx) as u32, (sy as i32 + dy) as u32).is_some_and(|c| c.red() > 0));
                if !lit {
                    missing.push(format!("width {const_width} scale {scale:.2} ({:.2} px wide): {name} side missing", const_width * scale));
                }
            }
        }
    }
    assert!(missing.is_empty(), "{} misses:\n{}", missing.len(), missing.join("\n"));
}

/// #1434: on screen a wide polyline also draws its centre line (a hairline in its colour, solid and
/// dashed alike); plots and exports (`min_dash` 0) draw the band alone.
#[test]
fn wide_polyline_centre_line_is_drawn_on_screen_only() {
    for lt in ["Continuous", "DASHED"] {
        let drawing = |transparency: u8| {
            let mut d = Drawing::new_imperial();
            d.linetypes.extend(cadcraft_doc::library::standard_linetypes().into_iter().filter(|l| l.name == "DASHED"));
            let vertices = [(0.0, 0.0), (3.0, 0.0), (3.0, 3.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y))).to_vec();
            let p = cadcraft_doc::LwPolyline { vertices, closed: false, const_width: 0.2, elevation: 0.0, plinegen: false };
            let common = Common {
                color: Color::Index(1),
                linetype: lt.into(),
                lineweight: cadcraft_doc::Lineweight::Mm100(50),
                transparency: cadcraft_doc::Transparency::Percent(transparency),
                ..Default::default()
            };
            d.add(&Space::Model, common, EntityKind::LwPolyline(p)).unwrap();
            d
        };
        let d = drawing(0);
        let opts = Options { lineweights: true, transparency: true, ..screen(0.01, None) };
        let lines = |l: &DisplayList| l.prims.iter().filter(|p| p.kind == Kind::Polyline).count();
        let on_screen = build(&d, &Space::Model, &opts);
        assert!(on_screen.prims.iter().any(|p| p.kind == Kind::Tris), "{lt}: the band");
        let hair: Vec<&DPrim> = on_screen.prims.iter().filter(|p| p.kind == Kind::Polyline).collect();
        assert!(!hair.is_empty(), "{lt}: the centre line on screen");
        assert!(hair.iter().all(|p| p.color == Rgb(255, 0, 0) && p.lw == 0.0), "{lt}: hairline in the polyline's colour");
        assert!(on_screen.points(hair[0]).iter().all(|q| q.y.abs() < 1e-9 || (q.x - 3.0).abs() < 1e-9), "{lt}: on the centre line");
        assert_eq!(lines(&build(&d, &Space::Model, &Options { min_dash: 0.0, ..opts.clone() })), 0, "{lt}: exports draw the band alone");
        assert_eq!(lines(&build_plot(&d, &Space::Model, &opts)), 0, "{lt}: plots draw the band alone");
        // Its alpha would add up along the centre: a transparent band gets no hairline.
        let see_through = build(&drawing(50), &Space::Model, &opts);
        assert!(see_through.prims.iter().any(|p| p.kind == Kind::Tris && p.transparency == 50), "{lt}: the transparent band");
        assert_eq!(lines(&see_through), 0, "{lt}: a transparent band gets no hairline");
    }
}

/// Each primitive stroked or filled on its own, as `render` did before it batched hairlines.
fn render_one_by_one(list: &DisplayList, view: &View, o: &RasterOptions) -> tiny_skia::Pixmap {
    use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let mut pm = Pixmap::new(view.width, view.height).unwrap();
    pm.fill(tiny_skia::Color::from_rgba8(o.background.0, o.background.1, o.background.2, 255));
    for p in &list.prims {
        let c = p.display_rgb(o.background);
        let mut paint = Paint::default();
        paint.set_color_rgba8(c.0, c.1, c.2, p.alpha());
        paint.anti_alias = o.antialias;
        let mut pb = PathBuilder::new();
        match p.kind {
            Kind::Polyline => {
                for (i, q) in list.points(p).iter().enumerate() {
                    let (x, y) = view.to_screen(*q);
                    if i == 0 {
                        pb.move_to(x, y);
                    } else {
                        pb.line_to(x, y);
                    }
                }
                if let Some(path) = pb.finish() {
                    let w = if o.px_per_mm > 0.0 && p.lw > 0.0 { (p.lw * o.px_per_mm).max(o.hairline) } else { o.hairline };
                    let stroke = Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
            Kind::Tris => {
                for t in list.points(p).chunks(3) {
                    let [a, b, c] = t else { continue };
                    let ((ax, ay), (bx, by), (cx, cy)) = (view.to_screen(*a), view.to_screen(*b), view.to_screen(*c));
                    pb.move_to(ax, ay);
                    pb.line_to(bx, by);
                    pb.line_to(cx, cy);
                    pb.close();
                }
                if let Some(path) = pb.finish() {
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
            _ => panic!("not drawn by this reference"),
        }
    }
    pm
}

fn push_prim(l: &mut DisplayList, kind: Kind, color: Rgb, transparency: u8, lw: f32, pts: &[Vec2]) {
    let arr = if kind == Kind::Tris { &mut l.tris } else { &mut l.verts };
    let start = arr.len() as u32;
    arr.extend_from_slice(pts);
    for p in pts.iter().filter(|p| p.is_finite() && p.x.abs() < 1e6) {
        l.bounds.add(*p);
    }
    l.prims.push(DPrim { handle: VIEWPORT_CONTENT, color, aci7: false, lw, transparency, kind, start, len: pts.len() as u32 });
}

/// Hairline polylines with the same paint are stroked as one path; the pixels must be exactly
/// those of stroking each on its own, whatever breaks a run (colour, transparency, lineweight,
/// fills, unusable coordinates) and with overlaps, caps and dots.
#[test]
fn raster_hairline_batches_match_one_by_one() {
    let red = Rgb(255, 0, 0);
    let blue = Rgb(0, 0, 255);
    let mut l = DisplayList::default();
    // A hatch-like run of crossing, overlapping and touching lines in one colour.
    for i in 0..40 {
        let x = f64::from(i) * 0.37;
        push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(x, 0.0), Vec2::new(x + 6.0, 9.0)]);
        push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(x + 3.0, 0.0), Vec2::new(x - 2.0, 9.0), Vec2::new(x + 1.0, 4.0)]);
    }
    // Zero-length (a dot), single-point (nothing), and repeated lines.
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(2.0, 2.0), Vec2::new(2.0, 2.0)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(3.0, 3.0)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(1.0, 1.0), Vec2::new(8.0, 2.0)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(1.0, 1.0), Vec2::new(8.0, 2.0)]);
    // Transparent lines build up where they overlap, within a run and across runs.
    for i in 0..10 {
        let y = f64::from(i) * 0.2;
        push_prim(&mut l, Kind::Polyline, blue, 50, 0.0, &[Vec2::new(0.0, y), Vec2::new(12.0, 8.0 - y)]);
    }
    push_prim(&mut l, Kind::Polyline, blue, 20, 0.0, &[Vec2::new(0.0, 5.0), Vec2::new(12.0, 5.0)]);
    // A lineweight (a filled outline when lineweights are shown), a fill, then the run again.
    push_prim(&mut l, Kind::Polyline, red, 0, 0.5, &[Vec2::new(0.0, 6.0), Vec2::new(12.0, 7.0), Vec2::new(5.0, 2.0)]);
    push_prim(&mut l, Kind::Tris, blue, 30, 0.0, &[Vec2::new(4.0, 4.0), Vec2::new(9.0, 4.0), Vec2::new(6.0, 8.0)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(0.0, 8.0), Vec2::new(12.0, 1.0)]);
    // Unusable or far-out coordinates are stroked on their own, as before.
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(1.0, 1.0), Vec2::new(f64::NAN, 3.0), Vec2::new(9.0, 9.0)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(-1e12, 4.0), Vec2::new(1e12, 4.5)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(5.0, -1e7), Vec2::new(5.5, 1e7)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(0.0, 0.5), Vec2::new(12.0, 0.5)]);
    // Partly and wholly off screen.
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(-30.0, -30.0), Vec2::new(40.0, 30.0)]);
    push_prim(&mut l, Kind::Polyline, red, 0, 0.0, &[Vec2::new(100.0, 100.0), Vec2::new(120.0, 101.0)]);
    let view = View::fit(&l.bounds, 160, 120, 0.05);
    let bg = Rgb(33, 40, 48);
    for o in [
        RasterOptions::default(),
        RasterOptions { antialias: false, ..Default::default() },
        RasterOptions { hairline: 0.5, ..Default::default() },
        RasterOptions { hairline: 0.0, antialias: false, ..Default::default() },
        RasterOptions { px_per_mm: 4.0, background: Rgb(255, 255, 255), ..Default::default() },
        RasterOptions { hairline: 1.5, background: bg, ..Default::default() },
    ] {
        let fast = render(&l, &view, &o).unwrap();
        let reference = render_one_by_one(&l, &view, &o);
        assert!(fast.data() == reference.data(), "pixels differ with {o:?}");
    }
}

/// A real hatch pattern (many parallel lines in one colour) renders exactly as stroked line by
/// line.
#[test]
fn raster_hatch_pattern_matches_one_by_one() {
    let mut d = sample();
    d.add(&Space::Model, Common::default(), EntityKind::Hatch(pattern_hatch("ANSI31", 0.2, (0.0, 0.0, 10.0, 10.0)))).unwrap();
    let l = build(&d, &Space::Model, &Options::default());
    assert!(l.prims.len() > 20);
    let view = View::fit(&l.bounds, 300, 200, 0.05);
    for o in [RasterOptions::default(), RasterOptions { background: Rgb(255, 255, 255), ..Default::default() }] {
        assert!(render(&l, &view, &o).unwrap().data() == render_one_by_one(&l, &view, &o).data());
    }
}

/// `n` references to a nested block (`Outer` holds two `Inner`, each three lines) along the x
/// axis, with a plain line after every tenth.
fn many_nested_inserts(n: usize) -> Drawing {
    let mut d = Drawing::new_imperial();
    let mut inner = cadcraft_doc::Block::new("Inner");
    for k in 0..3u64 {
        let (c, kind) = line_common(Common::default());
        inner.entities.push(block_entity(1 + k, c, kind));
    }
    d.blocks.insert("Inner".into(), std::sync::Arc::new(inner));
    let mut outer = cadcraft_doc::Block::new("Outer");
    for k in 0..2u32 {
        let EntityKind::Insert(mut i) = insert_of("Inner") else { unreachable!() };
        i.insert = Vec3::new(f64::from(k) * 20.0, 5.0, 0.0);
        outer.entities.push(block_entity(10 + u64::from(k), Common::default(), EntityKind::Insert(i)));
    }
    d.blocks.insert("Outer".into(), std::sync::Arc::new(outer));
    for k in 0..n {
        let EntityKind::Insert(mut i) = insert_of("Outer") else { unreachable!() };
        i.insert = Vec3::new(k as f64 * 100.0, 0.0, 0.0);
        d.add(&Space::Model, Common::default(), EntityKind::Insert(i)).unwrap();
        if k % 10 == 9 {
            let (c, kind) = line_common(Common::default());
            d.add(&Space::Model, c, kind).unwrap();
        }
    }
    d
}

/// Once the block-expansion cap is reached, a slice built on all cores is the list a one-thread
/// build makes (the first block contents in draw order take the budget), every run; and the build
/// says the cap was passed. Below the cap nothing is reported and the lists are the same too.
#[test]
fn a_slice_past_the_block_cap_is_the_serial_list_every_run() {
    let d = many_nested_inserts(600);
    let ents: Vec<&cadcraft_doc::Entity> = d.model.iter().map(|e| e.as_ref()).collect();
    assert!(ents.len() >= 2 * crate::pipeline::PARALLEL_CHUNK && ents.len() >= 600);
    let same = |a: &DisplayList, b: &DisplayList| {
        assert!(a.prims == b.prims && a.verts == b.verts && a.tris == b.tris, "lists differ");
        assert_eq!(a.expanded, b.expanded);
    };
    let capped = Options { max_block_expansion: 2_000, ..Default::default() };
    let serial = build_entities(&d, ents.iter().copied(), &capped);
    let full = build_entities(&d, ents.iter().copied(), &Options::default());
    assert!(capped.blocks_truncated(serial.expanded), "{}", serial.expanded);
    assert!(!Options::default().blocks_truncated(full.expanded));
    assert!(serial.prims.len() < full.prims.len() && !serial.prims.is_empty());
    for _ in 0..30 {
        same(&build_slice(&d, &ents, &capped, 0), &serial);
        same(&build(&d, &Space::Model, &capped), &serial);
    }
    // Slices after the first start from what the earlier ones expanded: together the serial list.
    let (head, tail) = ents.split_at(300);
    let first = build_slice(&d, head, &capped, 0);
    let second = build_slice(&d, tail, &capped, first.expanded);
    assert_eq!(first.prims.len() + second.prims.len(), serial.prims.len());
    assert_eq!(second.expanded, serial.expanded);
    // Under the cap: the parallel list, equal to the serial one.
    same(&build_slice(&d, &ents, &Options::default(), 0), &full);
}

fn uncapped() -> Options {
    Options { max_block_expansion: usize::MAX, ..Default::default() }
}

fn layer_named(name: &str, on: bool) -> cadcraft_doc::Layer {
    cadcraft_doc::Layer { name: name.into(), on, ..Default::default() }
}

/// With blocks drawn as boxes the count still is what an unlimited build expands, for each block
/// of the drawing alone and for all of them, in the screen build and the plot; and the references
/// the census says are drawn as boxes are the outlines (5-point polylines: these drawings hold
/// only lines otherwise) the build draws.
fn count_equals_build_with_boxes(d: &Drawing, space: &Space, what: &str) {
    let names: Vec<String> = d.blocks.keys().cloned().collect();
    let mut sets: Vec<Vec<String>> = names.iter().map(|n| vec![n.clone()]).collect();
    sets.push(names.clone());
    for set in sets {
        let boxes = std::sync::Arc::new(BoxBlocks::new(d, &set).unwrap());
        let o = Options { blocks_as_boxes: Some(boxes), ..uncapped() };
        let built = build(d, space, &o);
        let c = census_space(d, space, false, &o);
        assert_eq!(c.total, built.expanded as u64, "{what}: boxes {set:?}");
        let plot = build_plot(d, space, &o).expanded as u64;
        assert_eq!(count_space_with(d, space, true, &o), plot, "{what}: boxes {set:?} (plot)");
        // A subset of the objects (what a tile or an export slice builds): the count of those objects is
        // what `build_entities` expands.
        if matches!(space, Space::Model) {
            let subset: Vec<&cadcraft_doc::Entity> = d.model.iter().map(|e| e.as_ref()).step_by(3).collect();
            let built = build_entities(d, subset.iter().copied(), &o).expanded as u64;
            assert_eq!(census_entities(d, &subset, false, &o).total, built, "{what}: boxes {set:?} (subset)");
        }
        if matches!(what, "nested" | "layers and arrays" | "self reference") {
            let outlines = built.prims.iter().filter(|p| p.kind == Kind::Polyline && p.len == 5).count() as u64;
            assert_eq!(c.boxed.iter().map(|b| b.outlines).sum::<u64>(), outlines, "{what}: boxes {set:?}");
            assert_eq!(c.boxed.len(), set.len(), "{what}");
        }
    }
}

/// What the census says drawing one block as boxes leaves is what a count with that block boxed
/// says (so, what a build expands), for every block of the drawing that saves anything: the saving
/// is worked out from how often each copy is expanded, not by a walk per block, and must agree with
/// the walks, also for a block that is part of itself and for layouts.
fn heaviest_equals_walks(d: &Drawing, space: &Space, what: &str) {
    let o = Options { max_block_expansion: 1, ..Default::default() };
    let c = census_space(d, space, false, &o);
    let mut expected = Vec::new();
    for name in d.blocks.keys() {
        let boxed = Options { blocks_as_boxes: Some(std::sync::Arc::new(BoxBlocks::new(d, std::slice::from_ref(name)).unwrap())), ..uncapped() };
        let after = census_space(d, space, false, &boxed).total;
        if after < c.total {
            expected.push((name.clone(), after));
        }
    }
    expected.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let got: Vec<(String, u64)> = c.heaviest.iter().map(|h| (h.block.clone(), h.total_if_boxed)).collect();
    // The list ends where boxing all of it fits (never at a limit of 1 here, unless one block saves everything).
    assert_eq!(got, expected, "{what}");
    // The layers: what the census says switching each reference layer off leaves is what a count with it
    // off says.
    let mut layers_expected = Vec::new();
    for l in &c.reference_layers {
        let mut off = d.clone();
        if let Some(layer) = off.layer_mut(l) {
            layer.on = false;
        }
        let after = census_space(&off, space, false, &uncapped()).total;
        if after < c.total {
            layers_expected.push((l.clone(), after));
        }
    }
    layers_expected.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let layers_got: Vec<(String, u64)> = c.layers_off.iter().map(|l| (l.layer.clone(), l.total_if_off)).collect();
    assert_eq!(layers_got, layers_expected, "{what} (layers)");
}

fn count_space_with(d: &Drawing, space: &Space, plotting: bool, o: &Options) -> u64 {
    census_space(d, space, plotting, o).total
}

/// The count made from the block table alone is the number a build with no limit takes: nested
/// blocks, arrays, layers that hide references (and layer 0 contents following the reference's
/// layer), a block that refers to itself (stopped at the depth limit like the build), and a
/// layout whose viewports show only part of model space.
#[test]
fn the_up_front_count_is_what_an_unlimited_build_expands() {
    let check = |d: &Drawing, space: &Space, what: &str| {
        let built = build(d, space, &uncapped()).expanded as u64;
        assert!(built > 0, "{what}: nothing expanded");
        assert_eq!(count_space(d, space, false), built, "{what}");
        let plot = build_plot(d, space, &uncapped()).expanded as u64;
        assert_eq!(count_space(d, space, true), plot, "{what} (plot)");
        count_equals_build_with_boxes(d, space, what);
        heaviest_equals_walks(d, space, what);
    };
    // Nested blocks, the parallel path (600 > 2 chunks).
    let nested = many_nested_inserts(600);
    check(&nested, &Space::Model, "nested");
    let ents: Vec<&cadcraft_doc::Entity> = nested.model.iter().map(|e| e.as_ref()).collect();
    assert_eq!(count_expansions(&nested, ents.iter().copied(), false), build_entities(&nested, ents.iter().copied(), &uncapped()).expanded as u64);

    // An array of a block that holds a reference, a hidden layer and layer-0 contents.
    let mut d = Drawing::new_imperial();
    d.layers.push(layer_named("Shown", true));
    d.layers.push(layer_named("Hidden", false));
    d.layers.push(layer_named("NoPlot", true));
    if let Some(l) = d.layer_mut("NoPlot") {
        l.plot = false;
    }
    let mut leaf = cadcraft_doc::Block::new("Leaf");
    for (k, layer) in ["0", "Shown", "Hidden", "0"].into_iter().enumerate() {
        let (c, kind) = line_common(Common { layer: layer.into(), ..Default::default() });
        leaf.entities.push(block_entity(1 + k as u64, c, kind));
    }
    d.blocks.insert("Leaf".into(), std::sync::Arc::new(leaf));
    let mut mid = cadcraft_doc::Block::new("Mid");
    for (k, layer) in ["0", "Hidden", "Shown", "NoPlot"].into_iter().enumerate() {
        let EntityKind::Insert(i) = insert_of("Leaf") else { unreachable!() };
        mid.entities.push(block_entity(20 + k as u64, Common { layer: layer.into(), ..Default::default() }, EntityKind::Insert(i)));
    }
    d.blocks.insert("Mid".into(), std::sync::Arc::new(mid));
    for layer in ["Shown", "Hidden", "NoPlot", "0"] {
        let EntityKind::Insert(mut i) = insert_of("Mid") else { unreachable!() };
        i.cols = 3;
        i.rows = 4;
        i.col_spacing = 50.0;
        i.row_spacing = 50.0;
        d.add(&Space::Model, Common { layer: layer.into(), ..Default::default() }, EntityKind::Insert(i)).unwrap();
    }
    check(&d, &Space::Model, "layers and arrays");

    // A block that refers to itself twice: the depth limit stops it.
    let mut d = Drawing::new_imperial();
    let mut s = cadcraft_doc::Block::new("S");
    for k in 0..2u64 {
        let EntityKind::Insert(i) = insert_of("S") else { unreachable!() };
        s.entities.push(block_entity(1 + k, Common::default(), EntityKind::Insert(i)));
    }
    let (c, kind) = line_common(Common::default());
    s.entities.push(block_entity(9, c, kind));
    d.blocks.insert("S".into(), std::sync::Arc::new(s));
    d.add(&Space::Model, Common::default(), insert_of("S")).unwrap();
    check(&d, &Space::Model, "self reference");

    // A layout: one viewport shows the left part of model space with a layer frozen in it, a
    // second is off, and a paper-space reference.
    let mut d = many_nested_inserts(40);
    let paper = Space::Paper("Layout1".into());
    let mut v1 = vp(Vec2::new(50.0, 50.0), 100.0, 100.0, Vec2::new(1000.0, 0.0), 400.0, 2);
    v1.frozen_layers = vec!["Hidden".into()];
    let mut v2 = vp(Vec2::new(50.0, 50.0), 100.0, 100.0, Vec2::new(3000.0, 0.0), 400.0, 3);
    v2.on = false;
    for v in [v1, v2] {
        d.add(&paper, Common::default(), EntityKind::Viewport(v)).unwrap();
    }
    d.add(&paper, Common::default(), insert_of("Outer")).unwrap();
    // Annotation: dimensions with a block, multileaders with block content and annotative
    // references (see `annotative_dimensions_count_what_the_build_expands`), also nested inside
    // a block, where nothing is scaled and a dimension keeps its block.
    let mut a = Drawing::new_imperial();
    a.blocks.insert("DB".into(), std::sync::Arc::new(lines_block("DB", 5)));
    a.blocks.insert("B".into(), std::sync::Arc::new(lines_block("B", 3)));
    let mut holder = cadcraft_doc::Block::new("Holder");
    holder.entities.push(block_entity(1, annotative(), dim_with_block("DB")));
    holder.entities.push(block_entity(2, annotative(), mleader_with_block("B")));
    holder.entities.push(block_entity(3, annotative(), insert_of("B")));
    a.blocks.insert("Holder".into(), std::sync::Arc::new(holder));
    for kind in [dim_with_block("DB"), mleader_with_block("B"), insert_of("Holder")] {
        a.add(&Space::Model, Common::default(), kind.clone()).unwrap();
        a.add(&Space::Model, annotative(), kind).unwrap();
    }
    check(&a, &Space::Model, "dimensions, multileaders and annotative references");
    let paper = Space::Paper("Layout1".into());
    a.add(&paper, annotative(), dim_with_block("DB")).unwrap();
    a.add(&paper, annotative(), insert_of("Holder")).unwrap();
    a.add(&paper, Common::default(), EntityKind::Viewport(vp(Vec2::new(50.0, 50.0), 100.0, 100.0, Vec2::new(0.0, 0.0), 400.0, 2))).unwrap();
    check(&a, &paper, "annotation in a layout");

    let in_view = count_space(&d, &paper, false);
    let all_model = count_space(&d, &Space::Model, false);
    assert!(in_view > 0 && in_view < all_model, "{in_view} of {all_model}");
    check(&d, &paper, "layout");
}

/// Annotative at 1:1 (every viewport and the model show them: `ANNOALLVISIBLE` is on).
fn annotative() -> Common {
    Common { anno_scales: vec![cadcraft_doc::AnnoScale::new("1:1", 1.0, 1.0)], ..Default::default() }
}

/// A block of `n` lines.
fn lines_block(name: &str, n: u64) -> cadcraft_doc::Block {
    let mut b = cadcraft_doc::Block::new(name);
    for k in 0..n {
        let (c, kind) = line_common(Common::default());
        b.entities.push(block_entity(1 + k, c, kind));
    }
    b
}

fn dim_with_block(block: &str) -> EntityKind {
    EntityKind::Dimension(cadcraft_doc::Dimension { block: Some(block.into()), ..lin_dim() })
}

fn mleader_with_block(block: &str) -> EntityKind {
    let EntityKind::Insert(ins) = insert_of(block) else { unreachable!() };
    EntityKind::MLeader(cadcraft_doc::MLeader {
        leaders: Vec::new(),
        landing: Vec3::ZERO,
        dogleg: 0.0,
        text: None,
        style: "Standard".into(),
        arrow_size: 1.0,
        spline: false,
        block: Some(ins),
    })
}

/// Annotation objects: annotative dimensions at the current scale draw from their definition
/// (`anno_scaled` drops the stored block), so they expand nothing, in model space, in a layout's
/// paper space and through a viewport; plain dimensions, annotative multileaders and annotative
/// references keep their blocks. The count is the unlimited build's `expanded` in each, so a
/// drawing of annotative dimensions is not counted past what is built.
#[test]
fn annotative_dimensions_count_what_the_build_expands() {
    let check = |d: &Drawing, space: &Space, what: &str| -> u64 {
        let built = build(d, space, &uncapped()).expanded as u64;
        assert_eq!(count_space(d, space, false), built, "{what}");
        assert_eq!(count_space(d, space, true), build_plot(d, space, &uncapped()).expanded as u64, "{what} (plot)");
        built
    };
    let mut d = Drawing::new_imperial();
    d.blocks.insert("DB".into(), std::sync::Arc::new(lines_block("DB", 5)));
    d.blocks.insert("B".into(), std::sync::Arc::new(lines_block("B", 3)));
    let paper = Space::Paper("Layout1".into());
    // Only annotative dimensions: nothing is expanded, nothing is counted.
    for _ in 0..30 {
        d.add(&Space::Model, annotative(), dim_with_block("DB")).unwrap();
    }
    d.add(&paper, annotative(), dim_with_block("DB")).unwrap();
    assert_eq!(check(&d, &Space::Model, "annotative dimensions in model space"), 0);
    assert_eq!(check(&d, &paper, "annotative dimension in a layout"), 0);
    // Through a viewport: the model's annotative dimensions are drawn scaled there too.
    d.add(&paper, Common::default(), EntityKind::Viewport(vp(Vec2::new(50.0, 50.0), 100.0, 100.0, Vec2::new(0.0, 0.0), 400.0, 2))).unwrap();
    assert_eq!(check(&d, &paper, "annotative dimensions seen through a viewport"), 0);
    // Plain dimensions with a block expand it (5 entities each), as do annotative multileaders
    // with block content (the reference's copy and 3 entities) and annotative references.
    for _ in 0..4 {
        d.add(&Space::Model, Common::default(), dim_with_block("DB")).unwrap();
    }
    assert_eq!(check(&d, &Space::Model, "plain dimensions"), 4 * 5);
    for _ in 0..3 {
        d.add(&Space::Model, annotative(), mleader_with_block("B")).unwrap();
        d.add(&Space::Model, annotative(), insert_of("B")).unwrap();
    }
    assert_eq!(check(&d, &Space::Model, "plain dimensions, annotative multileaders and references"), 4 * 5 + 6 * 4);
    d.add(&paper, annotative(), mleader_with_block("B")).unwrap();
    d.add(&paper, annotative(), insert_of("B")).unwrap();
    d.add(&paper, Common::default(), dim_with_block("DB")).unwrap();
    assert!(check(&d, &paper, "a layout with annotation and a viewport") > 0);
    // A scale the view does not support, ANNOALLVISIBLE off: hidden, nothing.
    let mut h = Drawing::new_imperial();
    h.blocks.insert("B".into(), std::sync::Arc::new(lines_block("B", 3)));
    h.header.set("ANNOALLVISIBLE", cadcraft_doc::HVal::Int(0));
    let other = Common { anno_scales: vec![cadcraft_doc::AnnoScale::new("1:2", 1.0, 2.0)], ..Default::default() };
    h.add(&Space::Model, other, insert_of("B")).unwrap();
    assert_eq!(check(&h, &Space::Model, "hidden by scale"), 0);
}

/// A dimension block that holds dimensions whose block is itself: ten of them, 16 levels deep, is
/// 10^16 expansions, and counting it is the arithmetic over one block, at once (it was a walk of
/// every node), and stops when the build's cancel flag is set.
#[test]
fn a_dimension_block_of_dimensions_is_counted_at_once() {
    let mut d = Drawing::new_imperial();
    let mut b = cadcraft_doc::Block::new("D");
    for k in 0..10u64 {
        b.entities.push(block_entity(1 + k, Common::default(), dim_with_block("D")));
    }
    d.blocks.insert("D".into(), std::sync::Arc::new(b));
    d.add(&Space::Model, Common::default(), dim_with_block("D")).unwrap();
    let t = std::time::Instant::now();
    let n = count_space(&d, &Space::Model, false);
    // f(15) = 10 and f(depth) = 10 (1 + f(depth + 1)): 10 + 100 + ... + 10^16.
    assert_eq!(n, 11_111_111_111_111_110);
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
    // The same block is also a reference cost: references to it (copy unit and AttDefs left out)
    // are memoised separately and agree with the build on a size that can be built.
    let mut small = Drawing::new_imperial();
    let mut b = cadcraft_doc::Block::new("D");
    b.entities.push(block_entity(1, Common::default(), dim_with_block("D")));
    b.entities.push(block_entity(9, Common::default(), insert_of("L")));
    small.blocks.insert("D".into(), std::sync::Arc::new(b));
    small.blocks.insert("L".into(), std::sync::Arc::new(lines_block("L", 1)));
    small.add(&Space::Model, Common::default(), dim_with_block("D")).unwrap();
    small.add(&Space::Model, Common::default(), insert_of("D")).unwrap();
    assert_eq!(count_space(&small, &Space::Model, false), build(&small, &Space::Model, &uncapped()).expanded as u64);
    // A count that is stopped returns at once (the caller throws it away).
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let opts = Options { cancel: Some(stop), ..Default::default() };
    assert_eq!(census_space(&d, &Space::Model, false, &opts).total, 0);
}

/// Bytes the display list holds per expanded entity, for the block-expansion write-up
/// (`cargo test -p cadcraft-render bytes_per_expanded -- --nocapture --ignored`).
#[test]
#[ignore]
fn bytes_per_expanded_entity() {
    let d = many_nested_inserts(20_000);
    let l = build(&d, &Space::Model, &uncapped());
    let bytes = l.prims.len() * std::mem::size_of::<DPrim>() + (l.verts.len() + l.tris.len()) * std::mem::size_of::<Vec2>();
    println!(
        "expanded {} prims {} verts {} tris {} size_of DPrim {} -> {:.1} bytes per expanded entity, {:.1} per primitive",
        l.expanded,
        l.prims.len(),
        l.verts.len(),
        l.tris.len(),
        std::mem::size_of::<DPrim>(),
        bytes as f64 / l.expanded as f64,
        bytes as f64 / l.prims.len() as f64
    );
}

/// Counting against building on a nested site layout: `Tree` (a circle and six lines), `Row`
/// (100 trees), `Grid` (100 rows), `n` grids in model space. Prints the count, how long it took,
/// and the same for building (`cargo test -p cadcraft-render nested_count_vs_build -- --ignored --nocapture`).
#[test]
#[ignore]
fn nested_count_vs_build() {
    let mut d = Drawing::new_imperial();
    let mut tree = cadcraft_doc::Block::new("Tree");
    tree.entities.push(block_entity(1, Common::default(), EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::ZERO, radius: 2.0 })));
    for k in 0..6u64 {
        let (c, kind) = line_common(Common::default());
        tree.entities.push(block_entity(2 + k, c, kind));
    }
    d.blocks.insert("Tree".into(), std::sync::Arc::new(tree));
    for (name, inner, step) in [("Row", "Tree", (10.0, 0.0)), ("Grid", "Row", (0.0, 10.0))] {
        let mut b = cadcraft_doc::Block::new(name);
        for k in 0..100u64 {
            let EntityKind::Insert(mut i) = insert_of(inner) else { unreachable!() };
            i.insert = Vec3::new(step.0 * k as f64, step.1 * k as f64, 0.0);
            b.entities.push(block_entity(100 + k, Common::default(), EntityKind::Insert(i)));
        }
        d.blocks.insert(name.into(), std::sync::Arc::new(b));
    }
    for k in 0..20 {
        let EntityKind::Insert(mut i) = insert_of("Grid") else { unreachable!() };
        i.insert = Vec3::new(f64::from(k) * 2000.0, 0.0, 0.0);
        d.add(&Space::Model, Common::default(), EntityKind::Insert(i)).unwrap();
    }
    let t = std::time::Instant::now();
    let n = count_space(&d, &Space::Model, false);
    println!("count {n} in {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let l = build(&d, &Space::Model, &uncapped());
    let held = l.prims.len() * std::mem::size_of::<DPrim>() + (l.verts.len() + l.tris.len()) * 16;
    println!(
        "built expanded {} prims {} held {} MB in {:?}, {:.1} bytes per expanded",
        l.expanded,
        l.prims.len(),
        held >> 20,
        t.elapsed(),
        held as f64 / l.expanded as f64
    );
    let t = std::time::Instant::now();
    let l = build(&d, &Space::Model, &Options::default());
    println!("built at the limit: expanded {} (limit {}) prims {} in {:?}", l.expanded, MAX_BLOCK_EXPANSION, l.prims.len(), t.elapsed());
}

/// A metric drawing with 2,400 model objects (a third of them block references) and a layout:
/// lines below the viewports, a rectangular viewport, a viewport clipped to a triangle, a
/// viewport that is off, lines, a circle and a block reference over them.
fn layout_fixture() -> (Drawing, Space) {
    let mut d = Drawing::new_metric();
    let mut b = cadcraft_doc::Block::new("B");
    for k in 0..2u64 {
        let (c, kind) = line_common(Common { color: cadcraft_geom_red(), ..Default::default() });
        b.entities.push(block_entity(1 + k, c, kind));
    }
    b.entities.push(block_entity(5, Common::default(), EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(5.0, 5.0, 0.0), radius: 3.0 })));
    d.blocks.insert("B".into(), std::sync::Arc::new(b));
    for k in 0..2_400u32 {
        let at = Vec3::new(f64::from(k % 80) * 10.0, f64::from(k / 80) * 10.0, 0.0);
        let kind = match k % 3 {
            0 => {
                let EntityKind::Insert(mut i) = insert_of("B") else { unreachable!() };
                i.insert = at;
                EntityKind::Insert(i)
            }
            1 => EntityKind::Line(cadcraft_doc::Line { a: at, b: at + Vec3::new(6.0, 4.0, 0.0) }),
            _ => EntityKind::Circle(cadcraft_doc::Circle { center: at, radius: 2.0 }),
        };
        d.add(
            &Space::Model,
            Common {
                color: cadcraft_geom_red(),
                lineweight: cadcraft_doc::Lineweight::Mm100(if k % 5 == 0 { 100 } else { 25 }),
                ..Default::default()
            },
            kind,
        )
        .unwrap();
    }
    let paper = Space::Paper("Layout1".into());
    let line = |x: f64, y: f64| EntityKind::Line(cadcraft_doc::Line { a: Vec3::new(x, y, 0.0), b: Vec3::new(x + 120.0, y + 30.0, 0.0) });
    let wide = Common { color: cadcraft_geom_red(), lineweight: cadcraft_doc::Lineweight::Mm100(100), ..Default::default() };
    d.add(&paper, Common::default(), line(0.0, 10.0)).unwrap();
    d.add(&paper, wide.clone(), line(0.0, 40.0)).unwrap();
    d.add(&paper, Common::default(), EntityKind::Viewport(vp(Vec2::new(60.0, 60.0), 100.0, 80.0, Vec2::new(300.0, 150.0), 320.0, 2))).unwrap();
    let tri = [(150.0, 20.0), (250.0, 20.0), (200.0, 100.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y))).to_vec();
    let boundary = d
        .add(
            &paper,
            Common::default(),
            EntityKind::LwPolyline(cadcraft_doc::LwPolyline { vertices: tri, closed: true, const_width: 0.0, elevation: 0.0, plinegen: false }),
        )
        .unwrap();
    let mut clipped = vp(Vec2::new(200.0, 60.0), 100.0, 80.0, Vec2::new(500.0, 150.0), 340.0, 3);
    clipped.clip = Some(boundary);
    d.add(&paper, Common::default(), EntityKind::Viewport(clipped)).unwrap();
    let mut off = vp(Vec2::new(300.0, 60.0), 40.0, 40.0, Vec2::new(100.0, 100.0), 80.0, 4);
    off.on = false;
    d.add(&paper, Common::default(), EntityKind::Viewport(off)).unwrap();
    d.add(&paper, wide, line(20.0, 80.0)).unwrap();
    d.add(&paper, Common::default(), EntityKind::Circle(cadcraft_doc::Circle { center: Vec3::new(100.0, 60.0, 0.0), radius: 30.0 })).unwrap();
    let EntityKind::Insert(mut i) = insert_of("B") else { unreachable!() };
    i.insert = Vec3::new(90.0, 50.0, 0.0);
    d.add(&paper, Common::default(), EntityKind::Insert(i)).unwrap();
    (d, paper)
}

fn cadcraft_geom_red() -> cadcraft_color::Color {
    cadcraft_color::Color::Index(1)
}

/// The primitives of `lists` in order, each with its points (the arrays they index differ).
fn flat(lists: &[DisplayList]) -> Vec<(DPrim, Vec<Vec2>)> {
    lists.iter().flat_map(|l| l.prims.iter().map(|p| (DPrim { start: 0, ..*p }, l.points(p).to_vec()))).collect()
}

/// A layout built in pieces is `whole`, the list `build` makes of it: the same primitives in the
/// same order and the same expansion count, for slices of 1, 7, 512 and 16,384 objects.
fn assert_slices_are_the_whole(d: &Drawing, name: &str, opts: &Options, whole: &DisplayList, what: &str) {
    for slice in [1, 7, 512, 16_384] {
        let mut lists = Vec::new();
        let expanded = build_layout_slices::<()>(d, name, opts, slice, &mut |l| {
            lists.push(l.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(expanded, whole.expanded, "{what}, slice {slice}");
        assert!(flat(&lists) == flat(std::slice::from_ref(whole)), "{what}, slice {slice}: {} pieces", lists.len());
    }
}

/// The handle of the `k`th viewport of layout `paper`.
fn viewport_handle(d: &Drawing, paper: &Space, k: usize) -> cadcraft_doc::Handle {
    d.space(paper).unwrap().iter().filter(|e| matches!(e.kind, EntityKind::Viewport(_))).nth(k).unwrap().handle
}

/// A layout built in pieces is the whole build's list: the same primitives in the same order, for
/// slices of 1, 7, 512 and 16,384 objects, under the block-expansion limit and past it (the budget
/// is taken in draw order, so the same ones are left out), and with a plot style table (pens, the
/// pattern budget carried from piece to piece). Also with a layer frozen in a viewport, a viewport
/// whose contents are skipped (`skip_viewport`), annotative objects and a wipeout seen through a
/// viewport, and a layout with more viewports than are drawn.
#[test]
fn a_layout_built_in_slices_is_the_whole_list() {
    let (d, paper) = layout_fixture();
    let Space::Paper(name) = &paper else { unreachable!() };
    let table = {
        let t = cadcraft_doc::PlotStyleTable::color_dependent("pens.ctb", "", |i| {
            (i == 1).then(|| cadcraft_doc::PlotStyle {
                lineweight: Some(100),
                end: cadcraft_doc::EndStyle::Square,
                join: cadcraft_doc::JoinStyle::Diamond,
                fill: cadcraft_doc::FillStyle::Checkerboard,
                ..Default::default()
            })
        });
        Some(std::sync::Arc::new(t))
    };
    let full = build(&d, &paper, &Options { lineweights: true, ..Default::default() });
    let need = full.expanded;
    assert!(need > 3_000 && full.prims.len() > 3_000, "{need} {}", full.prims.len());
    for (what, opts) in [
        ("under the limit", Options { lineweights: true, ..Default::default() }),
        ("past the limit", Options { lineweights: true, max_block_expansion: need / 3, ..Default::default() }),
        ("plot styles", Options { lineweights: true, plot_style_table: table, ..Default::default() }),
    ] {
        let whole = build(&d, &paper, &opts);
        if what == "past the limit" {
            assert!(opts.blocks_truncated(whole.expanded), "{what}");
        }
        for _ in 0..3 {
            assert!(flat(&[build(&d, &paper, &opts)]) == flat(std::slice::from_ref(&whole)), "{what}: the whole build changes between runs");
        }
        assert_slices_are_the_whole(&d, name, &opts, &whole, what);
    }

    // Layers frozen in a viewport, annotative objects and a wipeout seen through viewports (one
    // of them frozen on the layer of some of the model objects): under and past the limit.
    let (mut v, paper) = layout_fixture();
    v.layers.push(layer_named("Trees", true));
    for k in 0..40u32 {
        let at = Vec3::new(f64::from(k) * 15.0, 130.0, 0.0);
        let EntityKind::Insert(mut i) = insert_of("B") else { unreachable!() };
        i.insert = at;
        v.add(&Space::Model, Common { layer: "Trees".into(), ..Default::default() }, EntityKind::Insert(i.clone())).unwrap();
        v.add(
            &Space::Model,
            Common { anno_scales: vec![cadcraft_doc::AnnoScale::new("1:2", 1.0, 2.0)], ..Default::default() },
            EntityKind::Insert(i),
        )
        .unwrap();
    }
    let wipe = [(150.0, 100.0), (400.0, 100.0), (400.0, 160.0), (150.0, 160.0)].map(|(x, y)| Vec2::new(x, y)).to_vec();
    v.add(&Space::Model, Common::default(), EntityKind::Wipeout(cadcraft_doc::Wipeout { boundary: wipe })).unwrap();
    let mut frozen = vp(Vec2::new(60.0, 160.0), 100.0, 80.0, Vec2::new(300.0, 130.0), 160.0, 7);
    frozen.frozen_layers = vec!["Trees".into()];
    v.add(&paper, Common::default(), EntityKind::Viewport(frozen)).unwrap();
    v.add(&paper, Common::default(), EntityKind::Viewport(vp(Vec2::new(200.0, 160.0), 100.0, 80.0, Vec2::new(300.0, 130.0), 160.0, 8))).unwrap();
    let Space::Paper(name) = &paper else { unreachable!() };
    let skipped = viewport_handle(&v, &paper, 1);
    let need = build(&v, &paper, &uncapped()).expanded;
    assert!(need > 3_000, "{need}");
    for (what, opts) in [
        ("frozen layer, annotative objects and a wipeout", Options::default()),
        ("the same past the limit", Options { max_block_expansion: need / 3, ..Default::default() }),
        ("skip_viewport", Options { skip_viewport: Some(skipped), ..Default::default() }),
        ("skip_viewport past the limit", Options { skip_viewport: Some(skipped), max_block_expansion: need / 4, ..Default::default() }),
    ] {
        let whole = build(&v, &paper, &opts);
        assert!(whole.prims.iter().any(|p| p.kind == Kind::Mask), "{what}: the wipeout's mask is in the list");
        assert_slices_are_the_whole(&v, name, &opts, &whole, what);
    }
    // The skipped viewport's contents are left out, the frozen layer's are not drawn there.
    let all = build(&v, &paper, &Options::default());
    let skip = build(&v, &paper, &Options { skip_viewport: Some(skipped), ..Default::default() });
    assert!(skip.expanded < all.expanded, "{} {}", skip.expanded, all.expanded);

    // More viewports than are drawn (256): the rest are left out in the slices too.
    let mut m = Drawing::new_metric();
    m.blocks.insert("B".into(), std::sync::Arc::new(lines_block("B", 3)));
    for k in 0..30u32 {
        let EntityKind::Insert(mut i) = insert_of("B") else { unreachable!() };
        i.insert = Vec3::new(f64::from(k) * 10.0, 0.0, 0.0);
        m.add(&Space::Model, Common::default(), EntityKind::Insert(i)).unwrap();
    }
    let paper = Space::Paper("Layout1".into());
    for k in 0..(crate::viewports::MAX_VIEWPORTS + 4) {
        let v =
            vp(Vec2::new(10.0 * (k % 20) as f64, 10.0 * (k / 20) as f64), 8.0, 8.0, Vec2::new(30.0 + (k % 7) as f64 * 40.0, 0.0), 30.0, 2 + k as u32);
        m.add(&paper, Common::default(), EntityKind::Viewport(v)).unwrap();
    }
    let Space::Paper(name) = &paper else { unreachable!() };
    let whole = build(&m, &paper, &Options::default());
    let drawn = build(&m, &paper, &Options { max_block_expansion: whole.expanded / 2, ..Default::default() });
    assert!(drawn.expanded > 0 && whole.expanded > 0);
    let few = {
        // The same layout without the four viewports past the limit draws the same list.
        let mut c = m.clone();
        let extra: Vec<cadcraft_doc::Handle> = c.space(&paper).unwrap().iter().skip(crate::viewports::MAX_VIEWPORTS).map(|e| e.handle).collect();
        assert_eq!(extra.len(), 4);
        for h in extra {
            c.remove_entity(h);
        }
        build(&c, &paper, &Options::default())
    };
    assert!(flat(std::slice::from_ref(&few)) == flat(std::slice::from_ref(&whole)), "viewports past MAX_VIEWPORTS are not drawn");
    assert_slices_are_the_whole(&m, name, &Options::default(), &whole, "more viewports than drawn");
    let capped = Options { max_block_expansion: whole.expanded / 2, ..Default::default() };
    assert_slices_are_the_whole(&m, name, &capped, &drawn, "more viewports than drawn, past the limit");
}

/// A block with one line on layer `contents` and, in a row of `n` references to it, one reference
/// per entity of the model on layer `refs`.
fn refs_on_layers(n: u32, refs: &str, contents: &str) -> Drawing {
    let mut d = Drawing::new_imperial();
    d.layers.push(layer_named(refs, true));
    d.layers.push(layer_named(contents, true));
    let mut b = cadcraft_doc::Block::new("Tree");
    for k in 0..4u64 {
        let (c, kind) = line_common(Common { layer: contents.into(), ..Default::default() });
        b.entities.push(block_entity(1 + k, c, kind));
    }
    d.blocks.insert("Tree".into(), std::sync::Arc::new(b));
    for k in 0..n {
        let EntityKind::Insert(mut i) = insert_of("Tree") else { unreachable!() };
        i.insert = Vec3::new(f64::from(k) * 20.0, 0.0, 0.0);
        d.add(&Space::Model, Common { layer: refs.into(), ..Default::default() }, EntityKind::Insert(i)).unwrap();
    }
    d
}

/// Which layer to switch off: the build takes one expansion for every entity of a block before it
/// looks at the entity's layer, so the layer of a block's contents reduces nothing and the layer
/// the references are on removes them all. The census lists the reference layers (and, past the
/// limit, what each would leave), not the contents' layers.
#[test]
fn only_the_layer_of_the_references_reduces_the_count() {
    let mut d = refs_on_layers(50, "Refs", "Contents");
    let full = build(&d, &Space::Model, &uncapped()).expanded;
    assert_eq!(full, 50 * 5);
    let opts = Options { max_block_expansion: 100, ..Default::default() };
    let c = census_space(&d, &Space::Model, false, &opts);
    assert_eq!(c.total, 250);
    assert_eq!(c.reference_layers, vec!["Refs".to_string()]);
    assert_eq!(c.layers_off, vec![LayerOff { layer: "Refs".into(), total_if_off: 0 }]);
    // Under the limit no recount is made.
    assert!(census_space(&d, &Space::Model, false, &Options::default()).layers_off.is_empty());
    // The contents' layer off: the build still asks for the same (the references are drawn, their
    // contents expanded, then not shown); the references' layer off removes every expansion.
    d.layer_mut("Contents").unwrap().on = false;
    assert_eq!(build(&d, &Space::Model, &uncapped()).expanded, full);
    d.layer_mut("Contents").unwrap().on = true;
    d.layer_mut("Refs").unwrap().on = false;
    assert_eq!(build(&d, &Space::Model, &uncapped()).expanded, 0);
    // Two layers of references: switching either off leaves the other's.
    let mut two = refs_on_layers(10, "Refs", "Contents");
    two.layers.push(layer_named("Other", true));
    for k in 0..30u32 {
        let EntityKind::Insert(mut i) = insert_of("Tree") else { unreachable!() };
        i.insert = Vec3::new(f64::from(k) * 20.0, 50.0, 0.0);
        two.add(&Space::Model, Common { layer: "Other".into(), ..Default::default() }, EntityKind::Insert(i)).unwrap();
    }
    let c = census_space(&two, &Space::Model, false, &Options { max_block_expansion: 100, ..Default::default() });
    assert_eq!(c.total, 200);
    assert_eq!(c.layers_off, vec![LayerOff { layer: "Other".into(), total_if_off: 50 }, LayerOff { layer: "Refs".into(), total_if_off: 150 }]);
    two.layer_mut("Other").unwrap().on = false;
    assert_eq!(build(&two, &Space::Model, &uncapped()).expanded, 50);
}

/// The largest single top-level object (windows cannot split it) is found in the counting pass,
/// with its handle, kind and block; arrays past the 10,000 side clamp are named; a count that
/// overflows is `COUNT_SATURATED`.
#[test]
fn the_census_names_the_largest_object_clamped_arrays_and_overflow() {
    // One 1000 x 1000 MINSERT of a two-line block and many small references.
    let mut d = Drawing::new_imperial();
    d.blocks.insert("Pole".into(), std::sync::Arc::new(lines_block("Pole", 2)));
    for _ in 0..5 {
        d.add(&Space::Model, Common::default(), insert_of("Pole")).unwrap();
    }
    let EntityKind::Insert(mut big) = insert_of("Pole") else { unreachable!() };
    big.cols = 1000;
    big.rows = 1000;
    big.col_spacing = 5.0;
    big.row_spacing = 5.0;
    let h = d.add(&Space::Model, Common::default(), EntityKind::Insert(big)).unwrap();
    let c = census_space(&d, &Space::Model, false, &Options::default());
    assert_eq!(c.total, 5 * 3 + 3_000_000);
    let l = c.largest.unwrap();
    assert_eq!((l.handle, l.kind, l.block.as_str(), l.count), (h, "array of block references (MINSERT)", "Pole", 3_000_000));
    assert!(c.clamped.is_empty());

    // 20,000 x 1: drawn as 10,000 x 1, and said so.
    let mut w = Drawing::new_imperial();
    w.blocks.insert("Pole".into(), std::sync::Arc::new(lines_block("Pole", 1)));
    let EntityKind::Insert(mut wide) = insert_of("Pole") else { unreachable!() };
    wide.cols = 20_000;
    wide.col_spacing = 2.0;
    let h = w.add(&Space::Model, Common::default(), EntityKind::Insert(wide)).unwrap();
    let c = census_space(&w, &Space::Model, false, &Options::default());
    assert_eq!(c.clamped, vec![ClampedArray { handle: h, block: "Pole".into(), cols: 20_000, rows: 1, drawn_cols: 10_000, drawn_rows: 1 }]);
    assert_eq!(c.total, build(&w, &Space::Model, &uncapped()).expanded as u64);
    assert_eq!(build(&w, &Space::Model, &uncapped()).prims.len(), 10_000);

    // Ten levels of 1,000 references: 10^30 does not fit.
    let mut o = Drawing::new_imperial();
    o.blocks.insert("L10".into(), std::sync::Arc::new(lines_block("L10", 1)));
    for k in (0..10).rev() {
        let mut b = cadcraft_doc::Block::new(&format!("L{k}"));
        let EntityKind::Insert(mut i) = insert_of(&format!("L{}", k + 1)) else { unreachable!() };
        i.cols = 1000;
        i.col_spacing = 1.0;
        b.entities.push(block_entity(1, Common::default(), EntityKind::Insert(i)));
        o.blocks.insert(format!("L{k}"), std::sync::Arc::new(b));
    }
    o.add(&Space::Model, Common::default(), insert_of("L0")).unwrap();
    let c = census_space(&o, &Space::Model, false, &Options::default());
    assert_eq!(c.total, COUNT_SATURATED);
    assert_eq!(c.largest.unwrap().count, COUNT_SATURATED);
}

/// The layers the references are on are one entry per layer whatever the case of the name, and a
/// reference on a layer the table does not have is not offered as a layer to switch off (it is
/// said apart, so "all references are on one layer" is not claimed).
#[test]
fn reference_layers_ignore_case_and_leave_out_layers_missing_from_the_table() {
    let mut d = refs_on_layers(10, "Trees", "Contents");
    for (k, layer) in ["TREES", "trees", "Ghost"].into_iter().enumerate() {
        let EntityKind::Insert(mut i) = insert_of("Tree") else { unreachable!() };
        i.insert = Vec3::new(k as f64 * 20.0, 50.0, 0.0);
        d.add(&Space::Model, Common { layer: layer.into(), ..Default::default() }, EntityKind::Insert(i)).unwrap();
    }
    d.layers.retain(|l| l.name != "Ghost");
    let opts = Options { max_block_expansion: 10, ..Default::default() };
    let c = census_space(&d, &Space::Model, false, &opts);
    assert_eq!(c.reference_layers, vec!["Trees".to_string()]);
    assert!(c.references_off_table);
    // Switching "Trees" off removes the 12 references on it (any spelling) and leaves the ghost's.
    assert_eq!(c.layers_off, vec![LayerOff { layer: "Trees".into(), total_if_off: 5 }]);
    d.layer_mut("Trees").unwrap().on = false;
    assert_eq!(build(&d, &Space::Model, &uncapped()).expanded, 5);
}

/// A block `name` whose contents fill the box (0, 0) to (10, 4): a line along each of two sides.
fn box_block(name: &str) -> cadcraft_doc::Block {
    let mut b = cadcraft_doc::Block::new(name);
    let line = |to: Vec3| EntityKind::Line(cadcraft_doc::Line { a: Vec3::ZERO, b: to });
    b.entities.push(block_entity(1, Common::default(), line(Vec3::new(10.0, 0.0, 0.0))));
    b.entities.push(block_entity(2, Common::default(), line(Vec3::new(0.0, 4.0, 0.0))));
    b
}

fn insert_struct(block: &str) -> cadcraft_doc::Insert {
    cadcraft_doc::Insert {
        block: block.into(),
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

fn boxed_options(d: &Drawing, names: &[&str]) -> Options {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    Options { blocks_as_boxes: Some(std::sync::Arc::new(BoxBlocks::new(d, &names).unwrap())), ..uncapped() }
}

fn near(a: Vec2, x: f64, y: f64) -> bool {
    (a.x - x).abs() < 1e-9 && (a.y - y).abs() < 1e-9
}

/// A reference to a boxed block is ONE closed outline of the block's extents under the reference's
/// transform, in the reference's colour: a rotated, scaled reference gives a rotated rectangle, a
/// MINSERT array is one outline of the whole array (also when turned), a base point is honoured,
/// and the contents are not drawn.
#[test]
fn a_boxed_reference_is_one_outline_of_its_extents_under_its_transform() {
    let mut d = Drawing::new_imperial();
    d.blocks.insert("Bx".into(), std::sync::Arc::new(box_block("Bx")));
    let red = Common { color: Color::Index(1), ..Default::default() };
    let at = |x: f64, y: f64, scale: (f64, f64), rotation: f64, cols: u32, rows: u32| {
        let mut i = insert_struct("bx");
        i.insert = Vec3::new(x, y, 0.0);
        i.scale = Vec3::new(scale.0, scale.1, 1.0);
        i.rotation = rotation;
        i.cols = cols;
        i.rows = rows;
        i.col_spacing = 30.0;
        i.row_spacing = 20.0;
        EntityKind::Insert(i)
    };
    // Scale (2, 3), turned a quarter: the box 0..10 x 0..4 becomes 0..20 x 0..12, then x' = -y, y' = x.
    d.add(&Space::Model, red.clone(), at(100.0, 50.0, (2.0, 3.0), std::f64::consts::FRAC_PI_2, 1, 1)).unwrap();
    // A 3 x 2 array, not turned.
    d.add(&Space::Model, red.clone(), at(0.0, 200.0, (1.0, 1.0), 0.0, 3, 2)).unwrap();
    // The same array turned by 30 degrees.
    let turn = 30f64.to_radians();
    d.add(&Space::Model, red.clone(), at(500.0, 0.0, (1.0, 1.0), turn, 3, 2)).unwrap();
    let list = build(&d, &Space::Model, &boxed_options(&d, &["Bx"]));
    assert_eq!(list.prims.len(), 3, "one outline per reference, contents not drawn");
    assert_eq!(list.expanded, 3, "one unit each");
    assert!(list.prims.iter().all(|p| p.kind == Kind::Polyline && p.len == 5 && p.color == Rgb(255, 0, 0)));
    let pts = |k: usize| list.points(&list.prims[k]).to_vec();
    let q = pts(0);
    let mut got: Vec<(i64, i64)> = q[..4].iter().map(|p| ((p.x * 1e6).round() as i64, (p.y * 1e6).round() as i64)).collect();
    got.sort();
    assert_eq!(got, vec![(88_000_000, 50_000_000), (88_000_000, 70_000_000), (100_000_000, 50_000_000), (100_000_000, 70_000_000)]);
    assert!(near(q[4], q[0].x, q[0].y), "closed");
    // The array: 10 + 2 x 30 wide and 4 + 20 high, one rectangle.
    let a = pts(1);
    let b = Bounds2::from_points(a.iter().copied());
    assert!(near(b.min, 0.0, 200.0) && near(b.max, 70.0, 224.0), "{b:?}");
    // Turned: the same rectangle seen in the turned frame, about the insertion point.
    let t = pts(2);
    let back = Mat3::rotate(-turn);
    let local: Vec<Vec2> = t.iter().map(|p| back.apply(*p - Vec2::new(500.0, 0.0))).collect();
    let lb = Bounds2::from_points(local.iter().copied());
    assert!(near(lb.min, 0.0, 0.0) && near(lb.max, 70.0, 24.0), "{lb:?}");
    // Every point is a corner of that rectangle (a rotated rectangle, not its bounding box).
    assert!(local[..4].iter().all(|p| (p.x.abs() < 1e-9 || (p.x - 70.0).abs() < 1e-9) && (p.y.abs() < 1e-9 || (p.y - 24.0).abs() < 1e-9)));
    // The same drawing unboxed draws the contents: 2 lines in each of 1 + 6 + 6 copies.
    let full = build(&d, &Space::Model, &uncapped());
    assert_eq!(full.prims.len(), 2 * 13);
}

/// A reference nested in a block is boxed too, once per copy of the block holding it, with the
/// transforms of both; the base point of the boxed block moves the outline; a block with no
/// contents draws no outline but still takes its unit; a reference on a layer that is off draws nothing.
#[test]
fn a_boxed_block_nested_in_another_is_boxed_at_every_depth() {
    let mut d = Drawing::new_imperial();
    let mut inner = box_block("Inner");
    inner.base = Vec3::new(2.0, 1.0, 0.0);
    d.blocks.insert("Inner".into(), std::sync::Arc::new(inner));
    d.blocks.insert("Empty".into(), std::sync::Arc::new(cadcraft_doc::Block::new("Empty")));
    let mut outer = cadcraft_doc::Block::new("Outer");
    let mut i = insert_struct("Inner");
    i.insert = Vec3::new(5.0, 0.0, 0.0);
    i.rotation = std::f64::consts::FRAC_PI_2;
    outer.entities.push(block_entity(1, Common::default(), EntityKind::Insert(i)));
    outer.entities.push(block_entity(2, Common::default(), insert_of("Empty")));
    d.blocks.insert("Outer".into(), std::sync::Arc::new(outer));
    d.layers.push(layer_named("Off", false));
    for x in [100.0, 300.0] {
        let mut o = insert_struct("Outer");
        o.insert = Vec3::new(x, 200.0, 0.0);
        d.add(&Space::Model, Common::default(), EntityKind::Insert(o)).unwrap();
    }
    d.add(&Space::Model, Common { layer: "Off".into(), ..Default::default() }, insert_of("Outer")).unwrap();
    let o = boxed_options(&d, &["INNER", "empty"]);
    let list = build(&d, &Space::Model, &o);
    // Per Outer copy: the Outer's copy unit, 2 entities, and one unit for each boxed reference: 1 + 2 + 2.
    assert_eq!(list.expanded, 2 * 5);
    assert_eq!(list.prims.len(), 2);
    let c = census_space(&d, &Space::Model, false, &o);
    assert_eq!(c.total, 10);
    let mut tally: Vec<(String, u64)> = c.boxed.iter().map(|b| (b.block.clone(), b.outlines)).collect();
    tally.sort();
    assert_eq!(tally, vec![("Empty".to_string(), 0), ("Inner".to_string(), 2)]);
    // Inner's box is (0..10, 0..4) around base (2, 1): in Inner's frame x in -2..8, y in -1..3; turned a
    // quarter (x' = -y, y' = x) and moved by (5, 0): x in 2..6, y in -2..8; then by the Outer's insertion point.
    let q = list.points(&list.prims[0]);
    let b = Bounds2::from_points(q.iter().copied());
    assert!(near(b.min, 102.0, 198.0) && near(b.max, 106.0, 208.0), "{b:?}");
}

/// Blocks that save the most when drawn as boxes, biggest saving first, ending where boxing all
/// the listed ones fits; every figure is what an unlimited build with that choice expands; and
/// when even boxing every block does not fit, every block is listed and the figure says so.
#[test]
fn the_heaviest_blocks_are_listed_down_to_where_boxing_them_fits() {
    let mut d = Drawing::new_imperial();
    // Tree 20 lines, Shrub 8, Stake 2; 10 / 10 / 10 references: 210 + 90 + 30 = 330 units.
    for (name, n) in [("Tree", 20u64), ("Shrub", 8), ("Stake", 2)] {
        d.blocks.insert(name.into(), std::sync::Arc::new(lines_block(name, n)));
        for k in 0..10 {
            let mut i = insert_struct(name);
            i.insert = Vec3::new(f64::from(k) * 20.0, 0.0, 0.0);
            d.add(&Space::Model, Common::default(), EntityKind::Insert(i)).unwrap();
        }
    }
    let cap = |n: usize| Options { max_block_expansion: n, ..Default::default() };
    let c = census_space(&d, &Space::Model, false, &cap(100));
    assert_eq!(c.total, 330);
    // Alone: Tree leaves 130 (10 + 90 + 30), Shrub 250, Stake 310. Tree + Shrub leave 50: fits.
    assert_eq!(
        c.heaviest,
        vec![BlockSaving { block: "Tree".into(), total_if_boxed: 130 }, BlockSaving { block: "Shrub".into(), total_if_boxed: 250 }]
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(c.total_if_listed_boxed, 50);
    // Each figure is what a build with that block boxed expands.
    for h in &c.heaviest {
        let o = boxed_options(&d, &[h.block.as_str()]);
        assert_eq!(build(&d, &Space::Model, &o).expanded as u64, h.total_if_boxed);
    }
    let all = boxed_options(&d, &["Tree", "Shrub"]);
    assert_eq!(build(&d, &Space::Model, &all).expanded as u64, 50);
    // A limit of 140: Tree alone fits, so the list ends at Tree.
    let c = census_space(&d, &Space::Model, false, &cap(140));
    assert_eq!(c.heaviest.iter().map(|h| h.block.as_str()).collect::<Vec<_>>(), vec!["Tree"]);
    assert_eq!(c.total_if_listed_boxed, 130);
    // A limit of 20: all three boxed leave 30 (still over): every block is listed.
    let c = census_space(&d, &Space::Model, false, &cap(20));
    assert_eq!(c.heaviest.iter().map(|h| h.block.as_str()).collect::<Vec<_>>(), vec!["Tree", "Shrub", "Stake"]);
    assert_eq!(c.total_if_listed_boxed, 30);
    // With Tree already boxed the figures are relative to that choice, and Tree is not offered again.
    let c = census_space(&d, &Space::Model, false, &Options { max_block_expansion: 100, ..boxed_options(&d, &["Tree"]) });
    assert_eq!(c.total, 130);
    assert_eq!(c.heaviest.iter().map(|h| (h.block.as_str(), h.total_if_boxed)).collect::<Vec<_>>(), vec![("Shrub", 50)]);
    // Under the limit nothing is worked out.
    assert!(census_space(&d, &Space::Model, false, &cap(1_000)).heaviest.is_empty());
}

/// What the refusal costs on a big drawing: the count, the per-layer and per-block tables and the
/// list of heaviest blocks, for drawings with `blocks` distinct blocks referred to `refs` times each
/// (the limit is set to a quarter of the total, so every table is made):
/// `cargo test -p cadcraft-render refusal_cost --release -- --ignored --nocapture`.
#[test]
#[ignore]
fn refusal_cost() {
    for (blocks, refs, lines, nested) in
        [(50usize, 2_000usize, 20u64, false), (500, 1_000, 8, false), (5_000, 100, 8, false), (20_000, 30, 5, false), (2_000, 300, 5, true)]
    {
        let mut d = Drawing::new_imperial();
        for b in 0..blocks {
            let name = format!("B{b}");
            let mut blk = lines_block(&name, lines);
            if nested {
                // Each block also holds three references to other blocks.
                for k in 0..3usize {
                    let i = insert_struct(&format!("B{}", (b + 1 + k * 7) % blocks.max(1)));
                    if (b + 1 + k * 7) % blocks > b {
                        blk.entities.push(block_entity(100 + k as u64, Common::default(), EntityKind::Insert(i)));
                    }
                }
            }
            d.blocks.insert(name, std::sync::Arc::new(blk));
        }
        for b in 0..blocks {
            for r in 0..refs {
                let mut i = insert_struct(&format!("B{b}"));
                i.insert = Vec3::new(r as f64 * 10.0, b as f64 * 10.0, 0.0);
                d.add(&Space::Model, Common::default(), EntityKind::Insert(i)).unwrap();
            }
        }
        let total = count_space(&d, &Space::Model, false);
        let t = std::time::Instant::now();
        let c = census_space(&d, &Space::Model, false, &Options { max_block_expansion: (total / 4) as usize, ..Default::default() });
        println!(
            "{blocks} blocks x {refs} refs ({} objects, nested {nested}): total {total}, tables in {:?}: {} blocks listed, boxing them leaves {}",
            d.model.len(),
            t.elapsed(),
            c.heaviest.len(),
            c.total_if_listed_boxed
        );
    }
}

/// `n` blocks `S0`..`S{n-1}`, each a line and `selfs` references to itself, each referred to `refs`
/// times from model space (`Cyc` extra: see the saturating shape).
fn self_referencing_blocks(n: usize, selfs: usize, refs: usize) -> Drawing {
    let mut d = Drawing::new_imperial();
    for b in 0..n {
        let name = format!("S{b}");
        let mut blk = lines_block(&name, 1);
        for k in 0..selfs {
            blk.entities.push(block_entity(100 + k as u64, Common::default(), EntityKind::Insert(insert_struct(&name))));
        }
        d.blocks.insert(name.clone(), std::sync::Arc::new(blk));
        for r in 0..refs {
            let mut i = insert_struct(&name);
            i.insert = Vec3::new(r as f64 * 10.0, b as f64 * 10.0, 0.0);
            d.add(&Space::Model, Common::default(), EntityKind::Insert(i)).unwrap();
        }
    }
    d
}

fn census_time(d: &Drawing, cap: usize) -> (std::time::Duration, BlockCensus) {
    let t = std::time::Instant::now();
    let c = census_space(d, &Space::Model, false, &Options { max_block_expansion: cap, ..Default::default() });
    (t.elapsed(), c)
}

/// The refusal's tables cost about as much as the drawing is big, not its square: blocks that refer to
/// themselves (each a cycle of its own), a saturated total with one block that overflows, and thousands
/// of layers; and the figures are still what a count says. (Before: 2,000 self-referencing blocks took 43 s.)
#[test]
fn the_refusal_grows_with_the_drawing_not_with_its_square() {
    // The figures are exact on a small one (cycles: the nested path).
    let small = self_referencing_blocks(30, 2, 1);
    heaviest_equals_walks(&small, &Space::Model, "self referencing blocks");
    // Time: 2,000 self-referencing blocks, each referred to once.
    let (t, c) = census_time(&self_referencing_blocks(2_000, 2, 1), 100);
    assert!(c.total > 100 && !c.heaviest.is_empty());
    assert!(t < std::time::Duration::from_secs(10), "{t:?}");
    // 4,000 blocks x 30 references plus one block holding 20 references to itself: the total saturates.
    let mut d = self_referencing_blocks(4_000, 0, 30);
    let mut cyc = lines_block("Cyc", 1);
    for k in 0..20u64 {
        cyc.entities.push(block_entity(100 + k, Common::default(), EntityKind::Insert(insert_struct("Cyc"))));
    }
    d.blocks.insert("Cyc".into(), std::sync::Arc::new(cyc));
    d.add(&Space::Model, Common::default(), EntityKind::Insert(insert_struct("Cyc"))).unwrap();
    let (t, c) = census_time(&d, 1_000);
    assert_eq!(c.total, COUNT_SATURATED);
    assert!(t < std::time::Duration::from_secs(10), "{t:?}");
    // Boxing Cyc alone leaves the 120,000 others' pieces + 1: that is what a walk says too.
    let boxed = Options { blocks_as_boxes: Some(std::sync::Arc::new(BoxBlocks::new(&d, &["Cyc".to_string()]).unwrap())), ..uncapped() };
    let after = census_space(&d, &Space::Model, false, &boxed).total;
    assert_eq!(c.heaviest.iter().map(|h| (h.block.as_str(), h.total_if_boxed)).collect::<Vec<_>>(), vec![("Cyc", after)]);
    // 3,000 layers of references, layer 0 contents following them.
    let mut d = Drawing::new_imperial();
    d.blocks.insert("Tree".into(), std::sync::Arc::new(lines_block("Tree", 4)));
    for l in 0..3_000 {
        d.layers.push(layer_named(&format!("L{l}"), true));
        for r in 0..100 {
            let mut i = insert_struct("Tree");
            i.insert = Vec3::new(r as f64 * 10.0, l as f64 * 10.0, 0.0);
            d.add(&Space::Model, Common { layer: format!("L{l}"), ..Default::default() }, EntityKind::Insert(i)).unwrap();
        }
    }
    let (t, c) = census_time(&d, 1_000);
    assert_eq!(c.layers_off.len(), 3_000);
    assert_eq!(c.layers_off[0].total_if_off, 300_000 * 5 / 2 * 2 - 500);
    assert!(t < std::time::Duration::from_secs(10), "{t:?}");
}

/// A stop during the tables ends them early (the callers then discard the count).
#[test]
fn a_stop_ends_the_heaviest_pass() {
    let d = self_referencing_blocks(6_000, 0, 20);
    let (full, _) = census_time(&d, 100);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let o = Options { max_block_expansion: 100, cancel: Some(cancel.clone()), ..Default::default() };
    let flag = cancel.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(full / 4);
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let t = std::time::Instant::now();
    let _ = census_space(&d, &Space::Model, false, &o);
    let stopped = t.elapsed();
    let _ = stopper.join();
    assert!(stopped < full.mul_f64(0.9), "{stopped:?} against {full:?}");
}

/// `cargo test -p cadcraft-render refusal_shapes --release -- --ignored --nocapture`
#[test]
#[ignore]
fn refusal_shapes() {
    for n in [250, 1_000, 2_000, 4_000, 20_000] {
        let (t, c) = census_time(&self_referencing_blocks(n, 2, 1), 100);
        println!("{n} self-referencing blocks: {t:?} ({} listed)", c.heaviest.len());
    }
    let mut d = self_referencing_blocks(4_000, 0, 30);
    let mut cyc = lines_block("Cyc", 1);
    for k in 0..20u64 {
        cyc.entities.push(block_entity(100 + k, Common::default(), EntityKind::Insert(insert_struct("Cyc"))));
    }
    d.blocks.insert("Cyc".into(), std::sync::Arc::new(cyc));
    d.add(&Space::Model, Common::default(), EntityKind::Insert(insert_struct("Cyc"))).unwrap();
    println!("4000 blocks x 30 + saturating Cyc: {:?}", census_time(&d, 1_000).0);
    for layers in [1_000, 5_000, 20_000] {
        let mut d = Drawing::new_imperial();
        d.blocks.insert("Tree".into(), std::sync::Arc::new(lines_block("Tree", 4)));
        for l in 0..layers {
            d.layers.push(layer_named(&format!("L{l}"), true));
            for r in 0..(300_000 / layers) {
                let mut i = insert_struct("Tree");
                i.insert = Vec3::new(r as f64 * 10.0, l as f64 * 10.0, 0.0);
                d.add(&Space::Model, Common { layer: format!("L{l}"), ..Default::default() }, EntityKind::Insert(i)).unwrap();
            }
        }
        println!("{layers} reference layers ({} objects): {:?}", d.model.len(), census_time(&d, 1_000).0);
    }
}

/// The cases where a box is easy to get wrong still agree with the build: a mirrored reference, a zero
/// scale, a non-uniformly scaled rotated MINSERT nested under a ByBlock reference (its outline takes
/// the parent's colour, not the layer's), a 12,000-column array of a block that holds a boxed MINSERT
/// (the array is drawn with 10,000), counted and built over the whole drawing and over a subset.
#[test]
fn boxes_agree_with_the_build_for_odd_references() {
    let mut d = Drawing::new_imperial();
    d.blocks.insert("Inner".into(), std::sync::Arc::new(box_block("Inner")));
    // Outer: a 3 x 2 array of Inner, scaled (2, 0.5), turned 0.7, colour ByBlock.
    let mut outer = cadcraft_doc::Block::new("Outer");
    let mut i = insert_struct("Inner");
    i.scale = Vec3::new(2.0, 0.5, 1.0);
    i.rotation = 0.7;
    (i.cols, i.rows, i.col_spacing, i.row_spacing) = (3, 2, 15.0, 9.0);
    outer.entities.push(block_entity(1, Common { color: Color::ByBlock, ..Default::default() }, EntityKind::Insert(i)));
    d.blocks.insert("Outer".into(), std::sync::Arc::new(outer));
    // Row: a block holding a MINSERT of Inner; an array of 12,000 Rows.
    let mut row = cadcraft_doc::Block::new("Row");
    let mut m = insert_struct("Inner");
    (m.cols, m.rows, m.col_spacing, m.row_spacing) = (3, 3, 12.0, 6.0);
    row.entities.push(block_entity(1, Common::default(), EntityKind::Insert(m)));
    d.blocks.insert("Row".into(), std::sync::Arc::new(row));
    let red = Common { color: Color::Index(1), ..Default::default() };
    let mut mirrored = insert_struct("Inner");
    mirrored.scale = Vec3::new(-1.0, 2.0, 1.0);
    mirrored.rotation = 0.4;
    d.add(&Space::Model, Common::default(), EntityKind::Insert(mirrored)).unwrap();
    let mut zero = insert_struct("Inner");
    zero.scale = Vec3::new(0.0, 0.0, 1.0);
    d.add(&Space::Model, Common::default(), EntityKind::Insert(zero)).unwrap();
    d.add(&Space::Model, red, EntityKind::Insert(insert_struct("Outer"))).unwrap();
    let mut big = insert_struct("Row");
    (big.cols, big.col_spacing) = (12_000, 50.0);
    d.add(&Space::Model, Common::default(), EntityKind::Insert(big)).unwrap();
    count_equals_build_with_boxes(&d, &Space::Model, "odd references");
    heaviest_equals_walks(&d, &Space::Model, "odd references");
    // Inner boxed: the red Outer's nested outlines (6 cells) are red, not the layer's colour.
    let o = boxed_options(&d, &["Inner"]);
    let list = build(&d, &Space::Model, &o);
    let red_outlines = list.prims.iter().filter(|p| p.kind == Kind::Polyline && p.len == 5 && p.color == Rgb(255, 0, 0)).count();
    assert_eq!(red_outlines, 1, "the MINSERT of Inner inside Outer is one outline, in the parent's colour");
    let c = census_space(&d, &Space::Model, false, &o);
    // 2 top-level + 1 in Outer + 10,000 Rows (12,000 clamped), each holding one MINSERT outline.
    assert_eq!(c.boxed.iter().map(|b| b.outlines).sum::<u64>(), 2 + 1 + 10_000);
    // The zero-scale reference draws an outline of a point, not nothing and not a crash.
    assert!(list.prims.iter().any(|p| p.kind == Kind::Polyline && p.len == 5));
}
