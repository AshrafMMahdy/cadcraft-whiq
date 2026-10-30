//! WHIQ: the domain tools of `cadcraft-cli mcp` (the drawing engine the White Hat IQ platform
//! drives): open, new, close, save; read and search; the Preview renderer; export; batched edits;
//! the round-trip check. The contract's own tools (server_status, job_status, list_jobs, cancel,
//! evict) are the runtime's (`runtime.rs`).

use std::path::Path;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{Drawing, EntityKind, Space};
use serde_json::{Map, Value, json};

use super::contract::{ToolError, code};
use super::docs::{Ctx, Doc, Docs, engine_error};
use super::files::{Access, WritePolicy, io_error, saved_entry, write_file};
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
        json!({"name": "read_drawing", "description": "Read a drawing for an agent. section: summary (default: units, version, extents, layouts, layer/block counts, entity counts, title blocks), text (every TEXT/MTEXT/attribute/multileader/table cell with layer, space, position, height; texts inside blocks too), dimensions (shown text and measured value, kind, layer, position), title_block (attributed block references ranked as title blocks, with every attribute and the block's fixed texts; plus texts on each layout sheet), entities (handles, types, layers, bounds; geometry: true adds the full geometry), layers, blocks, layouts (paper, viewports with scale), xrefs. Lists are windowed: limit (default 500) and continue_from; filters: space, layer, window, contains, type. format: json (default) or markdown (a table in the text block; the structured result stays JSON). Pass path instead of document to open and read in one call.",
            "inputSchema": {"type": "object", "properties": {
                "document": doc, "path": {"type": "string", "description": "Open this file first (if not open already)"},
                "section": {"type": "string", "enum": ["summary", "text", "dimensions", "title_block", "entities", "layers", "blocks", "layouts", "xrefs"]},
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
        json!({"name": "render_preview", "description": "The Preview renderer: vector SVG of model space, a layout (space: name) or a layout viewport's model view (viewport: handle) — the extents or a window, split into tiles for big drawings (tiles: {cols, rows}; tile: [col, row] for one), layers switched with layers_off / layers_on / only_layers. Each primitive's layer is in <g data-layer> so a viewer can hide layers too. format: png gives a picture (an image block) for a visual check. Answers tiles while they fit the answer (continue_from for the rest; never cut inside a tile); output writes them to files instead; path opens a drawing first.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "path": {"type": "string", "description": "Open this drawing first"},
                "space": {"type": "string"}, "viewport": {"type": ["string", "integer"]}, "window": window,
                "tiles": {"type": ["object", "array", "integer"]}, "tile": {"type": "array"},
                "layers_off": {"type": ["string", "array"]}, "layers_on": {"type": ["string", "array"]}, "only_layers": {"type": ["string", "array"]},
                "format": {"type": "string", "enum": ["svg", "png"]}, "width": {"type": "integer", "minimum": 16}, "height": {"type": "integer", "minimum": 16},
                "background": {"type": "string", "enum": ["light", "dark"]}, "lineweights": {"type": "boolean"},
                "output": {"type": "string", "description": "Write the SVG/PNG file(s) here (tiles get -c<col>r<row>) instead of answering them"}}},
            "annotations": ro, "_meta": {"whiq/files": {"read": ["path"], "opens": true}}}),
        json!({"name": "export", "description": "Write the drawing in another format: dxf (version 2000–2018, binary: true for binary DXF), dwg (2000–2018), pdf (model, a layout, a list or all layouts — one page each; paper, scale, window, lineweights), svg (one file per space), png. DWG→DXF, DWG/DXF→PDF and →SVG. A file already there is kept as OLD_<name> unless overwrite: true.",
            "inputSchema": {"type": "object", "properties": {"document": doc, "source": {"type": "string", "description": "Open this drawing first"}, "path": {"type": "string", "description": "The file to write (its extension picks the format)"},
                "format": {"type": "string", "enum": ["dxf", "dwg", "pdf", "svg", "png"]}, "version": {"type": ["string", "integer"]}, "binary": {"type": "boolean"},
                "space": {"type": ["string", "array"], "description": "model (default), a layout, a list, or all"}, "window": window,
                "layers_off": {"type": ["string", "array"]}, "layers_on": {"type": ["string", "array"]}, "only_layers": {"type": ["string", "array"]},
                "paper": {"type": "string"}, "landscape": {"type": "boolean"}, "scale": {"type": "number"}, "fit": {"type": "boolean"}, "lineweights": {"type": "boolean"},
                "width": {"type": "integer"}, "height": {"type": "integer"}, "overwrite": {"type": "boolean"}},
                "required": ["path"]},
            "annotations": writes, "_meta": {"whiq/files": {"read": ["source"], "opens": true}}}),
        json!({"name": "edit", "description": "Change a drawing with many operations in one call (all or nothing by default: atomic: false keeps the ones that worked). Each op: {op: \"command\", command, params} runs any CADCraft command (list_commands names them; e.g. line {points}, circle {center, radius}, text {at, text, height}, mtext, pline, hatch, dimlinear, insert {name, at}, move, copy, rotate, scale, mirror, erase {handles}, properties.set {handles, layer, color, …}, layer.new / layer.set / layer.delete, block {name, base, handles}, rename {table, from, to}, purge, layout.set {name}); {op: \"cmdline\", text} types at the command line; shorthands: add {type, …params}, delete {handles}, set_text {handle, text}, set_attributes {handle | block, values: {TAG: value}}, set_properties {handles, …}, layer {action: new|set|delete|current|rename, name, …}, block {action: define|insert|delete|rename, …}, space {name}, undo. Paths in params stay inside the roots; open/save/close are the server's own tools.",
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

/// The worker's side: the drawings and the tools that use them.
#[derive(Default)]
pub struct Engine {
    pub docs: Docs,
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
            "summary" => read::summary(doc, ctx)?,
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
        let v = Value::Object(o);
        let text = s(a, "format").is_some_and(|f| f.eq_ignore_ascii_case("markdown")).then(|| read::markdown("search", &v));
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
        for (k, (cr, win)) in tiles.iter().enumerate().skip(from) {
            ctx.check()?;
            let mut t = json!({"tile": cr, "window": preview::win_value(win)});
            let (bytes, mime, ext, info): (Vec<u8>, &str, &str, Value) = if png {
                let (b, w, h) = preview::png(&prep, win, width, height, dark, ctx)?;
                (b, "image/png", "png", json!({"width": w, "height": h}))
            } else {
                let list = prep.list_for(win, f64::from(width), ctx)?;
                let (svg, n) = preview::svg_tile(&list, &prep.layers, win, f64::from(width), dark, ctx)?;
                (svg.into_bytes(), "image/svg+xml", "svg", json!({"primitives": n}))
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
                let kept =
                    write_file(&p, &bytes, WritePolicy { old_copies: ctx.settings.old_copies, overwrite: flag(a, "overwrite").unwrap_or(false) })?;
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
                let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cadcraft_io::pdf::plot(d, &space, &Value::Object(o))))
                    .map_err(|_| ToolError::new(code::FAILED, "the PDF writer failed on this drawing", "try one layout, or a window"))?
                    .map_err(|e| ToolError::new(code::INVALID_ARGUMENT, e.to_string(), "check space, paper, scale and window"))?;
                let p = ctx.resolve(&file.to_string_lossy(), Access::Write)?;
                let kept = write_file(&p, &bytes, policy)?;
                saved.push(saved_entry(ctx.roots(), &p, bytes.len(), "pdf", kept.as_ref()));
            }
            "svg" | "png" => {
                let sp = match a.get("space") {
                    None | Some(Value::Null) => vec![("Model".to_string(), Space::Model)],
                    other => read::spaces_named(d, other)?,
                };
                let many = sp.len() > 1;
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
                    let bytes = if fmt == "svg" {
                        let wpx = f64::from(a.get("width").and_then(Value::as_u64).map_or(1600, |w| u32::try_from(w).unwrap_or(1600)));
                        let list = prep.list_for(&prep.window, wpx, ctx)?;
                        preview::svg_tile(&list, &prep.layers, &prep.window, wpx, false, ctx)?.0.into_bytes()
                    } else {
                        let w = a.get("width").and_then(Value::as_u64).map_or(2400, |w| u32::try_from(w).unwrap_or(2400));
                        preview::png(&prep, &prep.window, w, a.get("height").and_then(Value::as_u64).and_then(|h| u32::try_from(h).ok()), false, ctx)?
                            .0
                    };
                    let f = if many {
                        let stem = file.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| "drawing".into());
                        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || "-_ ".contains(c) { c } else { '_' }).collect();
                        file.with_file_name(format!("{stem}-{safe}.{fmt}"))
                    } else {
                        file.clone()
                    };
                    let p = ctx.resolve(&f.to_string_lossy(), Access::Write)?;
                    let kept = write_file(&p, &bytes, policy)?;
                    let mut e = saved_entry(ctx.roots(), &p, bytes.len(), &fmt, kept.as_ref());
                    e["space"] = json!(name);
                    saved.push(e);
                }
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
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(dir) = p.parent() {
                let old = (1..10_000)
                    .map(|n| if n == 1 { dir.join(format!("OLD_{name}")) } else { dir.join(format!("OLD_{n}_{name}")) })
                    .find(|x| !x.exists());
                if let Some(old) = old {
                    std::fs::rename(&p, &old).map_err(|e| io_error(&p, &e))?;
                    kept = Some((p.clone(), old));
                }
            }
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
        ctx.stage("writing", 0);
        let bytes = cadcraft_io::write_with(d, &name, &cadcraft_io::SaveOptions { version, binary: false })
            .map_err(|e| ToolError::new(code::FAILED, e.to_string(), "try the other format"))?;
        ctx.check()?;
        ctx.stage("reading back", 0);
        let back = cadcraft_io::read_limited(&bytes, &name, limits_for(bytes.len(), ctx.settings.max_memory_bytes)).map_err(|e| {
            ToolError::new(
                code::FAILED,
                format!("the written {fmt} does not read back: {e}"),
                "report this drawing; save as the other format meanwhile",
            )
        })?;
        let (a1, l1) = read::counts(d);
        let (a2, l2) = read::counts(&back);
        let mut problems = Vec::new();
        for (space, c) in &a1 {
            for (ty, n) in c {
                let m = a2.get(space).and_then(|x| x.get(ty)).copied().unwrap_or(0);
                if m != *n {
                    problems.push(json!({"what": "entities", "space": space, "type": ty, "before": n, "after": m}));
                }
            }
        }
        for (space, c) in &a2 {
            for (ty, m) in c {
                if a1.get(space).and_then(|x| x.get(ty)).is_none() {
                    problems.push(json!({"what": "entities", "space": space, "type": ty, "before": 0, "after": m}));
                }
            }
        }
        let _ = (l1, l2);
        let cmp = |what: &str, x: usize, y: usize, p: &mut Vec<Value>| {
            if x != y {
                p.push(json!({"what": what, "before": x, "after": y}));
            }
        };
        cmp("layers", d.layers.len(), back.layers.len(), &mut problems);
        cmp("blocks", d.blocks.values().filter(|b| !b.anonymous).count(), back.blocks.values().filter(|b| !b.anonymous).count(), &mut problems);
        cmp("layouts", d.layouts.len(), back.layouts.len(), &mut problems);
        let all = read::spaces(d);
        let flt = read::Filter { layers: None, window: None, contains: None, exact: false, types: None, expand_blocks: false };
        let t1: Vec<String> = read::texts(d, &all, &flt, ctx)?.iter().filter_map(|t| t["text"].as_str().map(str::to_string)).collect();
        let t2: Vec<String> =
            read::texts(&back, &read::spaces(&back), &flt, ctx)?.iter().filter_map(|t| t["text"].as_str().map(str::to_string)).collect();
        let mut s1 = t1.clone();
        let mut s2 = t2.clone();
        s1.sort();
        s2.sort();
        if s1 != s2 {
            let lost: Vec<&String> = s1.iter().filter(|x| !s2.contains(x)).take(20).collect();
            problems.push(json!({"what": "texts", "before": t1.len(), "after": t2.len(), "lost_examples": lost}));
        }
        Ok(Reply::json(json!({"document": doc.handle, "format": fmt, "bytes": bytes.len(), "lossless": problems.is_empty(), "problems": problems,
            "note": "compares what CADCraft models (entities per space and type, layers, blocks, layouts, texts); data CADCraft keeps raw is written back as read"})))
    }
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
        let doc: &mut Doc = &mut self.docs.list[i];
        let st = doc.session.state().map_err(engine_error)?;
        let (before, undo_len, space_before) = (st.doc.clone(), st.undo.len(), st.space.clone());
        let mut results = Vec::new();
        let mut failed_at = None;
        let total = ops.len() + usize::from(a.get("space").is_some());
        ctx.stage("editing", u64::try_from(total).unwrap_or(0));
        let mut all: Vec<Value> = Vec::new();
        if let Some(sp) = a.get("space").filter(|v| !v.is_null()) {
            all.push(json!({"op": "space", "name": sp}));
        }
        all.extend(ops);
        for (k, op) in all.iter().enumerate() {
            if ctx.cancelled() {
                rollback(doc, &before, undo_len, &space_before);
                return Err(ToolError::new(
                    code::CANCELLED,
                    format!("stopped as asked before operation {k}"),
                    "the drawing is as it was before the call",
                )
                .with("rolled_back", json!(true)));
            }
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
                            ToolError::new(
                                code::INTERNAL,
                                format!("`{id}` failed inside the engine"),
                                "the drawing was restored; check the parameters",
                            )
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
                Ok(v) => results
                    .push(json!({"index": k, "op": op.get("op").or(op.get("command")).cloned().unwrap_or(Value::Null), "ok": true, "result": v})),
                Err(e) => {
                    results.push(json!({"index": k, "op": op.get("op").or(op.get("command")).cloned().unwrap_or(Value::Null), "ok": false, "error": e.to_value()}));
                    if atomic {
                        failed_at = Some((k, e));
                        break;
                    }
                }
            }
        }
        // A command left waiting for input (a cmdline op that did not finish it) is ended.
        if doc.session.running.is_some() {
            doc.session.cancel();
        }
        if let Some((k, e)) = failed_at {
            rollback(doc, &before, undo_len, &space_before);
            return Err(ToolError::new(
                e.code.as_str(),
                format!("operation {k} failed: {}", e.reason),
                format!(
                    "{} — nothing was changed (atomic); fix that operation and send the batch again, or atomic: false to keep the others",
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
}

fn rollback(doc: &mut Doc, before: &std::sync::Arc<Drawing>, undo_len: usize, space: &Space) {
    if doc.session.running.is_some() {
        doc.session.cancel();
    }
    if let Ok(st) = doc.session.state_mut() {
        st.doc = before.clone();
        st.undo.truncate(undo_len);
        st.redo.clear();
        st.selection.clear();
        st.space = space.clone();
    }
}
