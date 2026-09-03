//! Filesystem primitives with the safety properties the rest of cubby relies
//! on: writes are atomic, symlinks are never followed by accident, and a file
//! is never copied onto itself.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Symlink,
    Dir,
    /// Sockets, fifos, devices: reported, never copied.
    Other,
}

impl Kind {
    pub fn describe(self) -> &'static str {
        match self {
            Kind::File => "a file",
            Kind::Symlink => "a symlink",
            Kind::Dir => "a directory",
            Kind::Other => "a special file",
        }
    }
}

/// What `lstat` tells us about a path, without following symlinks.
#[derive(Clone, Debug)]
pub struct Meta {
    /// The path this was read from, exactly as it exists on disk.
    pub path: PathBuf,
    pub kind: Kind,
    pub len: u64,
    pub mode: u32,
    pub mtime: SystemTime,
    pub dev: u64,
    pub ino: u64,
    /// The link target, for symlinks.
    pub target: Option<PathBuf>,
}

impl Meta {
    pub fn is_executable(&self) -> bool {
        self.mode & 0o111 != 0
    }
}

/// Metadata for `path`, or `None` when nothing is there. A missing parent
/// directory, or a parent that is a file, also counts as nothing there.
pub fn lstat(path: &Path) -> Result<Option<Meta>> {
    let md = match fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e).with_context(|| format!("cannot stat {}", path.display())),
    };
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else {
        Kind::Other
    };
    let target = if kind == Kind::Symlink {
        Some(fs::read_link(path).with_context(|| format!("cannot read link {}", path.display()))?)
    } else {
        None
    };
    Ok(Some(Meta {
        path: path.to_path_buf(),
        kind,
        len: md.len(),
        mode: md.mode() & 0o7777,
        mtime: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        dev: md.dev(),
        ino: md.ino(),
        target,
    }))
}

pub fn same_inode(a: &Meta, b: &Meta) -> bool {
    a.dev == b.dev && a.ino == b.ino
}

/// Whether two regular files have identical contents. Sizes are compared
/// first so most differing files never get read.
pub fn same_content(a: &Path, a_meta: &Meta, b: &Path, b_meta: &Meta) -> Result<bool> {
    if a_meta.len != b_meta.len {
        return Ok(false);
    }
    if same_inode(a_meta, b_meta) {
        return Ok(true);
    }
    let mut fa = File::open(a).with_context(|| format!("cannot read {}", a.display()))?;
    let mut fb = File::open(b).with_context(|| format!("cannot read {}", b.display()))?;
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// Copy a file or symlink from `src` to `dst`, replacing whatever is at
/// `dst` atomically. Directories at `dst` are never replaced.
///
/// Permissions: a newly created file takes the source's mode. When
/// replacing, the destination keeps its own permission bits except the
/// executable bits, which follow the source. That keeps a locked-down file
/// (say, mode 600) locked down when the store copy came from a git clone
/// that only remembers the executable bit, while still propagating
/// `chmod +x`.
pub fn copy_entry(src: &Path, src_meta: &Meta, dst: &Path, dst_meta: Option<&Meta>) -> Result<()> {
    if let Some(d) = dst_meta {
        if same_inode(src_meta, d) {
            bail!("{} and {} are the same file", src.display(), dst.display());
        }
        if d.kind == Kind::Dir {
            bail!("{} is a directory", dst.display());
        }
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    match src_meta.kind {
        Kind::File => copy_file(src, src_meta, dst, dst_meta),
        Kind::Symlink => {
            let target = src_meta
                .target
                .clone()
                .ok_or_else(|| anyhow!("missing link target"))?;
            replace_with_symlink(&target, dst)
        }
        Kind::Dir | Kind::Other => bail!("{} is {}", src.display(), src_meta.kind.describe()),
    }
}

fn copy_file(src: &Path, src_meta: &Meta, dst: &Path, dst_meta: Option<&Meta>) -> Result<()> {
    let dir = dst
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", dst.display()))?;
    let mut input = File::open(src).with_context(|| format!("cannot read {}", src.display()))?;
    let mut tmp = temp_in(dir)?;
    io::copy(&mut input, tmp.as_file_mut())
        .with_context(|| format!("cannot copy {} to {}", src.display(), dst.display()))?;

    let mode = match dst_meta {
        Some(d) if d.kind == Kind::File => {
            // Executable only where the destination is readable, so a mode
            // 600 file becomes 700 rather than 711.
            let exec = (src_meta.mode & 0o111) & ((d.mode & 0o444) >> 2);
            (d.mode & !0o111) | exec
        }
        _ => src_meta.mode,
    };
    let file = tmp.as_file_mut();
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    // Preserve the modification time so "which side is newer" stays
    // meaningful after a copy.
    let _ = file.set_modified(src_meta.mtime);
    file.sync_all()?;
    tmp.persist(dst)
        .map_err(|e| anyhow!("cannot replace {}: {}", dst.display(), e.error))?;
    Ok(())
}

/// Write `data` to `path` atomically (via a temporary file and rename).
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut tmp = temp_in(dir)?;
    tmp.write_all(data)?;
    if let Ok(existing) = fs::metadata(path) {
        tmp.as_file_mut().set_permissions(existing.permissions())?;
    }
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path)
        .map_err(|e| anyhow!("cannot replace {}: {}", path.display(), e.error))?;
    Ok(())
}

fn temp_in(dir: &Path) -> Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(".cubby-tmp-")
        .tempfile_in(dir)
        .with_context(|| format!("cannot create a temporary file in {}", dir.display()))
}

/// Create a symlink to `target` at `dst`, replacing an existing file or
/// symlink atomically.
fn replace_with_symlink(target: &Path, dst: &Path) -> Result<()> {
    let dir = dst
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", dst.display()))?;
    let name = dst
        .file_name()
        .ok_or_else(|| anyhow!("{} has no name", dst.display()))?;
    for attempt in 0..100u32 {
        let tmp = dir.join(format!(
            ".cubby-tmp-{}-{}-{}",
            std::process::id(),
            attempt,
            name.to_string_lossy()
        ));
        match std::os::unix::fs::symlink(target, &tmp) {
            Ok(()) => {
                if let Err(e) = fs::rename(&tmp, dst) {
                    let _ = fs::remove_file(&tmp);
                    return Err(e).with_context(|| format!("cannot replace {}", dst.display()));
                }
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("cannot create symlink {}", tmp.display()));
            }
        }
    }
    bail!("cannot find a free temporary name in {}", dir.display())
}

/// Remove a file or symlink (never a directory), then remove any parent
/// directories left empty, stopping at `stop`.
pub fn remove_entry(path: &Path, stop: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(md) if md.is_dir() => bail!("{} is a directory", path.display()),
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
    prune_empty_dirs(path.parent(), stop);
    Ok(())
}

/// Remove empty directories from `start` upward, stopping at `stop`.
pub fn prune_empty_dirs(start: Option<&Path>, stop: &Path) {
    let mut dir = start;
    while let Some(d) = dir {
        if d == stop || !d.starts_with(stop) {
            break;
        }
        if fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Whether a file looks binary: contains a NUL byte or is not valid UTF-8 in
/// its first 8 KiB.
pub fn looks_binary(path: &Path) -> bool {
    let mut buf = [0u8; 8192];
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let Ok(n) = read_full(&mut f, &mut buf) else {
        return false;
    };
    let head = &buf[..n];
    if head.contains(&0) {
        return true;
    }
    match std::str::from_utf8(head) {
        Ok(_) => false,
        // A multi-byte character may straddle the 8 KiB boundary.
        Err(e) => e.error_len().is_some(),
    }
}

/// Human-readable size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sandbox;

    #[test]
    fn copy_is_atomic_and_preserves_mode_and_mtime() {
        let sb = sandbox();
        let src = sb.path().join("src");
        let dst = sb.path().join("sub/dst");
        fs::write(&src, b"hello").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        let src_meta = lstat(&src).unwrap().unwrap();
        copy_entry(&src, &src_meta, &dst, None).unwrap();
        let dst_meta = lstat(&dst).unwrap().unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"hello");
        assert_eq!(dst_meta.mode, 0o755);
        assert_eq!(dst_meta.mtime, src_meta.mtime);
        assert!(
            fs::read_dir(sb.path().join("sub")).unwrap().count() == 1,
            "no temp files left"
        );
    }

    #[test]
    fn replacing_keeps_destination_permissions_but_follows_exec_bit() {
        let sb = sandbox();
        let src = sb.path().join("src");
        let dst = sb.path().join("dst");
        fs::write(&src, b"new").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&dst, b"old").unwrap();
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o600)).unwrap();
        let sm = lstat(&src).unwrap().unwrap();
        let dm = lstat(&dst).unwrap().unwrap();
        copy_entry(&src, &sm, &dst, Some(&dm)).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert_eq!(lstat(&dst).unwrap().unwrap().mode, 0o700);
    }

    #[test]
    fn refuses_to_copy_onto_itself_or_a_directory() {
        let sb = sandbox();
        let a = sb.path().join("a");
        fs::write(&a, b"x").unwrap();
        let am = lstat(&a).unwrap().unwrap();
        let err = copy_entry(&a, &am, &a, Some(&am)).unwrap_err();
        assert!(err.to_string().contains("same file"), "{err}");
        assert_eq!(fs::read(&a).unwrap(), b"x");

        let link = sb.path().join("link");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let lm = lstat(&link).unwrap().unwrap();
        assert_eq!(lm.kind, Kind::Symlink);
        let d = sb.path().join("d");
        fs::create_dir(&d).unwrap();
        let dm = lstat(&d).unwrap().unwrap();
        assert!(copy_entry(&a, &am, &d, Some(&dm)).is_err());
    }

    #[test]
    fn symlinks_are_copied_as_symlinks() {
        let sb = sandbox();
        let link = sb.path().join("link");
        let dst = sb.path().join("dst");
        std::os::unix::fs::symlink("target/elsewhere", &link).unwrap();
        fs::write(&dst, b"a real file").unwrap();
        let lm = lstat(&link).unwrap().unwrap();
        let dm = lstat(&dst).unwrap().unwrap();
        copy_entry(&link, &lm, &dst, Some(&dm)).unwrap();
        assert_eq!(
            fs::read_link(&dst).unwrap(),
            PathBuf::from("target/elsewhere")
        );
        assert_eq!(fs::read_dir(sb.path()).unwrap().count(), 2);
    }

    #[test]
    fn same_content_compares_bytes() {
        let sb = sandbox();
        let a = sb.path().join("a");
        let b = sb.path().join("b");
        fs::write(&a, vec![7u8; 200_000]).unwrap();
        fs::write(&b, vec![7u8; 200_000]).unwrap();
        let am = lstat(&a).unwrap().unwrap();
        let bm = lstat(&b).unwrap().unwrap();
        assert!(same_content(&a, &am, &b, &bm).unwrap());
        let mut data = vec![7u8; 200_000];
        data[199_999] = 8;
        fs::write(&b, data).unwrap();
        let bm = lstat(&b).unwrap().unwrap();
        assert!(!same_content(&a, &am, &b, &bm).unwrap());
    }

    #[test]
    fn remove_prunes_empty_parents_but_not_the_stop_dir() {
        let sb = sandbox();
        let root = sb.path().join("root");
        let file = root.join("a/b/c");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();
        fs::write(root.join("a/keep"), b"x").unwrap();
        remove_entry(&file, &root).unwrap();
        assert!(!root.join("a/b").exists());
        assert!(root.join("a/keep").exists());
        remove_entry(&root.join("a/keep"), &root).unwrap();
        assert!(!root.join("a").exists());
        assert!(root.exists());
        assert!(lstat(&root.join("nope/x")).unwrap().is_none());
        assert!(lstat(&root).unwrap().is_some());
    }

    #[test]
    fn lstat_treats_file_parent_as_absent() {
        let sb = sandbox();
        let f = sb.path().join("f");
        fs::write(&f, b"x").unwrap();
        assert!(lstat(&f.join("child")).unwrap().is_none());
    }

    #[test]
    fn binary_detection_and_sizes() {
        let sb = sandbox();
        let t = sb.path().join("t");
        let b = sb.path().join("b");
        fs::write(&t, "plain text\n").unwrap();
        fs::write(&b, b"\x00\x01binary").unwrap();
        assert!(!looks_binary(&t));
        assert!(looks_binary(&b));
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }
}
