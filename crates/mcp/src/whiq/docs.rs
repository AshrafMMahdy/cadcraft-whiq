//! WHIQ: the drawings the server holds (one headless engine session each, named by a `doc-<n>`
//! handle), the call context (cancel flag, progress) and the settings every tool reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cadcraft_engine::Session;
use serde_json::{Value, json};

use super::contract::{ToolError, closest, code};
use super::files::{Access, Roots};

/// What every tool reads from the server's flags.
#[derive(Clone, Debug)]
pub struct Settings {
    pub roots: Roots,
    /// `--automation-no-old-copies` off: a write over a file keeps it as `OLD_<name>`.
    pub old_copies: bool,
    /// `--author`: stamped on saves (`$LASTSAVEDBY`) when the call names no `caller`.
    pub author: Option<String>,
    /// `--max-result-bytes`: answers above this are trimmed with `continue_from`.
    pub max_result_bytes: usize,
    /// `--max-memory-mb` in bytes: the budget in-memory limits are shares of.
    pub max_memory_bytes: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { roots: Roots::default(), old_copies: true, author: None, max_result_bytes: 262_144, max_memory_bytes: None }
    }
}

/// How far the running call is: `stage`, steps `done` of `total` (0 = not counted), the stages
/// passed (current last). Read by `job_status`, `server_status` and progress notifications.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub stage: String,
    pub done: u64,
    pub total: u64,
    pub stages: Vec<String>,
}

/// A poisoned lock still holds usable data: a panic elsewhere must not stop the server.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a tool call can see besides its arguments.
#[derive(Clone)]
pub struct Ctx {
    pub settings: Arc<Settings>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Mutex<Progress>>,
    /// The call's `caller` (else `--author`).
    pub caller: Option<String>,
}

impl Ctx {
    pub fn new(settings: Settings) -> Self {
        Ctx { settings: Arc::new(settings), cancel: Arc::new(AtomicBool::new(false)), progress: Arc::default(), caller: None }
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// `Err(cancelled)` once `cancel` asked this call to stop.
    pub fn check(&self) -> Result<(), ToolError> {
        if self.cancelled() {
            return Err(ToolError::new(code::CANCELLED, "the call was stopped as asked", "nothing was changed; run it again when you need it"));
        }
        Ok(())
    }
    /// A new stage of `total` steps (0: not counted).
    pub fn stage(&self, stage: &str, total: u64) {
        let mut p = lock(&self.progress);
        p.stage = stage.to_string();
        p.done = 0;
        p.total = total;
        if p.stages.last().is_none_or(|s| s != stage) {
            p.stages.push(stage.to_string());
        }
    }
    pub fn step(&self, n: u64) {
        let mut p = lock(&self.progress);
        p.done = p.done.saturating_add(n);
    }
    pub fn roots(&self) -> &Roots {
        &self.settings.roots
    }
    pub fn resolve(&self, path: &str, access: Access) -> Result<PathBuf, ToolError> {
        self.settings.roots.resolve(path, access)
    }
}

/// One open drawing.
pub struct Doc {
    pub handle: String,
    pub uid: u64,
    pub session: Session,
    /// The file it was opened from (absolute), if any.
    pub source: Option<PathBuf>,
    /// `dwg`, `dxf` or `new`.
    pub format: String,
    /// The file's version code (`AC1032`…), when read from one.
    pub version: Option<String>,
    pub source_bytes: u64,
}

impl Doc {
    pub fn drawing(&self) -> Result<&cadcraft_engine::doc::Drawing, ToolError> {
        self.session.doc().map_err(engine_error)
    }
    pub fn dirty(&self) -> bool {
        self.session.state().is_ok_and(cadcraft_engine::DocState::is_dirty)
    }
    pub fn name(&self) -> String {
        self.session.state().map(|s| s.title.clone()).unwrap_or_default()
    }
    /// The answer's `document` block.
    pub fn info(&self, roots: &Roots) -> Value {
        json!({
            "document": self.handle,
            "name": self.name(),
            "path": self.source.as_deref().map(|p| roots.show(p)),
            "format": self.format,
            "version": self.version,
            "dirty": self.dirty(),
        })
    }
}

/// A drawing the server closed on its own (eviction), named on the calls that need it.
#[derive(Clone, Debug)]
pub struct Evicted {
    pub handle: String,
    pub name: String,
    pub reason: String,
    pub recovered_path: Option<String>,
    pub original_path: Option<String>,
}

impl Evicted {
    pub fn to_value(&self) -> Value {
        json!({"document": self.handle, "name": self.name, "reason": self.reason, "recovered_path": self.recovered_path, "original_path": self.original_path})
    }
}

/// The open drawings; one is active (the last opened or named).
#[derive(Default)]
pub struct Docs {
    pub list: Vec<Doc>,
    pub active: Option<String>,
    next: u64,
    pub evicted: Vec<Evicted>,
}

/// An engine error as a tool error (the engine's message is the reason).
pub fn engine_error(e: cadcraft_engine::EngineError) -> ToolError {
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    let c = if lower.contains("no drawing") || lower.contains("no document") {
        code::NO_DOCUMENT
    } else if lower.contains("unknown command") {
        code::NOT_FOUND
    } else {
        code::INVALID_ARGUMENT
    };
    ToolError::new(c, text, "fix the argument the message names (list_commands in `edit` shows each command's parameters) and retry")
}

impl Docs {
    /// Add a session holding one drawing; it becomes active.
    pub fn add(&mut self, session: Session, source: Option<PathBuf>, format: &str, version: Option<String>, source_bytes: u64) -> &mut Doc {
        self.next += 1;
        let handle = format!("doc-{}", self.next);
        let uid = self.next;
        self.evicted.clear();
        self.active = Some(handle.clone());
        self.list.push(Doc { handle, uid, session, source, format: format.to_string(), version, source_bytes });
        let i = self.list.len() - 1;
        &mut self.list[i]
    }

    fn handles(&self) -> Vec<String> {
        self.list.iter().map(|d| d.handle.clone()).collect()
    }

    /// The index the `document` argument names (a handle, its number, or the file name), else
    /// the active drawing.
    pub fn index(&self, args: &Value) -> Result<usize, ToolError> {
        let asked = match args.get("document") {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Some(Value::Number(n)) => Some(format!("doc-{n}")),
            _ => None,
        };
        match asked {
            Some(a) => {
                let want = if a.parse::<u64>().is_ok() { format!("doc-{a}") } else { a.clone() };
                let found = self.list.iter().position(|d| d.handle.eq_ignore_ascii_case(&want)).or_else(|| {
                    self.list.iter().position(|d| {
                        d.name().eq_ignore_ascii_case(&a)
                            || d.source.as_deref().and_then(Path::file_name).is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&a))
                    })
                });
                if let Some(i) = found {
                    return Ok(i);
                }
                if let Some(e) = self.evicted.iter().find(|e| e.handle.eq_ignore_ascii_case(&want)) {
                    return Err(evicted_error(e));
                }
                let open = self.handles();
                Err(ToolError::new(
                    code::NOT_FOUND,
                    format!("there is no open drawing `{a}`"),
                    "use a `document` handle from open_drawing / new_drawing (server_status lists them)",
                )
                .with("open_documents", json!(open))
                .with("closest", json!(closest(&a, open.iter().map(String::as_str), 3))))
            }
            None => {
                let i = self.active.as_ref().and_then(|h| self.list.iter().position(|d| &d.handle == h)).or(if self.list.is_empty() {
                    None
                } else {
                    Some(self.list.len() - 1)
                });
                match i {
                    Some(i) => Ok(i),
                    None => match self.evicted.first() {
                        Some(e) => Err(evicted_error(e)),
                        None => Err(ToolError::new(code::NO_DOCUMENT, "no drawing is open", "open_drawing {path} (DWG or DXF) or new_drawing first")),
                    },
                }
            }
        }
    }

    pub fn get(&mut self, args: &Value) -> Result<&mut Doc, ToolError> {
        let i = self.index(args)?;
        let d = &mut self.list[i];
        self.active = Some(d.handle.clone());
        Ok(d)
    }

    /// The drawing opened from `path` already (same file), if any.
    pub fn find_source(&self, path: &Path) -> Option<usize> {
        self.list.iter().position(|d| d.source.as_deref() == Some(path) && !d.dirty())
    }

    pub fn remove(&mut self, i: usize) -> Doc {
        let d = self.list.remove(i);
        if self.active.as_deref() == Some(d.handle.as_str()) {
            self.active = self.list.last().map(|d| d.handle.clone());
        }
        d
    }
}

fn evicted_error(e: &Evicted) -> ToolError {
    let next = match (&e.recovered_path, &e.original_path) {
        (Some(r), _) => format!("open_drawing {{path: \"{r}\"}} to continue the unsaved work (it was auto-saved there)"),
        (None, Some(o)) => format!("open_drawing {{path: \"{o}\"}} again (nothing unsaved was lost)"),
        _ => "new_drawing or open_drawing to start again".to_string(),
    };
    ToolError::new(code::EVICTED, format!("the drawing {} was closed by the server ({})", e.handle, e.reason), next)
        .with("recovered_path", json!(e.recovered_path))
        .with("original_path", json!(e.original_path))
}

/// `20261017-184512` (UTC), the recovered-file stamp every craft app uses.
pub fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    chrono::DateTime::from_timestamp(i64::try_from(secs).unwrap_or(0), 0).map(|t| t.format("%Y%m%d-%H%M%S").to_string()).unwrap_or_default()
}

/// `2026-10-17T18:45:12Z` (UTC).
pub fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    chrono::DateTime::from_timestamp(i64::try_from(secs).unwrap_or(0), 0).map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default()
}
