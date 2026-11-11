//! WHIQ: the domain tools of `cadcraft-cli mcp` (the drawing engine the White Hat IQ platform
//! drives): open, new, close, save; read and search; the Preview renderer; export; batched edits;
//! the round-trip check. The contract's own tools (server_status, job_status, list_jobs, cancel,
//! evict) are the runtime's (`runtime.rs`).

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{Drawing, EntityKind, Space};
use serde_json::{Map, Value, json};

use super::contract::{ToolError, code};
use super::docs::{Ctx, Doc, Docs, engine_error};
use super::files::{Access, WritePolicy, io_error, old_name, saved_entry, write_file, write_file_with};
use super::{preview, read};

/// What a tool answers: the structured result, extra content blocks (pictures), the text block
/// (markdown) when it is not the JSON itself, and whether the runtime may trim it (a tool that
/// pages itself says no).
pub struct Reply {
    pub value: Value,
    pub extra: Vec<Value>,
    pub text: Option<String>,
    pub trim: bool,
}

impl Reply {
    pub fn json(value: Value) -> Reply {
        Reply { value, extra: Vec::new(), text: None, trim: true }
    }
}

fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty())
}
fn flag(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(Value::as_bool)
}
fn need<'a>(a: &'a Value, k: &str, example: &str) -> Result<&'a str, ToolError> {
    s(a, k).ok_or_else(|| ToolError::new(code::INVALID_ARGUMENT, format!("`{k}` is required"), format!("pass `{k}`, e.g. {example}")))
}
fn usize_arg(a: &Value, k: &str, default: usize) -> usize {
    a.get(k).and_then(Value::as_u64).map_or(default, |n| usize::try_from(n).unwrap_or(usize::MAX))
}

/// The tool definitions (`tools/list`): schema, annotations and `_meta["whiq/files"]`.
pub fn definitions() -> Vec<Value> {
    let doc = json!({"type": ["string", "integer"], "description": "The drawing's handle (doc-N) from open_drawing / new_drawing; default: the active drawing"});
    let window = json!({"type": "array", "description": "[[x0, y0], [x1, y1]] in drawing units of the space"});
    let space = json!({"type": ["string", "array"], "description": "\"all\" (default for reads), \"model\", \"paper\" (every layout), a layout name, or a list"});
    let ro = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    let rw = json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false});
    let writes = json!({"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false});
    vec![
        json!({"name": "open_drawing", "description": "Open a DWG (R13 to 2018+, i.e. AC1012–AC1032) or DXF (R12 to 2018, ASCII or binary) drawing. Answers its `document` handle and a short summary; read_drawing gives the details. A big file opens as a job (job_status).",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string", "description": "The file (inside the read root; relative names are looked up there)"}}, "required": ["path"]},
            "annotations": rw, "_meta": {"whiq/files": {"read": ["path"], "opens": true}}}),
        json!({"name": "new_drawing", "description": "Start a new empty drawing (imperial, or metric: true). Answers its `document` handle.",
            "inputSchema": {"type": "object", "properties": {"metric": {"type": "boolean"}}}, "annotations": rw, "_meta": {"whiq/files": {"read": [], "opens": true}}}),
        json!({"name": "read_drawing", "description": "Read a drawing for an agent. section: summary (default: units, version, extents, layouts, layer/block counts, entity counts, title blocks, scale hints, text samples), takeoff (quantities in drawing units, per layer and totals: open lengths, closed perimeters and areas by type — HATCH listed but out of each total —, block reference counts by name, circle/text/dimension counts; model space by default; blocks read into by default; window: inside only unless crossing: true), text (every TEXT/MTEXT/attribute/multileader/table cell with layer, space, position, height; texts inside blocks too), dimensions (shown text and measured value, kind, layer, position), title_block (attributed block references ranked as title blocks, with every attribute and the block's fixed texts; plus texts on each layout sheet), entities (handles, types, layers, bounds; geometry: true adds the full geometry), layers, blocks, layouts (paper, viewports with scale), xrefs. Lists are windowed: limit (default 500) and continue_from; filters: space, layer, window, contains, type. format: json (default) or markdown (a table in the text block; the structured result stays JSON). Pass path instead of document to open and read in one call.",
            "inputSchema": {"type": "object", "properties": {
                "document": doc, "path": {"type": "string", "description": "Open this file first (if not open already)"},
                "section": {"type": "string", "enum": ["summary", "takeoff", "text", "dimensions", "title_block", "entities", "layers", "blocks", "layouts", "xrefs"]},
                "crossing": {"type": "boolean", "description": "takeoff: with a window, count objects that cross it too (default false: only those inside)"},
                "block_names": {"type": ["string", "array"], "description": "takeoff: only references of these blocks are counted and read into"},
                "unit_override": {"type": "string", "description": "takeoff: the unit label when $INSUNITS is missing or wrong (relabels only; nothing is converted)"},
                "space": space, "layer": {"type": ["string", "array"]}, "window": window, "contains": {"type": "string"},
                "match": {"type": "string", "enum": ["contains", "exact"]}, "type": {"type": ["string", "array"], "description": "Entity or text types (Line, Polyline, Text, MText, Attribute, Block Reference, Dimension…)"},
                "expand_blocks": {"type": "boolean", "description": "Read inside block references (default true)"}, "geometry": {"type": "boolean"},
                "anonymous": {"type": "boolean", "description": "blocks: list anonymous (*U, *D) blocks too"},
                "limit": {"type": "integer", "minimum": 1}, "format": {"type": "string", "enum": ["json", "markdown"]}}},
            "annotations": ro, "_meta": {"whiq/files": {"read": ["path"], "opens": true}}}),
        json!({"name": "search_drawing", "description": "Find text in a drawing like a CAD viewer's Find: TEXT/MTEXT/multileader/table text, attribute values and tags, dimension texts, block reference names, block, layer and layout names. Each hit names the entity handle, kind, layer, space (Model or layout) and position, and the block it is inside.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "path": {"type": "string"}, "query": {"type": "string"}, "match": {"type": "string", "enum": ["contains", "exact"]},
                "kinds": {"type": ["string", "array"], "description": "Any of text, attribute, tag, dimension, block_reference, block, layer, layout (default all)"},
                "space": space, "layer": {"type": ["string", "array"]}, "window": window, "expand_blocks": {"type": "boolean"}, "limit": {"type": "integer", "minimum": 1}, "format": {"type": "string", "enum": ["json", "markdown"]}},
                "required": ["query"]},
            "annotations": ro, "_meta": {"whiq/files": {"read": ["path"], "opens": true}}}),
        json!({"name": "render_preview", "description": "The Preview renderer: vector SVG of model space, a layout (space: name) or a layout viewport's model view (viewport: handle) — the extents or a window, split into tiles for big drawings (tiles: {cols, rows}; tile: [col, row] for one), layers switched with layers_off / layers_on / only_layers. Each primitive's layer is in <g data-layer> so a viewer can hide layers too. format: png gives a picture (an image block) for a visual check. Answers tiles while they fit the answer (continue_from for the rest; never cut inside a tile); output writes them to files instead; path opens a drawing first. One picture draws at most 2,000,000 block pieces (every copy of a block and every object in it, nested blocks counted). A picture that would need more is NOT made: no picture comes back and no file is written (a file already at the output path is left as it is). The answer is a refusal (code too_large) saying how many block pieces the picture needs (blocks_expanded_total), the limit (blocks_expanded_limit) and the ways to a complete picture that leave the drawing as it is, each with what it leaves and whether that fits one picture: layers_that_reduce (the layer the block references are on switched off, with layers_off / only_layers; reference_layers lists them), heaviest_blocks (the heaviest blocks by name, the biggest saving first, each with what is left if that block is drawn as outline boxes, listed down to the point where drawing all of them as boxes fits, and heaviest_blocks_all_boxed; when even every block as boxes does not fit, all of them are listed and it says so), windows or tiles where they help, and largest_object when one object alone needs more than the limit (windows cannot split it: only layers or boxes reduce it). Every figure is worked out before anything is drawn. With tiles each tile is its own picture: the tiles that fit are drawn (and written with output) and a tile over the limit has picture_made: false and its own refusal in refused; with only one picture asked for, the refusal is the call's error. A picture under the limit is drawn whole and says blocks_expanded_total. blocks_as_boxes: [block names] (names as read_drawing section \"blocks\" lists them, any case; a name that is not a block of the drawing is refused) draws every reference to a named block, at any nesting depth, as ONE outline of its extents (the block's extents under the reference's transform: a rotated reference gives a rotated rectangle; an array of references, a MINSERT, is one outline of the whole array) on the reference's own layer and colour instead of everything the reference draws (its contents AND its attribute texts are not drawn), and counts one block piece; the answer names blocks_drawn_as_boxes (each block and outlines_drawn, how many outlines were drawn: a block with nothing visible in it draws none) with a note on what the outline covers (layers that are off or frozen in the drawing are left out; layers frozen only in one layout viewport, or marked do-not-plot in a PDF, are not subtracted, so there the outline can be larger than what the block would draw, never smaller). The drawing itself is never changed. Field types: blocks_expanded_total, largest_object.blocks_expanded_total, layers_that_reduce[].blocks_expanded_total_if_off and heaviest_blocks[].blocks_expanded_total_if_boxed (and heaviest_blocks_all_boxed.blocks_expanded_total) are each a JSON number up to 2^53, a decimal string above 2^53 (JSON clients lose precision there), or the words \"more than can be counted (nested references multiply past 1.8e19)\" when the count overflows (then no number of windows is given); blocks_expanded_limit is the limit (a number). A window or a tile is a picture of its own that expands only the objects reaching it, so a drawing with more block content than that comes whole as tiles (tiles with output, one file each), as layer passes (layers_off / only_layers: the layer the block references are on, not the layers of their contents) or with its heaviest blocks drawn as boxes. One object (a MINSERT, a block holding a whole plan) that alone needs more than the limit is expanded whole in every window touching it. Reading, search, measuring and extents never make a picture and are not limited. A layout is built whole, so draw the model space its viewports show (viewport: handle) in tiles instead. A block array (MINSERT) is drawn with at most 10,000 columns and 10,000 rows: an answer says arrays_clamped when one was drawn short.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "path": {"type": "string", "description": "Open this drawing first"},
                "space": {"type": "string"}, "viewport": {"type": ["string", "integer"]}, "window": window,
                "tiles": {"type": ["object", "array", "integer"]}, "tile": {"type": "array"},
                "layers_off": {"type": ["string", "array"]}, "layers_on": {"type": ["string", "array"]}, "only_layers": {"type": ["string", "array"]},
                "blocks_as_boxes": {"type": ["string", "array"], "description": "Block names whose references are drawn as one outline box of their extents each instead of their contents"},
                "format": {"type": "string", "enum": ["svg", "png"]}, "width": {"type": "integer", "minimum": 16}, "height": {"type": "integer", "minimum": 16},
                "background": {"type": "string", "enum": ["light", "dark"]}, "lineweights": {"type": "boolean"},
                "output": {"type": "string", "description": "Write the SVG/PNG file(s) here (tiles get -c<col>r<row>) instead of answering them"}}},
            "annotations": ro, "_meta": {"whiq/files": {"read": ["path"], "opens": true}}}),
        json!({"name": "export", "description": "Write the drawing in another format: dxf (version 2000–2018, binary: true for binary DXF), dwg (2000–2018), pdf (model, a layout, a list or all layouts — one page each; paper, scale, window, lineweights), svg (one file per space), png. DWG→DXF, DWG/DXF→PDF and →SVG. A file already there is kept as OLD_<name> unless overwrite: true. SVG is written to the file in slices as it is made: model space in slices of objects in draw order, and a paper-space layout the same (its own objects and each viewport's model-space contents in draw order, each viewport's clip applied), so a big layout is not held whole in memory. One picture draws at most 2,000,000 block pieces (every copy of a block and every object in it, nested blocks counted). An svg / png / pdf that would need more is NOT made: no file is written (a file already at the path is left as it is, not renamed) and the answer is a refusal (code too_large) saying how many block pieces it needs (blocks_expanded_total), the limit (blocks_expanded_limit) and the ways to a complete one that leave the drawing as it is, each with what it leaves and whether that fits one picture: layers_that_reduce (the layer the block references are on switched off, layers_off / only_layers), heaviest_blocks (the heaviest blocks by name, the biggest saving first, each with what is left if that block is drawn as outline boxes, down to the point where drawing all of them as boxes fits, with heaviest_blocks_all_boxed; when even every block as boxes does not fit all are listed and it says so), a window per region (svg / png) where windows help, and largest_object when one object alone needs more than the limit (windows cannot split it). Every figure is worked out before anything is drawn. A PDF is one file: a sheet is one picture of the whole space (its window only places it on the page, it does not leave out block references outside it), and if any sheet is over the limit no PDF is written and the answer names the sheets over the limit with their page numbers (sheet_numbers_over_limit, sheets_over_limit with each sheet's own figures). With several spaces (svg / png of space: all) a space over the limit says so in its own entry and the others are written. blocks_as_boxes: [block names] (svg, png and pdf; names as read_drawing section \"blocks\" lists them, any case; a name that is not a block of the drawing is refused) draws every reference to a named block, at any nesting depth, as ONE outline of its extents (the block's extents under the reference's transform: a rotated reference gives a rotated rectangle; an array of references, a MINSERT, is one outline of the whole array) on the reference's own layer and colour instead of everything the reference draws (its contents AND its attribute texts are not drawn), and counts one block piece; the answer names blocks_drawn_as_boxes (each block and outlines_drawn, how many outlines were drawn: a block with nothing visible in it draws none) with a note on what the outline covers (layers that are off or frozen in the drawing are left out; layers frozen only in one layout viewport, or marked do-not-plot in a PDF, are not subtracted, so there the outline can be larger than what the block would draw, never smaller). The drawing itself is never changed. A picture under the limit is drawn whole and says blocks_expanded_total. Field types: blocks_expanded_total, largest_object.blocks_expanded_total, layers_that_reduce[].blocks_expanded_total_if_off and heaviest_blocks[].blocks_expanded_total_if_boxed (and heaviest_blocks_all_boxed.blocks_expanded_total) are each a JSON number up to 2^53, a decimal string above 2^53 (JSON clients lose precision there), or the words \"more than can be counted (nested references multiply past 1.8e19)\" when the count overflows (then no number of windows is given); blocks_expanded_limit is the limit (a number). Ways to a complete picture: export svg / png once per window (a window expands only the objects reaching it, each export is its own picture), layers_off / only_layers for a pass per layer set (the layer the block references are on), or blocks_as_boxes; one object that alone needs more than the limit is not split by windows. A block array (MINSERT) is drawn with at most 10,000 columns and 10,000 rows: an answer says arrays_clamped when one was drawn short.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "source": {"type": "string", "description": "Open this drawing first"}, "path": {"type": "string", "description": "The file to write (its extension picks the format)"},
                "format": {"type": "string", "enum": ["dxf", "dwg", "pdf", "svg", "png"]}, "version": {"type": ["string", "integer"]}, "binary": {"type": "boolean"},
                "space": {"type": ["string", "array"], "description": "model (default), a layout, a list, or all"}, "window": window,
                "layers_off": {"type": ["string", "array"]}, "layers_on": {"type": ["string", "array"]}, "only_layers": {"type": ["string", "array"]},
                "blocks_as_boxes": {"type": ["string", "array"], "description": "svg, png, pdf: block names whose references are drawn as one outline box of their extents each instead of their contents"},
                "paper": {"type": "string"}, "landscape": {"type": "boolean"}, "scale": {"type": "number"}, "fit": {"type": "boolean"}, "lineweights": {"type": "boolean"},
                "width": {"type": "integer"}, "height": {"type": "integer"}, "overwrite": {"type": "boolean"}},
                "required": ["path"]},
            "annotations": writes, "_meta": {"whiq/files": {"read": ["source"], "opens": true}}}),
        json!({"name": "edit", "description": "Change a drawing with many operations in one call (all or nothing by default: atomic: false keeps the ones that worked; a failed one is never kept in part). Each op: {op: \"command\", command, params} runs any CADCraft command (list_commands names them; e.g. line {points}, circle {center, radius}, text {at, text, height}, mtext, pline, hatch, dimlinear, insert {name, at}, move, copy, rotate, scale, mirror, erase {handles}, properties.set {handles, layer, color, …}, layer.new / layer.set / layer.delete, block {name, base, handles}, rename {table, from, to}, purge, layout.set {name}); {op: \"cmdline\", text} types at the command line; shorthands: add {type, …params}, delete {handles}, set_text {handle, text}, set_attributes {handle | block, values: {TAG: value}}, set_properties {handles, …}, layer {action: new|set|delete|current|rename, name, …}, block {action: define|insert|delete|rename, …}, space {name}, undo. Paths in params stay inside the roots; open/save/close are the server's own tools.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "ops": {"type": "array"}, "atomic": {"type": "boolean"}, "space": {"type": "string", "description": "Make this space current first (Model or a layout)"},
                "command": {"type": "string", "description": "One command without ops (with params)"}, "params": {"type": "object"}, "text": {"type": "string", "description": "One command line without ops"}}},
            "annotations": rw}),
        json!({"name": "list_commands", "description": "The CADCraft commands edit can run: id, label, aliases and parameter docs; filter by a substring.",
            "inputSchema": {"type": "object", "properties": {"filter": {"type": "string"}, "limit": {"type": "integer"}}}, "annotations": ro}),
        json!({"name": "save_drawing", "description": "Save the drawing as DWG or DXF (by the extension; version 2000–2018). Without a path: its own name in the write root. A file already there is kept as OLD_<name> unless overwrite: true.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "path": {"type": "string"}, "version": {"type": ["string", "integer"]}, "binary": {"type": "boolean"}, "overwrite": {"type": "boolean"}}},
            "annotations": writes}),
        json!({"name": "close_drawing", "description": "Close a drawing. Unsaved changes are refused unless discard: true.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "discard": {"type": "boolean"}}}, "annotations": writes}),
        json!({"name": "roundtrip_check", "description": "Write the drawing as DXF or DWG in memory, read it back and compare: entities per space and type, layers, blocks, layouts and texts. Says what a save would lose.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "path": {"type": "string"}, "format": {"type": "string", "enum": ["dxf", "dwg"]}, "version": {"type": ["string", "integer"]}}},
            "annotations": ro, "_meta": {"whiq/files": {"read": ["path"], "opens": false}}}),
    ]
}

/// $INSUNITS as short unit names (the take-off's `unit`).
const UNIT_SHORT: &[&str] =
    &["unitless", "in", "ft", "mi", "mm", "cm", "m", "km", "µin", "mil", "yd", "Å", "nm", "µm", "dm", "dam", "hm", "Gm", "AU", "ly", "pc"];

/// The worker's side: the drawings and the tools that use them.
#[derive(Default)]
pub struct Engine {
    pub docs: Docs,
    /// The `edit` call under way and its document as it was before the call: a call that stops
    /// part way (an error, a cancel, a panic) is undone whole ([`Engine::undo_interrupted_call`]).
    call: Option<Before>,
}

fn dwg_version(bytes: &[u8]) -> Option<String> {
    let v = bytes.get(0..6)?;
    (v.starts_with(b"AC10") && v.get(4..6).is_some_and(|d| d.iter().all(u8::is_ascii_digit))).then(|| String::from_utf8_lossy(v).into_owned())
}

/// The DWG bridge's limits for a file: no cap on the file itself, the DXF rendition bounded by a
/// share of the memory budget (at least the fixed default, and 16× the file).
fn limits_for(len: usize, budget: Option<u64>) -> cadcraft_io::DwgLimits {
    let floor = cadcraft_io::DwgLimits::DEFAULT.max_dxf_bytes;
    let by_budget = budget.map_or(0, |b| usize::try_from(b / 2).unwrap_or(usize::MAX));
    cadcraft_io::DwgLimits { max_dwg_bytes: usize::MAX, max_dxf_bytes: floor.max(len.saturating_mul(16)).max(by_budget) }
}

impl Engine {
    /// Open `path` (resolved inside the roots) as a new document.
    pub fn open_path(&mut self, path: &str, ctx: &Ctx) -> Result<usize, ToolError> {
        let p = ctx.resolve(path, Access::Read)?;
        let meta = std::fs::metadata(&p).map_err(|e| io_error(&p, &e))?;
        if !meta.is_file() {
            return Err(ToolError::new(code::INVALID_ARGUMENT, format!("{} is not a file", ctx.roots().show(&p)), "give a .dwg or .dxf file"));
        }
        ctx.stage("reading the file", meta.len());
        let bytes = std::fs::read(&p).map_err(|e| io_error(&p, &e))?;
        ctx.step(meta.len());
        ctx.check()?;
        let version = dwg_version(&bytes);
        let format = if version.is_some() { "dwg" } else { "dxf" };
        let binary = bytes.starts_with(b"AutoCAD Binary DXF");
        ctx.stage(if format == "dwg" { "decoding the DWG" } else { "reading the DXF" }, 0);
        let limits = limits_for(bytes.len(), ctx.settings.max_memory_bytes);
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cadcraft_io::read_limited(&bytes, &p, limits))).map_err(|_| {
            ToolError::new(
                code::FAILED,
                format!("{}: the reader failed on this file", ctx.roots().show(&p)),
                "the file may be damaged; try a DXF export of it",
            )
        })?;
        let drawing = read.map_err(|e| {
            let text = e.to_string();
            let c = if text.contains("unsupported") { code::UNSUPPORTED } else { code::FAILED };
            ToolError::new(
                c,
                format!("{}: {text}", ctx.roots().show(&p)),
                "check that it is a DWG (R13 or later) or DXF file; a damaged file may open in its program and be saved again",
            )
        })?;
        drop(bytes);
        ctx.check()?;
        let version = version.or_else(|| drawing.header.get("ACADVER").and_then(cadcraft_engine::doc::HVal::as_str).map(str::to_string));
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Drawing.dxf".into());
        let text_path = p.to_str().map(str::to_string);
        let mut session = Session::empty();
        session.finish_open(cadcraft_engine::cmd::file::ReadFile::from_drawing(drawing, binary, &name, text_path)).map_err(engine_error)?;
        let _ = name;
        self.docs.add(session, Some(p), format, version, meta.len());
        Ok(self.docs.list.len() - 1)
    }

    /// The document a call works on: `document`, else the file named by `key` (opened if it is
    /// not open), else the active one.
    fn target(&mut self, a: &Value, key: &str, ctx: &Ctx) -> Result<usize, ToolError> {
        if a.get("document").is_none_or(Value::is_null)
            && let Some(p) = s(a, key)
        {
            let resolved = ctx.resolve(p, Access::Read)?;
            if let Some(i) = self.docs.find_source(&resolved) {
                self.docs.active = Some(self.docs.list[i].handle.clone());
                return Ok(i);
            }
            return self.open_path(p, ctx);
        }
        let i = self.docs.index(a)?;
        self.docs.active = Some(self.docs.list[i].handle.clone());
        Ok(i)
    }

    pub fn call(&mut self, name: &str, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        match name {
            "open_drawing" => {
                let path = need(a, "path", "{path: \"plan.dwg\"}")?;
                let i = self.open_path(path, ctx)?;
                let d = &self.docs.list[i];
                let dr = d.drawing()?;
                let mut v = d.info(ctx.roots());
                v["entities"] = json!(dr.entity_count());
                v["layouts"] = json!(read::spaces(dr).into_iter().map(|(n, _)| n).collect::<Vec<_>>());
                v["layers"] = json!(dr.layers.len());
                v["blocks"] = json!(dr.blocks.values().filter(|b| !b.anonymous).count());
                v["release"] = json!(d.version.as_deref().map(read::version_name));
                v["warnings"] = json!(dr.read_notes);
                v["next_step"] = json!("read_drawing {section: \"summary\"} for units, layouts and title blocks; render_preview to look at it");
                Ok(Reply::json(v))
            }
            "new_drawing" => {
                let mut session = Session::empty();
                session.new_drawing(flag(a, "metric").unwrap_or(false));
                let d = self.docs.add(session, None, "new", None, 0);
                Ok(Reply::json(d.info(ctx.roots())))
            }
            "close_drawing" => {
                let i = self.docs.index(a)?;
                let d = &self.docs.list[i];
                if d.dirty() && !flag(a, "discard").unwrap_or(false) {
                    return Err(ToolError::new(
                        code::INVALID_REQUEST,
                        format!("{} has unsaved changes", d.handle),
                        "save_drawing first, or close_drawing with discard: true to drop them",
                    )
                    .with("document", json!(d.handle)));
                }
                let d = self.docs.remove(i);
                Ok(Reply::json(
                    json!({"closed": d.handle, "name": d.name(), "open_documents": self.docs.list.iter().map(|x| x.handle.clone()).collect::<Vec<_>>()}),
                ))
            }
            "read_drawing" => self.read(a, ctx),
            "search_drawing" => self.search(a, ctx),
            "render_preview" => self.render(a, ctx),
            "export" => self.export(a, ctx),
            "save_drawing" => self.save(a, ctx),
            "edit" => self.edit(a, ctx),
            "list_commands" => {
                let f = s(a, "filter").map(str::to_ascii_lowercase);
                let session = Session::new();
                let all: Vec<Value> = cadcraft_engine::command_specs()
                    .iter()
                    .map(|c| serde_json::to_value(c.info(&session)).unwrap_or_default())
                    .filter(|c| {
                        f.as_ref().is_none_or(|f| {
                            let has = |k: &str| c[k].as_str().is_some_and(|x| x.to_ascii_lowercase().contains(f.as_str()));
                            has("id")
                                || has("label")
                                || c["aliases"]
                                    .as_array()
                                    .is_some_and(|al| al.iter().filter_map(Value::as_str).any(|x| x.to_ascii_lowercase().contains(f.as_str())))
                        })
                    })
                    .map(|c| json!({"id": c["id"], "label": c["label"], "aliases": c["aliases"], "params": c["params"]}))
                    .collect();
                let from = usize_arg(a, "continue_from", 0);
                Ok(Reply::json(Value::Object(read::page("commands", all, from, usize_arg(a, "limit", 300)))))
            }
            "roundtrip_check" => self.roundtrip(a, ctx),
            other => Err(ToolError::new(code::NOT_FOUND, format!("unknown tool `{other}`"), "call tools/list")),
        }
    }

    fn read(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        let i = self.target(a, "path", ctx)?;
        let doc = &self.docs.list[i];
        let section = s(a, "section").unwrap_or("summary").to_ascii_lowercase();
        let markdown = s(a, "format").is_some_and(|f| f.eq_ignore_ascii_case("markdown") || f.eq_ignore_ascii_case("md"));
        let from = usize_arg(a, "continue_from", 0);
        let limit = usize_arg(a, "limit", 500).max(1);
        ctx.stage(&format!("reading {section}"), 0);
        let d = doc.drawing()?;
        let sp = read::spaces_named(d, a.get("space"))?;
        let flt = read::Filter::from_args(a)?;
        let mut v = match section.as_str() {
            "summary" => {
                let mut v = read::summary(doc, ctx)?;
                v["scale_hint"] = super::takeoff::scale_hint(d, ctx)?;
                let mut samples = Vec::new();
                for t in read::texts(
                    d,
                    &read::spaces(d),
                    &read::Filter { layers: None, window: None, contains: None, exact: false, types: None, expand_blocks: false },
                    ctx,
                )? {
                    if samples.len() >= 30 {
                        break;
                    }
                    samples.push(json!({"text": t["text"].as_str().map(|x| x.chars().take(200).collect::<String>()), "type": t["type"], "layer": t["layer"], "at": t["at"], "space": t["space"]}));
                }
                v["text_samples"] = json!(samples);
                v
            }
            "takeoff" | "quantities" | "take_off" => {
                // Model space unless asked: paper space holds title blocks and viewports, not quantities.
                let sp = if a.get("space").is_none_or(Value::is_null) { read::spaces_named(d, Some(&json!("model")))? } else { sp.clone() };
                let blocks = match a.get("block_names") {
                    Some(Value::String(x)) => Some(x.split(',').map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect()),
                    Some(Value::Array(x)) => Some(x.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()),
                    _ => None,
                };
                let crossing = flag(a, "crossing").unwrap_or(false);
                let t = super::takeoff::takeoff(d, &sp, &flt, crossing, blocks.clone(), ctx)?;
                let code = usize::try_from(d.header.i64("INSUNITS", 0)).unwrap_or(0);
                let short = UNIT_SHORT.get(code).copied().filter(|_| code > 0);
                let unit = s(a, "unit_override").map(str::to_string).or(short.map(str::to_string)).unwrap_or_else(|| "unitless".into());
                let paper_skipped: usize =
                    if a.get("space").is_none_or(Value::is_null) { d.layouts.iter().map(|l| l.entities.len()).sum() } else { 0 };
                let mut notes = vec![
                    format!("All values in drawing units ({unit}; areas in {unit}²) — nothing is converted."),
                    "lengths = open LINE/ARC/LWPOLYLINE/POLYLINE/SPLINE/partial ELLIPSE; closed shapes (closed flag, or first vertex == last vertex) go to perimeters + areas; polyline bulge arcs are exact.".to_string(),
                    "HATCH area/perimeter is listed under HATCH but EXCLUDED from each `total` (a hatch usually fills an outline already counted); islands are subtracted by nesting and the hatch style.".to_string(),
                    if flt.expand_blocks {
                        "Blocks are counted (references by block name, MINSERT rows×columns) AND read into: the geometry inside each reference is measured with its scale (expand_blocks: false counts references only).".to_string()
                    } else {
                        "Blocks are counted (references by block name, MINSERT rows×columns), not read into: geometry inside block definitions is not in the lengths/areas.".to_string()
                    },
                    "Straight-segment lengths are 3D; areas are the XY projection.".to_string(),
                ];
                if paper_skipped > 0 {
                    notes.push(format!(
                        "{paper_skipped} paper-space objects left out (title blocks and viewports; space: \"all\" or a layout name takes them in)."
                    ));
                }
                if t.unparsed_hatches > 0 {
                    notes.push(format!("{} HATCH objects have no boundary and are not counted.", t.unparsed_hatches));
                }
                notes.extend(t.notes.iter().cloned());
                let mut o = read::page("by_layer", t.layer_values(), from, limit);
                o.insert("totals".into(), t.totals_value());
                o.insert("unit".into(), json!(unit));
                o.insert("area_unit".into(), json!(format!("{unit}²")));
                o.insert("units".into(), json!({"insunits": code, "source": if s(a, "unit_override").is_some() { "unit_override" } else if short.is_some() { "$INSUNITS" } else { "not set" }}));
                o.insert("space".into(), json!(sp.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()));
                o.insert("expand_blocks".into(), json!(flt.expand_blocks));
                o.insert("references_read".into(), json!(t.references_read));
                o.insert("crossing".into(), json!(crossing));
                if let Some(b) = blocks {
                    o.insert("block_names".into(), json!(b));
                }
                o.insert("scale_hint".into(), super::takeoff::scale_hint(d, ctx)?);
                o.insert("note".into(), json!(notes.join(" ")));
                Value::Object(o)
            }
            "text" | "texts" => Value::Object(read::page("texts", read::texts(d, &sp, &flt, ctx)?, from, limit)),
            "dimensions" | "dims" | "dimension" => Value::Object(read::page("dimensions", read::dimensions(d, &sp, &flt, ctx)?, from, limit)),
            "title_block" | "titleblock" | "title_blocks" | "title" => {
                let tb = read::title_blocks(d, &sp, ctx)?;
                let mut o = read::page("candidates", tb["candidates"].as_array().cloned().unwrap_or_default(), from, limit);
                o.insert("sheet_texts".into(), tb["sheet_texts"].clone());
                Value::Object(o)
            }
            "entities" | "entity" | "objects" => {
                Value::Object(read::page("entities", read::entities(d, &sp, &flt, flag(a, "geometry").unwrap_or(false), ctx)?, from, limit))
            }
            "layers" => {
                let mut ls = read::layers(d);
                if let Some(c) = &flt.contains {
                    ls.retain(|l| l["name"].as_str().is_some_and(|n| n.to_lowercase().contains(c.as_str())));
                }
                Value::Object(read::page("layers", ls, from, limit))
            }
            "blocks" => {
                let mut bs = read::blocks(d, flag(a, "anonymous").unwrap_or(false));
                if let Some(c) = &flt.contains {
                    bs.retain(|b| b["name"].as_str().is_some_and(|n| n.to_lowercase().contains(c.as_str())));
                }
                Value::Object(read::page("blocks", bs, from, limit))
            }
            "layouts" | "spaces" | "sheets" => Value::Object(read::page("layouts", read::layouts(d), from, limit)),
            "xrefs" | "xref" => {
                let x: Vec<Value> = d.blocks.values().filter(|b| b.xref_path.is_some()).map(|b| json!({"name": b.name, "path": b.xref_path, "overlay": b.xref_overlay, "status": b.xref_status, "entities": b.entities.len()})).collect();
                Value::Object(read::page("xrefs", x, from, limit))
            }
            other => {
                return Err(ToolError::new(
                    code::INVALID_ARGUMENT,
                    format!("unknown section `{other}`"),
                    "use summary, text, dimensions, title_block, entities, layers, blocks, layouts or xrefs",
                )
                .with(
                    "closest",
                    json!(super::contract::closest(
                        other,
                        ["summary", "text", "dimensions", "title_block", "entities", "layers", "blocks", "layouts", "xrefs"],
                        3
                    )),
                ));
            }
        };
        if section != "summary" {
            v["document"] = json!(doc.handle);
            v["section"] = json!(section);
        }
        let text = markdown.then(|| read::markdown(&section, &v));
        if let Some(m) = &text {
            // The supervisor hands callers the structured result only: the markdown rides in it too.
            v["markdown"] = json!(m);
        }
        Ok(Reply { value: v, extra: Vec::new(), text, trim: true })
    }

    fn search(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        let query = need(a, "query", "{query: \"FIRE\"}")?.to_string();
        let i = self.target(a, "path", ctx)?;
        let doc = &self.docs.list[i];
        let d = doc.drawing()?;
        let sp = read::spaces_named(d, a.get("space"))?;
        let mut flt = read::Filter::from_args(a)?;
        flt.contains = None;
        let kinds: Option<Vec<String>> = match a.get("kinds") {
            Some(Value::String(k)) => Some(k.split(',').map(|x| x.trim().to_ascii_lowercase()).collect()),
            Some(Value::Array(k)) => Some(k.iter().filter_map(Value::as_str).map(str::to_ascii_lowercase).collect()),
            _ => None,
        };
        let exact = s(a, "match").is_some_and(|m| m.eq_ignore_ascii_case("exact"));
        ctx.stage("searching", 0);
        let hits = read::search(d, &sp, &query, exact, kinds.as_deref(), &flt, ctx)?;
        let mut o = read::page("hits", hits, usize_arg(a, "continue_from", 0), usize_arg(a, "limit", 500).max(1));
        o.insert("document".into(), json!(doc.handle));
        o.insert("query".into(), json!(query));
        let mut v = Value::Object(o);
        let text = s(a, "format").is_some_and(|f| f.eq_ignore_ascii_case("markdown")).then(|| read::markdown("search", &v));
        if let Some(m) = &text {
            v["markdown"] = json!(m);
        }
        Ok(Reply { value: v, extra: Vec::new(), text, trim: true })
    }

    fn render(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        let i = self.target(a, "path", ctx)?;
        let doc = &self.docs.list[i];
        let d = doc.drawing()?;
        let prep = preview::prepare(d, a, ctx)?;
        let tiles = preview::tiles(&prep.window, a)?;
        let png = s(a, "format").is_some_and(|f| f.eq_ignore_ascii_case("png"));
        let dark = s(a, "background").is_some_and(|b| b.eq_ignore_ascii_case("dark"));
        let width = a.get("width").and_then(Value::as_u64).map_or(1600, |w| u32::try_from(w).unwrap_or(1600));
        let height = a.get("height").and_then(Value::as_u64).and_then(|h| u32::try_from(h).ok());
        let output = s(a, "output");
        let budget = a.get("max_bytes").and_then(Value::as_u64).map_or(ctx.settings.max_result_bytes, |b| usize::try_from(b).unwrap_or(usize::MAX));
        let from = usize_arg(a, "continue_from", 0);
        let total = tiles.len();
        ctx.stage("drawing tiles", u64::try_from(total).unwrap_or(0));
        let mut out = Vec::new();
        let mut extra = Vec::new();
        let mut used = 0usize;
        let mut next = None;
        // A layout is built whole for every tile: one that is over the limit is refused once for the call,
        // not once per tile.
        if !matches!(prep.target.space, Space::Model)
            && let Some(refused) = prep.refusal(&prep.census(&prep.window, None, ctx)?)
        {
            return Err(refused);
        }
        let mut each = || -> Result<(), ToolError> {
            for (k, (cr, win)) in tiles.iter().enumerate().skip(from) {
                ctx.check()?;
                let mut t = json!({"tile": cr, "window": preview::win_value(win)});
                // The objects the tile takes are picked once: the count and the build both use them.
                let picked = prep.pick(win, ctx)?;
                // Each tile is its own picture: counted before anything is drawn, and one that needs
                // more than a picture draws is refused (no picture, no file). With one picture asked
                // for that is the call's error; among several the tile's entry says so.
                let census = prep.census(win, Some(&picked), ctx)?;
                if let Some(refused) = prep.refusal(&census) {
                    if tiles.len() == 1 {
                        return Err(refused);
                    }
                    t["picture_made"] = json!(false);
                    t["refused"] = refused.to_value();
                    // Like any other entry it counts against the answer's size (when the tiles are
                    // answered, not written to files).
                    let size = t.to_string().len();
                    if output.is_none() {
                        if !out.is_empty() && used + size > budget {
                            next = Some(k);
                            break;
                        }
                        used += size;
                    }
                    out.push(t);
                    ctx.step(1);
                    continue;
                }
                let (bytes, mime, ext, info): (Vec<u8>, &str, &str, Value) = if png {
                    let (b, w, h, built) = preview::png_of(&prep, &picked, win, width, height, dark, ctx)?;
                    (b, "image/png", "png", merged(json!({"width": w, "height": h}), Some(prep.blocks_note(built, &census))))
                } else {
                    let list = prep.list_of(&picked, win, f64::from(width), ctx)?;
                    let (svg, n) = preview::svg_tile(&list, &prep.layers, win, f64::from(width), dark, ctx)?;
                    (svg.into_bytes(), "image/svg+xml", "svg", merged(json!({"primitives": n}), Some(prep.blocks_note(list.expanded, &census))))
                };
                if let Value::Object(o) = info {
                    for (kk, vv) in o {
                        t[kk] = vv;
                    }
                }
                t["bytes"] = json!(bytes.len());
                if let Some(o) = output {
                    let base = Path::new(o);
                    let name = if total == 1 {
                        base.with_extension(ext)
                    } else {
                        let stem = base.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| "preview".into());
                        base.with_file_name(format!("{stem}-c{}r{}.{ext}", cr[0], cr[1]))
                    };
                    let p = ctx.resolve(&name.to_string_lossy(), Access::Write)?;
                    let kept = write_file(
                        &p,
                        &bytes,
                        WritePolicy { old_copies: ctx.settings.old_copies, overwrite: flag(a, "overwrite").unwrap_or(false) },
                    )?;
                    t["saved"] = saved_entry(ctx.roots(), &p, bytes.len(), ext, kept.as_ref());
                } else {
                    // Tiles go into the answer while they fit; the first always does (never cut inside one).
                    if !out.is_empty() && used + bytes.len() > budget {
                        next = Some(k);
                        break;
                    }
                    used += bytes.len();
                    if png {
                        extra.push(json!({"type": "image", "mimeType": mime, "data": cadcraft_engine::cmd::file::base64_encode(&bytes)}));
                    } else {
                        t["svg"] = json!(String::from_utf8_lossy(&bytes));
                    }
                }
                out.push(t);
                ctx.step(1);
            }
            Ok(())
        };
        if let Err(e) = each() {
            let written: Vec<Value> = out.iter().filter_map(|t| t.get("saved").cloned()).collect();
            return Err(with_written(e, &written));
        }
        let mut layers_state: Vec<Value> = prep.layer_states.iter().map(|(n, on)| json!({"name": n, "on": on})).collect();
        layers_state.sort_by(|x, y| x["name"].as_str().cmp(&y["name"].as_str()));
        let mut v = json!({
            "document": doc.handle, "space": prep.target.name, "format": if png { "png" } else { "svg" },
            "window": preview::win_value(&prep.window), "extents": if prep.extents.is_empty() { Value::Null } else { preview::win_value(&prep.extents) },
            "total_tiles": total, "returned": out.len(), "offset": from, "tiles": out, "layers": layers_state,
            "y_axis": "SVG y runs down: x_svg = x - window.x0, y_svg = window.y1 - y",
        });
        if !prep.unknown_layers.is_empty() {
            v["warnings"] = json!([format!("no such layer: {}", prep.unknown_layers.join(", "))]);
        }
        if output.is_none() && !png && used > budget {
            let k = ((used as f64 / budget.max(1) as f64).sqrt().ceil() as u64).max(2);
            v["hint"] = json!(format!(
                "a tile is {used} bytes, more than the {budget} the answer is meant to hold: tiles: {{cols: {k}, rows: {k}}} (or a window) gives smaller ones"
            ));
        }
        if let Some(n) = next {
            v["truncated"] = json!(true);
            v["continue_from"] = json!(n);
            v["next_step"] = json!(format!("call again with continue_from: {n} for the next tiles (or pass output to write them to files)"));
        }
        Ok(Reply { value: v, extra, text: None, trim: false })
    }

    fn export(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        let path = need(a, "path", "{path: \"plan.pdf\"}")?.to_string();
        let i = self.target(a, "source", ctx)?;
        let doc = &self.docs.list[i];
        let d = doc.drawing()?;
        let fmt = s(a, "format")
            .map(str::to_ascii_lowercase)
            .or_else(|| Path::new(&path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()))
            .unwrap_or_default();
        let mut file = std::path::PathBuf::from(&path);
        if file.extension().is_none_or(|e| !e.to_string_lossy().eq_ignore_ascii_case(&fmt)) && !fmt.is_empty() {
            file.set_extension(&fmt);
        }
        let policy = WritePolicy { old_copies: ctx.settings.old_copies, overwrite: flag(a, "overwrite").unwrap_or(false) };
        let sw = preview::LayerSwitch::from_args(a);
        let switched;
        let d: &Drawing = if sw.is_empty() {
            d
        } else {
            switched = sw.apply(d).0;
            &switched
        };
        let mut saved = Vec::new();
        ctx.stage(&format!("writing {fmt}"), 0);
        match fmt.as_str() {
            "dxf" | "dwg" => {
                let version = match a.get("version") {
                    None | Some(Value::Null) => None,
                    Some(v) => {
                        let t = match v {
                            Value::Number(n) => n.to_string(),
                            other => other.as_str().unwrap_or("").to_string(),
                        };
                        Some(cadcraft_io::Version::parse(&t).ok_or_else(|| {
                            ToolError::new(code::INVALID_ARGUMENT, format!("unknown version `{t}`"), "use 2000, 2004, 2007, 2010, 2013 or 2018")
                        })?)
                    }
                };
                let opts = cadcraft_io::SaveOptions { version, binary: flag(a, "binary").unwrap_or(false) && fmt == "dxf" };
                let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cadcraft_io::write_with(d, &file, &opts)))
                    .map_err(|_| ToolError::new(code::FAILED, "the writer failed on this drawing", "try DXF, or another version"))?
                    .map_err(|e| ToolError::new(code::FAILED, e.to_string(), "try DXF, or another version"))?;
                let p = ctx.resolve(&file.to_string_lossy(), Access::Write)?;
                let kept = write_file(&p, &bytes, policy)?;
                saved.push(saved_entry(ctx.roots(), &p, bytes.len(), &fmt, kept.as_ref()));
            }
            "pdf" => {
                let mut o = Map::new();
                for k in ["paper", "landscape", "scale", "fit", "lineweights", "window", "center", "margins"] {
                    if let Some(v) = a.get(k).filter(|v| !v.is_null()) {
                        o.insert(k.into(), v.clone());
                    }
                }
                o.insert("title".into(), json!(doc.name()));
                let boxed = preview::boxed_blocks(d, a)?;
                if !boxed.is_empty() {
                    o.insert("blocks_as_boxes".into(), json!(boxed));
                }
                let mut space = Space::Model;
                match a.get("space") {
                    Some(Value::String(x)) if x.eq_ignore_ascii_case("all") || x.eq_ignore_ascii_case("paper") => {
                        if x.eq_ignore_ascii_case("all") {
                            o.insert("layouts".into(), json!("all"));
                        } else {
                            o.insert("layouts".into(), json!(d.layouts.iter().map(|l| l.name.clone()).collect::<Vec<_>>()));
                        }
                    }
                    Some(Value::String(x)) if !x.eq_ignore_ascii_case("model") => {
                        let t = preview::target(d, &json!({"space": x}))?;
                        space = t.space;
                    }
                    Some(Value::Array(list)) => {
                        o.insert("layouts".into(), Value::Array(list.clone()));
                    }
                    _ => {}
                }
                // Every sheet is counted before any is drawn; one over the limit and no file is made.
                let plotted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cadcraft_io::plot_complete(d, &space, &Value::Object(o), preview::block_limit(), Some(ctx.cancel.clone()))
                }))
                .map_err(|_| ToolError::new(code::FAILED, "the PDF writer failed on this drawing", "try one layout, or a window"))?
                .map_err(|e| match ctx.check() {
                    // A stop ends the count (the plotter answers an error): say it was stopped.
                    Err(stopped) => stopped,
                    Ok(()) => ToolError::new(code::INVALID_ARGUMENT, e.to_string(), "check space, paper, scale, window and blocks_as_boxes"),
                })?;
                let plotted = match plotted {
                    cadcraft_io::Plot::Drawn(p) => p,
                    cadcraft_io::Plot::TooBig(sheets) => return Err(preview::pdf_refusal(&sheets, &boxed)),
                };
                let p = ctx.resolve(&file.to_string_lossy(), Access::Write)?;
                let kept = write_file(&p, &plotted.bytes, policy)?;
                saved.push(merged(
                    saved_entry(ctx.roots(), &p, plotted.bytes.len(), "pdf", kept.as_ref()),
                    Some(preview::pdf_blocks_note(&plotted.blocks)),
                ));
            }
            "svg" | "png" => {
                let sp = match a.get("space") {
                    None | Some(Value::Null) => vec![("Model".to_string(), Space::Model)],
                    other => read::spaces_named(d, other)?,
                };
                let many = sp.len() > 1;
                let mut each = || -> Result<(), ToolError> {
                    for (name, _) in &sp {
                        ctx.check()?;
                        let mut args = a.clone();
                        args["space"] = json!(name);
                        if let Some(o) = args.as_object_mut() {
                            for k in ["layers_off", "layers_on", "only_layers"] {
                                o.remove(k);
                            }
                        }
                        let prep = preview::prepare(d, &args, ctx)?;
                        // Counted before anything is drawn or any file is touched: a picture that needs
                        // more than one picture draws is refused (among several spaces, that space's
                        // entry says so and the others are still written).
                        let census = prep.census(&prep.window, None, ctx)?;
                        if let Some(refused) = prep.refusal(&census) {
                            if sp.len() == 1 {
                                return Err(refused);
                            }
                            let mut e = json!({"space": name, "picture_made": false, "refused": refused.to_value()});
                            e["written"] = json!(false);
                            saved.push(e);
                            continue;
                        }
                        let f = if many {
                            let stem = file.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| "drawing".into());
                            let safe: String = name.chars().map(|c| if c.is_alphanumeric() || "-_ ".contains(c) { c } else { '_' }).collect();
                            file.with_file_name(format!("{stem}-{safe}.{fmt}"))
                        } else {
                            file.clone()
                        };
                        let p = ctx.resolve(&f.to_string_lossy(), Access::Write)?;
                        let (kept, size, note) = if fmt == "svg" {
                            // Written as it is made (display list slice by slice) through the same
                            // atomic file write: a call that stops part way leaves nothing of it.
                            let wpx = f64::from(a.get("width").and_then(Value::as_u64).map_or(1600, |w| u32::try_from(w).unwrap_or(1600)));
                            let (kept, (size, built)) = write_file_with(&p, policy, |w| prep.write_svg(w, &prep.window, wpx, ctx))?;
                            (kept, size, prep.blocks_note(built, &census))
                        } else {
                            let w = a.get("width").and_then(Value::as_u64).map_or(2400, |w| u32::try_from(w).unwrap_or(2400));
                            let (bytes, _, _, built) = preview::png(
                                &prep,
                                &prep.window,
                                w,
                                a.get("height").and_then(Value::as_u64).and_then(|h| u32::try_from(h).ok()),
                                false,
                                ctx,
                            )?;
                            (write_file(&p, &bytes, policy)?, bytes.len(), prep.blocks_note(built, &census))
                        };
                        let mut e = merged(saved_entry(ctx.roots(), &p, size, &fmt, kept.as_ref()), Some(note));
                        e["space"] = json!(name);
                        saved.push(e);
                    }
                    Ok(())
                };
                each().map_err(|e| with_written(e, &saved))?;
            }
            other => {
                return Err(ToolError::new(
                    code::UNSUPPORTED,
                    format!("CADCraft does not export `{other}`"),
                    "use dxf, dwg, pdf, svg or png (from the path's extension or `format`)",
                ));
            }
        }
        Ok(Reply::json(json!({"document": doc.handle, "format": fmt, "saved": saved})))
    }

    fn save(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        let i = self.docs.index(a)?;
        let caller = ctx.caller.clone().or_else(|| ctx.settings.author.clone());
        let doc = &mut self.docs.list[i];
        let path = match s(a, "path") {
            Some(p) => p.to_string(),
            None => match doc.source.as_ref().and_then(|p| p.file_name()) {
                Some(n) => n.to_string_lossy().into_owned(),
                None => format!("{}.dwg", doc.name().trim_end_matches(".dwg").trim_end_matches(".dxf")),
            },
        };
        let ext = Path::new(&path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !["dwg", "dxf", "dwt"].contains(&ext.as_str()) {
            return Err(ToolError::new(
                code::INVALID_ARGUMENT,
                format!("save_drawing writes DWG or DXF, not `{ext}`"),
                "use export for pdf, svg or png; or give a .dwg / .dxf path",
            ));
        }
        let p = ctx.resolve(&path, Access::Write)?;
        let mut kept = None;
        if p.exists() && ctx.settings.old_copies && !flag(a, "overwrite").unwrap_or(false) {
            let old = old_name(&p)?;
            std::fs::rename(&p, &old).map_err(|e| io_error(&p, &e))?;
            kept = Some((p.clone(), old));
        }
        if let Some(c) = caller
            && let Ok(dr) = doc.session.doc_mut()
        {
            dr.header.set_str("LASTSAVEDBY", &c);
        }
        let text =
            p.to_str().ok_or_else(|| ToolError::new(code::INVALID_ARGUMENT, "the path is not valid Unicode", "use another file name"))?.to_string();
        let mut params = json!({"path": text});
        for k in ["version", "binary"] {
            if let Some(v) = a.get(k).filter(|v| !v.is_null()) {
                params[k] = v.clone();
            }
        }
        ctx.stage("saving", 0);
        let r = doc.session.execute("saveas", &params);
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                if let Some((p, old)) = &kept {
                    let _ = std::fs::rename(old, p);
                }
                return Err(engine_error(e));
            }
        };
        let bytes = r["bytes"].as_u64().unwrap_or(0);
        // The drawing now lives in the saved file (as after Save As).
        doc.source = Some(p.clone());
        doc.format = ext.replace("dwt", "dwg");
        doc.version =
            r["version"].as_u64().and_then(|y| cadcraft_io::Version::parse(&y.to_string())).map(|v| v.code().to_string()).or(doc.version.clone());
        let mut v = doc.info(ctx.roots());
        v["saved"] = json!([saved_entry(ctx.roots(), &p, usize::try_from(bytes).unwrap_or(0), &ext, kept.as_ref())]);
        v["version"] = r["version"].clone();
        Ok(Reply::json(v))
    }

    fn roundtrip(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        let i = self.target(a, "path", ctx)?;
        let doc = &self.docs.list[i];
        let d = doc.drawing()?;
        let fmt = s(a, "format").unwrap_or("dxf").to_ascii_lowercase();
        let name = format!("roundtrip.{fmt}");
        let version = a
            .get("version")
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))
            .and_then(|t| cadcraft_io::Version::parse(&t));
        // What is compared is taken from each drawing in turn, and the copy read back is dropped
        // before the comparison: at no point are the open drawing, the written file, the copy
        // read back and their texts all held at once (C3: a 1.5-million-entity DXF went past
        // 6.9 GB this way).
        ctx.stage("comparing", 0);
        let before = Modelled::of(d, ctx)?;
        ctx.stage("writing", 0);
        let bytes = cadcraft_io::write_with(d, &name, &cadcraft_io::SaveOptions { version, binary: false })
            .map_err(|e| ToolError::new(code::FAILED, e.to_string(), "try the other format"))?;
        let written = bytes.len();
        ctx.check()?;
        ctx.stage("reading back", 0);
        let back = cadcraft_io::read_limited(&bytes, &name, limits_for(written, ctx.settings.max_memory_bytes)).map_err(|e| {
            ToolError::new(
                code::FAILED,
                format!("the written {fmt} does not read back: {e}"),
                "report this drawing; save as the other format meanwhile",
            )
        })?;
        drop(bytes);
        ctx.check()?;
        ctx.stage("comparing", 0);
        let after = Modelled::of(&back, ctx)?;
        drop(back);
        let problems = before.problems(&after);
        Ok(Reply::json(json!({"document": doc.handle, "format": fmt, "bytes": written, "lossless": problems.is_empty(), "problems": problems,
            "note": "compares what CADCraft models (entities per space and type, layers, blocks, layouts, texts, multiline texts with their formatting codes, width, attachment and rotation); data CADCraft keeps raw is written back as read"})))
    }
}

/// What `roundtrip_check` compares of a drawing: entities per space and type, layer, block and
/// layout counts, every text (plain), and the multiline texts of each space and named block as
/// written (contents with formatting codes, width, attachment, rotation; anonymous blocks are
/// dimension and table pictures, rebuilt on save). Texts are sorted, so a comparison is a merge.
struct Modelled {
    entities: BTreeMap<String, BTreeMap<String, usize>>,
    layers: usize,
    blocks: usize,
    layouts: usize,
    texts: Vec<String>,
    mtexts: Vec<String>,
}

impl Modelled {
    fn of(d: &Drawing, ctx: &Ctx) -> Result<Self, ToolError> {
        let (entities, _) = read::counts(d);
        let flt = read::Filter { layers: None, window: None, contains: None, exact: false, types: None, expand_blocks: false };
        let mut texts = read::text_values(d, &read::spaces(d), &flt, ctx)?;
        texts.sort_unstable();
        let mut mtexts = Vec::new();
        let stores =
            read::spaces(d).into_iter().filter_map(|(_, sp)| d.space(&sp)).chain(d.blocks.values().filter(|b| !b.anonymous).map(|b| &b.entities));
        for store in stores {
            for e in store.iter() {
                if let EntityKind::MText(t) = &e.kind {
                    mtexts.push(mtext_key(t));
                }
            }
        }
        mtexts.sort_unstable();
        Ok(Modelled {
            entities,
            layers: d.layers.len(),
            blocks: d.blocks.values().filter(|b| !b.anonymous).count(),
            layouts: d.layouts.len(),
            texts,
            mtexts,
        })
    }

    fn problems(&self, after: &Modelled) -> Vec<Value> {
        let mut problems = Vec::new();
        for (space, c) in &self.entities {
            for (ty, n) in c {
                let m = after.entities.get(space).and_then(|x| x.get(ty)).copied().unwrap_or(0);
                if m != *n {
                    problems.push(json!({"what": "entities", "space": space, "type": ty, "before": n, "after": m}));
                }
            }
        }
        for (space, c) in &after.entities {
            for (ty, m) in c {
                if self.entities.get(space).and_then(|x| x.get(ty)).is_none() {
                    problems.push(json!({"what": "entities", "space": space, "type": ty, "before": 0, "after": m}));
                }
            }
        }
        for (what, x, y) in [("layers", self.layers, after.layers), ("blocks", self.blocks, after.blocks), ("layouts", self.layouts, after.layouts)] {
            if x != y {
                problems.push(json!({"what": what, "before": x, "after": y}));
            }
        }
        if self.texts != after.texts {
            let lost = only_in(&self.texts, &after.texts, 20);
            problems.push(json!({"what": "texts", "before": self.texts.len(), "after": after.texts.len(), "lost_examples": lost}));
        }
        if self.mtexts != after.mtexts {
            let lost = only_in(&self.mtexts, &after.mtexts, usize::MAX);
            let changed: Vec<Value> = lost.iter().take(5).map(|k| mtext_example(k)).collect();
            problems.push(json!({"what": "mtext", "before": self.mtexts.len(), "after": after.mtexts.len(), "changed": lost.len(),
                "examples": changed, "note": "multiline texts whose contents (formatting codes included), width, attachment or rotation changed"}));
        }
        problems
    }
}

/// The items of sorted `a` that sorted `b` does not hold (counting repeats), at most `limit`.
/// A merge, so a drawing with a million texts compares in one pass.
fn only_in<'a>(a: &'a [String], b: &[String], limit: usize) -> Vec<&'a String> {
    let mut out = Vec::new();
    let mut j = 0;
    for x in a {
        while b.get(j).is_some_and(|y| y < x) {
            j += 1;
        }
        if b.get(j) == Some(x) {
            j += 1;
        } else {
            out.push(x);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Separates the fields of an MTEXT key (a character no contents hold: control characters other
/// than tab and line feed are dropped on write).
const KEY_SEP: char = '\u{1f}';

/// A multiline text as written: contents with its formatting codes, width, attachment and
/// rotation (in [0, 2π) and rounded to 1e-9: DWG stores the direction, not the angle).
fn mtext_key(t: &cadcraft_engine::doc::MText) -> String {
    let r = |v: f64| if v.is_finite() { (v * 1e9).round() / 1e9 } else { 0.0 };
    let turn = std::f64::consts::TAU;
    let a = if t.rotation.is_finite() { t.rotation.rem_euclid(turn) } else { 0.0 };
    let a = if turn - a < 1e-9 { 0.0 } else { a };
    format!("{}{KEY_SEP}{}{KEY_SEP}{}{KEY_SEP}{}", t.contents, r(t.width), t.attach, r(a))
}

fn mtext_example(key: &str) -> Value {
    let mut it = key.split(KEY_SEP);
    let contents: String = it.next().unwrap_or_default().chars().take(300).collect();
    let (width, attach, rotation) = (it.next().unwrap_or_default(), it.next().unwrap_or_default(), it.next().unwrap_or_default());
    json!({"contents": contents, "width": width.parse::<f64>().ok(), "attach": attach.parse::<u8>().ok(), "rotation": rotation.parse::<f64>().ok()})
}

/// A call that writes several files and fails part way: the error lists the files already
/// written (`files_written`, as `saved` lists them) and says so, as they stay on disk.
/// `v` (an object) with the fields of `note` (the block-expansion cap report, when there is one).
fn merged(mut v: Value, note: Option<Value>) -> Value {
    if let (Some(o), Some(Value::Object(n))) = (v.as_object_mut(), note) {
        o.extend(n);
    }
    v
}

fn with_written(e: ToolError, written: &[Value]) -> ToolError {
    if written.is_empty() {
        return e;
    }
    let n = written.len();
    let reason = format!("{} ({n} file{} written before it, listed in files_written)", e.reason, if n == 1 { " was" } else { "s were" });
    ToolError { reason, ..e }.with("files_written", json!(written))
}

/// Commands `edit` refuses: they open, close or save documents, which the server's own tools do
/// inside the roots.
const REFUSED: &[&str] =
    &["open", "new", "qnew", "close", "closeall", "quit", "exit", "qsave", "save", "saveas", "document.switch", "recover", "partialopen"];

/// Argument names that hold file paths, and the commands whose paths are written.
const PATH_KEYS: &[&str] = &["path", "file", "filename", "output", "dir", "folder"];
fn writes_files(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    [
        "wblock",
        "plot",
        "publish",
        "export",
        "dxfout",
        "pdf",
        "png",
        "svg",
        "saveas",
        "layout.saveas",
        "mlstyle.save",
        "layerstate.export",
        "stlout",
        "wmfout",
        "bmpout",
        "jpgout",
        "tifout",
        "psout",
        "acisout",
        "etransmit",
        "archive",
    ]
    .iter()
    .any(|w| c.starts_with(w) || c.ends_with("out") || c.contains("export"))
}

/// Paths in a command's parameters, resolved inside the roots (absolute afterwards).
fn confine_paths(cmd: &str, params: &mut Value, ctx: &Ctx) -> Result<(), ToolError> {
    let access = if writes_files(cmd) { Access::Write } else { Access::Read };
    if let Some(o) = params.as_object_mut() {
        for (k, v) in o.iter_mut() {
            if PATH_KEYS.contains(&k.to_ascii_lowercase().as_str())
                && let Some(p) = v.as_str()
            {
                let r = ctx.resolve(p, access)?;
                *v = json!(r.to_string_lossy());
            }
        }
    }
    Ok(())
}

/// One `edit` operation as (command id, params), or a command-line text.
enum Step {
    Cmd(String, Value),
    Line(String),
    Undo,
    Many(Vec<(String, Value)>),
}

fn op_params(op: &Value) -> Value {
    match op.get("params") {
        Some(p) if p.is_object() => p.clone(),
        _ => {
            let mut o = op.as_object().cloned().unwrap_or_default();
            for k in ["op", "command", "type", "action"] {
                o.remove(k);
            }
            Value::Object(o)
        }
    }
}

fn step_of(op: &Value, d: &Drawing) -> Result<Step, ToolError> {
    let kind = op.get("op").and_then(Value::as_str).unwrap_or(if op.get("command").is_some() {
        "command"
    } else if op.get("text").is_some() && op.as_object().is_some_and(|o| o.len() == 1) {
        "cmdline"
    } else {
        ""
    });
    let p = op_params(op);
    let bad = |m: String, n: &str| ToolError::new(code::INVALID_ARGUMENT, m, n.to_string());
    Ok(match kind.to_ascii_lowercase().as_str() {
        "command" | "execute" | "run" => {
            let c = op.get("command").and_then(Value::as_str).ok_or_else(|| {
                bad("a command op needs `command`".into(), "e.g. {op: \"command\", command: \"line\", params: {points: [[0,0],[10,0]]}}")
            })?;
            Step::Cmd(c.to_string(), p)
        }
        "cmdline" | "line_input" | "type" => Step::Line(op.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
        "add" | "create" | "draw" => {
            let t = op.get("type").and_then(Value::as_str).ok_or_else(|| {
                bad(
                    "an add op needs `type` (a draw command: line, pline, circle, arc, text, mtext, insert, hatch, dimlinear…)".into(),
                    "e.g. {op: \"add\", type: \"circle\", center: [0,0], radius: 5}",
                )
            })?;
            let t = match t.to_ascii_lowercase().as_str() {
                "polyline" | "lwpolyline" => "pline".to_string(),
                "block_reference" | "blockref" | "insert" => "insert".to_string(),
                "dimension" | "linear_dimension" => "dimlinear".to_string(),
                other => other.to_string(),
            };
            Step::Cmd(t, p)
        }
        "delete" | "erase" | "remove" => Step::Cmd("erase".into(), p),
        "set_text" | "edit_text" => Step::Cmd("textedit".into(), p),
        "set_properties" | "properties" | "change" => Step::Cmd("properties.set".into(), p),
        "set_attributes" | "attributes" | "attedit" => {
            if p.get("handle").is_some() {
                Step::Cmd("attedit".into(), p)
            } else {
                let block = p.get("block").and_then(Value::as_str).ok_or_else(|| {
                    bad(
                        "set_attributes needs `handle` (a block reference) or `block` (every reference of that block)".into(),
                        "read_drawing section title_block names the handles",
                    )
                })?;
                let values = p.get("values").cloned().unwrap_or(json!({}));
                let mut v = Vec::new();
                for (_, space) in read::spaces(d) {
                    if let Some(store) = d.space(&space) {
                        for e in store.iter() {
                            if let EntityKind::Insert(i) = &e.kind
                                && i.block.eq_ignore_ascii_case(block)
                            {
                                v.push(("attedit".to_string(), json!({"handle": e.handle.hex(), "values": values})));
                            }
                        }
                    }
                }
                if v.is_empty() {
                    return Err(ToolError::new(
                        code::NOT_FOUND,
                        format!("no reference of block `{block}`"),
                        "read_drawing section blocks lists the blocks and their references",
                    ));
                }
                Step::Many(v)
            }
        }
        "layer" => {
            let action = op.get("action").and_then(Value::as_str).unwrap_or("set").to_ascii_lowercase();
            match action.as_str() {
                "new" | "create" | "add" => Step::Cmd("layer.new".into(), p),
                "set" | "change" | "edit" => Step::Cmd("layer.set".into(), p),
                "delete" | "remove" => Step::Cmd("layer.delete".into(), p),
                "current" => Step::Cmd("layer.current".into(), p),
                "rename" => {
                    let mut q = p.clone();
                    q["table"] = json!("layer");
                    Step::Cmd("rename".into(), q)
                }
                other => return Err(bad(format!("unknown layer action `{other}`"), "use new, set, delete, current or rename")),
            }
        }
        "block" => {
            let action = op.get("action").and_then(Value::as_str).unwrap_or("define").to_ascii_lowercase();
            match action.as_str() {
                "define" | "create" | "new" => Step::Cmd("block".into(), p),
                "insert" | "place" => Step::Cmd("insert".into(), p),
                "rename" => {
                    let mut q = p.clone();
                    q["table"] = json!("block");
                    Step::Cmd("rename".into(), q)
                }
                "delete" | "remove" | "purge" => {
                    let name = p
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad("block delete needs `name`".into(), "e.g. {op: \"block\", action: \"delete\", name: \"DOOR\"}"))?;
                    let mut v = Vec::new();
                    if p.get("references").and_then(Value::as_bool).unwrap_or(true) {
                        for (_, space) in read::spaces(d) {
                            let hs: Vec<String> = d
                                .space(&space)
                                .into_iter()
                                .flat_map(|st| st.iter())
                                .filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(name)))
                                .map(|e| e.handle.hex())
                                .collect();
                            if !hs.is_empty() {
                                let layout = match &space {
                                    Space::Model => "Model".to_string(),
                                    Space::Paper(n) => n.clone(),
                                };
                                v.push(("layout.set".to_string(), json!({"name": layout})));
                                v.push(("erase".to_string(), json!({"handles": hs})));
                            }
                        }
                    }
                    v.push(("purge".to_string(), json!({"type": "blocks", "names": name})));
                    Step::Many(v)
                }
                other => return Err(bad(format!("unknown block action `{other}`"), "use define, insert, delete or rename")),
            }
        }
        "space" | "layout" => Step::Cmd("layout.set".into(), json!({"name": p.get("name").cloned().unwrap_or(json!("Model"))})),
        "undo" => Step::Undo,
        "" => {
            return Err(bad(
                "each op needs `op`".into(),
                "e.g. {op: \"command\", command: \"line\", params: {...}} or {op: \"add\", type: \"text\", at: [0,0], text: \"A\"}",
            ));
        }
        other => {
            // A command id given as the op ("line", "circle", "layer.new").
            if cadcraft_engine::find_command(other).is_some() {
                Step::Cmd(other.to_string(), p)
            } else {
                return Err(bad(
                    format!("unknown op `{other}`"),
                    "use command, cmdline, add, delete, set_text, set_attributes, set_properties, layer, block, space or undo",
                ));
            }
        }
    })
}

impl Engine {
    fn edit(&mut self, a: &Value, ctx: &Ctx) -> Result<Reply, ToolError> {
        // One command (`{command, params}`) or command line (`{text}`) may come without `ops`.
        let single = match (a.get("command").and_then(Value::as_str), a.get("text").and_then(Value::as_str)) {
            (Some(c), _) => Some(json!([{"op": "command", "command": c, "params": a.get("params").cloned().unwrap_or(json!({}))}])),
            (None, Some(t)) => Some(json!([{"op": "cmdline", "text": t}])),
            _ => None,
        };
        let ops = a.get("ops").filter(|o| o.is_array()).cloned().or(single).and_then(|o| o.as_array().cloned()).ok_or_else(|| {
            ToolError::new(
                code::INVALID_ARGUMENT,
                "`ops` must be a list of operations",
                "e.g. ops: [{op: \"add\", type: \"line\", points: [[0,0],[10,0]]}]",
            )
        })?;
        let atomic = flag(a, "atomic").unwrap_or(true);
        let i = self.docs.index(a)?;
        self.docs.list[i].session.state().map_err(engine_error)?;
        let total = ops.len() + usize::from(a.get("space").is_some());
        ctx.stage("editing", u64::try_from(total).unwrap_or(0));
        let mut all: Vec<Value> = Vec::new();
        if let Some(sp) = a.get("space").filter(|v| !v.is_null()) {
            all.push(json!({"op": "space", "name": sp}));
        }
        all.extend(ops);
        let before = self.call.insert(Before::of(&self.docs.list[i]));
        let r = edit_ops(&mut self.docs.list[i], before, &all, atomic, ctx);
        self.call = None;
        r
    }

    /// After a call panicked: the `edit` it interrupted is undone whole, so the drawing (and what
    /// is published and auto-saved after the panic) is as it was before the call. Returns whether
    /// there was one.
    pub fn undo_interrupted_call(&mut self) -> bool {
        let Some(before) = self.call.take() else { return false };
        let Some(doc) = self.docs.list.iter_mut().find(|d| d.uid == before.uid) else { return false };
        rollback(doc, &before);
        true
    }
}

/// `edit`'s operations on `doc`; `before` is the document as it was before the call.
fn edit_ops(doc: &mut Doc, before: &Before, all: &[Value], atomic: bool, ctx: &Ctx) -> Result<Reply, ToolError> {
    let mut results = Vec::new();
    let mut failed_at = None;
    // A command that writes a picture (plot, export, publish, pdfbook, preview) draws it complete or
    // writes nothing, as the picture tools do.
    doc.session.complete_pictures = Some(preview::block_limit());
    for (k, op) in all.iter().enumerate() {
        if ctx.cancelled() {
            rollback(doc, before);
            return Err(ToolError::new(
                code::CANCELLED,
                format!("stopped as asked before operation {k}"),
                "the drawing is as it was before the call",
            )
            .with("rolled_back", json!(true)));
        }
        // With atomic: false each operation is all or nothing on its own: one that fails part
        // way (a shorthand that runs several commands, a script of several lines) is undone
        // whole. An operation typed into a command an earlier one left waiting is that
        // command's input: the command keeps its own undo (it ends restored on an error).
        let op_before = (!atomic && doc.session.running.is_none()).then(|| Before::of(doc));
        let outcome = (|| -> Result<Value, ToolError> {
            let step = step_of(op, doc.session.doc().map_err(engine_error)?)?;
            let run = |s: &mut Session, id: &str, mut p: Value| -> Result<Value, ToolError> {
                if REFUSED.contains(&id.to_ascii_lowercase().as_str()) {
                    return Err(ToolError::new(
                        code::UNSUPPORTED,
                        format!("`{id}` is not run inside edit"),
                        "use open_drawing, new_drawing, save_drawing or close_drawing",
                    ));
                }
                if cadcraft_engine::find_command(id).is_none() {
                    let ids: Vec<&str> = cadcraft_engine::command_specs().iter().map(|c| c.id).collect();
                    return Err(ToolError::new(
                        code::NOT_FOUND,
                        format!("unknown command `{id}`"),
                        "list_commands names every command and its parameters",
                    )
                    .with("closest", json!(super::contract::closest(id, ids, 5))));
                }
                confine_paths(id, &mut p, ctx)?;
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(id, &p)))
                    .map_err(|_| {
                        ToolError::new(code::INTERNAL, format!("`{id}` failed inside the engine"), "the drawing was restored; check the parameters")
                    })?
                    .map_err(engine_error)
            };
            match step {
                Step::Cmd(id, p) => run(&mut doc.session, &id, p),
                Step::Many(list) => {
                    let mut out = Vec::new();
                    for (id, p) in list {
                        out.push(run(&mut doc.session, &id, p)?);
                    }
                    Ok(json!(out))
                }
                Step::Line(text) => {
                    let s = &mut doc.session;
                    let n = s.log.len();
                    let r = if text.contains('\n') { s.script(&text) } else { s.cmdline(&text) };
                    let output: Vec<String> = s.log.iter().skip(n).cloned().collect();
                    r.map_err(engine_error)?;
                    Ok(json!({"output": output, "prompt": s.prompt_text(), "running": s.running.as_ref().map(|r| r.id.clone())}))
                }
                Step::Undo => doc.session.undo().map(|l| json!({"undone": l})).map_err(engine_error),
            }
        })();
        ctx.step(1);
        match outcome {
            Ok(v) => {
                results.push(json!({"index": k, "op": op.get("op").or(op.get("command")).cloned().unwrap_or(Value::Null), "ok": true, "result": v}))
            }
            Err(e) => {
                let mut r =
                    json!({"index": k, "op": op.get("op").or(op.get("command")).cloned().unwrap_or(Value::Null), "ok": false, "error": e.to_value()});
                if atomic {
                    results.push(r);
                    failed_at = Some((k, e));
                    break;
                }
                if let Some(b) = &op_before {
                    rollback(doc, b);
                    r["rolled_back"] = json!(true);
                    r["error"]["next_step"] = json!(format!("{} — nothing of this operation was applied; send it again (fixed)", e.next_step));
                }
                results.push(r);
            }
        }
    }
    // A command left waiting for input (a cmdline op that did not finish it) is ended.
    if doc.session.running.is_some() {
        doc.session.cancel();
    }
    if let Some((k, e)) = failed_at {
        rollback(doc, before);
        return Err(ToolError::new(
            e.code.as_str(),
            format!("operation {k} failed: {}", e.reason),
            format!(
                "{} — nothing of this call was applied (atomic); send it again with that operation fixed, or atomic: false to keep the others",
                e.next_step
            ),
        )
        .with("failed_at", json!(k))
        .with("rolled_back", json!(true))
        .with("results", json!(results)));
    }
    let ok = results.iter().filter(|r| r["ok"] == true).count();
    let mut v = doc.info(ctx.roots());
    v["applied"] = json!(ok);
    v["failed"] = json!(results.len() - ok);
    v["results"] = json!(results);
    v["entities"] = json!(doc.drawing()?.entity_count());
    v["next_step"] = json!("save_drawing to keep the changes; render_preview to look at them");
    Ok(Reply::json(v))
}

/// A document's session as it was at one point of an `edit` call: its drawings, each with its
/// undo and redo history, selection, space, views and Block Editor, and which one is active.
struct Before {
    uid: u64,
    docs: Vec<cadcraft_engine::DocState>,
    active: usize,
}

impl Before {
    fn of(doc: &Doc) -> Self {
        Before { uid: doc.uid, docs: doc.session.docs.clone(), active: doc.session.active }
    }
}

/// Put `doc` back as it was at `before`. A command the call left waiting for input is ended first.
fn rollback(doc: &mut Doc, before: &Before) {
    if doc.session.running.is_some() {
        doc.session.cancel();
    }
    doc.session.docs.clone_from(&before.docs);
    doc.session.active = before.active;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::super::docs::Settings;
    use super::super::files::Roots;
    use super::*;

    fn ctx(write: Option<PathBuf>, old_copies: bool) -> Ctx {
        Ctx::new(Settings { roots: Roots { read: None, write }, old_copies, ..Settings::default() })
    }

    fn engine() -> (Engine, Ctx) {
        let mut e = Engine::default();
        let c = ctx(None, true);
        e.call("new_drawing", &json!({}), &c).unwrap();
        (e, c)
    }

    fn state(e: &Engine) -> &cadcraft_engine::DocState {
        e.docs.list[0].session.state().unwrap()
    }

    fn labels(e: &Engine) -> Vec<String> {
        state(e).undo.iter().map(|u| u.label.clone()).collect()
    }

    fn edit(e: &mut Engine, c: &Ctx, args: Value) -> Result<Value, ToolError> {
        e.call("edit", &args, c).map(|r| r.value)
    }

    fn made(r: &Value, k: usize) -> String {
        let x = &r["results"][k]["result"];
        x["handle"].as_str().or_else(|| x["handles"][0].as_str()).unwrap_or_else(|| panic!("no handle in {r}")).to_string()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cadcraft-whiq-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A failed atomic call leaves the undo and redo history exactly as it was, even when an
    /// `undo` op in it moved steps before another op recorded a new one.
    #[test]
    fn a_failed_atomic_call_keeps_the_history_as_it_was() {
        let (mut e, c) = engine();
        let circle = json!({"op": "add", "type": "circle", "center": [0, 0], "radius": 1});
        edit(&mut e, &c, json!({"ops": [circle, circle]})).unwrap();
        edit(&mut e, &c, json!({"ops": [{"op": "undo"}]})).unwrap();
        let (doc, undo, redo) = (state(&e).doc.clone(), labels(&e), state(&e).redo.len());
        let err = edit(
            &mut e,
            &c,
            json!({"ops": [{"op": "undo"}, {"op": "add", "type": "line", "points": [[0, 0], [5, 5]]}, {"op": "command", "command": "no-such-command"}]}),
        )
        .unwrap_err();
        assert!(err.next_step.contains("nothing of this call was applied"), "{}", err.next_step);
        assert!(Arc::ptr_eq(&state(&e).doc, &doc));
        assert_eq!(labels(&e), undo);
        assert_eq!(state(&e).redo.len(), redo);
        assert!(e.call.is_none());
    }

    /// With atomic: false an operation that makes several changes and fails part way (a script of
    /// several lines) keeps none of them; the operations that worked are kept.
    #[test]
    fn an_operation_that_fails_part_way_keeps_none_of_it() {
        let (mut e, c) = engine();
        let (count, undo) = (state(&e).doc.entity_count(), labels(&e));
        let script = "circle {\"center\": [0, 0], \"radius\": 1}\nno-such-command";
        let r = edit(
            &mut e,
            &c,
            json!({"atomic": false, "ops": [{"op": "cmdline", "text": script}, {"op": "add", "type": "line", "points": [[0, 0], [5, 5]]}]}),
        )
        .unwrap();
        assert_eq!((r["applied"].as_u64(), r["failed"].as_u64()), (Some(1), Some(1)), "{r}");
        assert_eq!(r["results"][0]["rolled_back"], true, "{r}");
        assert!(r["results"][0]["error"]["next_step"].as_str().unwrap().contains("nothing of this operation was applied"), "{r}");
        assert_eq!(state(&e).doc.entity_count(), count + 1, "the script's circle is gone, the line stays");
        assert_eq!(labels(&e), [undo, vec!["Line".to_string()]].concat());
        assert!(e.docs.list[0].session.running.is_none());
    }

    /// A shorthand that runs one command per block reference (set_attributes by block) and fails
    /// on the second reference keeps the first one's change neither.
    #[test]
    fn a_shorthand_that_fails_on_its_second_command_keeps_none_of_it() {
        let (mut e, c) = engine();
        let r = edit(
            &mut e,
            &c,
            json!({"ops": [
                {"op": "add", "type": "line", "points": [[0, 0], [10, 0]]},
                {"op": "add", "type": "attdef", "tag": "NO", "prompt": "Number", "default": "1", "at": [0, 5], "height": 1}
            ]}),
        )
        .unwrap();
        let (l, t) = (made(&r, 0), made(&r, 1));
        let r = edit(
            &mut e,
            &c,
            json!({"ops": [
                {"op": "block", "action": "define", "name": "B", "base": [0, 0], "handles": [l, t], "keep": "delete"},
                {"op": "block", "action": "insert", "name": "B", "at": [100, 0]},
                {"op": "block", "action": "insert", "name": "B", "at": [0, 50]}
            ]}),
        )
        .unwrap();
        let a = made(&r, 1);
        // While reference A is edited in place, only its working set may change: the attribute
        // edit of the other reference is refused after A's has been made.
        edit(&mut e, &c, json!({"ops": [{"op": "command", "command": "refedit", "params": {"handle": a}}]})).unwrap();
        let (doc, undo) = (state(&e).doc.clone(), labels(&e));
        let r = edit(&mut e, &c, json!({"atomic": false, "ops": [{"op": "set_attributes", "block": "B", "values": {"NO": "2"}}]})).unwrap();
        assert_eq!(r["failed"], 1, "{r}");
        assert_eq!(r["results"][0]["rolled_back"], true, "{r}");
        assert!(Arc::ptr_eq(&state(&e).doc, &doc), "reference A keeps its old value");
        assert_eq!(labels(&e), undo);
    }

    /// What a panic part way through an edit leaves (a change made, a command waiting for input)
    /// is undone by the runtime's panic arm before it publishes and auto-saves.
    #[test]
    fn an_interrupted_edit_is_undone_whole() {
        let (mut e, c) = engine();
        edit(&mut e, &c, json!({"ops": [{"op": "add", "type": "circle", "center": [0, 0], "radius": 1}]})).unwrap();
        assert!(e.call.is_none(), "a finished call leaves nothing to undo");
        let (doc, undo) = (state(&e).doc.clone(), labels(&e));
        e.call = Some(Before::of(&e.docs.list[0]));
        let s = &mut e.docs.list[0].session;
        s.execute("line", &json!({"points": [[0, 0], [5, 5]]})).unwrap();
        s.cmdline("line").unwrap();
        s.cmdline("0,0").unwrap();
        assert!(s.running.is_some());
        assert!(e.undo_interrupted_call());
        assert!(Arc::ptr_eq(&state(&e).doc, &doc));
        assert_eq!(labels(&e), undo);
        assert!(e.docs.list[0].session.running.is_none());
        assert!(!e.undo_interrupted_call(), "once only");
    }

    /// An export of several layouts that fails part way names the files it already wrote.
    #[test]
    fn a_failed_multi_layout_export_lists_the_files_written() {
        let dir = scratch("export-written");
        let c = ctx(Some(dir.clone()), false);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        edit(&mut e, &c, json!({"ops": [{"op": "add", "type": "line", "points": [[0, 0], [10, 10]]}]})).unwrap();
        let spaces = read::spaces(e.docs.list[0].drawing().unwrap());
        assert!(spaces.len() > 1, "a new drawing has layouts");
        // The second space's file can't be written: a folder stands in its place.
        std::fs::create_dir_all(dir.join(format!("plan-{}.svg", spaces[1].0)).join("x")).unwrap();
        let err = e.call("export", &json!({"path": "plan.svg", "space": "all"}), &c).map(|_| ()).unwrap_err();
        let written = err.extra["files_written"].as_array().unwrap_or_else(|| panic!("{}", err.to_value()));
        assert_eq!(written.len(), 1);
        assert!(written[0]["path"].as_str().unwrap().ends_with("plan-Model.svg"), "{written:?}");
        assert!(err.reason.contains("1 file was written before it"), "{}", err.reason);
        assert!(dir.join("plan-Model.svg").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A preview written to files tile by tile that fails part way names the tiles written.
    #[test]
    fn a_failed_tiled_preview_lists_the_files_written() {
        let dir = scratch("render-written");
        let c = ctx(Some(dir.clone()), false);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        edit(&mut e, &c, json!({"ops": [{"op": "add", "type": "line", "points": [[0, 0], [10, 10]]}]})).unwrap();
        std::fs::create_dir_all(dir.join("prev-c1r0.svg").join("x")).unwrap();
        let err = e.call("render_preview", &json!({"output": "prev.svg", "tiles": {"cols": 2, "rows": 1}}), &c).map(|_| ()).unwrap_err();
        let written = err.extra["files_written"].as_array().unwrap_or_else(|| panic!("{}", err.to_value()));
        assert_eq!(written.len(), 1);
        assert!(written[0]["path"].as_str().unwrap().ends_with("prev-c0r0.svg"), "{written:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sample drawing the SVG fixture was made from (with the code that wrote the whole SVG
    /// into one string): lines, a circle, text, a point, an infinite line, a wipeout, on 3 layers.
    fn svg_sample() -> (Engine, Ctx) {
        let (mut e, c) = engine();
        edit(
            &mut e,
            &c,
            json!({"ops": [
                {"op": "command", "command": "layer.new", "params": {"name": "WALLS", "color": 1}},
                {"op": "command", "command": "layer.new", "params": {"name": "TEXT", "color": 3}},
                {"op": "add", "type": "line", "points": [[0, 0], [100, 50]]},
                {"op": "layer", "action": "current", "name": "WALLS"},
                {"op": "add", "type": "circle", "center": [30, 30], "radius": 12.5},
                {"op": "add", "type": "line", "points": [[5, 90], [60, 10]]},
                {"op": "layer", "action": "current", "name": "TEXT"},
                {"op": "add", "type": "text", "at": [10, 70], "text": "Hello & <World>", "height": 6},
                {"op": "add", "type": "mtext", "at": [50, 80], "text": "Multi line\\Ptext", "height": 4},
                {"op": "layer", "action": "current", "name": "0"},
                {"op": "add", "type": "point", "at": [70, 20]},
                {"op": "add", "type": "xline", "base": [20, 20], "angle": 45},
                {"op": "add", "type": "pline", "points": [[0, 0], [20, 0], [20, 20], [0, 20]], "closed": true},
                {"op": "add", "type": "wipeout", "points": [[25, 25], [45, 25], [45, 45], [25, 45]]},
                {"op": "add", "type": "line", "points": [[0, 100], [100, 0]]}
            ]}),
        )
        .unwrap();
        (e, c)
    }

    /// The SVG written slice by slice is byte for byte the one the old whole-string code wrote
    /// (`testdata/export_sample.svg` was made with it before the change), whatever the slice size,
    /// and so is `svg_tile` of the whole display list.
    #[test]
    fn an_svg_written_in_slices_is_byte_identical_to_the_whole_string_one() {
        let (e, c) = svg_sample();
        let d = e.docs.list[0].drawing().unwrap();
        let expected = include_str!("testdata/export_sample.svg");
        let prep = preview::prepare(d, &json!({"space": "Model"}), &c).unwrap();
        let whole = preview::svg_tile(&prep.list_for(&prep.window, 1600.0, &c).unwrap(), &prep.layers, &prep.window, 1600.0, false, &c).unwrap().0;
        assert_eq!(whole, expected);
        for slice in [1, 2, 3, 7, preview::SLICE_ENTITIES] {
            let mut out = Vec::new();
            let (n, _) = prep.write_svg_sliced(&mut out, &prep.window, 1600.0, &c, slice).unwrap_or_else(|_| panic!("slice {slice}"));
            assert_eq!(n, out.len());
            assert_eq!(String::from_utf8(out).unwrap(), expected, "slice of {slice} entities");
        }
    }

    /// `export` writes that same SVG to the file and answers its size; an earlier file is kept as
    /// OLD_<name>; no temporary file stays.
    #[test]
    fn export_svg_writes_the_file_and_reports_its_size() {
        let dir = scratch("export-svg-stream");
        let c = ctx(Some(dir.clone()), true);
        let (mut e, _) = svg_sample();
        let r = e.call("export", &json!({"path": "plan.svg"}), &c).unwrap().value;
        let expected = include_str!("testdata/export_sample.svg");
        assert_eq!(std::fs::read_to_string(dir.join("plan.svg")).unwrap(), expected);
        assert_eq!(r["saved"][0]["bytes"], json!(expected.len()));
        e.call("export", &json!({"path": "plan.svg"}), &c).unwrap();
        assert!(dir.join("OLD_plan.svg").is_file());
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|f| f.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(left.iter().all(|n| !n.ends_with(".cctmp")), "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// More than 2,000 entities, among them inserts of a block, so `build_slice` builds big slices
    /// on all cores and seeds the block-expansion count: lines, circles and references to a
    /// block of a line and a circle.
    fn many_entities() -> (Engine, Ctx) {
        let (mut e, c) = engine();
        let r = edit(
            &mut e,
            &c,
            json!({"ops": [
                {"op": "add", "type": "line", "points": [[0, 0], [8, 0]]},
                {"op": "add", "type": "circle", "center": [4, 4], "radius": 2}
            ]}),
        )
        .unwrap();
        let (l, ci) = (made(&r, 0), made(&r, 1));
        let mut ops = vec![json!({"op": "block", "action": "define", "name": "B", "base": [0, 0], "handles": [l, ci], "keep": "delete"})];
        for k in 0..700 {
            let x = f64::from(k) * 20.0;
            ops.push(json!({"op": "add", "type": "line", "points": [[x, 20], [x + 5.0, 30]]}));
            ops.push(json!({"op": "block", "action": "insert", "name": "B", "at": [x, 40]}));
            ops.push(json!({"op": "add", "type": "circle", "center": [x, 60], "radius": 3}));
        }
        edit(&mut e, &c, json!({"ops": ops})).unwrap();
        (e, c)
    }

    /// The same, for a drawing big enough that the slices of 512 and more are built in parallel
    /// and later slices start from the earlier ones' expansion count: slices of 1, 512 and 16,384
    /// entities are byte for byte the SVG of the whole display list.
    #[test]
    fn a_big_svg_written_in_slices_is_byte_identical_to_the_whole_list_one() {
        let (e, c) = many_entities();
        let d = e.docs.list[0].drawing().unwrap();
        assert!(d.model.len() >= 2_000, "{}", d.model.len());
        let prep = preview::prepare(d, &json!({"space": "Model"}), &c).unwrap();
        let whole = preview::svg_tile(&prep.list_for(&prep.window, 1600.0, &c).unwrap(), &prep.layers, &prep.window, 1600.0, false, &c).unwrap().0;
        for slice in [1, 512, preview::SLICE_ENTITIES] {
            let mut out = Vec::new();
            let (n, expanded) = prep.write_svg_sliced(&mut out, &prep.window, 1600.0, &c, slice).unwrap_or_else(|_| panic!("slice {slice}"));
            assert_eq!(n, out.len());
            let note = prep.blocks_note(expanded, &prep.census(&prep.window, None, &c).unwrap());
            assert!(expanded > 0 && note["blocks_expanded_total"] == json!(expanded), "{note}");
            assert_eq!(note, json!({"blocks_expanded_total": expanded}));
            assert!(String::from_utf8(out).unwrap() == whole, "slice of {slice} entities");
        }
    }

    /// A layout with sheet objects below and over two viewports (one clipped to a triangle) and
    /// 2,400 model objects, a third of them block references.
    fn layout_drawing() -> Drawing {
        use cadcraft_engine::doc::{Block, Circle, Common, Entity, Handle, Insert, Line, LwPolyline, Solid, Viewport};
        use cadcraft_engine::geom::{PolyVertex, Vec2, Vec3};
        let mut d = Drawing::new_metric();
        let mut b = Block::new("B");
        for k in 0..2u64 {
            let mut e = Entity::new(Handle(900 + k), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(6.0, 4.0, 0.0) }));
            e.common = Common::default();
            b.entities.push(e);
        }
        d.blocks.insert("B".into(), std::sync::Arc::new(b));
        let insert = |at: Vec3| {
            EntityKind::Insert(Insert {
                block: "B".into(),
                insert: at,
                scale: Vec3::new(1.0, 1.0, 1.0),
                rotation: 0.0,
                attribs: vec![],
                cols: 1,
                rows: 1,
                col_spacing: 0.0,
                row_spacing: 0.0,
            })
        };
        for k in 0..2_400u32 {
            let at = Vec3::new(f64::from(k % 80) * 10.0, f64::from(k / 80) * 10.0, 0.0);
            let kind = match k % 3 {
                0 => insert(at),
                1 => EntityKind::Line(Line { a: at, b: at + Vec3::new(6.0, 4.0, 0.0) }),
                _ => EntityKind::Circle(Circle { center: at, radius: 2.0 }),
            };
            d.add(&Space::Model, Common::default(), kind).unwrap();
        }
        let solid = |x: f64| {
            EntityKind::Solid(Solid {
                corners: [Vec3::new(x, 0.0, 0.0), Vec3::new(x + 8.0, 0.0, 0.0), Vec3::new(x, 8.0, 0.0), Vec3::new(x + 8.0, 8.0, 0.0)],
            })
        };
        d.add(&Space::Model, Common::default(), solid(100.0)).unwrap();
        let paper = Space::Paper("Layout1".into());
        d.add(&paper, Common::default(), solid(10.0)).unwrap();
        let line = |x: f64, y: f64| EntityKind::Line(Line { a: Vec3::new(x, y, 0.0), b: Vec3::new(x + 120.0, y + 30.0, 0.0) });
        let vp = |c: Vec2, vc: Vec2, h: f64, id: u32| Viewport {
            center: c.to3(0.0),
            width: 100.0,
            height: 80.0,
            view_center: vc,
            view_height: h,
            id,
            locked: false,
            frozen_layers: Vec::new(),
            layer_colors: Vec::new(),
            clip: None,
            on: true,
        };
        d.add(&paper, Common::default(), line(0.0, 10.0)).unwrap();
        d.add(&paper, Common::default(), EntityKind::Viewport(vp(Vec2::new(60.0, 60.0), Vec2::new(300.0, 150.0), 320.0, 2))).unwrap();
        let tri = [(150.0, 20.0), (250.0, 20.0), (200.0, 100.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y))).to_vec();
        let boundary = d
            .add(
                &paper,
                Common::default(),
                EntityKind::LwPolyline(LwPolyline { vertices: tri, closed: true, const_width: 0.0, elevation: 0.0, plinegen: false }),
            )
            .unwrap();
        let mut clipped = vp(Vec2::new(200.0, 60.0), Vec2::new(500.0, 150.0), 340.0, 3);
        clipped.clip = Some(boundary);
        d.add(&paper, Common::default(), EntityKind::Viewport(clipped)).unwrap();
        d.add(&paper, Common::default(), line(20.0, 80.0)).unwrap();
        d.add(&paper, Common::default(), EntityKind::Circle(Circle { center: Vec3::new(100.0, 60.0, 0.0), radius: 30.0 })).unwrap();
        d
    }

    /// A paper-space layout written in slices (its own objects and each viewport's model-space
    /// contents, clipped as in the whole build) is byte for byte the SVG of the whole display list,
    /// for slices of 1, 512 and 16,384; and past the limit (lowered for the test) no picture is built.
    #[test]
    fn a_layout_svg_written_in_slices_is_byte_identical_to_the_whole_list_one() {
        let c = ctx(None, true);
        let d = layout_drawing();
        assert!(d.model.len() >= 2_000);
        let mut prep = preview::prepare(&d, &json!({"space": "Layout1"}), &c).unwrap();
        let need = prep.list_for(&prep.window, 1600.0, &c).unwrap().expanded;
        assert!(need > 300, "{need}");
        let list = prep.list_for(&prep.window, 1600.0, &c).unwrap();
        assert!(list.prims.len() > 2_000, "{}", list.prims.len());
        let whole = preview::svg_tile(&list, &prep.layers, &prep.window, 1600.0, false, &c).unwrap().0;
        assert!(whole.contains("<polyline") && whole.contains("<path") && whole.contains(r#"data-layer="""#));
        for slice in [1, 512, preview::SLICE_ENTITIES] {
            let mut out = Vec::new();
            let (n, expanded) = prep.write_svg_sliced(&mut out, &prep.window, 1600.0, &c, slice).unwrap_or_else(|_| panic!("slice {slice}"));
            assert_eq!(n, out.len());
            assert_eq!(expanded, list.expanded, "slice {slice}");
            assert!(String::from_utf8(out).unwrap() == whole, "slice of {slice} objects");
        }
        // Past the limit no picture is built, however it is asked for.
        prep.block_cap = need / 3;
        assert!(prep.list_for(&prep.window, 1600.0, &c).is_err());
        assert!(prep.write_svg_sliced(Vec::new(), &prep.window, 1600.0, &c, 512).is_err());
        // A stop between slices leaves the writer with an error, not a short file.
        prep.block_cap = cadcraft_engine::render::MAX_BLOCK_EXPANSION;
        c.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(prep.write_svg_sliced(Vec::new(), &prep.window, 1600.0, &c, 512).is_err());
    }

    /// A block array of `cols` x `rows` copies of the 4-line block `Tree`, on layer `layer`.
    fn tree_array(cols: u32, rows: u32) -> cadcraft_engine::doc::EntityKind {
        use cadcraft_engine::doc::Insert;
        use cadcraft_engine::geom::Vec3;
        EntityKind::Insert(Insert {
            block: "Tree".into(),
            insert: Vec3::ZERO,
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: vec![],
            cols,
            rows,
            col_spacing: 10.0,
            row_spacing: 10.0,
        })
    }

    /// A drawing with the block `Tree` (four lines) and the layers `names`.
    fn tree_drawing(names: &[&str]) -> Drawing {
        use cadcraft_engine::doc::{Block, Entity, Handle, Layer, Line};
        use cadcraft_engine::geom::Vec3;
        let mut d = Drawing::new_metric();
        for n in names {
            d.layers.push(Layer { name: (*n).into(), on: true, ..Default::default() });
        }
        let mut b = Block::new("Tree");
        for k in 0..4u64 {
            b.entities.push(Entity::new(Handle(900 + k), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(3.0, 1.0 + k as f64, 0.0) })));
        }
        d.blocks.insert("Tree".into(), std::sync::Arc::new(b));
        d
    }

    /// The ways to a complete picture, as the refusal's `next_step` says them.
    fn words(note: &Value) -> String {
        note["next_step"].as_str().unwrap().to_string()
    }

    /// The refusal of the picture `prep` would make (its figures as one object), from the count made
    /// before anything is drawn.
    fn refused(prep: &preview::Prepared<'_>, c: &Ctx) -> Value {
        let census = prep.census(&prep.window, None, c).unwrap();
        let err = prep.refusal(&census).expect("the picture is over the limit");
        let mut v = err.to_value();
        v["text"] = json!(err.text());
        v
    }

    /// An engine whose drawing has the 4-line block `Tree`, a 30 x 30 array of it on layer `Refs`
    /// (900 copies of 5 pieces: 4,500) and three single references on `Small` (15): 4,515 block
    /// pieces. With the limit at 300 (`preview::lower_limit`), nothing less than the layer, the block
    /// as boxes or the layer off fits.
    fn forest() -> (Engine, Ctx, PathBuf) {
        use cadcraft_engine::doc::Common;
        let dir = scratch("forest");
        let c = ctx(Some(dir.clone()), true);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        let d = e.docs.list[0].session.doc_mut().unwrap();
        let t = tree_drawing(&["Refs", "Small"]);
        d.layers.extend(t.layers.iter().filter(|l| l.name == "Refs" || l.name == "Small").cloned());
        d.blocks.insert("Tree".into(), t.blocks["Tree"].clone());
        d.add(&Space::Model, Common { layer: "Refs".into(), ..Common::default() }, tree_array(30, 30)).unwrap();
        for k in 0..3 {
            let mut one = tree_array(1, 1);
            if let EntityKind::Insert(i) = &mut one {
                i.insert = cadcraft_engine::geom::Vec3::new(f64::from(k) * 400.0 + 1000.0, 0.0, 0.0);
            }
            d.add(&Space::Model, Common { layer: "Small".into(), ..Common::default() }, one).unwrap();
        }
        (e, c, dir)
    }

    fn polylines(svg: &str) -> usize {
        svg.matches("<polyline").count()
    }

    /// Past the limit NO picture is made and NO file is written, however it is asked for
    /// (render_preview svg / png, with output, export svg / png / pdf); a file already at the path is
    /// not touched, renamed or removed; the refusal says what the picture needs, the limit and every
    /// way to a complete one with its figure; and each way works: layers off, the block as boxes (one
    /// outline per reference, an array one outline, the answer names the block and its references,
    /// the name is matched in any case). Still over after a choice: refused again with the new figures.
    #[test]
    fn a_picture_over_the_limit_is_refused_and_nothing_is_written() {
        let (mut e, c, dir) = forest();
        let _limit = preview::lower_limit(300);
        let refusal_of = |r: Result<Reply, ToolError>| r.map(|_| ()).expect_err("a picture was made");
        // render_preview, svg and png, inline and with output.
        for args in [json!({}), json!({"format": "png"}), json!({"output": "p"}), json!({"format": "png", "output": "p"})] {
            let err = refusal_of(e.call("render_preview", &args, &c));
            assert_eq!(err.code, code::TOO_LARGE, "{args}");
            assert_eq!(err.extra["blocks_expanded_total"], json!(4_515));
            assert_eq!(err.extra["blocks_expanded_limit"], json!(300));
            assert_eq!(err.extra["picture_made"], json!(false));
        }
        // export: a file already at the path stays as it is, byte for byte, with no OLD_ copy and no
        // temporary file beside it.
        for f in ["plan.svg", "plan.png", "plan.pdf"] {
            std::fs::write(dir.join(f), "earlier").unwrap();
        }
        let before = std::fs::read_dir(&dir).unwrap().count();
        for f in ["plan.svg", "plan.png", "plan.pdf"] {
            let err = refusal_of(e.call("export", &json!({"path": f}), &c));
            assert_eq!(err.code, code::TOO_LARGE, "{f}");
            assert_eq!(std::fs::read_to_string(dir.join(f)).unwrap(), "earlier", "{f}");
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before, "nothing was added: no file, no OLD_ copy, no temporary file");
        let _ = std::fs::remove_file(dir.join("plan.pdf"));
        // The refusal: figures, tables and ways.
        let err = refusal_of(e.call("render_preview", &json!({}), &c));
        let x = &err.extra;
        assert!(
            err.reason.contains("needs 4515 block pieces") && err.reason.contains("at most 300") && err.reason.contains("no file was written"),
            "{}",
            err.reason
        );
        assert_eq!(x["largest_object"]["block"], json!("Tree"));
        assert_eq!(x["largest_object"]["blocks_expanded_total"], json!(4_500));
        assert_eq!(
            x["layers_that_reduce"],
            json!([
                {"layer": "Refs", "blocks_expanded_total_if_off": 15, "fits_one_picture": true},
                {"layer": "Small", "blocks_expanded_total_if_off": 4_500, "fits_one_picture": false}
            ])
        );
        // Tree as boxes: the array is one piece and each single reference one: 4. All listed fits.
        assert_eq!(x["heaviest_blocks"], json!([{"block": "Tree", "blocks_expanded_total_if_boxed": 4, "fits_one_picture": true}]));
        assert_eq!(x["heaviest_blocks_all_boxed"], json!({"blocks_expanded_total": 4, "fits_one_picture": true}));
        let ways = words(&err.to_value());
        assert!(ways.contains("blocks_as_boxes") && ways.contains("Tree leaves 4, which fits one picture"), "{ways}");
        assert!(ways.contains("Refs leaves 15, which fits one picture") && ways.contains("windows and tiles cannot complete this"), "{ways}");
        assert!(err.text().contains("blocks_as_boxes") && err.text().starts_with("[too_large]"), "{}", err.text());
        for edit in ["explode", "arrayrect", "undo", "discard", "save_drawing"] {
            assert!(!err.text().contains(edit), "an editing route is offered: {edit}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each way the refusal offers works: the layer off draws the picture whole and writes it, the
    /// block as boxes draws ONE outline per reference (an array one outline; the name is matched in any
    /// case; svg, png and pdf; the answer names the block and its references), and when the choice
    /// still leaves more than the limit the picture is refused again with the figures that choice leaves.
    #[test]
    fn the_ways_the_refusal_offers_each_make_a_complete_picture() {
        let (mut e, c, dir) = forest();
        let _limit = preview::lower_limit(300);
        let refusal_of = |r: Result<Reply, ToolError>| r.map(|_| ()).expect_err("a picture was made");
        // Way 1: the layer off. Drawn whole and written, complete.
        let r = e.call("render_preview", &json!({"layers_off": ["REFS"]}), &c).unwrap().value;
        let tile = &r["tiles"][0];
        assert_eq!(tile["blocks_expanded_total"], json!(15));
        assert_eq!(polylines(tile["svg"].as_str().unwrap()), 3 * 4);
        let r = e.call("export", &json!({"path": "off.svg", "layers_off": ["Refs"]}), &c).unwrap().value;
        assert_eq!(r["saved"][0]["blocks_expanded_total"], json!(15));
        assert_eq!(polylines(&std::fs::read_to_string(dir.join("off.svg")).unwrap()), 12);

        // Way 2: the block as boxes (any case): the array is ONE outline, each single reference one.
        let r = e.call("render_preview", &json!({"blocks_as_boxes": ["tree"]}), &c).unwrap().value;
        let tile = &r["tiles"][0];
        assert_eq!(tile["blocks_expanded_total"], json!(4));
        assert_eq!(tile["blocks_drawn_as_boxes"], json!([{"block": "Tree", "outlines_drawn": 4}]));
        let svg = tile["svg"].as_str().unwrap();
        assert_eq!(polylines(svg), 4, "one outline per reference, the array one");
        let r = e.call("export", &json!({"path": "boxed.svg", "blocks_as_boxes": "Tree"}), &c).unwrap().value;
        assert_eq!(r["saved"][0]["blocks_drawn_as_boxes"], json!([{"block": "Tree", "outlines_drawn": 4}]));
        assert_eq!(std::fs::read_to_string(dir.join("boxed.svg")).unwrap(), svg);
        for f in ["boxed.png", "boxed.pdf"] {
            let r = e.call("export", &json!({"path": f, "blocks_as_boxes": ["Tree"]}), &c).unwrap().value;
            assert_eq!(r["saved"][0]["blocks_expanded_total"], json!(4), "{f}");
            assert!(std::fs::metadata(dir.join(f)).unwrap().len() > 100, "{f}");
        }
        // The pdf answer names the blocks too.
        let r = e.call("export", &json!({"path": "boxed2.pdf", "blocks_as_boxes": ["Tree"]}), &c).unwrap().value;
        assert_eq!(r["saved"][0]["blocks_drawn_as_boxes"], json!([{"block": "Tree", "outlines_drawn": 4}]));

        // Still over after a choice: refused again with the figures that choice leaves.
        let _tighter = preview::lower_limit(3);
        let err = refusal_of(e.call("render_preview", &json!({"blocks_as_boxes": ["Tree"]}), &c));
        assert_eq!(err.extra["blocks_expanded_total"], json!(4));
        assert_eq!(err.extra["blocks_expanded_limit"], json!(3));
        assert!(err.extra.get("heaviest_blocks").is_none(), "nothing is left to box");
        assert!(words(&err.to_value()).contains("blocks_as_boxes cannot shorten this"));
        assert_eq!(err.extra["layers_that_reduce"][0], json!({"layer": "Small", "blocks_expanded_total_if_off": 1, "fits_one_picture": true}));
        assert_eq!(err.extra["layers_that_reduce"][1], json!({"layer": "Refs", "blocks_expanded_total_if_off": 3, "fits_one_picture": true}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file format hooks the engine's own picture commands need (the host installs them; once).
    fn install_io() {
        use cadcraft_engine::cmd::file::{IoHooks, set_io};
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, s, o| cadcraft_io::plot(d, s, o).map_err(|e| e.to_string())),
        });
    }

    /// Every picture-making command reached through `edit` follows the rule of the picture tools: over
    /// the limit nothing is written (a file already there is untouched) and the refusal says what
    /// happened and where a complete picture is made; under the limit they still work.
    #[test]
    fn the_picture_commands_of_edit_write_nothing_over_the_limit() {
        install_io();
        let (mut e, c, dir) = forest();
        let _limit = preview::lower_limit(300);
        // A frame for pdfbook: a closed polyline 420 x 297.
        edit(&mut e, &c, json!({"ops": [{"op": "add", "type": "pline", "points": [[0, 0], [420, 0], [420, 297], [0, 297]], "closed": true}]}))
            .unwrap();
        std::fs::write(dir.join("plan.svg"), "earlier").unwrap();
        let before = std::fs::read_dir(&dir).unwrap().count();
        let cmds = [
            ("export", json!({"path": "x.svg"})),
            ("export", json!({"path": "x.png"})),
            ("export", json!({"path": "plan.svg"})),
            ("export", json!({"path": "x.pdf"})),
            ("plot", json!({"layout": "Model", "path": "x.pdf"})),
            ("plot", json!({"layout": "Model", "output": "png", "path": "x2.png"})),
            ("plot", json!({"layouts": ["Model"], "path": "x3.pdf"})),
            ("publish", json!({"path": "pub", "layouts": ["Model"]})),
            ("pdfbook", json!({"path": "book.pdf"})),
            ("preview", json!({"layout": "Model"})),
            ("pngout", json!({"path": "o.png"})),
            ("jpgout", json!({"path": "o.jpg"})),
            ("bmpout", json!({"path": "o.bmp"})),
        ];
        for (cmd, params) in cmds {
            let err = e
                .call("edit", &json!({"ops": [{"op": "command", "command": cmd, "params": params}]}), &c)
                .map(|_| ())
                .expect_err(&format!("{cmd} {params} made a picture"));
            let known = cadcraft_engine::find_command(cmd).is_some();
            let text = format!("{} {}", err.reason, err.next_step);
            if known {
                assert!(
                    text.contains("block pieces")
                        && text.contains("no file was written")
                        && text.contains("`export` tool")
                        && text.contains("blocks_as_boxes")
                        && text.contains("call it again"),
                    "{cmd}: {text}"
                );
            } else {
                assert!(text.contains("unknown command"), "{cmd}: {text}");
            }
        }
        assert_eq!(std::fs::read_to_string(dir.join("plan.svg")).unwrap(), "earlier");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before, "no file was written");
        // Under the limit (the real one) the same commands work.
        drop(_limit);
        let r = e.call("edit", &json!({"ops": [{"op": "command", "command": "export", "params": {"path": "ok.svg"}}]}), &c);
        assert!(r.is_ok() && dir.join("ok.svg").is_file(), "{:?}", r.map(|_| ()).err().map(|x| x.text()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A layout over the limit is refused once for the call (it is built whole for every tile), before
    /// any tile or file; its slices are not written either.
    #[test]
    fn a_layout_over_the_limit_is_refused_once_before_any_tile() {
        let dir = scratch("layout-refused");
        let c = ctx(Some(dir.clone()), true);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        *e.docs.list[0].session.doc_mut().unwrap() = layout_drawing();
        let _limit = preview::lower_limit(300);
        std::fs::write(dir.join("l.svg"), "earlier").unwrap();
        for args in [
            json!({"space": "Layout1"}),
            json!({"space": "Layout1", "tiles": {"cols": 3, "rows": 3}}),
            json!({"space": "Layout1", "tiles": {"cols": 3, "rows": 3}, "output": "t.svg"}),
        ] {
            let err = e.call("render_preview", &args, &c).map(|_| ()).unwrap_err();
            assert_eq!(err.code, code::TOO_LARGE, "{args}");
            assert!(err.extra["blocks_expanded_total"].as_u64().unwrap() > 300);
        }
        let err = e.call("export", &json!({"path": "l.svg", "space": "Layout1"}), &c).map(|_| ()).unwrap_err();
        assert_eq!(err.code, code::TOO_LARGE);
        assert_eq!(std::fs::read_to_string(dir.join("l.svg")).unwrap(), "earlier");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Anything given for `blocks_as_boxes` that is not a list of block names is refused naming what was
    /// given, on every path (tools, the PDF JSON, the engine's plot hook): a number, a number in a list,
    /// an object, a name that is no block.
    #[test]
    fn a_blocks_as_boxes_that_is_not_a_list_of_block_names_is_refused() {
        let (mut e, c, dir) = forest();
        for bad in [json!(5), json!([123]), json!({"a": 1}), json!(["Tree", null]), json!(true)] {
            for (tool, args) in [
                ("render_preview", json!({"blocks_as_boxes": bad})),
                ("export", json!({"path": "b.svg", "blocks_as_boxes": bad})),
                ("export", json!({"path": "b.pdf", "blocks_as_boxes": bad})),
            ] {
                let err = e.call(tool, &args, &c).map(|_| ()).unwrap_err();
                assert_eq!(err.code, code::INVALID_ARGUMENT, "{tool} {args}");
                let given = match &bad {
                    Value::Array(a) => a.iter().find(|n| !n.is_string()).unwrap().to_string(),
                    other => other.to_string(),
                };
                assert!(err.reason.contains("must be a list of block names, got") && err.reason.contains(&given), "{}", err.reason);
                assert!(err.next_step.contains("[\"Tree\", \"Chair\"]") && err.next_step.contains("call again"), "{}", err.next_step);
            }
        }
        // The same on the io paths that take the JSON.
        let d = e.docs.list[0].drawing().unwrap();
        for opts in [json!({"blocks_as_boxes": 5}), json!({"blocks_as_boxes": ["Nope"]}), json!({"blocks_as_boxes": [1]})] {
            let msg = cadcraft_io::plot_complete(d, &Space::Model, &opts, 300, None).map(|_| ()).unwrap_err().to_string();
            assert!(msg.contains("try again") && (msg.contains("[\"Tree\", \"Chair\"]") || msg.contains("read_drawing")), "{msg}");
            assert!(cadcraft_io::plot(d, &Space::Model, &opts).is_err(), "{opts}");
            assert!(cadcraft_io::plot(d, &Space::Model, &json!({"output": "png", "blocks_as_boxes": opts["blocks_as_boxes"]})).is_err(), "{opts}");
        }
        // The PDF JSON takes the string form as the tools do.
        let pdf = cadcraft_io::plot_complete(d, &Space::Model, &json!({"blocks_as_boxes": "Tree"}), 300, None).unwrap();
        assert!(matches!(pdf, cadcraft_io::Plot::Drawn(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The heaviest list is said once in words and once as the field; the example does not repeat the
    /// names; a block that boxing does not shorten is named as that case; refused tile entries count
    /// against the answer's size.
    #[test]
    fn the_refusal_says_the_heaviest_blocks_once_and_tells_which_case() {
        use cadcraft_engine::render::{BlockCensus, BlockSaving};
        let list: Vec<BlockSaving> = (0..50).map(|k| BlockSaving { block: format!("Block{k:02}"), total_if_boxed: 1_000 + k }).collect();
        let census = BlockCensus { total: 90_000, heaviest: list, total_if_listed_boxed: 250, ..Default::default() };
        let e = preview::refusal(300, 90_000, preview::Through::Model, &census, &[]);
        for k in ["Block00", "Block49"] {
            assert_eq!(e.next_step.matches(k).count(), 1, "{k} is said once in the words");
        }
        assert!(e.next_step.contains("Pass the block names from the top of heaviest_blocks") && !e.next_step.contains("Example"), "{}", e.next_step);
        let one = BlockCensus {
            total: 900,
            heaviest: vec![BlockSaving { block: "Tree".into(), total_if_boxed: 100 }],
            total_if_listed_boxed: 100,
            ..Default::default()
        };
        let e = preview::refusal(300, 900, preview::Through::Model, &one, &[]);
        assert!(e.next_step.contains("Example: blocks_as_boxes: [\"Tree\"]"), "{}", e.next_step);
        // Blocks are referred to, but boxing saves nothing / no block is referred to: two different sentences.
        let none = BlockCensus { total: 900, blocks_referenced: 3, ..Default::default() };
        assert!(preview::refusal(300, 900, preview::Through::Model, &none, &[]).next_step.contains("the 3 block(s) the picture refers to"));
        let none = BlockCensus { total: 900, ..Default::default() };
        assert!(preview::refusal(300, 900, preview::Through::Model, &none, &[]).next_step.contains("refers to no block"));
    }

    /// Refused tile entries count against the answer's size like any other (the first is always
    /// given; the next is left for `continue_from`).
    #[test]
    fn refused_tile_entries_count_against_the_answer_size() {
        let (mut e, c, dir) = forest();
        let _limit = preview::lower_limit(300);
        let r = e.call("render_preview", &json!({"tiles": {"cols": 1, "rows": 3}, "max_bytes": 1}), &c).unwrap().value;
        assert_eq!(r["returned"], json!(1), "{r}");
        assert_eq!(r["tiles"][0]["picture_made"], json!(false));
        assert_eq!(r["continue_from"], json!(1));
        let r = e.call("render_preview", &json!({"tiles": {"cols": 1, "rows": 3}, "continue_from": 1}), &c).unwrap().value;
        assert_eq!(r["returned"], json!(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The count said a picture fits and the build went over the limit anyway: no picture, and the answer
    /// says so in words with both figures and what to do next (PDF sheets too).
    #[test]
    fn a_build_over_the_limit_that_the_count_missed_says_what_to_try() {
        let c = ctx(None, true);
        let d = tree_drawing(&["Refs"]);
        let mut d = d;
        d.add(&Space::Model, cadcraft_engine::doc::Common::default(), tree_array(10, 10)).unwrap();
        let mut prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        let counted = prep.census(&prep.window, None, &c).unwrap().total;
        assert_eq!(counted, 500);
        // Force the mismatch: the limit drops after the count was made.
        prep.block_cap = 50;
        let err = prep.list_for(&prep.window, 800.0, &c).map(|_| ()).unwrap_err();
        let text = err.text();
        assert_eq!(err.code, code::TOO_LARGE);
        assert!(
            text.contains("still came out over the 50 block-piece limit")
                && text.contains("count made before drawing said it fits (it counted 500)")
                && text.contains("the count may be wrong"),
            "{text}"
        );
        assert!(text.contains("switch off more layers") && text.contains("blocks_as_boxes") && text.contains("try again"), "{text}");
        assert_eq!(err.extra["blocks_expanded_counted"], json!(500));
        // The same from a PDF sheet whose count said it fits.
        let sheet = cadcraft_io::SheetBlocks {
            sheet: "Model".into(),
            number: 2,
            total: 60,
            limit: 50,
            census: cadcraft_engine::render::BlockCensus { total: 0, ..Default::default() },
        };
        let err = preview::pdf_refusal(&[sheet], &[]);
        assert!(
            err.reason.contains("PDF sheet 2 (Model) still came out over the 50 block-piece limit") && err.next_step.contains("try again"),
            "{}",
            err.text()
        );
    }

    /// A name in `blocks_as_boxes` that is no block of the drawing is refused in words naming it
    /// (never ignored), nothing is drawn or written, and the nearest names are offered.
    #[test]
    fn an_unknown_block_name_is_refused_naming_it() {
        let (mut e, c, dir) = forest();
        for tool_args in [
            ("render_preview", json!({"blocks_as_boxes": ["Tre", "tree"]})),
            ("export", json!({"path": "u.svg", "blocks_as_boxes": "Tre"})),
            ("export", json!({"path": "u.pdf", "blocks_as_boxes": ["Tre"]})),
        ] {
            let err = e.call(tool_args.0, &tool_args.1, &c).map(|_| ()).unwrap_err();
            assert_eq!(err.code, code::NOT_FOUND, "{}", tool_args.1);
            assert!(
                err.reason.contains("`Tre`") && err.reason.contains("not a block of this drawing") && !err.reason.contains("`tree`"),
                "{}",
                err.reason
            );
            assert_eq!(err.extra["unknown_blocks"], json!(["Tre"]));
            assert_eq!(err.extra["closest"]["Tre"][0], json!("Tree"));
            assert!(
                err.next_step.contains("read_drawing section \"blocks\"")
                    && err.next_step.contains("call again with blocks_as_boxes names taken from that list"),
                "{}",
                err.next_step
            );
        }
        assert!(!dir.join("u.svg").exists() && !dir.join("u.pdf").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each tile is its own picture: the tiles that fit are drawn and written, a tile over the limit
    /// is not (no file, an existing file at its path untouched) and its entry says so with its own
    /// figures. Asked for one tile, the refusal is the call's error.
    #[test]
    fn tiles_that_fit_are_written_and_a_tile_over_the_limit_is_refused_on_its_own() {
        use cadcraft_engine::doc::Common;
        use cadcraft_engine::geom::Vec3;
        let dir = scratch("tiles-limit");
        let c = ctx(Some(dir.clone()), true);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        let d = e.docs.list[0].session.doc_mut().unwrap();
        d.blocks.insert("Tree".into(), tree_drawing(&[]).blocks["Tree"].clone());
        // 40 references (200 pieces) on the left, 2 (10 pieces) far right.
        for k in 0..42 {
            let x = if k < 40 { f64::from(k) * 20.0 } else { 5_000.0 + f64::from(k) * 20.0 };
            let mut one = tree_array(1, 1);
            if let EntityKind::Insert(i) = &mut one {
                i.insert = Vec3::new(x, 0.0, 0.0);
            }
            d.add(&Space::Model, Common::default(), one).unwrap();
        }
        let _limit = preview::lower_limit(100);
        std::fs::write(dir.join("t-c0r0.svg"), "earlier").unwrap();
        let r = e.call("render_preview", &json!({"tiles": {"cols": 2, "rows": 1}, "output": "t.svg"}), &c).unwrap().value;
        let t = r["tiles"].as_array().unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["picture_made"], json!(false));
        assert_eq!(t[0]["refused"]["code"], json!("too_large"));
        assert_eq!(t[0]["refused"]["blocks_expanded_total"], json!(200));
        assert!(t[0].get("saved").is_none() && t[0].get("svg").is_none());
        assert_eq!(t[1]["blocks_expanded_total"], json!(10));
        assert!(t[1]["saved"]["path"].as_str().unwrap().ends_with("t-c1r0.svg"), "{}", t[1]);
        assert!(dir.join("t-c1r0.svg").is_file());
        assert_eq!(std::fs::read_to_string(dir.join("t-c0r0.svg")).unwrap(), "earlier");
        // Inline: the refused tile carries no picture, the other does.
        let r = e.call("render_preview", &json!({"tiles": {"cols": 2, "rows": 1}}), &c).unwrap().value;
        assert!(r["tiles"][0].get("svg").is_none() && r["tiles"][0]["refused"]["code"] == json!("too_large"));
        assert!(r["tiles"][1]["svg"].as_str().unwrap().contains("<polyline"));
        let r = e.call("render_preview", &json!({"tiles": {"cols": 2, "rows": 1}, "format": "png"}), &c).unwrap();
        assert_eq!(r.extra.len(), 1, "one image: the tile that fits");
        // One tile asked for: the call's error.
        let err =
            e.call("render_preview", &json!({"tiles": {"cols": 2, "rows": 1}, "tile": [0, 0], "output": "one.svg"}), &c).map(|_| ()).unwrap_err();
        assert_eq!(err.code, code::TOO_LARGE);
        assert!(!dir.join("one-c0r0.svg").exists() && !dir.join("one.svg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A PDF is one file: if any sheet is over the limit no PDF is written (an existing file at the
    /// path stays), the refusal names the sheets over the limit with their page numbers and gives each
    /// sheet's own figures; with the block drawn as boxes the file is made.
    #[test]
    fn a_pdf_with_a_sheet_over_the_limit_is_not_written_and_names_the_sheets() {
        use cadcraft_engine::doc::Common;
        let dir = scratch("pdf-limit");
        let c = ctx(Some(dir.clone()), true);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        let d = e.docs.list[0].session.doc_mut().unwrap();
        d.blocks.insert("Tree".into(), tree_drawing(&[]).blocks["Tree"].clone());
        let names: Vec<String> = d.layouts.iter().map(|l| l.name.clone()).collect();
        assert!(names.len() >= 2, "{names:?}");
        d.add(&Space::Model, Common::default(), tree_array(1, 1)).unwrap();
        d.add(&Space::Paper(names[0].clone()), Common::default(), tree_array(1, 1)).unwrap();
        d.add(&Space::Paper(names[1].clone()), Common::default(), tree_array(10, 10)).unwrap();
        let _limit = preview::lower_limit(50);
        std::fs::write(dir.join("book.pdf"), "earlier").unwrap();
        let err = e.call("export", &json!({"path": "book.pdf", "space": "all"}), &c).map(|_| ()).unwrap_err();
        assert_eq!(err.code, code::TOO_LARGE);
        assert_eq!(std::fs::read_to_string(dir.join("book.pdf")).unwrap(), "earlier");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(err.extra["sheet_numbers_over_limit"], json!([3]));
        assert!(err.reason.contains(&format!("sheet 3 ({}) needs 500", names[1])) && err.reason.contains("no PDF was written"), "{}", err.reason);
        assert_eq!(err.extra["sheets_over_limit"][0]["sheet_number"], json!(3));
        assert_eq!(err.extra["sheets_over_limit"][0]["blocks_expanded_total"], json!(500));
        assert_eq!(err.extra["sheets_over_limit"][0]["heaviest_blocks"][0]["blocks_expanded_total_if_boxed"], json!(1));
        assert_eq!(err.extra["sheets_within_limit"].as_array().unwrap().len(), 2);
        assert!(words(&err.to_value()).contains("only places the drawing on the page"));
        // Boxes: the sheet is one piece, the file is written over the earlier one (kept as OLD_).
        let r = e.call("export", &json!({"path": "book.pdf", "space": "all", "blocks_as_boxes": ["Tree"]}), &c).unwrap().value;
        let saved = &r["saved"][0];
        assert_eq!(saved["blocks_drawn_as_boxes"], json!([{"block": "Tree", "outlines_drawn": 3}]));
        assert_eq!(saved["sheets"].as_array().unwrap().len(), 3);
        assert!(std::fs::read(dir.join("book.pdf")).unwrap().starts_with(b"%PDF"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing under the limit changes: a drawing with block references is drawn as before, the
    /// answer is the build's count, and no refusal fields appear.
    #[test]
    fn under_the_limit_the_answer_is_the_count_and_nothing_else() {
        let (mut e, c, dir) = forest();
        let r = e.call("render_preview", &json!({}), &c).unwrap().value;
        let tile = &r["tiles"][0];
        assert_eq!(tile["blocks_expanded_total"], json!(4_515));
        for k in ["refused", "picture_made", "ways_through", "blocks_truncated", "blocks_drawn_as_boxes", "heaviest_blocks"] {
            assert!(tile.get(k).is_none(), "{k}: {tile}");
        }
        let r = e.call("export", &json!({"path": "all.svg"}), &c).unwrap().value;
        assert_eq!(r["saved"][0]["blocks_expanded_total"], json!(4_515));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The refusal of a count that overflows says the words and never a number; above 2^53 a decimal
    /// string (a JSON client reads numbers as doubles), up to it a number.
    #[test]
    fn a_count_that_overflows_answers_the_words_and_no_number() {
        use cadcraft_engine::render::{BlockCensus, BlockSaving, LargestRef, LayerOff};
        let words_only = "more than can be counted (nested references multiply past 1.8e19)";
        let huge = BlockCensus {
            total: u64::MAX,
            largest: Some(LargestRef { handle: cadcraft_engine::doc::Handle(0x2A), kind: "block reference", block: "L0".into(), count: u64::MAX }),
            ..Default::default()
        };
        let e = preview::refusal(300, u64::MAX, preview::Through::Model, &huge, &[]);
        let text = format!("{} {}", e.to_value(), e.text());
        assert!(!text.contains("18446744073709551615") && !text.contains("at least") && !text.contains("render_preview with tiles"), "{text}");
        assert_eq!(e.extra["blocks_expanded_total"], json!(words_only));
        assert_eq!(e.extra["largest_object"]["blocks_expanded_total"], json!(words_only));
        assert!(e.reason.contains(words_only));
        let census = |total| BlockCensus { total, ..Default::default() };
        let e = preview::refusal(300, (1u64 << 53) + 1, preview::Through::Model, &census((1u64 << 53) + 1), &[]);
        assert_eq!(e.extra["blocks_expanded_total"], json!("9007199254740993"));
        let e = preview::refusal(300, 1u64 << 53, preview::Through::Model, &census(1u64 << 53), &[]);
        assert_eq!(e.extra["blocks_expanded_total"], json!(1u64 << 53));
        // The tables carry the same types.
        let tables = BlockCensus {
            total: 1u64 << 60,
            layers_off: vec![LayerOff { layer: "A".into(), total_if_off: 1u64 << 60 }, LayerOff { layer: "B".into(), total_if_off: 7 }],
            heaviest: vec![BlockSaving { block: "Big".into(), total_if_boxed: u64::MAX }, BlockSaving { block: "Small".into(), total_if_boxed: 9 }],
            total_if_listed_boxed: 9,
            ..Default::default()
        };
        let e = preview::refusal(300, 1u64 << 60, preview::Through::Model, &tables, &[]);
        assert_eq!(e.extra["layers_that_reduce"][0]["blocks_expanded_total_if_off"], json!("1152921504606846976"));
        assert_eq!(e.extra["layers_that_reduce"][1]["blocks_expanded_total_if_off"], json!(7));
        assert_eq!(e.extra["heaviest_blocks"][0]["blocks_expanded_total_if_boxed"], json!(words_only));
        assert_eq!(e.extra["heaviest_blocks"][1]["blocks_expanded_total_if_boxed"], json!(9));
        assert_eq!(e.extra["heaviest_blocks_all_boxed"], json!({"blocks_expanded_total": 9, "fits_one_picture": true}));
    }

    /// When boxing every block still does not fit, the refusal lists every block that saves anything,
    /// in order, and says plainly that boxes alone are not enough; a layout and a PDF get their own
    /// words about windows.
    #[test]
    fn when_even_boxing_every_block_does_not_fit_it_says_so() {
        use cadcraft_engine::render::{BlockCensus, BlockSaving};
        let census = BlockCensus {
            total: 2_100,
            heaviest: vec![BlockSaving { block: "A".into(), total_if_boxed: 1_000 }, BlockSaving { block: "B".into(), total_if_boxed: 1_500 }],
            total_if_listed_boxed: 700,
            ..Default::default()
        };
        let e = preview::refusal(300, 2_100, preview::Through::Model, &census, &[]);
        let ways = words(&e.to_value());
        assert!(ways.contains("A leaves 1000; B leaves 1500"), "{ways}");
        assert!(ways.contains("still leaves 700, more than 300: boxes alone do not give a complete picture"), "{ways}");
        assert_eq!(e.extra["heaviest_blocks_all_boxed"], json!({"blocks_expanded_total": 700, "fits_one_picture": false}));
        let layout = preview::ways_through(300, 2100, preview::Through::Layout, &BlockCensus::default(), &[]).join(" ");
        assert!(layout.contains("viewport: <handle>") && layout.contains("does not reduce"), "{layout}");
        let pdf = preview::ways_through(300, 2100, preview::Through::Pdf, &BlockCensus::default(), &[]).join(" ");
        assert!(pdf.contains("only places the drawing on the page"), "{pdf}");
    }

    /// The descriptions of `render_preview` and `export` say what a picture over the limit does (no
    /// picture, no file), what each field that can be a number or a string is, what
    /// `blocks_as_boxes` draws and what the answer names; neither tells the agent to change the
    /// drawing to get a picture, and the old vocabulary of a cut picture is gone.
    #[test]
    fn the_descriptions_document_the_limit_and_the_boxes() {
        let defs = definitions();
        for tool in ["render_preview", "export"] {
            let d = defs.iter().find(|t| t["name"] == tool).unwrap()["description"].as_str().unwrap().to_string();
            for field in [
                "blocks_expanded_total",
                "blocks_expanded_limit",
                "largest_object.blocks_expanded_total",
                "blocks_expanded_total_if_off",
                "heaviest_blocks",
                "blocks_expanded_total_if_boxed",
                "decimal string above 2^53",
                "more than can be counted",
                "blocks_as_boxes",
                "blocks_drawn_as_boxes",
                "ONE outline",
                "NOT made",
                "too_large",
                "2,000,000 block pieces",
            ] {
                assert!(d.contains(field), "{tool}: {field}");
            }
            for gone in ["blocks_truncated", "blocks_not_drawn", "is the first", "went past the limit"] {
                assert!(!d.contains(gone), "{tool} still describes a cut picture: {gone}");
            }
            for edit in ["explode", "arrayrect", "discard", "undo", "save_drawing"] {
                assert!(!d.contains(edit), "{tool} names an editing route: {edit}");
            }
            let props = defs.iter().find(|t| t["name"] == tool).unwrap()["inputSchema"]["properties"].clone();
            assert!(props.get("blocks_as_boxes").is_some(), "{tool}");
        }
        let export = defs.iter().find(|t| t["name"] == "export").unwrap()["description"].as_str().unwrap().to_string();
        assert!(export.contains("sheet_numbers_over_limit") && export.contains("no PDF is written"), "{export}");
    }

    /// Annotative dimensions are drawn from their definition, so a drawing of them is a complete
    /// picture (the build expands nothing) even though the stored blocks they name hold more than
    /// the limit: render_preview and export (svg and png) still DRAW it (a count that read the
    /// block table naively would refuse a picture the build draws whole).
    #[test]
    fn a_picture_of_annotative_dimensions_is_still_drawn() {
        use cadcraft_engine::doc::{AnnoScale, Block, Common, Dimension, Entity, Handle, Line};
        use cadcraft_engine::geom::Vec3;
        let dir = scratch("annotative-dims");
        let c = ctx(Some(dir.clone()), true);
        let mut e = Engine::default();
        e.call("new_drawing", &json!({}), &c).unwrap();
        let d = e.docs.list[0].session.doc_mut().unwrap();
        // 50,000 lines in the block each dimension names: 41 dimensions would count 2,050,041 > 2,000,000.
        let mut b = Block::new("DB");
        for k in 0..50_000u64 {
            b.entities.push(Entity::new(Handle(10_000 + k), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(1.0, 1.0, 0.0) })));
        }
        d.blocks.insert("DB".into(), std::sync::Arc::new(b));
        let annotative = Common { anno_scales: vec![AnnoScale::new("1:1", 1.0, 1.0)], ..Common::default() };
        let dims = |d: &mut Drawing, common: &Common| {
            for k in 0..41 {
                let at = f64::from(k) * 20.0;
                let dim = Dimension {
                    kind: cadcraft_engine::doc::DimKind::Linear { rotation: 0.0 },
                    defpt: Vec3::new(at, 2.0, 0.0),
                    text_mid: Vec3::ZERO,
                    p13: Vec3::new(at, 0.0, 0.0),
                    p14: Vec3::new(at + 10.0, 0.0, 0.0),
                    p15: Vec3::ZERO,
                    p16: Vec3::ZERO,
                    text: String::new(),
                    style: "Standard".into(),
                    measurement: 0.0,
                    text_rotation: 0.0,
                    user_text_pos: false,
                    block: Some("DB".into()),
                    overrides: Default::default(),
                    assoc: Vec::new(),
                    marks: Default::default(),
                };
                d.add(&Space::Model, common.clone(), EntityKind::Dimension(dim)).unwrap();
            }
        };
        // The same dimensions, not annotative, take the stored block's entities: that is the count
        // a naive reading of the block table gives, over the limit.
        let mut plain = d.clone();
        dims(&mut plain, &Common::default());
        let naive = cadcraft_engine::render::count_space(&plain, &Space::Model, false);
        assert_eq!(naive, 41 * 50_000);
        assert!(naive > cadcraft_engine::render::MAX_BLOCK_EXPANSION as u64);
        dims(d, &annotative);
        let r = e.call("render_preview", &json!({}), &c).unwrap().value;
        let tile = &r["tiles"][0];
        assert_eq!(tile["blocks_expanded_total"], json!(0), "{tile}");
        assert!(
            tile["svg"].as_str().is_some_and(|s| s.contains("<polyline")) && tile.get("refused").is_none() && tile.get("ways_through").is_none(),
            "{tile}"
        );
        let r = e.call("render_preview", &json!({"format": "png"}), &c).unwrap().value;
        assert!(r["tiles"][0].get("refused").is_none() && r["tiles"][0]["blocks_expanded_total"] == json!(0), "{r}");
        assert_eq!(r["returned"], json!(1));
        for f in ["a.svg", "a.png"] {
            let r = e.call("export", &json!({"path": f}), &c).unwrap().value;
            let saved = &r["saved"][0];
            assert_eq!(saved["blocks_expanded_total"], json!(0), "{f}: {saved}");
            assert!(saved.get("refused").is_none() && saved.get("ways_through").is_none() && dir.join(f).is_file(), "{f}: {saved}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One reference that alone needs more than the limit is expanded whole in every window that
    /// touches it. The refusal names it (handle, kind, block, count), says windows cannot split it,
    /// does not advise windows, and offers only what leaves the drawing as it is: the layer the
    /// references are on, whose switching off really reduces the build to the figure given, the block
    /// drawn as boxes (the figure given is what a build with it expands), and reading.
    #[test]
    fn one_object_over_the_limit_is_named_and_only_routes_that_leave_the_drawing_are_offered() {
        use cadcraft_engine::doc::Common;
        let c = ctx(None, true);
        let cap = 300;
        // A 30 x 30 array on "Refs" (900 copies x 5 = 4,500) and three small references on "Small" (15).
        let mut d = tree_drawing(&["Refs", "Small", "Other"]);
        let big = d.add(&Space::Model, Common { layer: "Refs".into(), ..Common::default() }, tree_array(30, 30)).unwrap();
        for _ in 0..3 {
            d.add(&Space::Model, Common { layer: "Small".into(), ..Common::default() }, tree_array(1, 1)).unwrap();
        }
        let mut prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        prep.block_cap = cap;
        let note = refused(&prep, &c);
        assert_eq!(note["blocks_expanded_total"], json!(4_515));
        assert_eq!(
            note["largest_object"],
            json!({"handle": big.hex(), "kind": "array of block references (MINSERT)", "block": "Tree", "blocks_expanded_total": 4_500, "fits_one_picture": false})
        );
        let ways = words(&note);
        assert!(ways.contains("windows and tiles cannot complete this"), "{ways}");
        assert!(ways.contains(&big.hex()) && ways.contains("block Tree") && ways.contains("4500"), "{ways}");
        assert!(!ways.contains("at least") && !ways.contains("draw it in windows"), "windows are not advised: {ways}");
        assert!(ways.contains("reading is not affected"), "{ways}");
        for edit in ["explode", "arrayrect", "undo", "discard", "save_drawing", "edit"] {
            assert!(!ways.contains(edit) && !note.to_string().contains(edit), "an editing route is offered: {edit}");
        }
        // Windows really do not help: every tile that reaches the array is over the limit.
        let mut cut = 0;
        let tiles = preview::tiles(&prep.window, &json!({"tiles": {"cols": 6, "rows": 6}})).unwrap();
        for (_, win) in &tiles {
            cut += usize::from(prep.expansions(win, &c).unwrap() > cap as u64);
        }
        assert!(cut >= 6, "{cut}");
        // The layer route: "Refs" off leaves the 15 of the small ones, the figure the answer gave,
        // and a build with that layer off takes exactly that, under the limit.
        assert_eq!(note["layers_that_reduce"][0], json!({"layer": "Refs", "blocks_expanded_total_if_off": 15, "fits_one_picture": true}));
        assert!(ways.contains("Refs leaves 15, which fits one picture"), "{ways}");
        let mut off = preview::prepare(&d, &json!({"space": "Model", "layers_off": ["REFS"]}), &c).unwrap();
        off.block_cap = cap;
        assert_eq!(off.list_for(&off.window, 800.0, &c).unwrap().expanded, 15);
        // The box route: the figure given is what a build with the block as boxes expands.
        assert_eq!(note["heaviest_blocks"], json!([{"block": "Tree", "blocks_expanded_total_if_boxed": 4, "fits_one_picture": true}]));
        let mut boxed = preview::prepare(&d, &json!({"space": "Model", "blocks_as_boxes": ["Tree"]}), &c).unwrap();
        boxed.block_cap = cap;
        assert_eq!(boxed.list_for(&boxed.window, 800.0, &c).unwrap().expanded, 4);

        // Two big arrays on two layers: no layer leaves a picture that fits, and the block as boxes does.
        let mut d = tree_drawing(&["Refs", "Other"]);
        d.add(&Space::Model, Common { layer: "Refs".into(), ..Common::default() }, tree_array(30, 30)).unwrap();
        d.add(&Space::Model, Common { layer: "Other".into(), ..Common::default() }, tree_array(30, 30)).unwrap();
        let mut prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        prep.block_cap = cap;
        let note = refused(&prep, &c);
        for l in note["layers_that_reduce"].as_array().unwrap() {
            assert_eq!(l["fits_one_picture"], json!(false));
        }
        assert_eq!(note["heaviest_blocks_all_boxed"], json!({"blocks_expanded_total": 2, "fits_one_picture": true}));
        assert!(!words(&note).contains("none of these alone"), "boxes fit: {}", words(&note));
        // Boxes that do not fit either: 400 arrays each one piece, limit 300, one layer.
        let mut d = tree_drawing(&["Refs"]);
        for _ in 0..400 {
            d.add(&Space::Model, Common { layer: "Refs".into(), ..Common::default() }, tree_array(1, 1)).unwrap();
        }
        let mut prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        prep.block_cap = 300;
        let note = refused(&prep, &c);
        assert_eq!(note["heaviest_blocks_all_boxed"], json!({"blocks_expanded_total": 400, "fits_one_picture": false}));
        let ways = words(&note);
        // Windows still help here (no single object is over the limit), so only the boxes' own words.
        assert!(ways.contains("boxes alone do not give a complete picture") && ways.contains("draw it in windows"), "{ways}");
    }

    /// Which layer to switch off: the build takes one expansion for every entity of a block before
    /// it looks at the entity's layer, so switching off the layer of a block's CONTENTS reduces
    /// nothing and only the layer the REFERENCES are on does. The answer says so, lists the
    /// reference layers with what each leaves, and the advised route really reduces the build's
    /// count; when every reference is on one layer it says there is no split into passes.
    #[test]
    fn the_layer_advice_names_the_reference_layers_and_it_reduces_the_build() {
        use cadcraft_engine::doc::{Block, Common, Entity, Handle, Insert, Layer, Line};
        use cadcraft_engine::geom::Vec3;
        let c = ctx(None, true);
        let mut d = Drawing::new_metric();
        for n in ["Trees", "Bark", "Shrubs"] {
            d.layers.push(Layer { name: n.into(), on: true, ..Default::default() });
        }
        let mut b = Block::new("Tree");
        for k in 0..4u64 {
            let mut e = Entity::new(Handle(900 + k), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(3.0, 4.0, 0.0) }));
            e.common = Common { layer: "Bark".into(), ..Common::default() };
            b.entities.push(e);
        }
        d.blocks.insert("Tree".into(), std::sync::Arc::new(b));
        let tree = |x: f64| {
            EntityKind::Insert(Insert {
                block: "Tree".into(),
                insert: Vec3::new(x, 0.0, 0.0),
                scale: Vec3::new(1.0, 1.0, 1.0),
                rotation: 0.0,
                attribs: vec![],
                cols: 1,
                rows: 1,
                col_spacing: 0.0,
                row_spacing: 0.0,
            })
        };
        for k in 0..60 {
            let layer = if k % 3 == 0 { "Shrubs" } else { "Trees" };
            d.add(&Space::Model, Common { layer: layer.into(), ..Common::default() }, tree(f64::from(k) * 10.0)).unwrap();
        }
        let mut prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        prep.block_cap = 100;
        let note = refused(&prep, &c);
        // 60 references, each its copy and 4 entities.
        assert_eq!(note["blocks_expanded_total"], json!(300));
        assert_eq!(note["reference_layers"], json!(["Shrubs", "Trees"]));
        assert_eq!(
            note["layers_that_reduce"],
            json!([
                {"layer": "Trees", "blocks_expanded_total_if_off": 100, "fits_one_picture": true},
                {"layer": "Shrubs", "blocks_expanded_total_if_off": 200, "fits_one_picture": false}
            ])
        );
        let ways = words(&note);
        assert!(ways.contains("switch off the layer the block references are on"), "{ways}");
        assert!(ways.contains("Switching off the layers of the references' contents does not reduce the count"), "{ways}");
        assert!(ways.contains("Trees leaves 100, which fits one picture; Shrubs leaves 200"), "{ways}");
        assert!(!ways.contains("Bark"), "the contents' layer is not advised: {ways}");
        // The advice is right: the references' layer off cuts the build's count to what is said, the
        // contents' layer off changes nothing.
        let built = |args: Value| {
            let mut a = json!({"space": "Model"});
            a.as_object_mut().unwrap().extend(args.as_object().unwrap().clone());
            let mut p = preview::prepare(&d, &a, &c).unwrap();
            p.block_cap = usize::MAX;
            p.list_for(&p.window, 800.0, &c).unwrap().expanded
        };
        assert_eq!(built(json!({})), 300);
        assert_eq!(built(json!({"layers_off": ["Bark"]})), 300);
        assert_eq!(built(json!({"layers_off": ["Trees"]})), 100);
        assert_eq!(built(json!({"layers_off": ["Shrubs"]})), 200);
        assert_eq!(built(json!({"layers_off": ["Trees", "Shrubs"]})), 0);
        // Every reference on one layer: said so, and no list of layers that split anything.
        let mut one = Drawing::new_metric();
        one.layers.push(Layer { name: "Trees".into(), on: true, ..Default::default() });
        one.blocks.insert("Tree".into(), d.blocks["Tree"].clone());
        for k in 0..60 {
            one.add(&Space::Model, Common { layer: "Trees".into(), ..Common::default() }, tree(f64::from(k) * 10.0)).unwrap();
        }
        let mut prep = preview::prepare(&one, &json!({"space": "Model"}), &c).unwrap();
        prep.block_cap = 100;
        let note = refused(&prep, &c);
        let ways = words(&note);
        assert!(ways.contains("every block reference is on one layer (Trees)") && ways.contains("cannot split the drawing into passes"), "{ways}");
    }

    /// An array of more than 10,000 columns is drawn short; every answer that draws says so
    /// (also when nothing is truncated) and names the way to draw all of it.
    #[test]
    fn an_array_drawn_short_is_reported() {
        use cadcraft_engine::doc::{Block, Common, Entity, Handle, Insert, Line};
        use cadcraft_engine::geom::Vec3;
        let c = ctx(None, true);
        let mut d = Drawing::new_metric();
        let mut b = Block::new("Pole");
        b.entities.push(Entity::new(Handle(900), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(0.0, 4.0, 0.0) })));
        d.blocks.insert("Pole".into(), std::sync::Arc::new(b));
        let array = |cols: u32| {
            EntityKind::Insert(Insert {
                block: "Pole".into(),
                insert: Vec3::ZERO,
                scale: Vec3::new(1.0, 1.0, 1.0),
                rotation: 0.0,
                attribs: vec![],
                cols,
                rows: 1,
                col_spacing: 2.0,
                row_spacing: 0.0,
            })
        };
        let h = d.add(&Space::Model, Common::default(), array(12_000)).unwrap();
        let prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        let list = prep.list_for(&prep.window, 800.0, &c).unwrap();
        assert_eq!(list.prims.len(), 10_000);
        let note = prep.blocks_note(list.expanded, &prep.census(&prep.window, None, &c).unwrap());
        assert_eq!(
            note["arrays_clamped"],
            json!([{"handle": h.hex(), "block": "Pole", "cols": 12_000, "rows": 1, "drawn_cols": 10_000, "drawn_rows": 1}])
        );
        let text = note["arrays_clamped_note"].as_str().unwrap();
        assert!(text.contains("drawn short") && text.contains("10,000 columns and 10,000 rows"), "{text}");
        assert!(!text.contains("arrayrect") && !text.contains("explode") && text.contains("No way to draw more"), "{text}");
        // An array within the limit says nothing.
        let mut ok = d.clone();
        ok.remove_entity(h);
        ok.add(&Space::Model, Common::default(), array(9_000)).unwrap();
        let prep = preview::prepare(&ok, &json!({"space": "Model"}), &c).unwrap();
        let list = prep.list_for(&prep.window, 800.0, &c).unwrap();
        let note = prep.blocks_note(list.expanded, &prep.census(&prep.window, None, &c).unwrap());
        assert!(note.get("arrays_clamped").is_none() && note["blocks_expanded_total"] == json!(18_000), "{note}");
    }

    /// A PDF answer gives each sheet's figures and totals that add up, the blocks drawn as boxes over
    /// all sheets, and (past the limit) the refusal names the sheets over it by number.
    #[test]
    fn a_pdf_note_gives_the_sheets_figures_and_they_add_up() {
        use cadcraft_engine::doc::{Block, Common, Entity, Handle, Insert, Line};
        use cadcraft_engine::geom::Vec3;
        let mut d = Drawing::new_metric();
        let mut b = Block::new("A");
        b.entities.push(Entity::new(Handle(900), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(1.0, 1.0, 0.0) })));
        d.blocks.insert("A".into(), std::sync::Arc::new(b));
        let array = |cols: u32, rows: u32| {
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
        // The model needs 2 x 100 = 200 and the layout's own reference 2 x 20 = 40, with a limit of 50.
        d.add(&Space::Model, Common::default(), array(10, 10)).unwrap();
        d.add(&Space::Paper("Layout1".into()), Common::default(), array(5, 4)).unwrap();
        let spaces = [Space::Model, Space::Paper("Layout1".into())];
        let o = cadcraft_io::PdfOptions { max_block_expansion: 50, complete_only: true, ..Default::default() };
        let cadcraft_io::Plot::TooBig(sheets) = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &spaces, &o).unwrap() else { panic!("a file was made") };
        let err = preview::pdf_refusal(&sheets, &[]);
        assert_eq!(err.extra["sheet_numbers_over_limit"], json!([1]));
        assert_eq!(err.extra["blocks_expanded_total"], json!(200));
        assert_eq!(err.extra["sheets_over_limit"][0]["largest_object"]["kind"], json!("array of block references (MINSERT)"));
        assert_eq!(err.extra["sheets_within_limit"], json!([{"sheet_number": 2, "sheet": "Layout1", "blocks_expanded_total": 40}]));
        assert!(
            err.reason.starts_with("1 of the 2 sheets of this PDF needs more block pieces") && err.reason.contains("sheet 1 (Model) needs 200"),
            "{}",
            err.reason
        );
        // Under the limit: the figures only, and the sums are the answer's.
        let fine = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &spaces, &cadcraft_io::PdfOptions::default()).unwrap();
        let cadcraft_io::Plot::Drawn(fine) = fine else { panic!("too big") };
        let note = preview::pdf_blocks_note(&fine.blocks);
        assert_eq!(
            note,
            json!({"blocks_expanded_total": 240, "sheets": [
                {"sheet": "Model", "sheet_number": 1, "blocks_expanded_total": 200},
                {"sheet": "Layout1", "sheet_number": 2, "blocks_expanded_total": 40}]})
        );
        // Boxed: one piece per sheet and the references named over both.
        let o = cadcraft_io::PdfOptions { blocks_as_boxes: vec!["A".into()], ..Default::default() };
        let cadcraft_io::Plot::Drawn(p) = cadcraft_io::pdf::pdf_sheets_with_blocks(&d, &spaces, &o).unwrap() else { panic!("too big") };
        let note = preview::pdf_blocks_note(&p.blocks);
        assert_eq!(note["blocks_expanded_total"], json!(2));
        assert_eq!(note["blocks_drawn_as_boxes"], json!([{"block": "A", "outlines_drawn": 2}]));
        assert_eq!(note["sheets"][0]["blocks_drawn_as_boxes"], json!([{"block": "A", "outlines_drawn": 1}]));
    }

    /// What counting costs per tile of a tiled preview (L2): the objects a tile takes were picked
    /// twice (once to build, once to count), now once. Prints both for 4 x 4 tiles of 400,000
    /// objects (`cargo test -p cadcraft-mcp counting_cost -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn counting_cost_per_tile() {
        use cadcraft_engine::doc::{Block, Common, Entity, Handle, Insert, Line};
        use cadcraft_engine::geom::Vec3;
        let c = ctx(None, true);
        let mut d = Drawing::new_metric();
        let mut b = Block::new("Tree");
        for k in 0..4u64 {
            b.entities.push(Entity::new(Handle(900 + k), EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(3.0, 1.0 + k as f64, 0.0) })));
        }
        d.blocks.insert("Tree".into(), std::sync::Arc::new(b));
        for k in 0..400_000u32 {
            let at = Vec3::new(f64::from(k % 800) * 10.0, f64::from(k / 800) * 10.0, 0.0);
            let kind = if k % 4 == 0 {
                EntityKind::Insert(Insert {
                    block: "Tree".into(),
                    insert: at,
                    scale: Vec3::new(1.0, 1.0, 1.0),
                    rotation: 0.0,
                    attribs: vec![],
                    cols: 1,
                    rows: 1,
                    col_spacing: 0.0,
                    row_spacing: 0.0,
                })
            } else {
                EntityKind::Line(Line { a: at, b: at + Vec3::new(6.0, 4.0, 0.0) })
            };
            d.add(&Space::Model, Common::default(), kind).unwrap();
        }
        let prep = preview::prepare(&d, &json!({"space": "Model"}), &c).unwrap();
        let tiles = preview::tiles(&prep.window, &json!({"tiles": {"cols": 4, "rows": 4}})).unwrap();
        let t = std::time::Instant::now();
        for (_, win) in &tiles {
            let picked = prep.pick(win, &c).unwrap(); // the build's pick
            let again = prep.pick(win, &c).unwrap(); // the count's own pick (before)
            prep.census(win, Some(&again), &c).unwrap();
            assert_eq!(picked.len(), again.len());
        }
        let before = t.elapsed();
        let t = std::time::Instant::now();
        for (_, win) in &tiles {
            let picked = prep.pick(win, &c).unwrap();
            prep.census(win, Some(&picked), &c).unwrap();
        }
        let after = t.elapsed();
        let t = std::time::Instant::now();
        for (_, win) in &tiles {
            prep.pick(win, &c).unwrap();
        }
        println!("16 tiles of 400,000 objects: pick + count again {before:?}; pick once, count from it {after:?}; the pick alone {:?}", t.elapsed());
    }

    /// An SVG export that is stopped leaves nothing of itself: no file, no temporary file, and an
    /// earlier file stays where it was. Also when the stop comes after bytes were written.
    #[test]
    fn a_stopped_svg_export_leaves_nothing() {
        let dir = scratch("export-svg-stopped");
        let c = ctx(Some(dir.clone()), true);
        let (mut e, _) = svg_sample();
        std::fs::write(dir.join("plan.svg"), "earlier").unwrap();
        c.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(e.call("export", &json!({"path": "plan.svg"}), &c).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("plan.svg")).unwrap(), "earlier");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        // The stop arrives once the file has bytes in it: the writer sets the flag after its
        // first write, the next slice's check ends the call.
        struct StopAfterBytes<'a, W> {
            inner: W,
            stop: &'a std::sync::atomic::AtomicBool,
            wrote: &'a std::cell::Cell<usize>,
        }
        impl<W: std::io::Write> std::io::Write for StopAfterBytes<'_, W> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let n = self.inner.write(buf)?;
                self.wrote.set(self.wrote.get() + n);
                self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }
        let (big, _) = many_entities();
        let d = big.docs.list[0].drawing().unwrap();
        let c = ctx(Some(dir.clone()), true);
        let prep = preview::prepare(d, &json!({"space": "Model"}), &c).unwrap();
        let wrote = std::cell::Cell::new(0);
        let err = write_file_with(&dir.join("plan.svg"), WritePolicy { old_copies: true, overwrite: false }, |w| {
            let out = StopAfterBytes { inner: w, stop: &c.cancel, wrote: &wrote };
            prep.write_svg_sliced(out, &prep.window, 1600.0, &c, 512)
        })
        .unwrap_err();
        assert!(wrote.get() > 0, "bytes were written before the stop");
        assert_eq!(err.code, code::CANCELLED);
        assert_eq!(std::fs::read_to_string(dir.join("plan.svg")).unwrap(), "earlier");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file, no OLD_ copy");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
