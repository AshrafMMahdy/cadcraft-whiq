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
            line += &format!(" built {} prims {} bytes {} per_expanded {:.0}", l.expanded, l.prims.len(), held, held as f64 / l.expanded.max(1) as f64);
        }
        println!("{line}");
    }
}

/// A PDF plot says per sheet how many block expansions it needed and drew; under the limit
/// nothing is truncated, and the figure is the build's count.
#[test]
fn a_pdf_plot_reports_its_block_expansions() {
    let d = read(&minsert(3, 2));
    let plotted = cadcraft_io::plot_with_blocks(&d, &Space::Model, &serde_json::json!({})).unwrap();
    assert!(plotted.bytes.starts_with(b"%PDF"));
    // Six copies, each the copy itself and its one line.
    let [sheet] = &plotted.blocks[..] else { panic!("{:?}", plotted.blocks) };
    assert_eq!((sheet.sheet.as_str(), sheet.total, sheet.expanded, sheet.limit), ("Model", 12, 12, MAX_BLOCK_EXPANSION));
    assert!(!sheet.truncated() && sheet.not_drawn() == 0);
    assert_eq!(sheet.census.total, 12);
    assert_eq!(cadcraft_io::plot(&d, &Space::Model, &serde_json::json!({})).unwrap().len(), plotted.bytes.len());
}

/// A real plot past the limit (lowered through `PdfOptions`, the way the render tests lower
/// `Options::max_block_expansion`): the sheet is truncated, says what the block table asks for and
/// what was not drawn, and names the one object that needs it all.
#[test]
fn a_pdf_plot_past_the_limit_says_how_much_was_not_drawn() {
    let d = read(&minsert(10, 10));
    let o = cadcraft_io::PdfOptions { max_block_expansion: 50, ..Default::default() };
    let plotted = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[Space::Model], &o).unwrap();
    assert!(plotted.bytes.starts_with(b"%PDF"));
    let [sheet] = &plotted.blocks[..] else { panic!("{:?}", plotted.blocks) };
    // 100 copies, each the copy and its line: 200 asked for; the build stopped past 50.
    assert!(sheet.truncated());
    assert_eq!((sheet.total, sheet.limit, sheet.not_drawn()), (200, 50, 150));
    assert!(sheet.expanded > 50 && sheet.expanded < 200, "{}", sheet.expanded);
    let largest = sheet.census.largest.as_ref().unwrap();
    assert_eq!((largest.kind, largest.block.as_str(), largest.count), ("array of block references (MINSERT)", "A", 200));
    // The same plot with the real limit draws it whole.
    let whole = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &[Space::Model], &cadcraft_io::PdfOptions::default()).unwrap();
    assert!(!whole.blocks[0].truncated());
    assert_eq!(whole.blocks[0].total, 200);
    assert!(whole.bytes.len() > plotted.bytes.len());
}
