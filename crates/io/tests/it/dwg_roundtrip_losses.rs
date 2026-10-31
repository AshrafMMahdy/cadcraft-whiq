//! Data that used to be lost only when a drawing is saved as DWG and reopened (the DWG bridge
//! converts our DXF with acadrust), and the paper-space viewport the DXF writer adds for it.

use cadcraft_color::{Color, Rgb};
use cadcraft_doc::*;
use cadcraft_geom::{Vec2, Vec3};

fn through_dwg(d: &Drawing) -> Drawing {
    let dwg = cadcraft_io::write(d, "x.dwg").unwrap();
    cadcraft_io::read(&dwg, "x.dwg").unwrap()
}

fn line(d: &mut Drawing, space: &Space, common: Common) -> Handle {
    d.add(space, common, EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(10.0, 5.0, 0.0) })).unwrap()
}

fn viewport(id: u32) -> EntityKind {
    EntityKind::Viewport(Viewport {
        center: Vec3::new(50.0, 40.0, 0.0),
        width: 80.0,
        height: 60.0,
        view_center: Vec2::new(10.0, 10.0),
        view_height: 50.0,
        id,
        locked: false,
        frozen_layers: vec![],
        layer_colors: vec![],
        clip: None,
        on: true,
    })
}

/// The (code, value) pairs of every `kind` record in DXF text.
fn records(text: &str, kind: &str) -> Vec<Vec<(String, String)>> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out: Vec<Vec<(String, String)>> = Vec::new();
    let mut inside = false;
    for pair in lines.chunks(2) {
        let [code, value] = pair else { break };
        if *code == "0" {
            inside = *value == kind;
            if inside {
                out.push(Vec::new());
            }
        } else if inside && let Some(rec) = out.last_mut() {
            rec.push((code.to_string(), value.to_string()));
        }
    }
    out
}

fn group<'a>(rec: &'a [(String, String)], code: &str) -> Option<&'a str> {
    rec.iter().find(|(c, _)| c == code).map(|(_, v)| v.as_str())
}

fn viewport_ids(l: &Layout) -> Vec<u32> {
    l.entities.iter().filter_map(|e| if let EntityKind::Viewport(v) = &e.kind { Some(v.id) } else { None }).collect()
}

/// True colours (entity and layer) and entity transparency: a new drawing's DWG is written as
/// AutoCAD 2018 (R2000 DWG has neither), while the DXF the bridge reads stays R2000.
#[test]
fn dwg_keeps_true_colours_and_transparency() {
    let mut d = Drawing::new_metric();
    d.layers.push(Layer { name: "Fancy".into(), color: Color::True(Rgb(12, 200, 99)), ..Layer::default() });
    let h = line(
        &mut d,
        &Space::Model,
        Common { layer: "Fancy".into(), color: Color::True(Rgb(1, 2, 3)), transparency: Transparency::Percent(40), ..Common::default() },
    );

    let dwg = cadcraft_io::write(&d, "x.dwg").unwrap();
    assert_eq!(cadcraft_dwg::version(&dwg).as_deref(), Some("AC1032"));
    let header = records(&cadcraft_io::write_dxf(&d), "SECTION");
    assert_eq!(header.first().and_then(|h| group(h, "1")), Some("AC1015"), "the DXF stays R2000");

    let back = cadcraft_io::read(&dwg, "x.dwg").unwrap();
    let e = back.entity(h).unwrap();
    assert_eq!(e.common.color, Color::True(Rgb(1, 2, 3)));
    assert_eq!(e.common.transparency, Transparency::Percent(40));
    assert_eq!(back.layers.iter().find(|l| l.name == "Fancy").map(|l| l.color), Some(Color::True(Rgb(12, 200, 99))));
}

/// Entities of a layout other than the first stay in that layout.
#[test]
fn dwg_keeps_paper_space_entities_in_their_layout() {
    let mut d = Drawing::new_metric();
    let second = Space::Paper(d.layouts[1].name.clone());
    let h = d.add(&second, Common::default(), EntityKind::Circle(Circle { center: Vec3::new(1.0, 1.0, 0.0), radius: 0.5 })).unwrap();
    let back = through_dwg(&d);
    assert!(back.layouts[1].entities.contains(h), "{:?}", back.layouts.iter().map(|l| l.entities.len()).collect::<Vec<_>>());
    assert!(!back.layouts[0].entities.contains(h));
}

/// Table styles (R2000 DWG has no TABLESTYLE objects).
#[test]
fn dwg_keeps_table_styles() {
    let mut d = Drawing::new_metric();
    let sched = TableStyle { name: "Sched".into(), text_height: 2.5, margin: 1.0, title: false, header: true, ..TableStyle::default() };
    d.table_styles.push(sched.clone());
    let back = through_dwg(&d);
    assert_eq!(back.table_styles.iter().find(|s| s.name == "Sched"), Some(&sched));
}

/// DWG doesn't store viewport ids; readers number a layout's viewports in order, and id 1 is the
/// paper-space viewport (the sheet, not drawn). The writer puts that viewport ahead of a layout's
/// own viewports, so a viewport made in CADCraft (id 2) keeps its id instead of becoming the
/// sheet; our reader drops the added viewport again, so a DXF round trip is unchanged.
#[test]
fn viewport_ids_survive_dwg_and_the_paper_viewport_stays_implicit() {
    let mut d = Drawing::new_metric();
    let first = Space::Paper(d.layouts[0].name.clone());
    let h = d.add(&first, Common::default(), viewport(2)).unwrap();
    d.add(&first, Common::default(), viewport(3)).unwrap();

    let text = cadcraft_io::write_dxf(&d);
    let vps = records(&text, "VIEWPORT");
    assert_eq!(vps.len(), 3, "{text}");
    assert_eq!(group(&vps[0], "69"), Some("1"));
    assert_eq!(group(&vps[0], "1000"), Some("PAPERVIEW"));
    // Viewports are written "on": status 68 positive, and 90 with the always-set bit 32768.
    assert_eq!(group(&vps[1], "68"), Some("2"));
    assert_eq!(group(&vps[1], "90"), Some("32768"));

    let back = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    assert_eq!(viewport_ids(&back.layouts[0]), [2, 3]);
    assert_eq!(back.layouts[0].entities.len(), 2);

    let back = through_dwg(&d);
    assert_eq!(viewport_ids(&back.layouts[0]), [2, 3]);
    assert!(back.layouts[0].entities.contains(h));

    // A layout that has its paper-space viewport (from another program) gets no second one.
    let mut d = Drawing::new_metric();
    d.add(&first, Common::default(), viewport(1)).unwrap();
    d.add(&first, Common::default(), viewport(2)).unwrap();
    let text = cadcraft_io::write_dxf(&d);
    assert!(!text.contains("PAPERVIEW"));
    assert_eq!(viewport_ids(&cadcraft_io::read_dxf(text.as_bytes()).unwrap().layouts[0]), [1, 2]);
}

/// The paper-space viewport (id 1) of a drawing from another program can come after a model
/// viewport in the layout, e.g. when the draw order (SORTENTS) put it behind. DWG numbers in
/// order, so the writer puts it first: otherwise the model viewport became the sheet on reopen.
#[test]
fn a_paper_viewport_behind_a_model_viewport_stays_id_1_through_dwg() {
    let mut d = Drawing::new_metric();
    let names: Vec<String> = d.layouts.iter().map(|l| l.name.clone()).collect();
    for name in &names {
        let space = Space::Paper(name.clone());
        let model = d.add(&space, Common::default(), viewport(2)).unwrap();
        let sheet = d.add(&space, Common::default(), viewport(1)).unwrap();
        let back = through_dwg(&d);
        let l = back.layouts.iter().find(|l| &l.name == name).unwrap();
        let id = |h: Handle| match &l.entities.get(h).map(|e| &e.kind) {
            Some(EntityKind::Viewport(v)) => Some(v.id),
            _ => None,
        };
        assert_eq!((id(model), id(sheet)), (Some(2), Some(1)), "layout {name}");
    }
}

/// MTEXT contents, formatting codes, width, attachment and rotation survive a DWG round trip,
/// also when a tab (`^I`) or a literal caret (`^ `) falls where the writer splits the contents
/// into 250-character groups. The DWG bridge decodes each group on its own: a `^` ending one group
/// and the `I` starting the next came back as the two characters `^I` instead of a tab (the 12th
/// MTEXT of an AutoCAD sample drawing, a font table with a tab on every line).
#[test]
fn dwg_keeps_mtext_contents_formatting_width_attachment_and_rotation() {
    let mut d = Drawing::new_metric();
    let line = "\\fArial|b1|i0|c0|p34;\\C5;abcdefghij\t{\\H0.7x;ABC}\\P";
    let mut texts = Vec::new();
    // The escape at every position around the first and second group boundaries.
    for (k, (pad, esc)) in (240..=252).flat_map(|n| [(n, '\t'), (n, '^'), (n + 250, '\t'), (n + 250, '^')]).enumerate() {
        let contents = format!("{}{esc}I after{}\\Ptail ^ {}", "x".repeat(pad), line.repeat(k % 3), "\t".repeat(k % 2));
        texts.push(MText {
            insert: Vec3::new(k as f64, 2.0 * k as f64, 0.0),
            height: 0.3,
            width: 19.930391160798923 + k as f64,
            attach: u8::try_from(k % 9 + 1).unwrap(),
            rotation: (k as f64 * 7.5).to_radians(),
            style: "Standard".into(),
            contents,
            line_spacing: 1.0,
            line_spacing_exact: false,
            columns: None,
        });
    }
    let handles: Vec<Handle> = texts.iter().map(|t| d.add(&Space::Model, Common::default(), EntityKind::MText(t.clone())).unwrap()).collect();
    for version in [None, Some(cadcraft_io::Version::R2007), Some(cadcraft_io::Version::R2018)] {
        let dwg = cadcraft_io::write_with(&d, "x.dwg", &cadcraft_io::SaveOptions { version, binary: false }).unwrap();
        let back = cadcraft_io::read(&dwg, "x.dwg").unwrap();
        for (h, before) in handles.iter().zip(&texts) {
            let Some(EntityKind::MText(after)) = back.entity(*h).map(|e| &e.kind) else { panic!("{version:?}: MTEXT {h:?} is gone") };
            assert_eq!(after.contents, before.contents, "{version:?}: contents of {h:?}");
            assert_eq!(after.width, before.width, "{version:?}: width of {h:?}");
            assert_eq!(after.attach, before.attach, "{version:?}: attachment of {h:?}");
            // DWG stores the direction: 360° reads back as 0°.
            let turn = (after.rotation - before.rotation).rem_euclid(std::f64::consts::TAU);
            assert!(turn.min(std::f64::consts::TAU - turn) < 1e-9, "{version:?}: rotation of {h:?}: {} -> {}", before.rotation, after.rotation);
            assert_eq!((after.insert, after.height, &after.style), (before.insert, before.height, &before.style), "{version:?}: {h:?}");
        }
    }
}
