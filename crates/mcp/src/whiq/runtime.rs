//! WHIQ: the server `cadcraft-cli mcp` runs — nothing hangs, work is never lost (the shared
//! "no dead stops" contract of the craft forks, CONTRACT-PARITY.md there).
//!
//! - **One worker thread** owns the drawings. The stdio loop never waits for it: a call that
//!   arrives while another runs waits `--busy-after-ms`, then is answered `busy` with
//!   `retry_after_ms` and the running job's id.
//! - **Jobs.** A call still running after `--job-after-ms` answers `{job_id, status: "running"}`
//!   and carries on; `async: true` (or opening a file of `--open-job-mb` or more) is a job from
//!   the start. `job_status` reports progress and keeps the result.
//! - `server_status`, `job_status`, `list_jobs` and `cancel` are answered by the stdio loop
//!   itself, even mid-call. `cancel` stops the running call at its next safe point; an edit is
//!   rolled back whole.
//! - **Auto-save.** Unsaved drawings are written to `<name>.recovered-<stamp>.dxf` in the write
//!   root on eviction (`evict`, idle, memory), a panic, SIGTERM/SIGINT/SIGHUP, the client hanging
//!   up, and as checkpoints (`--checkpoint-edits`, `--checkpoint-secs`); the next answer names
//!   the file, and files found at start are announced.
#![allow(clippy::disallowed_methods)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::contract::{self, ToolError, code};
use super::docs::{Ctx, Evicted, Progress, Settings, lock, rfc3339_now, stamp};
use super::tools::{Engine, Reply, definitions};

/// How the server behaves (the contract's flags).
#[derive(Clone, Debug)]
pub struct Config {
    pub settings: Settings,
    /// `--busy-after-ms`.
    pub busy_after: Duration,
    /// `--job-after-ms`.
    pub job_after: Duration,
    /// `--open-job-mb` in bytes.
    pub open_job_bytes: u64,
    /// `--idle-evict-secs`.
    pub idle_evict: Option<Duration>,
    /// `--checkpoint-edits` (0: off).
    pub checkpoint_edits: u64,
    /// `--checkpoint-secs` (`None`: off).
    pub checkpoint_interval: Option<Duration>,
    pub handle_signals: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            settings: Settings::default(),
            busy_after: Duration::from_millis(300),
            job_after: Duration::from_millis(15_000),
            open_job_bytes: 20 * 1024 * 1024,
            idle_evict: None,
            checkpoint_edits: 20,
            checkpoint_interval: Some(Duration::from_secs(60)),
            handle_signals: true,
        }
    }
}

const CONTROL_TOOLS: &[&str] = &["server_status", "job_status", "list_jobs", "cancel"];

/// The contract's tools beside the domain ones.
fn control_definitions() -> Vec<Value> {
    let ro = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    let ctl = json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    vec![
        json!({"name": "server_status", "description": "The server: version, memory, fonts, roots, open drawings, the call running, jobs, recovered files, limits.", "inputSchema": {"type": "object", "properties": {}}, "annotations": ro}),
        json!({"name": "job_status", "description": "A job's status, progress and (when done) result. Without job_id: every job.", "inputSchema": {"type": "object", "properties": {"job_id": {"type": ["string", "integer"]}}}, "annotations": ro}),
        json!({"name": "list_jobs", "description": "Every job: id, tool, status, progress.", "inputSchema": {"type": "object", "properties": {}}, "annotations": ro}),
        json!({"name": "cancel", "description": "Stop the running call (or the job named) at its next safe point; nothing it started is kept (an edit is rolled back whole).", "inputSchema": {"type": "object", "properties": {"job_id": {"type": ["string", "integer"]}, "queued": {"type": "boolean"}}}, "annotations": ctl}),
        json!({"name": "evict", "description": "Auto-save every drawing with unsaved changes to its recovered file, then close them all (for a supervisor releasing memory). Answers the recovered and original paths.", "inputSchema": {"type": "object", "properties": {"reason": {"type": "string"}}}, "annotations": ctl}),
    ]
}

/// Every tool definition, with the common arguments added to each schema.
pub fn all_definitions() -> Vec<Value> {
    let mut v = control_definitions();
    v.extend(definitions());
    for d in &mut v {
        if let Some(props) = d.pointer_mut("/inputSchema/properties").and_then(Value::as_object_mut) {
            props.entry("caller").or_insert(json!({"type": "string", "description": "Who is asking (stamped on saves)"}));
            props.entry("async").or_insert(json!({"type": "boolean", "description": "Run as a job at once (job_status for the result)"}));
            props.entry("continue_from").or_insert(json!({"type": ["integer", "string"], "description": "Resume a trimmed list here"}));
            props.entry("max_bytes").or_insert(json!({"type": "integer", "description": "Trim the answer to this many bytes of JSON"}));
        }
    }
    v
}

#[derive(Clone, Debug, PartialEq)]
enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

fn state_name(s: &JobState) -> &'static str {
    match s {
        JobState::Running => "running",
        JobState::Done => "done",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

struct Job {
    tool: String,
    state: JobState,
    started: Instant,
    finished: Option<Instant>,
    result: Option<Value>,
    handed_off: bool,
    client_cancelled: bool,
    progress: Progress,
}

#[derive(Clone)]
struct Running {
    tool: String,
    job: u64,
    started: Instant,
    id: Value,
    token: Option<Value>,
}

enum Work {
    Call { tool: String, args: Value, job: u64, done: mpsc::Sender<Value> },
    Evict { reason: String, done: Option<mpsc::Sender<Value>> },
}

/// What the stdio side knows of each open drawing (after every call).
#[derive(Clone)]
struct DocSnap {
    handle: String,
    uid: u64,
    name: String,
    path: Option<String>,
    dirty: bool,
    active: bool,
    entities: usize,
    format: String,
    drawing: Arc<cadcraft_engine::doc::Drawing>,
}

#[derive(Default)]
struct Checkpoint {
    path: Option<PathBuf>,
    seen: usize,
    written: usize,
    edits: u64,
    since: Option<Instant>,
}

struct Shared {
    out: Mutex<Box<dyn Write + Send>>,
    jobs: Mutex<(u64, BTreeMap<u64, Job>)>,
    busy: Mutex<Option<Running>>,
    snapshot: Mutex<Vec<DocSnap>>,
    recovered: Mutex<Vec<Value>>,
    unannounced: Mutex<Vec<Value>>,
    evicted: Mutex<Vec<Evicted>>,
    checkpoints: Mutex<HashMap<u64, Checkpoint>>,
    checkpoint_writes: AtomicU64,
    calls: AtomicU64,
    busy_replies: AtomicU64,
    last_activity: Mutex<Instant>,
    idle_done: AtomicBool,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    note: Mutex<(u64, f64, Option<Instant>)>,
    config: Config,
    started: Instant,
}

fn job_name(n: u64) -> String {
    format!("job-{n}")
}
fn parse_job(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase();
    t.trim_start_matches("job").trim_start_matches(['-', '_', ' ']).parse().ok()
}
fn retry_after(elapsed_ms: u64) -> u64 {
    (elapsed_ms / 4).clamp(250, 5000)
}

/// `{content, structuredContent, isError}` of a result or an error.
fn tool_value(r: Result<Reply, ToolError>, max: usize, from: usize) -> Value {
    match r {
        Ok(reply) => {
            let mut sc = reply.value;
            if reply.trim {
                sc = if from > 0 && sc.get("continue_from").is_none() && sc.get("offset").is_none() {
                    contract::trim_from(sc, from, max)
                } else {
                    contract::trim_result(sc, max)
                };
            }
            let text = reply.text.unwrap_or_else(|| serde_json::to_string(&sc).unwrap_or_default());
            let mut content = vec![json!({"type": "text", "text": text})];
            content.extend(reply.extra);
            let sc = if sc.is_object() { sc } else { json!({"result": sc}) };
            json!({"content": content, "structuredContent": sc, "isError": false})
        }
        Err(e) => failure(&e),
    }
}

fn failure(e: &ToolError) -> Value {
    json!({"content": [{"type": "text", "text": e.text()}], "structuredContent": {"error": e.to_value()}, "isError": true})
}

/// The memory the process uses, from the OS.
fn memory() -> Value {
    match memory_stats::memory_stats() {
        Some(m) => {
            json!({"rss_bytes": m.physical_mem, "committed_bytes": m.virtual_mem, "measured": true, "source": if cfg!(windows) { "GetProcessMemoryInfo" } else if cfg!(target_os = "macos") { "task_info" } else { "/proc/self" }})
        }
        None => json!({"rss_bytes": null, "committed_bytes": null, "measured": false, "source": "unavailable"}),
    }
}
fn rss() -> Option<u64> {
    memory_stats::memory_stats().map(|m| m.physical_mem as u64)
}

/// The fonts the binary carries (craft-fonts at build time).
pub fn fonts() -> Value {
    let f = cadcraft_engine::fonts::CRAFT_FONTS;
    let mut families: Vec<String> = f.iter().map(|x| x.family.to_string()).collect();
    families.dedup();
    let arabic = f.iter().find(|x| x.scripts.contains(&"Arab")).map(|x| format!("{} {}", x.family, x.style).trim_end_matches(" Regular").to_string());
    json!({"source": "craft-fonts", "faces": f.len(), "families": families, "default": "CADCraft stroke font (built in)", "arabic": arabic})
}

impl Shared {
    fn write(&self, v: &Value) {
        let mut out = lock(&self.out);
        let _ = out.write_all(v.to_string().as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
    fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
        self.idle_done.store(false, Ordering::SeqCst);
    }

    fn announce(&self, reply: &mut Value) {
        let news = std::mem::take(&mut *lock(&self.unannounced));
        if news.is_empty() {
            return;
        }
        let paths: Vec<String> = news.iter().filter_map(|e| e["path"].as_str().map(str::to_string)).collect();
        if let Some(sc) = reply.pointer_mut("/result/structuredContent").and_then(Value::as_object_mut) {
            sc.insert("recovered_files".into(), Value::Array(news));
        }
        if let Some(c) = reply.pointer_mut("/result/content").and_then(Value::as_array_mut) {
            c.push(json!({"type": "text", "text": format!("unsaved work was auto-saved to recovered files: {} (open_drawing on one to continue it)", paths.join(", "))}));
        }
    }

    /// Write `snap`'s drawing to its recovered file (the checkpoint file when it has one).
    fn autosave(&self, snap: &DocSnap, why: &str) -> Option<PathBuf> {
        let dir = self
            .config
            .settings
            .roots
            .write
            .clone()
            .or_else(|| snap.path.as_deref().and_then(|p| Path::new(p).parent().map(Path::to_path_buf)).filter(|d| !d.as_os_str().is_empty()))
            .or_else(|| std::env::current_dir().ok())?;
        let stem = Path::new(&snap.name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Drawing".into());
        let stem: String = stem.chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).collect();
        let existing = lock(&self.checkpoints).get(&snap.uid).and_then(|c| c.path.clone());
        let path = existing.unwrap_or_else(|| {
            let base = format!("{}.recovered-{}", stem.trim(), stamp());
            let mut p = dir.join(format!("{base}.dxf"));
            let mut n = 2;
            while p.exists() && n < 1000 {
                p = dir.join(format!("{base}-{n}.dxf"));
                n += 1;
            }
            p
        });
        let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cadcraft_io::write_with(&snap.drawing, "recovered.dxf", &cadcraft_io::SaveOptions::default())
        }));
        let Ok(Ok(bytes)) = bytes else {
            eprintln!("cadcraft-cli: {why}: the drawing could not be written for auto-save");
            return None;
        };
        match super::files::write_file(&path, &bytes, super::files::WritePolicy { old_copies: false, overwrite: true }) {
            Ok(_) => {
                eprintln!("cadcraft-cli: {why}: unsaved work auto-saved to {}", path.display());
                {
                    let mut m = lock(&self.checkpoints);
                    let c = m.entry(snap.uid).or_default();
                    c.path = Some(path.clone());
                    c.written = Arc::as_ptr(&snap.drawing) as usize;
                    c.seen = c.written;
                    c.edits = 0;
                    c.since = None;
                }
                if why != "checkpoint" {
                    let shown = self.config.settings.roots.show(&path);
                    let e = json!({"path": shown, "reason": why, "original": snap.path, "at": rfc3339_now(), "document": snap.handle});
                    let mut r = lock(&self.recovered);
                    r.retain(|x| x["path"] != e["path"]);
                    r.push(e.clone());
                    let mut u = lock(&self.unannounced);
                    u.retain(|x| x["path"] != e["path"]);
                    u.push(e);
                }
                Some(path)
            }
            Err(e) => {
                eprintln!("cadcraft-cli: {why}: auto-save to {} failed: {}", path.display(), e.reason);
                None
            }
        }
    }

    fn autosave_all(&self, why: &str) -> Vec<PathBuf> {
        let snap = lock(&self.snapshot).clone();
        snap.iter().filter(|d| d.dirty).filter_map(|d| self.autosave(d, why)).collect()
    }

    /// After each call: count changes per drawing, forget a checkpoint once its drawing is saved.
    fn note_call(&self) {
        let snap = lock(&self.snapshot).clone();
        let mut m = lock(&self.checkpoints);
        m.retain(|uid, _| snap.iter().any(|d| d.uid == *uid));
        for s in &snap {
            let ptr = Arc::as_ptr(&s.drawing) as usize;
            let c = m.entry(s.uid).or_insert_with(|| Checkpoint {
                seen: ptr,
                written: if s.dirty { 0 } else { ptr },
                edits: u64::from(s.dirty),
                since: s.dirty.then(Instant::now),
                path: None,
            });
            if ptr != c.seen {
                c.seen = ptr;
                if s.dirty {
                    c.edits += 1;
                    c.since.get_or_insert_with(Instant::now);
                }
            }
            if !s.dirty {
                if let Some(p) = c.path.take() {
                    let shown = self.config.settings.roots.show(&p);
                    if !lock(&self.recovered).iter().any(|r| r["path"] == shown.as_str()) {
                        let _ = std::fs::remove_file(&p);
                    }
                }
                c.edits = 0;
                c.since = None;
                c.written = ptr;
            }
        }
    }

    fn checkpoint_if_due(&self) {
        let snap = lock(&self.snapshot).clone();
        for d in snap.iter().filter(|d| d.dirty) {
            let due = {
                let m = lock(&self.checkpoints);
                let n = self.config.checkpoint_edits;
                m.get(&d.uid).is_some_and(|c| {
                    Arc::as_ptr(&d.drawing) as usize != c.written
                        && c.edits > 0
                        && ((n > 0 && c.edits >= n) || self.config.checkpoint_interval.is_some_and(|i| c.since.is_some_and(|t| t.elapsed() >= i)))
                })
            };
            if due && self.autosave(d, "checkpoint").is_some() {
                self.checkpoint_writes.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn progress_fields(p: &Progress, tool: &str, done: bool) -> Value {
        let stage = if p.stage.is_empty() { tool.to_string() } else { p.stage.clone() };
        let mut stages = p.stages.clone();
        if stages.is_empty() {
            stages.push(stage.clone());
        }
        let frac = if done {
            1.0
        } else if p.total > 0 {
            ((p.done as f64 / p.total as f64).min(0.99) * 1000.0).round() / 1000.0
        } else {
            0.0
        };
        json!({"progress": frac, "stage": stage, "steps": {"done": p.done, "total": p.total}, "stages": stages})
    }

    fn job_summary(&self, n: u64, j: &Job) -> Value {
        let elapsed = j.finished.unwrap_or_else(Instant::now).duration_since(j.started).as_millis() as u64;
        let p = if j.state == JobState::Running { lock(&self.progress).clone() } else { j.progress.clone() };
        let mut v = json!({"job_id": job_name(n), "tool": j.tool, "status": state_name(&j.state), "elapsed_ms": elapsed});
        if let (Some(o), Value::Object(f)) = (v.as_object_mut(), Self::progress_fields(&p, &j.tool, j.state == JobState::Done)) {
            o.extend(f);
        }
        v
    }

    fn status(&self) -> Value {
        let snap = lock(&self.snapshot).clone();
        let busy = lock(&self.busy).clone();
        let jobs: Vec<Value> = {
            lock(&self.jobs).1.iter().filter(|(_, j)| j.handed_off || j.state == JobState::Running).map(|(n, j)| self.job_summary(*n, j)).collect()
        };
        let current = busy.map(|b| {
            let mut v = json!({"tool": b.tool, "job_id": job_name(b.job), "elapsed_ms": b.started.elapsed().as_millis() as u64, "cancelling": self.cancel.load(Ordering::Relaxed)});
            if let (Some(o), Value::Object(f)) = (v.as_object_mut(), Self::progress_fields(&lock(&self.progress), &b.tool, false)) {
                o.extend(f);
            }
            v
        });
        let cps = {
            let m = lock(&self.checkpoints);
            json!({
                "every_edits": self.config.checkpoint_edits, "every_secs": self.config.checkpoint_interval.map(|d| d.as_secs()).unwrap_or(0),
                "pending_edits": m.values().map(|c| c.edits).sum::<u64>(), "writes": self.checkpoint_writes.load(Ordering::Relaxed),
                "files": m.iter().filter_map(|(uid, c)| c.path.as_ref().map(|p| json!({"path": self.config.settings.roots.show(p), "document": format!("doc-{uid}")}))).collect::<Vec<_>>(),
            })
        };
        let s = &self.config.settings;
        let mut mem = memory();
        mem["limit_bytes"] = json!(s.max_memory_bytes);
        json!({
            "server": "cadcraft", "version": env!("CARGO_PKG_VERSION"), "protocol": crate::PROTOCOL_VERSION, "pid": std::process::id(),
            "uptime_ms": self.started.elapsed().as_millis() as u64, "backend": "headless CADCraft engine (DWG via the acadrust bridge, DXF native)", "author": s.author,
            "memory": mem, "fonts": fonts(),
            "roots": {"read": s.roots.read.as_ref().map(|p| p.display().to_string()), "write": s.roots.write.as_ref().map(|p| p.display().to_string())},
            "documents": snap.iter().map(|d| json!({"document": d.handle, "name": d.name, "path": d.path, "dirty": d.dirty, "active": d.active, "entities": d.entities, "format": d.format})).collect::<Vec<_>>(),
            "current_call": current, "jobs": jobs,
            "recovered_files": lock(&self.recovered).clone(),
            "evicted": lock(&self.evicted).iter().map(Evicted::to_value).collect::<Vec<_>>(),
            "checkpoints": cps,
            "idle_ms": lock(&self.last_activity).elapsed().as_millis() as u64,
            "calls": self.calls.load(Ordering::Relaxed), "busy_replies": self.busy_replies.load(Ordering::Relaxed),
            "formats": {"read": ["dwg (R13–2018+: AC1012–AC1032)", "dxf (R12–2018, ASCII and binary)"], "write": ["dwg (2000–2018)", "dxf (2000–2018)", "pdf", "svg", "png"]},
            "limits": {
                "max_result_bytes": s.max_result_bytes, "max_memory_mb": s.max_memory_bytes.map(|b| b / (1024 * 1024)),
                "busy_after_ms": self.config.busy_after.as_millis() as u64, "job_after_ms": self.config.job_after.as_millis() as u64,
                "open_job_bytes": self.config.open_job_bytes, "idle_evict_secs": self.config.idle_evict.map(|d| d.as_secs()), "old_copies": s.old_copies,
                "file_size": "none (DWG/DXF read as a whole; the DXF rendition of a DWG is bounded by a share of the memory budget)",
            },
        })
    }

    fn cancel_call(&self, args: &Value) -> Value {
        let asked = args.get("job_id").or(args.get("id")).or(args.get("job")).map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
        let busy = lock(&self.busy).clone();
        let target = match (&asked, busy.as_ref()) {
            (None, Some(b)) => Some(b.clone()),
            (Some(t), Some(b)) if parse_job(t) == Some(b.job) => Some(b.clone()),
            _ => None,
        };
        if let Some(b) = target {
            self.cancel.store(true, Ordering::SeqCst);
            if let Some(j) = lock(&self.jobs).1.get_mut(&b.job) {
                j.handed_off = true;
            }
            return tool_value(
                Ok(Reply::json(json!({"cancelled": true, "status": "cancelling", "job_id": job_name(b.job), "tool": b.tool, "retry_after_ms": 500,
                "next_step": format!("it stops at its next safe point and keeps nothing; job_status with job_id \"{}\" then says cancelled", job_name(b.job))}))),
                usize::MAX,
                0,
            );
        }
        let jobs = lock(&self.jobs);
        match asked {
            None => tool_value(
                Ok(Reply::json(json!({"cancelled": false, "status": "not_running", "next_step": "no call is running: nothing to stop"}))),
                usize::MAX,
                0,
            ),
            Some(t) => match parse_job(&t).and_then(|n| jobs.1.get(&n).map(|j| (n, j))) {
                Some((n, j)) => tool_value(
                    Ok(Reply::json(
                        json!({"cancelled": false, "status": state_name(&j.state), "job_id": job_name(n), "tool": j.tool, "next_step": format!("the job had ended; job_status with job_id \"{}\" has its result", job_name(n))}),
                    )),
                    usize::MAX,
                    0,
                ),
                None => {
                    let ids: Vec<String> = jobs.1.keys().map(|n| job_name(*n)).collect();
                    failure(
                        &ToolError::new(code::NOT_FOUND, format!("there is no job `{t}`"), "use a job_id a call returned; list_jobs lists the jobs")
                            .with("closest", json!(contract::closest(&t, ids.iter().map(String::as_str), 5))),
                    )
                }
            },
        }
    }

    fn list_jobs(&self) -> Value {
        let jobs = lock(&self.jobs);
        let list: Vec<Value> =
            jobs.1.iter().filter(|(_, j)| j.handed_off || j.state == JobState::Running).map(|(n, j)| self.job_summary(*n, j)).collect();
        tool_value(Ok(Reply::json(json!({"jobs": list}))), usize::MAX, 0)
    }

    fn job_status(&self, args: &Value) -> Value {
        let Some(asked) = args.get("job_id").or(args.get("id")).or(args.get("job")).filter(|v| !v.is_null()) else { return self.list_jobs() };
        let text = match asked {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let jobs = lock(&self.jobs);
        let Some((n, j)) = parse_job(&text).and_then(|n| jobs.1.get(&n).map(|j| (n, j))) else {
            let ids: Vec<String> = jobs.1.keys().map(|n| job_name(*n)).collect();
            return failure(
                &ToolError::new(code::NOT_FOUND, format!("there is no job `{text}`"), "use a job_id a call returned; list_jobs lists the jobs")
                    .with("closest", json!(contract::closest(&text, ids.iter().map(String::as_str), 5))),
            );
        };
        let mut v = self.job_summary(n, j);
        let mut pictures = Vec::new();
        match (&j.state, &j.result) {
            (JobState::Running, _) => {
                v["retry_after_ms"] = json!(retry_after(j.started.elapsed().as_millis() as u64));
                v["cancelling"] = json!(self.cancel.load(Ordering::Relaxed));
                v["next_step"] = json!(format!("the job is still running; call job_status with job_id \"{}\" again", job_name(n)));
            }
            (state, Some(r)) => {
                if r["isError"] == true {
                    v["error"] = r["structuredContent"]["error"].clone();
                    v["next_step"] = json!(if *state == JobState::Cancelled {
                        "the job was cancelled; the drawing is as it was before it"
                    } else {
                        "the job failed; see error.next_step"
                    });
                } else {
                    v["result"] = r["structuredContent"].clone();
                    v["next_step"] = json!("the job is finished; its result is in `result`");
                }
                pictures = r["content"].as_array().map(|c| c.iter().filter(|b| b["type"] != "text").cloned().collect()).unwrap_or_default();
            }
            _ => {}
        }
        let mut out = tool_value(Ok(Reply::json(v)), usize::MAX, 0);
        if let Some(c) = out["content"].as_array_mut() {
            c.extend(pictures);
        }
        out
    }

    fn progress_notification(&self) -> Option<Value> {
        let (job, token, tool, started) = {
            let b = lock(&self.busy);
            let r = b.as_ref()?;
            (r.job, r.token.clone()?, r.tool.clone(), r.started)
        };
        let p = lock(&self.progress).clone();
        let mut note = lock(&self.note);
        if note.0 != job {
            *note = (job, 0.0, None);
        }
        let now = Instant::now();
        let counted = p.total > 0 && (p.done as f64) > note.1;
        let beat = note.2.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
        if !counted && !beat {
            return None;
        }
        let value = if counted { p.done as f64 } else { note.1 + 0.001 };
        note.1 = value;
        note.2 = Some(now);
        let stage = if p.stage.is_empty() { tool } else { p.stage };
        let secs = started.elapsed().as_secs();
        let mut params = json!({"progressToken": token, "progress": value, "message": if p.total > 0 { format!("{stage}: {} of {} ({secs} s)", p.done, p.total) } else { format!("{stage} ({secs} s)") }});
        if p.total > 0 {
            params["total"] = json!(p.total);
        }
        Some(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}))
    }
}

/// Recovered files already in `dir` (an earlier process's), reason `found`.
fn find_recovered(roots: &super::files::Roots) -> Vec<Value> {
    let Some(dir) = roots.write.as_deref() else { return Vec::new() };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().contains(".recovered-") && !e.file_name().to_string_lossy().ends_with(".cctmp"))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.into_iter().map(|p| json!({"path": roots.show(&p), "reason": "found", "original": null, "at": null})).collect()
}

const INSTRUCTIONS: &str = "CADCraft reads and writes AutoCAD drawings (DWG R13–2018+, DXF R12–2018). open_drawing {path} answers a \
`document` handle. read_drawing gives the summary (units, layouts, layers, blocks, title blocks) and, by section, every text, \
dimension value, title-block attribute, entity, layer, block, layout — windowed with limit / continue_from; search_drawing finds text, \
attribute values, block, layer and layout names. render_preview draws a space as SVG (tiles, windows, layers on/off) or a PNG to look \
at. export writes DXF, DWG, PDF, SVG and PNG. edit runs many operations per call (any CADCraft command; create, change and delete \
entities, layers, blocks, attributes), all or nothing. save_drawing keeps the changes. Every error says what to do next.";

/// Serve MCP on `input` / `output` until `input` closes.
pub fn serve(config: Config, input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> std::io::Result<()> {
    let found = find_recovered(&config.settings.roots);
    let shared = Arc::new(Shared {
        out: Mutex::new(Box::new(output)),
        jobs: Mutex::new((0, BTreeMap::new())),
        busy: Mutex::new(None),
        snapshot: Mutex::new(Vec::new()),
        recovered: Mutex::new(found.clone()),
        unannounced: Mutex::new(found.clone()),
        evicted: Mutex::new(Vec::new()),
        checkpoints: Mutex::new(HashMap::new()),
        checkpoint_writes: AtomicU64::new(0),
        calls: AtomicU64::new(0),
        busy_replies: AtomicU64::new(0),
        last_activity: Mutex::new(Instant::now()),
        idle_done: AtomicBool::new(false),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(Mutex::new(Progress::default())),
        note: Mutex::new((0, 0.0, None)),
        config: config.clone(),
        started: Instant::now(),
    });
    if config.handle_signals {
        let s = shared.clone();
        if let Err(e) = ctrlc::set_handler(move || {
            s.autosave_all("signal");
            std::process::exit(143);
        }) {
            eprintln!("cadcraft-cli: signals are not handled ({e}); unsaved work is still auto-saved on eviction and hang-up");
        }
    }

    let (work_tx, work_rx) = mpsc::channel::<Work>();
    let ws = shared.clone();
    let worker = std::thread::Builder::new().name("cadcraft-worker".into()).stack_size(64 << 20).spawn(move || worker_loop(&ws, work_rx))?;

    if let Some(idle) = config.idle_evict {
        let s = shared.clone();
        let tx = work_tx.clone();
        let _ = std::thread::Builder::new().name("cadcraft-idle".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                if lock(&s.last_activity).elapsed() < idle
                    || lock(&s.busy).is_some()
                    || s.idle_done.load(Ordering::SeqCst)
                    || lock(&s.snapshot).is_empty()
                {
                    continue;
                }
                s.idle_done.store(true, Ordering::SeqCst);
                if tx.send(Work::Evict { reason: "idle".into(), done: None }).is_err() {
                    return;
                }
            }
        });
    }
    if config.checkpoint_edits > 0 || config.checkpoint_interval.is_some() {
        let s = shared.clone();
        let _ = std::thread::Builder::new().name("cadcraft-checkpoint".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if lock(&s.busy).is_none() {
                    s.checkpoint_if_due();
                }
            }
        });
    }
    {
        let s = shared.clone();
        let _ = std::thread::Builder::new().name("cadcraft-progress".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if let Some(n) = s.progress_notification() {
                    s.write(&n);
                }
            }
        });
    }

    let (line_tx, line_rx) = mpsc::channel::<Option<Vec<u8>>>();
    let _ = std::thread::Builder::new().name("cadcraft-stdin".into()).spawn(move || {
        let mut input = input;
        loop {
            let mut buf = Vec::new();
            match input.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => {
                    let _ = line_tx.send(None);
                    return;
                }
                Ok(_) => {
                    if line_tx.send(Some(buf)).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let defs = all_definitions();
    let names: Vec<String> = defs.iter().filter_map(|d| d["name"].as_str().map(str::to_string)).collect();
    while let Ok(Some(raw)) = line_rx.recv() {
        let Ok(line) = String::from_utf8(raw) else {
            shared.write(&rpc_error(
                Value::Null,
                -32700,
                &ToolError::new(code::INVALID_REQUEST, "the message is not valid UTF-8", "send UTF-8 JSON, one message per line"),
            ));
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                shared.write(&rpc_error(
                    Value::Null,
                    -32700,
                    &ToolError::new(code::INVALID_REQUEST, format!("not JSON: {e}"), "send one JSON-RPC message per line"),
                ));
                continue;
            }
        };
        if msg.is_array() {
            shared.write(&rpc_error(
                Value::Null,
                -32600,
                &ToolError::new(code::INVALID_REQUEST, "batches are not supported", "send one message per line"),
            ));
            continue;
        }
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let id = msg.get("id").cloned();
        match (method.as_str(), id) {
            ("initialize", Some(id)) => {
                let asked = msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or(crate::PROTOCOL_VERSION);
                let version = if ["2025-06-18", "2025-03-26", "2024-11-05"].contains(&asked) { asked } else { crate::PROTOCOL_VERSION };
                let rec = lock(&shared.recovered).iter().filter_map(|r| r["path"].as_str().map(str::to_string)).collect::<Vec<_>>();
                let extra = if rec.is_empty() { String::new() } else { format!(" Recovered files from an earlier session: {}.", rec.join(", ")) };
                shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "cadcraft", "title": "CADCraft", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": format!("{INSTRUCTIONS} Backend: headless CADCraft engine.{extra}"),
                }}));
            }
            ("ping", Some(id)) => shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            ("tools/list", Some(id)) => shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": {"tools": defs}})),
            ("resources/list", Some(id)) => shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": {"resources": []}})),
            ("prompts/list", Some(id)) => shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": {"prompts": []}})),
            ("tools/call", Some(id)) => {
                let Some(asked) = msg.pointer("/params/name").and_then(Value::as_str).map(str::to_string) else {
                    shared.write(&rpc_error(
                        id,
                        -32602,
                        &ToolError::new(code::INVALID_REQUEST, "tools/call without a tool `name`", "send params: {name, arguments}"),
                    ));
                    continue;
                };
                let args = msg.pointer("/params/arguments").cloned().unwrap_or(json!({}));
                if !args.is_object() {
                    shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": failure(&ToolError::new(code::INVALID_ARGUMENT, "`arguments` must be an object", "send arguments: {…}"))}));
                    continue;
                }
                match contract::normalize_tool_name(&asked, &names) {
                    Some(name) if CONTROL_TOOLS.contains(&name.as_str()) => {
                        let r = match name.as_str() {
                            "server_status" => tool_value(Ok(Reply::json(shared.status())), usize::MAX, 0),
                            "cancel" => {
                                let mut r = shared.cancel_call(&args);
                                if args.get("queued").is_some_and(|q| q == true) {
                                    r["structuredContent"]["dropped_from_queue"] = json!(0);
                                }
                                r
                            }
                            "list_jobs" => shared.list_jobs(),
                            _ => shared.job_status(&args),
                        };
                        shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": r}));
                    }
                    Some(name) => {
                        let schema = defs.iter().find(|d| d["name"] == name.as_str()).map(|d| d["inputSchema"].clone()).unwrap_or(json!({}));
                        let obj = args.as_object().cloned().unwrap_or_default();
                        let (mut fitted, warnings) = contract::normalize_args(&schema, &obj);
                        for k in contract::GLOBAL_ARGS {
                            if let Some(v) = obj.get(*k) {
                                fitted.insert((*k).to_string(), v.clone());
                            }
                        }
                        let token = msg.pointer("/params/_meta/progressToken").filter(|t| t.is_string() || t.is_number()).cloned();
                        submit(&shared, &work_tx, id, &name, Value::Object(fitted), warnings, token);
                    }
                    None => {
                        let near = contract::closest(&asked, names.iter().map(String::as_str), 3);
                        let e =
                            ToolError::new(code::NOT_FOUND, format!("unknown tool `{asked}`"), "call tools/list; the closest tools are in `closest`")
                                .with("closest", json!(near));
                        shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": failure(&e)}));
                    }
                }
            }
            ("notifications/cancelled", None) => {
                let asked = msg.pointer("/params/requestId").cloned().unwrap_or(Value::Null);
                let busy = lock(&shared.busy).clone();
                if let Some(b) = busy.filter(|b| !asked.is_null() && b.id == asked)
                    && let Some(j) = lock(&shared.jobs).1.get_mut(&b.job)
                {
                    j.handed_off = true;
                    j.client_cancelled = true;
                }
            }
            (_, None) => {}
            (m, Some(id)) => {
                shared.write(&rpc_error(
                    id,
                    -32601,
                    &ToolError::new(code::NOT_FOUND, format!("method `{m}` not found"), "use initialize, tools/list, tools/call or ping"),
                ));
            }
        }
    }
    // The client hung up: let the running call finish (for a while), then keep unsaved work.
    let deadline = Instant::now() + Duration::from_secs(60);
    while lock(&shared.busy).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(work_tx);
    if lock(&shared.busy).is_none() {
        let _ = worker.join();
    }
    shared.autosave_all("disconnect");
    Ok(())
}

fn rpc_error(id: Value, c: i64, e: &ToolError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": c, "message": e.text(), "data": e.to_value()}})
}

fn starts_as_job(shared: &Shared, name: &str, args: &Value) -> bool {
    match args.get("async") {
        Some(Value::Bool(b)) => return *b,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => return true,
        _ => {}
    }
    let key = match name {
        "open_drawing" => "path",
        "read_drawing" | "render_preview" | "search_drawing" | "roundtrip_check" if args.get("document").is_none() => "path",
        "export" if args.get("document").is_none() => "source",
        _ => return false,
    };
    let Some(p) = args.get(key).and_then(Value::as_str) else { return false };
    shared
        .config
        .settings
        .roots
        .resolve(p, super::files::Access::Read)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.len() >= shared.config.open_job_bytes)
}

fn submit(shared: &Arc<Shared>, work_tx: &mpsc::Sender<Work>, id: Value, name: &str, args: Value, warnings: Vec<String>, token: Option<Value>) {
    shared.touch();
    let grace = Instant::now() + shared.config.busy_after;
    while lock(&shared.busy).is_some() && Instant::now() < grace {
        std::thread::sleep(Duration::from_millis(5));
    }
    let job = {
        let mut busy = lock(&shared.busy);
        if let Some(r) = busy.as_ref() {
            let elapsed = r.started.elapsed().as_millis() as u64;
            let e = ToolError::new(
                code::BUSY,
                format!("another call is running ({} for {elapsed} ms, {})", r.tool, job_name(r.job)),
                format!("retry after {} ms; server_status and job_status answer meanwhile", retry_after(elapsed)),
            )
            .with("retry_after_ms", json!(retry_after(elapsed)))
            .with("job_id", json!(job_name(r.job)))
            .with("running", json!(r.tool));
            drop(busy);
            shared.busy_replies.fetch_add(1, Ordering::Relaxed);
            shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": failure(&e)}));
            return;
        }
        let mut jobs = lock(&shared.jobs);
        jobs.0 += 1;
        let n = jobs.0;
        jobs.1.insert(
            n,
            Job {
                tool: name.to_string(),
                state: JobState::Running,
                started: Instant::now(),
                finished: None,
                result: None,
                handed_off: false,
                client_cancelled: false,
                progress: Progress::default(),
            },
        );
        while jobs.1.len() > 200 {
            let Some(old) = jobs.1.iter().find(|(_, j)| j.state != JobState::Running).map(|(k, _)| *k) else { break };
            jobs.1.remove(&old);
        }
        *busy = Some(Running { tool: name.to_string(), job: n, started: Instant::now(), id: id.clone(), token });
        shared.calls.fetch_add(1, Ordering::Relaxed);
        n
    };
    let as_job = starts_as_job(shared, name, &args);
    let (done_tx, done_rx) = mpsc::channel::<Value>();
    let mut args = args;
    if !warnings.is_empty() {
        args["__warnings"] = json!(warnings);
    }
    let work = if name == "evict" {
        Work::Evict { reason: args.get("reason").and_then(Value::as_str).unwrap_or("evict").to_string(), done: Some(done_tx) }
    } else {
        Work::Call { tool: name.to_string(), args, job, done: done_tx }
    };
    if work_tx.send(work).is_err() {
        *lock(&shared.busy) = None;
        let saved = shared.autosave_all("panic");
        let mut e = ToolError::new(code::INTERNAL, "the drawing worker stopped", "restart the server and open the recovered file");
        if let Some(p) = saved.first() {
            e = e.with("recovered_path", json!(shared.config.settings.roots.show(p)));
        }
        shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": failure(&e)}));
        return;
    }
    if as_job {
        if let Some(j) = lock(&shared.jobs).1.get_mut(&job) {
            j.handed_off = true;
        }
        handoff(shared, id, job, name, "long work runs as a job");
        return;
    }
    let s = shared.clone();
    let tool = name.to_string();
    let timeout = if name == "evict" { Duration::from_secs(3600) } else { shared.config.job_after };
    let _ = std::thread::Builder::new().name("cadcraft-wait".into()).spawn(move || match done_rx.recv_timeout(timeout) {
        Ok(result) => {
            let mut jobs = lock(&s.jobs);
            let quiet = jobs.1.get(&job).is_some_and(|j| j.client_cancelled);
            if !jobs.1.get(&job).is_some_and(|j| j.handed_off) {
                jobs.1.remove(&job);
            }
            drop(jobs);
            if !quiet {
                let mut reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                s.announce(&mut reply);
                s.write(&reply);
            }
        }
        Err(_) => {
            let mut jobs = lock(&s.jobs);
            match jobs.1.get(&job).and_then(|j| j.result.clone()) {
                Some(r) => {
                    jobs.1.remove(&job);
                    drop(jobs);
                    let mut reply = json!({"jsonrpc": "2.0", "id": id, "result": r});
                    s.announce(&mut reply);
                    s.write(&reply);
                }
                None => {
                    let quiet = jobs.1.get(&job).is_some_and(|j| j.client_cancelled);
                    if let Some(j) = jobs.1.get_mut(&job) {
                        j.handed_off = true;
                    }
                    drop(jobs);
                    if !quiet {
                        handoff(&s, id, job, &tool, "the call is taking longer than --job-after-ms; it carries on as a job");
                    }
                }
            }
        }
    });
}

fn handoff(shared: &Shared, id: Value, job: u64, name: &str, why: &str) {
    let v = json!({"job_id": job_name(job), "status": "running", "tool": name, "reason": why, "retry_after_ms": 1000,
        "next_step": format!("poll job_status with job_id \"{}\" until status is not \"running\"; the result will be there", job_name(job))});
    shared.write(&json!({"jsonrpc": "2.0", "id": id, "result": tool_value(Ok(Reply::json(v)), usize::MAX, 0)}));
}

/// Publish the drawings' state for the stdio side.
fn publish(shared: &Shared, engine: &Engine) {
    let active = engine.docs.active.clone();
    let snap: Vec<DocSnap> = engine
        .docs
        .list
        .iter()
        .filter_map(|d| {
            let st = d.session.state().ok()?;
            Some(DocSnap {
                handle: d.handle.clone(),
                uid: d.uid,
                name: d.name(),
                path: d.source.as_ref().map(|p| shared.config.settings.roots.show(p)),
                dirty: d.dirty(),
                active: active.as_deref() == Some(d.handle.as_str()),
                entities: st.doc.entity_count(),
                format: d.format.clone(),
                drawing: st.doc.clone(),
            })
        })
        .collect();
    *lock(&shared.snapshot) = snap;
    shared.note_call();
}

/// Auto-save the dirty drawings, then close them all; what each was is remembered for the
/// calls that name it.
fn evict(shared: &Shared, engine: &mut Engine, reason: &str) -> Vec<Evicted> {
    publish(shared, engine);
    let snap = lock(&shared.snapshot).clone();
    let mut out = Vec::new();
    for d in &snap {
        let recovered = if d.dirty { shared.autosave(d, if reason == "idle" || reason == "memory" { reason } else { "evict" }) } else { None };
        out.push(Evicted {
            handle: d.handle.clone(),
            name: d.name.clone(),
            reason: reason.to_string(),
            recovered_path: recovered.map(|p| shared.config.settings.roots.show(&p)),
            original_path: d.path.clone(),
        });
    }
    engine.docs.list.clear();
    engine.docs.active = None;
    engine.docs.evicted = out.clone();
    *lock(&shared.evicted) = out.clone();
    publish(shared, engine);
    out
}

fn worker_loop(shared: &Shared, rx: mpsc::Receiver<Work>) {
    let mut engine = Engine::default();
    while let Ok(work) = rx.recv() {
        match work {
            Work::Evict { reason, done } => {
                let list = evict(shared, &mut engine, &reason);
                *lock(&shared.busy) = None;
                if let Some(d) = done {
                    let _ = d.send(tool_value(
                        Ok(Reply::json(json!({"evicted": list.iter().map(Evicted::to_value).collect::<Vec<_>>()}))),
                        usize::MAX,
                        0,
                    ));
                }
            }
            Work::Call { tool, args, job, done } => {
                shared.cancel.store(false, Ordering::SeqCst);
                *lock(&shared.progress) = Progress { stage: tool.clone(), ..Progress::default() };
                let ctx = Ctx {
                    settings: Arc::new(shared.config.settings.clone()),
                    cancel: shared.cancel.clone(),
                    progress: shared.progress.clone(),
                    caller: args.get("caller").and_then(Value::as_str).map(str::to_string),
                };
                let max = args
                    .get("max_bytes")
                    .and_then(Value::as_u64)
                    .map_or(shared.config.settings.max_result_bytes, |b| usize::try_from(b).unwrap_or(usize::MAX))
                    .max(1024);
                let from = match args.get("continue_from") {
                    Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
                    Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
                    _ => 0,
                };
                let mut a = args.clone();
                a["continue_from"] = json!(from);
                let warnings = a.as_object_mut().and_then(|o| o.remove("__warnings"));
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.call(&tool, &a, &ctx)));
                let r = match r {
                    Ok(r) => r,
                    Err(_) => {
                        // An edit the panic stopped part way is undone first: the live drawing and
                        // the recovered file are the drawing as it was before the call.
                        let undone = engine.undo_interrupted_call();
                        publish(shared, &engine);
                        let saved = shared.autosave_all("panic");
                        let mut e = ToolError::new(
                            code::INTERNAL,
                            format!("{tool} failed inside the engine"),
                            if undone {
                                "nothing of this call was applied: the drawing is as it was before it (also auto-saved as the recovered file) — send it again (fixed), or with other arguments"
                            } else {
                                "the drawing may hold part of this call (it is auto-saved as the recovered file): inspect it before going on, then send what is missing (fixed), or with other arguments"
                            },
                        )
                        .with("rolled_back", json!(undone));
                        if let Some(p) = saved.first() {
                            e = e.with("recovered_path", json!(shared.config.settings.roots.show(p)));
                        }
                        Err(e)
                    }
                };
                let state = match &r {
                    Ok(_) => JobState::Done,
                    Err(e) if e.code == code::CANCELLED => JobState::Cancelled,
                    Err(_) => JobState::Failed,
                };
                let mut result = tool_value(r, max, usize::try_from(from).unwrap_or(0));
                if let Some(Value::Array(w)) = warnings
                    && !w.is_empty()
                {
                    if let Some(sc) = result["structuredContent"].as_object_mut() {
                        sc.insert("warnings".into(), Value::Array(w.clone()));
                    }
                    if let Some(c) = result["content"].as_array_mut() {
                        c.push(json!({"type": "text", "text": format!("warnings: {}", w.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "))}));
                    }
                }
                publish(shared, &engine);
                {
                    let mut jobs = lock(&shared.jobs);
                    if let Some(j) = jobs.1.get_mut(&job) {
                        j.state = state;
                        j.finished = Some(Instant::now());
                        j.result = Some(result.clone());
                        j.progress = lock(&shared.progress).clone();
                    }
                }
                *lock(&shared.busy) = None;
                let _ = done.send(result);
                // Memory over the budget after a call: release (auto-save, close) while idle.
                if let (Some(limit), Some(now)) = (shared.config.settings.max_memory_bytes, rss())
                    && now > limit
                    && !engine.docs.list.is_empty()
                {
                    evict(shared, &mut engine, "memory");
                }
            }
        }
    }
    publish(shared, &engine);
}
