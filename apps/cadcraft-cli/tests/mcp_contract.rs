//! WHIQ: the headless engine's contract, tested against the built binary (`CADCRAFT_CLI=path`
//! runs it against another one, the release workflow's): every tool the platform calls, on
//! drawings made here (no binary fixtures in the repository), with the craft forks' shared
//! shapes (error codes, jobs, busy, roots, OLD_ copies, recovered files).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

/// The error codes the craft engines share; every error must carry one.
const CODES: &[&str] = &[
    "busy",
    "cancelled",
    "disk_full",
    "evicted",
    "failed",
    "internal",
    "invalid_argument",
    "invalid_request",
    "locked",
    "no_document",
    "not_available",
    "not_found",
    "out_of_range",
    "outside_root",
    "password_required",
    "too_large",
    "unsupported",
    "wrong_password",
];

fn bin() -> PathBuf {
    std::env::var_os("CADCRAFT_CLI").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_cadcraft-cli")))
}

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    next: u64,
    /// Answers read while waiting for another one.
    pending: Vec<Value>,
    pub dir: PathBuf,
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cadcraft-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("in")).unwrap();
    std::fs::create_dir_all(d.join("out")).unwrap();
    d
}

impl Mcp {
    fn start(tag: &str, extra: &[&str]) -> Mcp {
        let dir = scratch(tag);
        Mcp::start_in(dir, extra)
    }
    fn start_in(dir: PathBuf, extra: &[&str]) -> Mcp {
        let mut cmd = Command::new(bin());
        cmd.arg("mcp").arg("--automation-read-root").arg(dir.join("in")).arg("--automation-write-root").arg(dir.join("out")).args(extra);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().expect("start cadcraft-cli mcp");
        let stdin = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines() {
                let Ok(l) = l else { return };
                if tx.send(l).is_err() {
                    return;
                }
            }
        });
        let mut m = Mcp { child, stdin, lines: rx, next: 0, pending: Vec::new(), dir };
        let r = m.rpc("initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
        assert_eq!(r["result"]["serverInfo"]["name"], "cadcraft");
        m.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }
    fn send(&mut self, v: &Value) {
        let s = self.stdin.as_mut().unwrap();
        writeln!(s, "{v}").unwrap();
        s.flush().unwrap();
    }
    fn recv(&mut self, id: u64) -> Value {
        if let Some(i) = self.pending.iter().position(|v| v["id"] == id) {
            return self.pending.remove(i);
        }
        loop {
            let l = self.lines.recv_timeout(Duration::from_secs(600)).expect("an answer within 10 minutes");
            let v: Value = serde_json::from_str(&l).unwrap();
            if v["id"] == id {
                return v;
            }
            if v.get("id").is_some_and(|i| !i.is_null()) {
                self.pending.push(v);
            }
        }
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.recv(id)
    }
    /// A tool call's result (`{content, structuredContent, isError}`).
    fn call_raw(&mut self, tool: &str, args: Value) -> Value {
        let r = self.rpc("tools/call", json!({"name": tool, "arguments": args}));
        let res = r["result"].clone();
        if res["isError"] == true {
            let code = res["structuredContent"]["error"]["code"].as_str().unwrap_or("");
            assert!(CODES.contains(&code), "{tool}: error code {code:?} is not a contract code: {res}");
            assert!(res["structuredContent"]["error"]["next_step"].as_str().is_some_and(|s| !s.is_empty()), "{tool}: no next_step: {res}");
        }
        res
    }
    /// A successful call's structured result (a job is waited for).
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let mut r = self.call_raw(tool, args);
        assert_ne!(r["isError"], true, "{tool} failed: {}", r["content"][0]["text"]);
        if r["structuredContent"]["status"] == "running"
            && let Some(job) = r["structuredContent"]["job_id"].as_str().map(str::to_string)
        {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let s = self.call_raw("job_status", json!({"job_id": job}));
                let st = s["structuredContent"].clone();
                if st["status"] != "running" {
                    assert_eq!(st["status"], "done", "{tool} job: {st}");
                    r = json!({"structuredContent": st["result"], "content": s["content"]});
                    break;
                }
            }
        }
        r["structuredContent"].clone()
    }
    fn error(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call_raw(tool, args);
        assert_eq!(r["isError"], true, "{tool} should fail: {r}");
        r["structuredContent"]["error"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.stdin.take());
        for _ in 0..100 {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The handle an edit operation made (`handle`, or the first of `handles`).
fn made(op: &Value) -> String {
    let r = &op["result"];
    r["handle"].as_str().or_else(|| r["handles"][0].as_str()).unwrap_or_else(|| panic!("no handle in {op}")).to_string()
}

/// A construction-style sheet built through `edit`: layers, walls, a door block, a title block
/// with attributes on a layout, texts, notes, dimensions, a hatch.
fn build_sheet(m: &mut Mcp) -> Value {
    let doc = m.call("new_drawing", json!({"metric": true}));
    assert!(doc["document"].as_str().unwrap().starts_with("doc-"));
    let ops = json!([
        {"op": "layer", "action": "new", "name": "A-WALL", "color": "red"},
        {"op": "layer", "action": "new", "name": "A-ANNO", "color": "green"},
        {"op": "layer", "action": "new", "name": "A-DIMS"},
        {"op": "layer", "action": "new", "name": "TITLE"},
        {"op": "layer", "action": "current", "name": "A-WALL"},
        {"op": "add", "type": "pline", "vertices": [[0, 0], [12000, 0], [12000, 8000], [0, 8000]], "closed": true},
        {"op": "add", "type": "line", "points": [[6000, 0], [6000, 8000]]},
        {"op": "layer", "action": "current", "name": "A-ANNO"},
        {"op": "add", "type": "text", "at": [1000, 4000], "text": "OFFICE 101", "height": 250},
        {"op": "add", "type": "mtext", "at": [7000, 4000], "text": "MEETING\\PROOM 102", "height": 250},
        {"op": "add", "type": "text", "at": [1000, 7000], "text": "ملاحظة: الجدار مقاوم للحريق", "height": 200},
        {"op": "layer", "action": "current", "name": "A-DIMS"},
        {"op": "add", "type": "dimlinear", "p1": [0, 0], "p2": [12000, 0], "at": [6000, -1000]},
        {"op": "add", "type": "dimlinear", "p1": [0, 0], "p2": [0, 8000], "at": [-1000, 4000]},
        {"op": "layer", "action": "current", "name": "TITLE"},
        {"op": "add", "type": "attdef", "tag": "DWG_NO", "prompt": "Drawing number", "default": "A-000", "at": [100, 100], "height": 3},
        {"op": "add", "type": "attdef", "tag": "TITLE", "prompt": "Title", "default": "TITLE", "at": [100, 50], "height": 5},
        {"op": "add", "type": "attdef", "tag": "SCALE", "prompt": "Scale", "default": "1:100", "at": [100, 20], "height": 3},
        {"op": "add", "type": "attdef", "tag": "DATE", "prompt": "Date", "default": "", "at": [100, 5], "height": 3},
        {"op": "add", "type": "text", "at": [10, 120], "text": "WHITE HAT IQ TEST CO.", "height": 4}
    ]);
    let r = m.call("edit", json!({"ops": ops}));
    assert_eq!(r["failed"], 0, "{r}");
    // The title block: the attribute definitions and the company line become a block.
    let handles: Vec<String> =
        r["results"].as_array().unwrap()[15..20].iter().map(|x| x["result"]["handle"].as_str().unwrap_or_default().to_string()).collect();
    assert!(handles.iter().all(|h| !h.is_empty()), "attdef/text handles: {r}");
    let r = m.call(
        "edit",
        json!({"ops": [
            {"op": "block", "action": "define", "name": "TB-A3", "base": [0, 0], "handles": handles, "keep": "delete"},
            {"op": "layer", "action": "current", "name": "A-WALL"},
            {"op": "add", "type": "circle", "center": [0, 0], "radius": 450},
            {"op": "add", "type": "line", "points": [[0, 0], [900, 0]]}
        ]}),
    );
    assert_eq!(r["failed"], 0, "{r}");
    let c = made(&r["results"][2]);
    let l = made(&r["results"][3]);
    let r = m.call("edit", json!({"ops": [
        {"op": "block", "action": "define", "name": "DOOR", "base": [0, 0], "handles": [c, l], "keep": "delete"},
        {"op": "block", "action": "insert", "name": "DOOR", "at": [3000, 0]},
        {"op": "block", "action": "insert", "name": "DOOR", "at": [9000, 0]},
        {"op": "space", "name": "Layout1"},
        {"op": "block", "action": "insert", "name": "TB-A3", "at": [0, 0], "attribs": {"DWG_NO": "A-101", "TITLE": "GROUND FLOOR PLAN", "SCALE": "1:100"}}
    ]}));
    assert_eq!(r["failed"], 0, "{r}");
    let tb = made(&r["results"][4]);
    let r =
        m.call("edit", json!({"ops": [{"op": "set_attributes", "handle": tb, "values": {"DATE": "2026-10-30"}}, {"op": "space", "name": "Model"}]}));
    assert_eq!(r["failed"], 0, "{r}");
    doc
}

#[test]
fn tools_list_names_every_tool_with_file_annotations() {
    let mut m = Mcp::start("list", &[]);
    let r = m.rpc("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for want in [
        "server_status",
        "job_status",
        "list_jobs",
        "cancel",
        "evict",
        "open_drawing",
        "new_drawing",
        "read_drawing",
        "search_drawing",
        "render_preview",
        "export",
        "edit",
        "save_drawing",
        "close_drawing",
        "list_commands",
        "roundtrip_check",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    let open = tools.iter().find(|t| t["name"] == "open_drawing").unwrap();
    assert_eq!(open["_meta"]["whiq/files"], json!({"read": ["path"], "opens": true}));
    let export = tools.iter().find(|t| t["name"] == "export").unwrap();
    assert_eq!(export["_meta"]["whiq/files"]["read"], json!(["source"]));
    for t in tools {
        assert!(t["inputSchema"]["properties"]["caller"].is_object(), "{} has no caller", t["name"]);
        assert_eq!(t["annotations"]["openWorldHint"], false);
    }
    let st = m.call("server_status", json!({}));
    assert_eq!(st["server"], "cadcraft");
    assert!(st["memory"]["rss_bytes"].as_u64().unwrap_or(0) > 0 || st["memory"]["measured"] == false);
    assert_eq!(st["limits"]["old_copies"], true);
    assert!(st["roots"]["write"].as_str().unwrap().ends_with("out"));
    assert!(st["fonts"]["source"] == "craft-fonts");
    // An unknown tool is an answer with the closest names, not a protocol error.
    let e = m.error("reed_drawing", json!({}));
    assert_eq!(e["code"], "not_found");
    assert!(e["closest"].as_array().unwrap().iter().any(|c| c == "read_drawing"));
    // Forgiving names and arguments.
    let st = m.call("status", json!({}));
    assert_eq!(st["server"], "cadcraft");
}

#[test]
fn read_summary_text_dimensions_title_block_and_sections() {
    let mut m = Mcp::start("read", &[]);
    build_sheet(&mut m);
    // READ: summary.
    let s = m.call("read_drawing", json!({"section": "summary"}));
    assert_eq!(s["units"]["measurement"], "metric");
    assert!(s["layers"]["names"].as_array().unwrap().iter().any(|l| l == "A-WALL"));
    assert!(s["layouts"].as_array().unwrap().iter().any(|l| l["name"] == "Layout1"));
    assert_eq!(s["title_blocks"][0]["block"], "TB-A3", "{s}");
    // READ: texts (inside blocks too) with layer, space, position.
    let t = m.call("read_drawing", json!({"section": "text", "space": "all"}));
    let texts: Vec<&str> = t["texts"].as_array().unwrap().iter().filter_map(|x| x["text"].as_str()).collect();
    for want in ["OFFICE 101", "ROOM 102", "A-101", "GROUND FLOOR PLAN", "2026-10-30", "WHITE HAT IQ TEST CO.", "ملاحظة: الجدار مقاوم للحريق"]
    {
        assert!(texts.iter().any(|x| x.contains(want)), "{want} not in {texts:?}");
    }
    let office = t["texts"].as_array().unwrap().iter().find(|x| x["text"] == "OFFICE 101").unwrap();
    assert_eq!(office["layer"], "A-ANNO");
    assert_eq!(office["space"], "Model");
    assert_eq!(office["at"], json!([1000.0, 4000.0]));
    let company = t["texts"].as_array().unwrap().iter().find(|x| x["text"] == "WHITE HAT IQ TEST CO.").unwrap();
    assert_eq!(company["space"], "Layout1");
    assert_eq!(company["block"], "TB-A3");
    // READ: dimensions with their values.
    let d = m.call("read_drawing", json!({"section": "dimensions"}));
    let vals: Vec<f64> = d["dimensions"].as_array().unwrap().iter().map(|x| x["measurement"].as_f64().unwrap()).collect();
    assert!(vals.contains(&12000.0) && vals.contains(&8000.0), "{d}");
    assert!(d["dimensions"][0]["text"].as_str().unwrap().contains("000"), "{d}");
    assert_eq!(d["dimensions"][0]["layer"], "A-DIMS");
    // READ: title block attributes.
    let tb = m.call("read_drawing", json!({"section": "title_block"}));
    let c = &tb["candidates"][0];
    assert_eq!(c["likely"], true, "{tb}");
    assert_eq!(c["attributes"]["DWG_NO"], "A-101");
    assert_eq!(c["attributes"]["DATE"], "2026-10-30");
    assert_eq!(c["space"], "Layout1");
    // READ: layers, blocks, layouts, entities; markdown.
    let l = m.call("read_drawing", json!({"section": "layers"}));
    assert!(l["layers"].as_array().unwrap().iter().any(|x| x["name"] == "A-WALL" && x["entities"].as_u64().unwrap() >= 2));
    let b = m.call("read_drawing", json!({"section": "blocks"}));
    let door = b["blocks"].as_array().unwrap().iter().find(|x| x["name"] == "DOOR").unwrap();
    assert_eq!(door["references"], 2);
    let lay = m.call("read_drawing", json!({"section": "layouts"}));
    assert!(lay["layouts"].as_array().unwrap().iter().any(|x| x["name"] == "Layout1"));
    let e = m.call("read_drawing", json!({"section": "entities", "space": "model", "layer": "A-WALL"}));
    assert!(e["entities"].as_array().unwrap().iter().all(|x| x["layer"] == "A-WALL"));
    let raw = m.call_raw("read_drawing", json!({"section": "text", "format": "markdown"}));
    assert!(raw["content"][0]["text"].as_str().unwrap().contains("| text |"));
    // Windowing: limit + continue_from cover every text once.
    let all = t["total"].as_u64().unwrap();
    let mut seen = 0;
    let mut from = 0;
    loop {
        let p = m.call("read_drawing", json!({"section": "text", "limit": 2, "continue_from": from}));
        seen += p["texts"].as_array().unwrap().len() as u64;
        match p["continue_from"].as_u64() {
            Some(n) => from = n,
            None => break,
        }
    }
    assert_eq!(seen, all);
}

#[test]
fn search_finds_texts_attributes_dimensions_layers_and_blocks() {
    let mut m = Mcp::start("search", &[]);
    build_sheet(&mut m);
    // SEARCH: text, attribute value, layer and block names → entity, layer, layout, position.
    let h = m.call("search_drawing", json!({"query": "floor plan"}));
    let hit = &h["hits"][0];
    assert_eq!(hit["kind"], "Attribute");
    assert_eq!(hit["tag"], "TITLE");
    assert_eq!(hit["space"], "Layout1");
    assert!(hit["at"].is_array() && hit["handle"].is_string());
    let h = m.call("search_drawing", json!({"query": "A-WALL", "kinds": "layer"}));
    assert_eq!(h["hits"][0]["kind"], "Layer");
    let h = m.call("search_drawing", json!({"query": "door", "kinds": ["block", "block_reference"]}));
    assert_eq!(h["total"], 3, "{h}");
    let h = m.call("search_drawing", json!({"query": "12000", "kinds": "dimension"}));
    assert_eq!(h["total"], 1, "{h}");
}

#[test]
fn edit_and_delete_in_batches_all_or_nothing() {
    let mut m = Mcp::start("edit", &[]);
    build_sheet(&mut m);
    let office = m.call("search_drawing", json!({"query": "OFFICE 101", "match": "exact"}))["hits"][0].clone();
    // EDIT: change a text, an attribute by block, properties; atomic rollback.
    let office_h = office["handle"].as_str().unwrap().to_string();
    let r = m.call(
        "edit",
        json!({"ops": [
            {"op": "set_text", "handle": office_h, "text": "OFFICE 101A"},
            {"op": "set_attributes", "block": "TB-A3", "values": {"SCALE": "1:50"}},
            {"op": "set_properties", "handles": [office_h], "layer": "A-WALL"}
        ]}),
    );
    assert_eq!(r["failed"], 0, "{r}");
    let h = m.call("search_drawing", json!({"query": "OFFICE 101A", "match": "exact"}));
    assert_eq!(h["hits"][0]["layer"], "A-WALL");
    let before = m.call("read_drawing", json!({}))["entities"].clone();
    let e =
        m.error("edit", json!({"ops": [{"op": "add", "type": "line", "points": [[0, 0], [1, 1]]}, {"op": "command", "command": "no-such-command"}]}));
    assert_eq!(e["rolled_back"], true);
    assert_eq!(e["failed_at"], 1);
    assert_eq!(m.call("read_drawing", json!({}))["entities"], before, "an atomic batch that failed changed nothing");
    // A batch of many ops in one call.
    let many: Vec<Value> = (0..200).map(|i| json!({"op": "add", "type": "circle", "center": [i * 50, 9000], "radius": 10})).collect();
    let r = m.call("edit", json!({"ops": many}));
    assert_eq!(r["applied"], 200);
    // DELETE: entities, a layer, a block (its references too).
    let circles = m.call("read_drawing", json!({"section": "entities", "type": "Circle", "space": "model", "limit": 1000}));
    let hs: Vec<Value> = circles["entities"].as_array().unwrap().iter().filter(|x| x["in_block"].is_null()).map(|x| x["handle"].clone()).collect();
    assert_eq!(hs.len(), 200);
    let r = m.call("edit", json!({"ops": [{"op": "delete", "handles": hs}, {"op": "layer", "action": "new", "name": "TEMP"}, {"op": "layer", "action": "delete", "name": "TEMP"}, {"op": "block", "action": "delete", "name": "DOOR"}]}));
    assert_eq!(r["failed"], 0, "{r}");
    let b = m.call("read_drawing", json!({"section": "blocks"}));
    assert!(!b["blocks"].as_array().unwrap().iter().any(|x| x["name"] == "DOOR"), "{b}");
    let l = m.call("read_drawing", json!({"section": "layers"}));
    assert!(!l["layers"].as_array().unwrap().iter().any(|x| x["name"] == "TEMP"));
}

#[test]
fn save_as_dwg_and_dxf_versions_and_open_them_back() {
    let mut m = Mcp::start("save", &[]);
    build_sheet(&mut m);
    let office = m.call("search_drawing", json!({"query": "OFFICE 101", "match": "exact"}))["hits"][0]["handle"].as_str().unwrap().to_string();
    let r = m.call("edit", json!({"ops": [{"op": "set_text", "handle": office, "text": "OFFICE 101A"}, {"op": "set_attributes", "block": "TB-A3", "values": {"SCALE": "1:50"}}]}));
    assert_eq!(r["failed"], 0, "{r}");
    // SAVE as DWG and DXF, open them back, read the same texts.
    for (name, ver) in [("sheet.dwg", 2018), ("sheet2013.dwg", 2013), ("sheet2000.dwg", 2000), ("sheet.dxf", 2018), ("sheet2007.dxf", 2007)] {
        let s = m.call("save_drawing", json!({"path": name, "version": ver}));
        assert!(s["saved"][0]["bytes"].as_u64().unwrap() > 1000, "{s}");
        assert!(m.dir.join("out").join(name).exists());
    }
    for name in ["sheet.dwg", "sheet2013.dwg", "sheet2000.dwg", "sheet.dxf", "sheet2007.dxf"] {
        std::fs::copy(m.dir.join("out").join(name), m.dir.join("in").join(name)).unwrap();
        let o = m.call("open_drawing", json!({"path": name}));
        let t = m.call("read_drawing", json!({"document": o["document"], "section": "text"}));
        let texts: Vec<&str> = t["texts"].as_array().unwrap().iter().filter_map(|x| x["text"].as_str()).collect();
        for want in ["OFFICE 101A", "GROUND FLOOR PLAN", "1:50", "ملاحظة: الجدار مقاوم للحريق"] {
            assert!(texts.contains(&want), "{name}: {want} not in {texts:?}");
        }
        let d = m.call("read_drawing", json!({"document": o["document"], "section": "dimensions"}));
        assert_eq!(d["total"], 2, "{name}: {d}");
        let rt = m.call("roundtrip_check", json!({"document": o["document"], "format": "dxf"}));
        assert_eq!(rt["lossless"], true, "{name}: {rt}");
        m.call("close_drawing", json!({"document": o["document"]}));
    }
}

#[test]
fn preview_and_export() {
    let mut m = Mcp::start("preview", &[]);
    build_sheet(&mut m);
    let p = m.call("render_preview", json!({}));
    let svg = p["tiles"][0]["svg"].as_str().unwrap();
    assert!(svg.starts_with("<svg") && svg.contains("data-layer=\"A-WALL\""), "{}", &svg[..svg.len().min(300)]);
    // Layers off: nothing of that layer is drawn.
    let p = m.call("render_preview", json!({"layers_off": ["A-WALL"]}));
    assert!(!p["tiles"][0]["svg"].as_str().unwrap().contains("data-layer=\"A-WALL\""));
    let p = m.call("render_preview", json!({"only_layers": "A-DIMS"}));
    let svg = p["tiles"][0]["svg"].as_str().unwrap();
    assert!(svg.contains("data-layer=\"A-DIMS\"") && !svg.contains("data-layer=\"A-ANNO\""));
    // Tiles: 3 x 2, each holding only what it shows; answered while they fit, then continue_from.
    let p = m.call("render_preview", json!({"tiles": {"cols": 3, "rows": 2}, "max_bytes": 4096}));
    assert_eq!(p["total_tiles"], 6);
    assert!(p["returned"].as_u64().unwrap() >= 1);
    let mut got = p["returned"].as_u64().unwrap();
    let mut next = p["continue_from"].as_u64();
    while let Some(n) = next {
        let q = m.call("render_preview", json!({"tiles": {"cols": 3, "rows": 2}, "max_bytes": 4096, "continue_from": n}));
        got += q["returned"].as_u64().unwrap();
        next = q["continue_from"].as_u64();
    }
    assert_eq!(got, 6);
    // A window, a layout, a PNG picture.
    let p = m.call("render_preview", json!({"window": [[0, 0], [6000, 8000]]}));
    assert_eq!(p["window"], json!([[0.0, 0.0], [6000.0, 8000.0]]));
    let p = m.call("render_preview", json!({"space": "Layout1"}));
    // The title block reference is on A-WALL (the layer current when it was inserted).
    assert!(p["tiles"][0]["svg"].as_str().unwrap().contains("data-layer=\"A-WALL\""), "{p}");
    let raw = m.call_raw("render_preview", json!({"format": "png", "width": 400}));
    assert_eq!(raw["content"][1]["type"], "image");
    assert_eq!(raw["content"][1]["mimeType"], "image/png");
    // Files instead of answers.
    let p = m.call("render_preview", json!({"tiles": [2, 1], "output": "prev.svg"}));
    assert!(m.dir.join("out/prev-c0r0.svg").exists() && m.dir.join("out/prev-c1r0.svg").exists(), "{p}");
    // EXPORT: DXF, DWG, PDF (all layouts), SVG per space, PNG.
    let e = m.call("export", json!({"path": "plan.pdf", "space": "all"}));
    let pdf = std::fs::read(m.dir.join("out/plan.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF"), "{e}");
    m.call("export", json!({"path": "plan.dxf"}));
    assert!(std::fs::read_to_string(m.dir.join("out/plan.dxf")).unwrap().contains("ENTITIES"));
    m.call("export", json!({"path": "plan.dwg", "version": 2018}));
    assert!(std::fs::read(m.dir.join("out/plan.dwg")).unwrap().starts_with(b"AC1032"));
    let e = m.call("export", json!({"path": "plan.svg", "space": "all"}));
    let spaces: Vec<&str> = e["saved"].as_array().unwrap().iter().filter_map(|x| x["space"].as_str()).collect();
    assert_eq!(spaces, ["Model", "Layout1", "Layout2"], "{e}");
    m.call("export", json!({"path": "plan.png", "width": 800}));
    assert!(std::fs::read(m.dir.join("out/plan.png")).unwrap().starts_with(b"\x89PNG"));
    // A second export over the same name keeps the first as OLD_.
    let e = m.call("export", json!({"path": "plan.pdf"}));
    assert!(e["saved"][0]["previous_renamed_to"].as_str().unwrap().contains("OLD_plan.pdf"), "{e}");
    let e = m.call("export", json!({"path": "plan.pdf", "overwrite": true}));
    assert!(e["saved"][0]["previous_renamed_to"].is_null());
    // DWG → DXF / PDF / SVG of a file opened from disk.
    std::fs::copy(m.dir.join("out/plan.dwg"), m.dir.join("in/from.dwg")).unwrap();
    for (out, magic) in [("from.dxf", &b"  0"[..]), ("from.pdf", &b"%PDF"[..]), ("from.svg", &b"<svg"[..])] {
        m.call("export", json!({"source": "from.dwg", "path": out, "space": if out.ends_with("pdf") { json!("all") } else { json!("model") }}));
        let b = std::fs::read(m.dir.join("out").join(out)).unwrap();
        assert!(
            b.starts_with(magic)
                || String::from_utf8_lossy(&b[..b.len().min(64)]).trim_start().starts_with(std::str::from_utf8(magic).unwrap().trim()),
            "{out}"
        );
    }
}

#[test]
fn roots_errors_old_copies_and_handles() {
    let mut m = Mcp::start("roots", &[]);
    std::fs::write(m.dir.join("elsewhere.dwg"), b"x").unwrap();
    let outside = m.dir.join("elsewhere.dwg").to_string_lossy().into_owned();
    let e = m.error("open_drawing", json!({"path": outside}));
    assert_eq!(e["code"], "outside_root");
    let e = m.error("open_drawing", json!({"path": "missing.dwg"}));
    assert_eq!(e["code"], "not_found");
    let e = m.error("read_drawing", json!({}));
    assert_eq!(e["code"], "no_document");
    m.call("new_drawing", json!({}));
    let e = m.error("export", json!({"path": "../escape.pdf"}));
    assert_eq!(e["code"], "outside_root");
    let e = m.error("read_drawing", json!({"document": "doc-99"}));
    assert_eq!(e["code"], "not_found");
    assert!(e["open_documents"].is_array());
    let e = m.error("edit", json!({"ops": [{"op": "command", "command": "saveas", "params": {"path": "/tmp/x.dxf"}}]}));
    assert_eq!(e["code"], "unsupported");
    let e = m.error(
        "edit",
        json!({"ops": [{"op": "command", "command": "wblock", "params": {"path": m.dir.join("x.dwg").to_string_lossy(), "handles": []}}]}),
    );
    assert_eq!(e["code"], "outside_root");
    // Not a drawing.
    std::fs::write(m.dir.join("in/junk.dwg"), b"AC1032 this is not a drawing at all").unwrap();
    let e = m.error("open_drawing", json!({"path": "junk.dwg"}));
    assert!(["failed", "unsupported"].contains(&e["code"].as_str().unwrap()), "{e}");
    // Unsaved changes are not closed silently.
    m.call("edit", json!({"ops": [{"op": "add", "type": "line", "points": [[0, 0], [1, 1]]}]}));
    let e = m.error("close_drawing", json!({}));
    assert_eq!(e["code"], "invalid_request");
    m.call("close_drawing", json!({"discard": true}));
}

#[test]
fn no_old_copies_flag_replaces_files() {
    let mut m = Mcp::start("noold", &["--automation-no-old-copies"]);
    m.call("new_drawing", json!({}));
    m.call("export", json!({"path": "a.pdf"}));
    let e = m.call("export", json!({"path": "a.pdf"}));
    assert!(e["saved"][0]["previous_renamed_to"].is_null());
    assert!(!m.dir.join("out/OLD_a.pdf").exists());
    assert_eq!(m.call("server_status", json!({}))["limits"]["old_copies"], false);
}

#[test]
fn jobs_busy_cancel_and_evict() {
    let mut m = Mcp::start("jobs", &["--busy-after-ms", "0"]);
    build_sheet(&mut m);
    // async: true is a job at once; job_status has the result.
    let r = m.call_raw("read_drawing", json!({"section": "text", "async": true}));
    let job = r["structuredContent"]["job_id"].as_str().unwrap().to_string();
    assert_eq!(r["structuredContent"]["status"], "running");
    let mut done = Value::Null;
    for _ in 0..200 {
        let s = m.call_raw("job_status", json!({"job_id": job}))["structuredContent"].clone();
        if s["status"] != "running" {
            done = s;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(done["status"], "done", "{done}");
    assert!(done["result"]["texts"].is_array());
    assert!(m.call("list_jobs", json!({}))["jobs"].as_array().unwrap().iter().any(|j| j["job_id"] == job.as_str()));
    let e = m.error("job_status", json!({"job_id": "job-9999"}));
    assert_eq!(e["code"], "not_found");
    let c = m.call("cancel", json!({}));
    assert_eq!(c["status"], "not_running");
    // Busy: a big batch keeps the worker, a call meanwhile is answered busy with retry_after_ms.
    let many: Vec<Value> = (0..20_000).map(|i| json!({"op": "add", "type": "line", "points": [[i, 0], [i, 10]]})).collect();
    m.next += 1;
    let big = m.next;
    m.send(&json!({"jsonrpc": "2.0", "id": big, "method": "tools/call", "params": {"name": "edit", "arguments": {"ops": many}}}));
    std::thread::sleep(Duration::from_millis(300));
    let r = m.call_raw("read_drawing", json!({}));
    let status = m.call("server_status", json!({}));
    if r["isError"] == true {
        assert_eq!(r["structuredContent"]["error"]["code"], "busy");
        assert!(r["structuredContent"]["error"]["retry_after_ms"].as_u64().is_some());
    }
    // cancel stops the batch and rolls it back whole.
    let c = m.call("cancel", json!({}));
    let reply = m.recv(big);
    if c["status"] == "cancelling" {
        assert_eq!(reply["result"]["structuredContent"]["error"]["code"], "cancelled", "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["error"]["rolled_back"], true);
    }
    let _ = status;
    // evict auto-saves the unsaved drawing; the next call names the recovered file.
    let ev = m.call("evict", json!({"reason": "test"}));
    let rec = ev["evicted"][0]["recovered_path"].as_str().unwrap().to_string();
    assert!(rec.contains(".recovered-") && rec.ends_with(".dxf"), "{ev}");
    assert!(m.dir.join("out").join(&rec).exists());
    let e = m.error("read_drawing", json!({}));
    assert_eq!(e["code"], "evicted");
    assert_eq!(e["recovered_path"], rec.as_str());
    std::fs::copy(m.dir.join("out").join(&rec), m.dir.join("in").join(&rec)).unwrap();
    let o = m.call("open_drawing", json!({"path": rec}));
    let t = m.call("read_drawing", json!({"document": o["document"], "section": "text", "contains": "GROUND"}));
    assert_eq!(t["total"], 1, "the recovered file holds the work: {t}");
}

#[test]
fn hang_up_with_unsaved_work_leaves_a_recovered_file() {
    let dir = scratch("hangup");
    {
        let mut m = Mcp::start_in(dir.clone(), &[]);
        m.call("new_drawing", json!({}));
        m.call("edit", json!({"ops": [{"op": "add", "type": "text", "at": [0, 0], "text": "UNSAVED NOTE", "height": 2.5}]}));
        drop(m.stdin.take());
        for _ in 0..200 {
            if m.child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        m.dir = std::env::temp_dir().join("cadcraft-mcp-nothing-to-remove");
    }
    let found: Vec<PathBuf> = std::fs::read_dir(dir.join("out"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".recovered-"))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(std::fs::read_to_string(&found[0]).unwrap().contains("UNSAVED NOTE"));
    // The next server announces it.
    let mut m = Mcp::start_in(dir, &[]);
    let st = m.call("server_status", json!({}));
    assert_eq!(st["recovered_files"][0]["reason"], "found", "{st}");
    let _ = Path::new("");
}

#[test]
fn xrefs_attach_list_and_read_through() {
    let mut m = Mcp::start("xref", &[]);
    m.call("new_drawing", json!({}));
    m.call("edit", json!({"ops": [{"op": "add", "type": "text", "at": [0, 0], "text": "XREF NOTE", "height": 2.5}, {"op": "add", "type": "line", "points": [[0, 0], [10, 0]]}]}));
    m.call("save_drawing", json!({"path": "part.dxf"}));
    std::fs::copy(m.dir.join("out/part.dxf"), m.dir.join("in/part.dxf")).unwrap();
    m.call("new_drawing", json!({}));
    let r = m.call("edit", json!({"ops": [{"op": "command", "command": "xattach", "params": {"path": "part.dxf", "at": [100, 0]}}]}));
    assert_eq!(r["failed"], 0, "{r}");
    let x = m.call("read_drawing", json!({"section": "xrefs"}));
    assert_eq!(x["xrefs"][0]["name"], "part");
    assert_eq!(x["xrefs"][0]["status"], "loaded", "{x}");
    let h = m.call("search_drawing", json!({"query": "XREF NOTE"}));
    assert_eq!(h["hits"][0]["block"], "part");
    assert_eq!(h["hits"][0]["at"], json!([100.0, 0.0]));
    // An xref outside the roots is refused, like any path.
    let e = m.error(
        "edit",
        json!({"ops": [{"op": "command", "command": "xattach", "params": {"path": m.dir.join("elsewhere.dxf").to_string_lossy(), "at": [0, 0]}}]}),
    );
    assert_eq!(e["code"], "outside_root");
}
