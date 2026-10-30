//! WHIQ: where files are read from and written to (`--automation-read-root`,
//! `--automation-write-root`), and how a write replaces a file: temp + fsync + rename, with the
//! file it replaces kept as `OLD_<name>` unless the server runs with `--automation-no-old-copies`
//! or the call says `overwrite: true` (CONTRACT-PARITY.md, "OLD_ rename" and "Roots").

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::contract::{ToolError, code};

/// What a path is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The two roots. Without a root, paths are the caller's (relative to the working folder).
#[derive(Clone, Debug, Default)]
pub struct Roots {
    pub read: Option<PathBuf>,
    pub write: Option<PathBuf>,
}

/// `a/./b/../c` → `a/c` without touching the disk (for paths that do not exist yet).
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The path with links resolved as far as it exists (a file not written yet: its folder's).
fn real(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    let lex = lexical(p);
    match (lex.parent(), lex.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => real(dir).join(name),
        _ => lex,
    }
}

impl Roots {
    fn canon(root: &Option<PathBuf>) -> Option<PathBuf> {
        root.as_ref().map(|r| real(r))
    }

    fn outside(&self, shown: &str) -> ToolError {
        ToolError::new(
            code::OUTSIDE_ROOT,
            format!("`{shown}` is outside the folders this server may use"),
            "use a path inside the write root (relative names are resolved there) or a file staged in the read root",
        )
        .with("read_root", json!(self.read.as_ref().map(|p| p.display().to_string())))
        .with("write_root", json!(self.write.as_ref().map(|p| p.display().to_string())))
    }

    /// Resolve `path` for reading or writing. Relative paths: reads look in the read root, then
    /// the write root; writes go into the write root. The result must lie inside a root (both for
    /// reads, the write root for writes); links are followed, so a link out of a root is refused.
    pub fn resolve(&self, path: &str, access: Access) -> Result<PathBuf, ToolError> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(ToolError::new(code::INVALID_ARGUMENT, "the path is empty", "give a file name, e.g. \"plan.dwg\""));
        }
        let p = Path::new(trimmed);
        let candidates: Vec<PathBuf> = if p.is_absolute() {
            vec![p.to_path_buf()]
        } else {
            match access {
                Access::Read => [&self.read, &self.write].iter().filter_map(|r| r.as_ref().map(|r| r.join(p))).collect(),
                Access::Write => self.write.iter().map(|r| r.join(p)).collect(),
            }
        };
        let candidates = if candidates.is_empty() { vec![p.to_path_buf()] } else { candidates };
        let allowed: Vec<PathBuf> = match access {
            Access::Read => [Self::canon(&self.read), Self::canon(&self.write)].into_iter().flatten().collect(),
            Access::Write => Self::canon(&self.write).into_iter().collect(),
        };
        let unrestricted = self.read.is_none() && self.write.is_none();
        // A write with only a read root given: refused (nowhere to write).
        if access == Access::Write && self.write.is_none() && !unrestricted {
            return Err(self.outside(trimmed));
        }
        let mut first_ok = None;
        for c in &candidates {
            let r = real(c);
            if unrestricted || allowed.iter().any(|a| r.starts_with(a)) {
                if access == Access::Write || r.exists() {
                    return Ok(r);
                }
                first_ok.get_or_insert(r);
            }
        }
        match first_ok {
            Some(r) => Ok(r),
            None => Err(self.outside(trimmed)),
        }
    }

    /// `path` as shown to the agent: relative to the root it lies in.
    pub fn show(&self, path: &Path) -> String {
        for r in [Self::canon(&self.write), Self::canon(&self.read)].into_iter().flatten() {
            if let Ok(rel) = path.strip_prefix(&r) {
                return rel.display().to_string().replace('\\', "/");
            }
        }
        path.display().to_string()
    }
}

/// How a write treats a file already at its path.
#[derive(Clone, Copy, Debug)]
pub struct WritePolicy {
    /// Keep the replaced file as `OLD_<name>` (the default; off with `--automation-no-old-copies`).
    pub old_copies: bool,
    /// The call's `overwrite: true`: replace without a copy.
    pub overwrite: bool,
}

/// The free `OLD_<name>`, `OLD_2_<name>`, … beside `path`.
fn old_name(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    (1..10_000).map(|n| if n == 1 { dir.join(format!("OLD_{name}")) } else { dir.join(format!("OLD_{n}_{name}")) }).find(|p| !p.exists())
}

/// Write `bytes` to `path` atomically (a temporary file in the same folder, flushed to disk, then
/// renamed over it). Returns `{path, previous}` when an earlier file was kept as an OLD_ copy.
pub fn write_file(path: &Path, bytes: &[u8], policy: WritePolicy) -> Result<Option<(PathBuf, PathBuf)>, ToolError> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, &e))?;
    let mut kept = None;
    if path.exists()
        && policy.old_copies
        && !policy.overwrite
        && let Some(old) = old_name(path)
    {
        std::fs::rename(path, &old).map_err(|e| io_error(path, &e))?;
        kept = Some((path.to_path_buf(), old));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let mut tmp = None;
    for n in 0..1000u32 {
        let t = dir.join(format!(".{name}.{}-{n}.cctmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&t) {
            Ok(f) => {
                tmp = Some((t, f));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(&t, &e)),
        }
    }
    let Some((tmp_path, mut f)) = tmp else {
        return Err(ToolError::new(code::FAILED, format!("no temporary file could be made in {}", dir.display()), "free the folder and retry"));
    };
    let written = f.write_all(bytes).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(io_error(path, &e));
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(io_error(path, &e));
    }
    Ok(kept)
}

/// A file-system failure in the contract's words (`disk_full`, `locked`, `failed`).
pub fn io_error(path: &Path, e: &std::io::Error) -> ToolError {
    let shown = path.display();
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("no space") || lower.contains("disk full") || lower.contains("quota") {
        ToolError::new(code::DISK_FULL, format!("{shown}: {text}"), "free disk space and retry")
    } else if e.kind() == std::io::ErrorKind::PermissionDenied || lower.contains("being used by another process") {
        ToolError::new(code::LOCKED, format!("{shown}: {text}"), "close the program holding the file, or write to another name")
    } else if e.kind() == std::io::ErrorKind::NotFound {
        ToolError::new(code::NOT_FOUND, format!("{shown}: {text}"), "check the name; list the folder or stage the file first")
    } else {
        ToolError::new(code::FAILED, format!("{shown}: {text}"), "check the path and retry")
    }
}

/// The `saved` entry of an answer, with the OLD_ copy when one was kept.
pub fn saved_entry(roots: &Roots, path: &Path, bytes: usize, format: &str, kept: Option<&(PathBuf, PathBuf)>) -> Value {
    let mut v = json!({"path": roots.show(path), "bytes": bytes, "format": format});
    if let Some((p, old)) = kept {
        v["previous_renamed_to"] = json!(roots.show(old));
        v["replaced"] = json!(roots.show(p));
    }
    v
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cadcraft-whiq-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("in")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        d
    }

    #[test]
    fn roots_confine_reads_and_writes() {
        let d = tmp("roots");
        std::fs::write(d.join("in/a.dxf"), b"x").unwrap();
        let roots = Roots { read: Some(d.join("in")), write: Some(d.join("out")) };
        assert!(roots.resolve("a.dxf", Access::Read).unwrap().ends_with("in/a.dxf"));
        assert!(roots.resolve("b.pdf", Access::Write).unwrap().ends_with("out/b.pdf"));
        let e = roots.resolve("../in/c.pdf", Access::Write).unwrap_err();
        assert_eq!(e.code, code::OUTSIDE_ROOT);
        assert_eq!(roots.resolve("/etc/passwd", Access::Read).unwrap_err().code, code::OUTSIDE_ROOT);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", d.join("out/escape")).unwrap();
            assert_eq!(roots.resolve("escape/passwd", Access::Read).unwrap_err().code, code::OUTSIDE_ROOT);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn writes_keep_old_copies_unless_told() {
        let d = tmp("old");
        let p = d.join("out/a.pdf");
        assert!(write_file(&p, b"1", WritePolicy { old_copies: true, overwrite: false }).unwrap().is_none());
        let kept = write_file(&p, b"2", WritePolicy { old_copies: true, overwrite: false }).unwrap().unwrap();
        assert!(kept.1.ends_with("OLD_a.pdf"));
        let kept = write_file(&p, b"3", WritePolicy { old_copies: true, overwrite: false }).unwrap().unwrap();
        assert!(kept.1.ends_with("OLD_2_a.pdf"));
        assert!(write_file(&p, b"4", WritePolicy { old_copies: false, overwrite: false }).unwrap().is_none());
        assert!(write_file(&p, b"5", WritePolicy { old_copies: true, overwrite: true }).unwrap().is_none());
        assert_eq!(std::fs::read(&p).unwrap(), b"5");
        assert_eq!(std::fs::read(d.join("out/OLD_a.pdf")).unwrap(), b"1");
        let leftovers =
            std::fs::read_dir(d.join("out")).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".cctmp")).count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
