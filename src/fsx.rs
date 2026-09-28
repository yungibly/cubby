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
    /// Modification and status-change times in nanoseconds since the
    /// epoch, for noticing change without reading a file.
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub dev: u64,
    pub ino: u64,
    /// The link target, for symlinks.
    pub target: Option<PathBuf>,
    /// For symlinks: whether the target, followed from where the link is,
    /// is a directory.
    pub points_to_dir: bool,
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
    let points_to_dir = kind == Kind::Symlink && fs::metadata(path).is_ok_and(|m| m.is_dir());
    Ok(Some(Meta {
        path: path.to_path_buf(),
        kind,
        len: md.len(),
        mode: md.mode() & 0o7777,
        mtime: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        mtime_ns: md.mtime().saturating_mul(1_000_000_000) + md.mtime_nsec(),
        ctime_ns: md.ctime().saturating_mul(1_000_000_000) + md.ctime_nsec(),
        dev: md.dev(),
        ino: md.ino(),
        target,
        points_to_dir,
    }))
}

pub fn same_inode(a: &Meta, b: &Meta) -> bool {
    a.dev == b.dev && a.ino == b.ino
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

/// How a copy treats permissions beyond the usual rules.
#[derive(Clone, Debug, Default)]
pub struct Perms {
    /// Take away the group and other permissions this mode does not grant
    /// (see [`crate::perms`]).
    pub restrict: Option<u32>,
    /// The same, for parent directories the copy has to create.
    pub dirs: Vec<(PathBuf, u32)>,
}

/// Copy a file or symlink from `src` to `dst`, replacing whatever is at
/// `dst` atomically. Directories at `dst` are never replaced.
///
/// Permissions: a newly created file takes the source's mode. When
/// replacing, the destination keeps its own permission bits except the
/// executable bits, which follow the source. That keeps a locked-down file
/// (say, mode 600) locked down when the store copy came from a git clone
/// that only remembers the executable bit, while still propagating
/// `chmod +x`. `perms` can then take group and other access away.
pub fn copy_entry(
    src: &Path,
    src_meta: &Meta,
    dst: &Path,
    dst_meta: Option<&Meta>,
    perms: &Perms,
) -> Result<()> {
    if let Some(d) = dst_meta {
        if same_inode(src_meta, d) {
            bail!("{} and {} are the same file", src.display(), dst.display());
        }
        if d.kind == Kind::Dir {
            bail!("{} is a directory", dst.display());
        }
    }
    if let Some(parent) = dst.parent() {
        create_dirs(parent, &perms.dirs)?;
    }
    match src_meta.kind {
        Kind::File => copy_file(src, src_meta, dst, dst_meta, perms.restrict),
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

fn copy_file(
    src: &Path,
    src_meta: &Meta,
    dst: &Path,
    dst_meta: Option<&Meta>,
    restrict: Option<u32>,
) -> Result<()> {
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
    let mode = restrict.map_or(mode, |r| crate::perms::restrict(mode, r));
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

/// Create `dir` and any missing parents. Each directory created that has an
/// entry in `modes` loses the group and other permissions its mode does not
/// grant; directories that already existed are left alone.
pub fn create_dirs(dir: &Path, modes: &[(PathBuf, u32)]) -> Result<()> {
    let mut missing = Vec::new();
    let mut d = Some(dir);
    while let Some(p) = d
        && fs::symlink_metadata(p).is_err()
    {
        missing.push(p.to_path_buf());
        d = p.parent();
    }
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for m in missing {
        if let Some((_, mode)) = modes.iter().find(|(p, _)| *p == m) {
            let current = fs::metadata(&m)?.mode() & 0o7777;
            chmod(&m, crate::perms::restrict(current, *mode))?;
        }
    }
    Ok(())
}

/// Set the permission bits of `path` (following a symlink to a directory).
pub fn chmod(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("cannot change the permissions of {}", path.display()))
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

/// Remove a file or symlink (never a directory). With `prune_to`, parent
/// directories left empty are removed too, up to (not including) it.
pub fn remove_entry(path: &Path, prune_to: Option<&Path>) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(md) if md.is_dir() => bail!("{} is a directory", path.display()),
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
    if let Some(stop) = prune_to {
        prune_empty_dirs(path.parent(), stop);
    }
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
        copy_entry(&src, &src_meta, &dst, None, &Perms::default()).unwrap();
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
        copy_entry(&src, &sm, &dst, Some(&dm), &Perms::default()).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert_eq!(lstat(&dst).unwrap().unwrap().mode, 0o700);
    }

    #[test]
    fn refuses_to_copy_onto_itself_or_a_directory() {
        let sb = sandbox();
        let a = sb.path().join("a");
        fs::write(&a, b"x").unwrap();
        let am = lstat(&a).unwrap().unwrap();
        let err = copy_entry(&a, &am, &a, Some(&am), &Perms::default()).unwrap_err();
        assert!(err.to_string().contains("same file"), "{err}");
        assert_eq!(fs::read(&a).unwrap(), b"x");

        let link = sb.path().join("link");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let lm = lstat(&link).unwrap().unwrap();
        assert_eq!(lm.kind, Kind::Symlink);
        let d = sb.path().join("d");
        fs::create_dir(&d).unwrap();
        let dm = lstat(&d).unwrap().unwrap();
        assert!(copy_entry(&a, &am, &d, Some(&dm), &Perms::default()).is_err());
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
        copy_entry(&link, &lm, &dst, Some(&dm), &Perms::default()).unwrap();
        assert_eq!(
            fs::read_link(&dst).unwrap(),
            PathBuf::from("target/elsewhere")
        );
        assert_eq!(fs::read_dir(sb.path()).unwrap().count(), 2);
    }

    #[test]
    fn restrictions_apply_to_the_file_and_to_directories_created() {
        let sb = sandbox();
        let src = sb.path().join("src");
        fs::write(&src, b"secret").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
        let sm = lstat(&src).unwrap().unwrap();
        let existing = sb.path().join("home");
        fs::create_dir(&existing).unwrap();
        let dst = existing.join(".ssh/keys/config");
        let perms = Perms {
            restrict: Some(0o600),
            dirs: vec![(existing.join(".ssh"), 0o700), (existing.clone(), 0o700)],
        };
        copy_entry(&src, &sm, &dst, None, &perms).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().mode() & 0o777;
        assert_eq!(mode(&dst), 0o600);
        assert_eq!(mode(&existing.join(".ssh")), 0o700);
        assert!(existing.join(".ssh/keys").is_dir());
        // Directories that already existed are not touched.
        assert_ne!(mode(&existing), 0o700);
    }

    #[test]
    fn remove_prunes_empty_parents_but_not_the_stop_dir() {
        let sb = sandbox();
        let root = sb.path().join("root");
        let file = root.join("a/b/c");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();
        fs::write(root.join("a/keep"), b"x").unwrap();
        remove_entry(&file, Some(&root)).unwrap();
        assert!(!root.join("a/b").exists());
        assert!(root.join("a/keep").exists());
        remove_entry(&root.join("a/keep"), Some(&root)).unwrap();
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
