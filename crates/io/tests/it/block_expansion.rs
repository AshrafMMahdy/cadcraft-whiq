//! Nested, self-referencing and arrayed block references in a hostile DXF must not hang drawing
//! or extents (issue #255): the expansion is bounded, not only its depth.

use cadcraft_doc::{Drawing, EntityKind, Space};
use cadcraft_render::{MAX_BLOCK_EXPANSION, Options, build};

fn read(text: &str) -> Drawing {
    cadcraft_io::read(text.as_bytes(), "x.dxf").unwrap()
}

/// Block `A` holds a line and `refs` references to itself; model space holds one `A`.
fn self_referencing(refs: usize) -> String {
    let mut s = String::from("0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n2\nA\n0\nLINE\n10\n0\n20\n0\n11\n1\n21\n1\n");
    for k in 1..=refs {
        s += &format!("0\nINSERT\n2\nA\n10\n{k}\n20\n0\n");
    }
    s + "0\nENDBLK\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nINSERT\n2\nA\n10\n0\n20\n0\n0\nENDSEC\n0\nEOF\n"
}

/// Blocks `F0`..`F14`, each holding four references to the next; `F15` holds a line (drawn
/// 4^15 times: its references sit exactly at `MAX_BLOCK_DEPTH`).
fn fan_out() -> String {
    let mut s = String::from("0\nSECTION\n2\nBLOCKS\n");
    for k in 0..15 {
        s += &format!("0\nBLOCK\n2\nF{k}\n");
        for x in 0..4 {
            s += &format!("0\nINSERT\n2\nF{}\n10\n{x}\n20\n0\n", k + 1);
        }
        s += "0\nENDBLK\n";
    }
    s + "0\nBLOCK\n2\nF15\n0\nLINE\n10\n0\n20\n0\n11\n1\n21\n1\n0\nENDBLK\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nINSERT\n2\nF0\n10\n0\n20\n0\n0\nENDSEC\n0\nEOF\n"
}

/// One MINSERT of a one-line block with `cols` x `rows` copies 2 units apart.
fn minsert(cols: u32, rows: u32) -> String {
    format!(
        "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n2\nA\n0\nLINE\n10\n0\n20\n0\n11\n1\n21\n1\n0\nENDBLK\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nINSERT\n2\nA\n10\n0\n20\n0\n70\n{cols}\n71\n{rows}\n44\n2\n45\n2\n0\nENDSEC\n0\nEOF\n"
    )
}

#[test]
fn block_expansion_bombs_are_bounded() {
    for text in [self_referencing(4), self_referencing(12), fan_out(), minsert(10_000, 10_000)] {
        let d = read(&text);
        let ext = d.extents(&Space::Model);
        assert!(!ext.is_empty() && ext.min.is_finite() && ext.max.is_finite(), "{ext:?}");
        let list = build(&d, &Space::Model, &Options::default());
        assert!(!list.prims.is_empty() && list.prims.len() <= MAX_BLOCK_EXPANSION, "{}", list.prims.len());
        // Saving computes the extents again ($EXTMIN/$EXTMAX).
        assert!(cadcraft_io::write_dxf(&d).contains("$EXTMAX"));
    }
}

#[test]
fn ordinary_nested_and_arrayed_blocks_are_drawn_whole() {
    // 3 x 2 copies of a one-line block: six lines, extents spanning all copies.
    let d = read(&minsert(3, 2));
    assert_eq!(build(&d, &Space::Model, &Options::default()).prims.len(), 6);
    let ext = d.extents(&Space::Model);
    assert_eq!((ext.min.x, ext.min.y, ext.max.x, ext.max.y), (0.0, 0.0, 5.0, 3.0));
    // A self-referencing block draws its line at every level down to MAX_BLOCK_DEPTH.
    let d = read(&self_referencing(1));
    assert!(matches!(d.model.iter().next().map(|e| &e.kind), Some(EntityKind::Insert(_))));
    assert_eq!(build(&d, &Space::Model, &Options::default()).prims.len(), cadcraft_doc::MAX_BLOCK_DEPTH);
}

/// A survey of the block expansions real drawings ask for, counted from the block table, and the
/// bytes their display lists hold per expanded entity when the count is small enough to build:
/// `BLOCK_SURVEY=/dir1:/dir2 cargo test -p cadcraft-io --test it block_survey -- --ignored --nocapture`.
#[test]
#[ignore]
fn block_survey() {
    use cadcraft_render::{DPrim, count_space};
    let Ok(dirs) = std::env::var("BLOCK_SURVEY") else { return };
    let mut files = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = dirs.split(':').map(Into::into).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            stack.extend(std::fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dxf") || e.eq_ignore_ascii_case("dwg")) {
            files.push(p);
        }
    }
    files.sort();
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let Ok(Ok(d)) = std::panic::catch_unwind(|| cadcraft_io::read(&bytes, &f)) else { continue };
        let model = count_space(&d, &Space::Model, false);
        let layouts: u64 = d.layouts.iter().map(|l| count_space(&d, &Space::Paper(l.name.clone()), false)).max().unwrap_or(0);
        let mut line = format!("{} entities {} blocks {} model {model} layout {layouts}", f.display(), d.model.len(), d.blocks.len());
        if model > 0 && model <= 300_000 {
            let l = build(&d, &Space::Model, &Options::default());
            let held = l.prims.len() * std::mem::size_of::<DPrim>() + (l.verts.len() + l.tris.len()) * 16;
            line +=
                &format!(" built {} prims {} bytes {} per_expanded {:.0}", l.expanded, l.prims.len(), held, held as f64 / l.expanded.max(1) as f64);
        }
        println!("{line}");
    }
}

/// A PDF plot says per sheet how many block expansions it needs; under the limit the file is made and
/// is the same bytes `plot` (the engine's hook) writes, and the figure is the build's count.
#[test]
fn a_pdf_plot_reports_its_block_expansions() {
    use cadcraft_io::Plot;
    let d = read(&minsert(3, 2));
    let Plot::Drawn(plotted) = cadcraft_io::plot_complete(&d, &Space::Model, &serde_json::json!({}), MAX_BLOCK_EXPANSION, None).unwrap() else {
        panic!("too big")
    };
    assert!(plotted.bytes.starts_with(b"%PDF"));
    // Six copies, each the copy itself and its one line.
    let [sheet] = &plotted.blocks[..] else { panic!("{:?}", plotted.blocks) };
    assert_eq!((sheet.sheet.as_str(), sheet.number, sheet.total, sheet.limit), ("Model", 1, 12, MAX_BLOCK_EXPANSION));
    assert!(!sheet.over_limit());
    assert_eq!(sheet.census.total, 12);
    assert_eq!(cadcraft_io::plot(&d, &Space::Model, &serde_json::json!({})).unwrap(), plotted.bytes);
}

/// A real plot past the limit (lowered through `PdfOptions` or `plot_complete`'s limit, the way the
/// render tests lower `Options::max_block_expansion`) that must be complete makes NO file: it answers what each sheet
/// needs, with the largest object and what drawing a block as a box leaves; the same plot with
/// the block drawn as boxes (or with the real limit) makes the file.
#[test]
fn a_complete_pdf_plot_past_the_limit_makes_no_file() {
    use cadcraft_io::Plot;
    let d = read(&minsert(10, 10));
    let o = cadcraft_io::PdfOptions { max_block_expansion: 50, complete_only: true, ..Default::default() };
    let Plot::TooBig(sheets) = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[Space::Model], &o).unwrap() else { panic!("a file was made") };
    let [sheet] = &sheets[..] else { panic!("{sheets:?}") };
    // 100 copies, each the copy and its line: 200 asked for.
    assert!(sheet.over_limit());
    assert_eq!((sheet.number, sheet.total, sheet.limit), (1, 200, 50));
    let largest = sheet.census.largest.as_ref().unwrap();
    assert_eq!((largest.kind, largest.block.as_str(), largest.count), ("array of block references (MINSERT)", "A", 200));
    // Drawing the block A as a box: one unit for the whole array.
    assert_eq!(sheet.census.heaviest.iter().map(|h| (h.block.as_str(), h.total_if_boxed)).collect::<Vec<_>>(), vec![("A", 1)]);
    let boxed = cadcraft_io::PdfOptions { blocks_as_boxes: vec!["a".into()], ..o.clone() };
    let Plot::Drawn(p) = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[Space::Model], &boxed).unwrap() else { panic!("still too big") };
    assert!(p.bytes.starts_with(b"%PDF"));
    assert_eq!((p.blocks[0].total, p.blocks[0].census.boxed.len()), (1, 1));
    assert_eq!(p.blocks[0].census.boxed[0].outlines, 1);
    // The real limit draws it whole.
    let whole = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[Space::Model], &cadcraft_io::PdfOptions { complete_only: true, ..Default::default() })
        .unwrap();
    let Plot::Drawn(whole) = whole else { panic!("too big") };
    assert_eq!(whole.blocks[0].total, 200);
    // Through the JSON entry point with a lowered limit: the same refusal, with the sheet's page number.
    let json = serde_json::json!({"blocks_as_boxes": ["A"]});
    assert!(matches!(cadcraft_io::plot_complete(&d, &Space::Model, &json, 50, None).unwrap(), Plot::Drawn(_)));
    let Plot::TooBig(s) = cadcraft_io::plot_complete(&d, &Space::Model, &serde_json::json!({}), 50, None).unwrap() else { panic!("a file was made") };
    assert_eq!((s[0].number, s[0].total, s[0].limit), (1, 200, 50));
    // The engine's hook (not `complete_only`) still draws a cut sheet past a lowered limit, as before.
    let cut =
        cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[Space::Model], &cadcraft_io::PdfOptions { max_block_expansion: 50, ..Default::default() })
            .unwrap();
    let Plot::Drawn(cut) = cut else { panic!("the engine's plot was refused") };
    assert!(cut.bytes.starts_with(b"%PDF") && cut.bytes.len() < whole.bytes.len());
}

/// Several sheets are one file: if any sheet is over the limit no file is made, and every sheet's
/// figures come back with its page number so the sheets over the limit can be named.
#[test]
fn a_pdf_of_several_sheets_with_one_over_the_limit_makes_no_file() {
    use cadcraft_doc::{Block, Common, Entity, Handle, Insert, Line};
    use cadcraft_geom::Vec3;
    use cadcraft_io::Plot;
    let mut d = Drawing::new_metric();
    let mut b = Block::new("A");
    b.entities.push(Entity::new(Handle(900), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(1.0, 1.0, 0.0) })));
    d.blocks.insert("A".into(), std::sync::Arc::new(b));
    let insert = |cols: u32, rows: u32| {
        EntityKind::Insert(Insert {
            block: "A".into(),
            insert: Vec3::ZERO,
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: vec![],
            cols,
            rows,
            col_spacing: 2.0,
            row_spacing: 2.0,
        })
    };
    d.layouts = (1..=3).map(|k| cadcraft_doc::Layout::new(&format!("L{k}"), k)).collect();
    d.add(&Space::Model, Common::default(), insert(1, 1)).unwrap();
    d.add(&Space::Paper("L1".into()), Common::default(), insert(1, 1)).unwrap();
    d.add(&Space::Paper("L2".into()), Common::default(), insert(10, 10)).unwrap();
    d.add(&Space::Paper("L3".into()), Common::default(), insert(1, 1)).unwrap();
    let spaces = [Space::Model, Space::Paper("L1".into()), Space::Paper("L2".into()), Space::Paper("L3".into())];
    let o = cadcraft_io::PdfOptions { max_block_expansion: 50, complete_only: true, ..Default::default() };
    let Plot::TooBig(sheets) = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &spaces, &o).unwrap() else { panic!("a file was made") };
    let over: Vec<(usize, &str)> = sheets.iter().filter(|s| s.over_limit()).map(|s| (s.number, s.sheet.as_str())).collect();
    assert_eq!(over, vec![(3, "L2")]);
    assert_eq!(sheets.iter().map(|s| s.total).collect::<Vec<_>>(), vec![2, 2, 200, 2]);
    // Without the sheet over the limit the file is made.
    let ok = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[spaces[0].clone(), spaces[1].clone(), spaces[3].clone()], &o).unwrap();
    assert!(matches!(ok, Plot::Drawn(_)));
}

/// A stop ends the count of a complete plot with an error (no file), and `pages` of one space are
/// counted once, each page still named with its own number.
#[test]
fn a_complete_plot_stops_when_asked_and_counts_identical_pages_once() {
    use cadcraft_io::Plot;
    let d = read(&minsert(10, 10));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(cadcraft_io::plot_complete(&d, &Space::Model, &serde_json::json!({}), 50, Some(stop)).is_err());
    let pages: Vec<serde_json::Value> = (0..3).map(|k| serde_json::json!({"window": [[0, 0], [10 + k, 10]]})).collect();
    let Plot::TooBig(sheets) = cadcraft_io::plot_complete(&d, &Space::Model, &serde_json::json!({"pages": pages}), 50, None).unwrap() else {
        panic!("a file was made")
    };
    assert_eq!(sheets.iter().map(|s| (s.number, s.total)).collect::<Vec<_>>(), vec![(1, 200), (2, 200), (3, 200)]);
}
