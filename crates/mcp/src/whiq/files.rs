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
        let mut escaped = false;
        for c in &candidates {
            let r = real(c);
            if unrestricted || allowed.iter().any(|a| r.starts_with(a)) {
                if access == Access::Write || r.exists() {
                    return Ok(r);
                }
                first_ok.get_or_insert(r);
            } else {
                escaped = true;
            }
        }
        match first_ok {
            // Nothing inside the roots exists there, and the name reaches outside (a link out of a
            // root): that is the answer, not "not found".
            Some(_) if escaped && access == Access::Read => Err(self.outside(trimmed)),
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

/// The free `OLD_<name>`, `OLD_2_<name>`, … beside `path`: the search has no end (an earlier file
/// is never overwritten for want of a name). A dangling link counts as taken. Only a path with no
/// file name has none, and that call is refused with the earlier file untouched.
pub fn old_name(path: &Path) -> Result<PathBuf, ToolError> {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Err(ToolError::new(
            code::INVALID_ARGUMENT,
            format!("{}: there is no file name to keep an OLD_ copy under", path.display()),
            "give a file name, or overwrite: true to replace without a copy",
        ));
    };
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let mut n = 1u64;
    loop {
        let p = if n == 1 { dir.join(format!("OLD_{name}")) } else { dir.join(format!("OLD_{n}_{name}")) };
        if std::fs::symlink_metadata(&p).is_err() {
            return Ok(p);
        }
        n += 1;
    }
}

/// Why a [`write_file_with`] filler stopped: the call's own error (a cancel, a bad argument) or a
/// failed write to the file.
pub enum WriteErr {
    Tool(ToolError),
    Io(std::io::Error),
}

impl From<ToolError> for WriteErr {
    fn from(e: ToolError) -> Self {
        WriteErr::Tool(e)
    }
}

impl From<std::io::Error> for WriteErr {
    fn from(e: std::io::Error) -> Self {
        WriteErr::Io(e)
    }
}

/// Removes the temporary file unless the write finished (also when the filler panics).
struct TmpFile(Option<PathBuf>);

impl Drop for TmpFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Write `bytes` to `path` atomically (a temporary file in the same folder, flushed to disk, then
/// renamed over it). Returns `{path, previous}` when an earlier file was kept as an OLD_ copy.
pub fn write_file(path: &Path, bytes: &[u8], policy: WritePolicy) -> Result<Option<(PathBuf, PathBuf)>, ToolError> {
    write_file_with(path, policy, |w| w.write_all(bytes).map_err(WriteErr::from)).map(|(kept, ())| kept)
}

/// [`write_file`] for content produced as it is written: `fill` writes it to a buffered writer on
/// the temporary file, so a huge file is never held whole in memory. The guarantees are the
/// same: nothing is at `path` until `fill` and the flush to disk have finished, an earlier file
/// is renamed to its OLD_ copy only then, and a failed or stopped call leaves no temporary file
/// and the earlier file where it was. Returns what `fill` returned beside the OLD_ copy.
pub fn write_file_with<T>(
    path: &Path,
    policy: WritePolicy,
    fill: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<T, WriteErr>,
) -> Result<(Option<(PathBuf, PathBuf)>, T), ToolError> {
    write_file_renaming(path, policy, fill, &|from, to| std::fs::rename(from, to))
}

/// [`write_file_with`] with the rename as a parameter (a test makes it fail).
fn write_file_renaming<T>(
    path: &Path,
    policy: WritePolicy,
    fill: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<T, WriteErr>,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(Option<(PathBuf, PathBuf)>, T), ToolError> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, &e))?;
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
    let Some((tmp_path, f)) = tmp else {
        return Err(ToolError::new(code::FAILED, format!("no temporary file could be made in {}", dir.display()), "free the folder and retry"));
    };
    let mut guard = TmpFile(Some(tmp_path.clone()));
    let mut w = std::io::BufWriter::new(f);
    let made = match fill(&mut w) {
        Ok(v) => v,
        Err(WriteErr::Tool(e)) => return Err(e),
        Err(WriteErr::Io(e)) => return Err(io_error(path, &e)),
    };
    let f = w.into_inner().map_err(|e| io_error(path, e.error()))?;
    f.sync_all().map_err(|e| io_error(path, &e))?;
    drop(f);
    let mut kept = None;
    if path.exists() && policy.old_copies && !policy.overwrite {
        let old = old_name(path)?;
        rename(path, &old).map_err(|e| io_error(path, &e))?;
        kept = Some((path.to_path_buf(), old));
    }
    if let Err(e) = rename(&tmp_path, path) {
        let mut err = io_error(path, &e);
        if let Some((_, old)) = &kept
            && let Err(undo) = rename(old, path)
        {
            // The earlier file stays where the first rename put it; say where.
            let at = old.display().to_string();
            err.reason = format!("{}; the earlier file could not be put back ({undo}) and is at {at}", err.reason);
            err.next_step = format!("the earlier file is at {at}: rename it back to {} once the cause is gone, then retry", path.display());
            err = err.with("previous_renamed_to", json!(at));
        }
        return Err(err);
    }
    guard.0 = None;
    Ok((kept, made))
}

/// Whether `e` is a full disk or quota: the error kinds, Windows' ERROR_DISK_FULL (112) and
/// ERROR_HANDLE_DISK_FULL (39) (on other systems those numbers mean something else), and the words
/// ("No space left on device", "There is not enough space on the disk", "Disk quota exceeded").
fn disk_full(e: &std::io::Error, lower: &str) -> bool {
    use std::io::ErrorKind::{QuotaExceeded, StorageFull};
    matches!(e.kind(), StorageFull | QuotaExceeded)
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(112 | 39)))
        || ["no space", "not enough space", "disk full", "quota"].iter().any(|w| lower.contains(w))
}

/// A file-system failure in the contract's words (`disk_full`, `locked`, `failed`).
pub fn io_error(path: &Path, e: &std::io::Error) -> ToolError {
    let shown = path.display();
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    if disk_full(e, &lower) {
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

    /// A streamed write that stops part way (a cancel, a write error, a panic) leaves nothing: no
    /// temporary file, no file at the path, and the earlier file where it was (not renamed).
    #[test]
    fn a_streamed_write_that_stops_leaves_nothing() {
        let d = tmp("stream");
        let p = d.join("out/a.svg");
        let policy = WritePolicy { old_copies: true, overwrite: false };
        std::fs::write(&p, b"earlier").unwrap();
        let stopped = |w: &mut std::io::BufWriter<std::fs::File>| -> Result<(), WriteErr> {
            w.write_all(&vec![b'x'; 1 << 20])?;
            Err(ToolError::new(code::CANCELLED, "stopped", "run it again").into())
        };
        assert_eq!(write_file_with(&p, policy, stopped).unwrap_err().code, code::CANCELLED);
        let broke =
            |_: &mut std::io::BufWriter<std::fs::File>| -> Result<(), WriteErr> { Err(std::io::Error::other("no space left on device").into()) };
        assert_eq!(write_file_with(&p, policy, broke).unwrap_err().code, code::DISK_FULL);
        let panicked = std::panic::catch_unwind(|| write_file_with(&p, policy, |_| -> Result<(), WriteErr> { panic!("boom") }));
        assert!(panicked.is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"earlier");
        assert_eq!(std::fs::read_dir(d.join("out")).unwrap().count(), 1, "no OLD_ copy, no temporary file");
        // Finished: the pieces are one file, the earlier one kept.
        let (kept, n) = write_file_with(&p, policy, |w| w.write_all(b"new").map(|()| 3).map_err(WriteErr::from)).unwrap();
        assert_eq!(n, 3);
        assert!(kept.unwrap().1.ends_with("OLD_a.svg"));
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The final rename fails and so does the undo of the OLD_ rename: the error says where the
    /// earlier file is, in its words and its data. When only the final one fails the earlier file
    /// is back at its path.
    #[test]
    fn a_failed_undo_names_where_the_earlier_file_is() {
        let d = tmp("undo");
        let p = d.join("out/a.svg");
        let policy = WritePolicy { old_copies: true, overwrite: false };
        std::fs::write(&p, b"earlier").unwrap();
        let fill = |w: &mut std::io::BufWriter<std::fs::File>| -> Result<(), WriteErr> { w.write_all(b"new").map_err(WriteErr::from) };
        let tmp_name = |from: &Path| from.to_string_lossy().ends_with(".cctmp");
        // The new file cannot be moved in; putting the earlier one back works.
        let one = |from: &Path, to: &Path| if tmp_name(from) { Err(std::io::Error::other("locked")) } else { std::fs::rename(from, to) };
        assert!(write_file_renaming(&p, policy, fill, &one).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"earlier");
        // Neither works.
        let both = |from: &Path, to: &Path| {
            if tmp_name(from) || to == p.as_path() { Err(std::io::Error::other("locked")) } else { std::fs::rename(from, to) }
        };
        let e = write_file_renaming(&p, policy, fill, &both).unwrap_err();
        let old = d.join("out/OLD_a.svg");
        assert_eq!(std::fs::read(&old).unwrap(), b"earlier");
        assert_eq!(e.extra["previous_renamed_to"], json!(old.display().to_string()));
        assert!(e.reason.contains(&old.display().to_string()), "{}", e.reason);
        assert!(e.next_step.contains(&old.display().to_string()));
        assert!(!p.exists());
        assert_eq!(std::fs::read_dir(d.join("out")).unwrap().count(), 1, "no temporary file");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// No count ends the search for a free OLD_ name in an overwrite: with 10,010 names taken the
    /// next is used and every earlier file is still there.
    #[test]
    fn the_old_name_search_has_no_end() {
        let d = tmp("oldname");
        let p = d.join("out/a.pdf");
        std::fs::write(d.join("out/OLD_a.pdf"), b"1").unwrap();
        for n in 2..=10_010 {
            std::fs::write(d.join(format!("out/OLD_{n}_a.pdf")), b"x").unwrap();
        }
        std::fs::write(&p, b"earlier").unwrap();
        let kept = write_file(&p, b"new", WritePolicy { old_copies: true, overwrite: false }).unwrap().unwrap();
        assert!(kept.1.ends_with("OLD_10011_a.pdf"), "{:?}", kept.1);
        assert_eq!(std::fs::read(&kept.1).unwrap(), b"earlier");
        assert_eq!(std::fs::read(d.join("out/OLD_a.pdf")).unwrap(), b"1");
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        // A path with no file name has nowhere to keep a copy: refused, not overwritten.
        assert_eq!(old_name(Path::new("/")).unwrap_err().code, code::INVALID_ARGUMENT);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A full disk is `disk_full` whichever way the system says it.
    #[test]
    fn a_full_disk_is_disk_full_in_every_wording() {
        use std::io::{Error, ErrorKind};
        let p = Path::new("a.svg");
        let words = "There is not enough space on the disk. (os error 112)";
        let cases = [
            Error::from(ErrorKind::StorageFull),
            Error::from(ErrorKind::QuotaExceeded),
            Error::other(words),
            Error::other("No space left on device (os error 28)"),
            Error::other("Disk quota exceeded"),
        ];
        for e in &cases {
            assert_eq!(io_error(p, e).code, code::DISK_FULL, "{e}");
        }
        #[cfg(windows)]
        for raw in [112, 39] {
            assert_eq!(io_error(p, &Error::from_raw_os_error(raw)).code, code::DISK_FULL, "os error {raw}");
        }
        #[cfg(unix)]
        {
            assert_eq!(io_error(p, &Error::from_raw_os_error(28)).code, code::DISK_FULL);
            // 39 and 112 are other errors here (not empty, host down): not a full disk.
            for raw in [39, 112] {
                assert_eq!(io_error(p, &Error::from_raw_os_error(raw)).code, code::FAILED, "os error {raw}");
            }
        }
        assert_eq!(io_error(p, &Error::from(ErrorKind::PermissionDenied)).code, code::LOCKED);
        assert_eq!(io_error(p, &Error::other("boom")).code, code::FAILED);
    }
}
