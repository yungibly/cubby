//! What cubby remembers about each tracked path, per store, on this machine.
//!
//! For every path the index keeps:
//!
//! * the **baseline**: a fingerprint of the content both sides had the last
//!   time they matched. Comparing each side with it tells "changed at home"
//!   from "changed in the store" from "changed on both sides", and "deleted
//!   at home" from "new in the store".
//! * a **stat cache** for each side: size, times, inode, and mode, with the
//!   fingerprint the path had then. A file whose metadata has not changed is
//!   not read again, which keeps `status --quiet` quick enough for a prompt.
//!
//! A file that changed within two seconds of being looked at is not cached:
//! a second change in the same clock tick could leave its metadata as it
//! was. The file lives at `<state>/index/<hash of the store's path>.tsv`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::fsx::{self, Kind, Meta};
use crate::paths::{Rel, Side};
use crate::scan::{Scan, Scope, State};

const HEADER: &str = "cubby index 1";
/// How recently a file may have changed and still be cached.
const RACY_NS: i64 = 2_000_000_000;

/// A fingerprint of a file's content and executable bit, or of a symlink's
/// target.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fp([u8; 16]);

impl Fp {
    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    pub fn parse(text: &str) -> Option<Fp> {
        let bytes = unhex(text)?;
        Some(Fp(bytes.try_into().ok()?))
    }
}

/// What is known about the last time both sides matched.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Base {
    /// Never synced on this machine, as far as cubby knows.
    #[default]
    None,
    /// Synced before, content unknown (from cubby 2's history log).
    Seen,
    /// Synced with this content.
    Is(Fp),
}

impl Base {
    pub fn exists(self) -> bool {
        self != Base::None
    }

    /// `-`, `?`, or the fingerprint, as the index and run records write it.
    pub fn to_text(self) -> String {
        match self {
            Base::None => "-".to_owned(),
            Base::Seen => "?".to_owned(),
            Base::Is(fp) => fp.hex(),
        }
    }

    pub fn from_text(text: &str) -> Option<Base> {
        match text {
            "-" => Some(Base::None),
            "?" => Some(Base::Seen),
            hex => Fp::parse(hex).map(Base::Is),
        }
    }
}

/// The metadata that changes whenever a file does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stat {
    kind: u8,
    len: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    dev: u64,
    ino: u64,
    mode: u32,
}

impl Stat {
    pub fn of(m: &Meta) -> Stat {
        Stat {
            kind: match m.kind {
                Kind::File => b'f',
                Kind::Symlink => b'l',
                Kind::Dir => b'd',
                Kind::Other => b'o',
            },
            len: m.len,
            mtime_ns: m.mtime_ns,
            ctime_ns: m.ctime_ns,
            dev: m.dev,
            ino: m.ino,
            mode: m.mode,
        }
    }

    /// Changed too recently to trust: another change in the same clock tick
    /// would not show.
    fn is_racy(&self, now_ns: i64) -> bool {
        now_ns - self.mtime_ns < RACY_NS || now_ns - self.ctime_ns < RACY_NS
    }
}

/// A side's metadata and the fingerprint it had with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cached {
    pub stat: Stat,
    pub fp: Fp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub base: Base,
    pub home: Option<Cached>,
    pub store: Option<Cached>,
}

impl Record {
    fn side(&self, side: Side) -> Option<&Cached> {
        match side {
            Side::Home => self.home.as_ref(),
            Side::Store => self.store.as_ref(),
        }
    }

    fn is_empty(&self) -> bool {
        *self == Record::default()
    }
}

pub struct Index {
    path: PathBuf,
    store: PathBuf,
    records: BTreeMap<Rel, Record>,
    /// Whether there was an index file to load.
    existed: bool,
    changed: bool,
}

impl Index {
    /// Where the index for `store` lives.
    pub fn path_for(state_dir: &Path, store: &Path) -> PathBuf {
        let digest = Sha256::digest(store.as_os_str().as_bytes());
        state_dir
            .join("index")
            .join(format!("{}.tsv", hex(&digest[..8])))
    }

    /// Load the index for `store`. A missing or unreadable one starts empty:
    /// the index only ever saves work or adds precision, so losing it costs
    /// nothing but a slower scan.
    pub fn load(state_dir: &Path, store: &Path) -> Index {
        let path = Index::path_for(state_dir, store);
        let mut index = Index {
            path,
            store: store.to_path_buf(),
            records: BTreeMap::new(),
            existed: false,
            changed: false,
        };
        let Ok(text) = std::fs::read_to_string(&index.path) else {
            return index;
        };
        let mut lines = text.lines();
        if !lines.next().is_some_and(|h| h.starts_with(HEADER)) {
            return index;
        }
        index.existed = true;
        for line in lines {
            if let Some((rel, record)) = parse_line(line) {
                index.records.insert(rel, record);
            }
        }
        index
    }

    /// Whether this store had an index on this machine before.
    pub fn existed(&self) -> bool {
        self.existed
    }

    pub fn base(&self, rel: &Rel) -> Base {
        self.records.get(rel).map_or(Base::None, |r| r.base)
    }

    /// The fingerprint cached for one side of `rel`, if `meta` shows that
    /// side has not changed since.
    pub fn cached(&self, rel: &Rel, side: Side, meta: &Meta) -> Option<Fp> {
        let c = self.records.get(rel)?.side(side)?;
        (c.stat == Stat::of(meta)).then_some(c.fp)
    }

    /// Both sides of `rel` now hold content with fingerprint `fp`.
    pub fn synced(&mut self, rel: &Rel, fp: Fp) {
        let record = Record {
            base: Base::Is(fp),
            home: None,
            store: None,
        };
        if self.records.get(rel) != Some(&record) {
            self.records.insert(rel.clone(), record);
            self.changed = true;
        }
    }

    /// Put back a baseline, as it was before a run that is being undone.
    pub fn set_base(&mut self, rel: &Rel, base: Base) {
        if base == Base::None {
            self.forget(rel);
            return;
        }
        let record = Record {
            base,
            home: None,
            store: None,
        };
        if self.records.get(rel) != Some(&record) {
            self.records.insert(rel.clone(), record);
            self.changed = true;
        }
    }

    /// Nothing to remember about `rel` any more.
    pub fn forget(&mut self, rel: &Rel) {
        self.changed |= self.records.remove(rel).is_some();
    }

    /// Take in what a scan found: baselines for paths that match, and
    /// cached metadata for paths that were read. Paths in the scope that
    /// neither side has any more are forgotten.
    pub fn learn(&mut self, scan: &Scan, scope: &Scope) {
        let now = now_ns();
        let keep = |meta: Option<&Meta>, fp: Option<Fp>| {
            let (m, fp) = (meta?, fp?);
            let stat = Stat::of(m);
            (!stat.is_racy(now)).then_some(Cached { stat, fp })
        };
        for e in &scan.entries {
            let mut record = self.records.get(&e.rel).cloned().unwrap_or_default();
            record.home = keep(e.home.as_ref(), e.home_fp);
            record.store = keep(e.store.as_ref(), e.store_fp);
            if e.state == State::Same
                && let Some(fp) = e.home_fp
            {
                record.base = Base::Is(fp);
            }
            if record.is_empty() {
                self.forget(&e.rel);
            } else if self.records.get(&e.rel) != Some(&record) {
                self.records.insert(e.rel.clone(), record);
                self.changed = true;
            }
        }
        let seen: BTreeSet<&Rel> = scan.entries.iter().map(|e| &e.rel).collect();
        let before = self.records.len();
        self.records
            .retain(|rel, _| !scope.includes(rel) || seen.contains(rel));
        self.changed |= self.records.len() != before;
    }

    /// Paths cubby 2 saved or restored on this machine count as synced
    /// before, so a file deleted at home before the upgrade is still
    /// removed from the store the way cubby 2 would have.
    pub fn import_legacy(&mut self, synced: impl IntoIterator<Item = Rel>) {
        for rel in synced {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.records.entry(rel) {
                slot.insert(Record {
                    base: Base::Seen,
                    ..Record::default()
                });
                self.changed = true;
            }
        }
    }

    /// Write the index if anything changed.
    pub fn save(&mut self) -> Result<()> {
        if !self.changed {
            return Ok(());
        }
        let mut out = format!("{HEADER}\t{}\n", escape(&self.store.to_string_lossy()));
        for (rel, r) in &self.records {
            let base = r.base.to_text();
            out.push_str(&format!(
                "{}\t{base}\t{}\t{}\n",
                escape(rel.as_str()),
                show_cached(r.home.as_ref()),
                show_cached(r.store.as_ref())
            ));
        }
        fsx::write_atomic(&self.path, out.as_bytes())
            .with_context(|| format!("cannot write {}", self.path.display()))?;
        self.changed = false;
        self.existed = true;
        Ok(())
    }
}

/// Fingerprint a file (its content and executable bit) or a symlink (its
/// target).
pub fn fingerprint(meta: &Meta) -> Result<Fp> {
    let mut hasher = Sha256::new();
    match meta.kind {
        Kind::Symlink => {
            hasher.update(b"l");
            let target = meta.target.as_deref().unwrap_or(Path::new(""));
            hasher.update(target.as_os_str().as_bytes());
        }
        Kind::File => {
            hasher.update([b'f', u8::from(meta.is_executable())]);
            let mut file = File::open(&meta.path)
                .with_context(|| format!("cannot read {}", meta.path.display()))?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => hasher.update(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("cannot read {}", meta.path.display()));
                    }
                }
            }
        }
        Kind::Dir | Kind::Other => bail!("{} is {}", meta.path.display(), meta.kind.describe()),
    }
    let digest = hasher.finalize();
    let mut fp = [0u8; 16];
    fp.copy_from_slice(&digest[..16]);
    Ok(Fp(fp))
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

fn show_cached(c: Option<&Cached>) -> String {
    match c {
        None => "-".to_owned(),
        Some(c) => {
            let s = c.stat;
            format!(
                "{},{},{},{},{},{},{},{}",
                s.kind as char,
                s.len,
                s.mtime_ns,
                s.ctime_ns,
                s.dev,
                s.ino,
                s.mode,
                c.fp.hex()
            )
        }
    }
}

fn parse_cached(text: &str) -> Option<Option<Cached>> {
    if text == "-" {
        return Some(None);
    }
    let mut parts = text.split(',');
    let kind = parts.next()?.bytes().next()?;
    let mut num = || parts.next();
    let len = num()?.parse().ok()?;
    let mtime_ns = num()?.parse().ok()?;
    let ctime_ns = num()?.parse().ok()?;
    let dev = num()?.parse().ok()?;
    let ino = num()?.parse().ok()?;
    let mode = num()?.parse().ok()?;
    let fp = Fp::parse(num()?)?;
    Some(Some(Cached {
        stat: Stat {
            kind,
            len,
            mtime_ns,
            ctime_ns,
            dev,
            ino,
            mode,
        },
        fp,
    }))
}

fn parse_line(line: &str) -> Option<(Rel, Record)> {
    let mut fields = line.split('\t');
    let rel = Rel::parse(&unescape(fields.next()?)?).ok()?;
    let base = Base::from_text(fields.next()?)?;
    let home = parse_cached(fields.next()?)?;
    let store = parse_cached(fields.next()?)?;
    Some((rel, Record { base, home, store }))
}

/// Tabs, newlines, and backslashes in names, escaped so each path stays on
/// its own line.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            '\\' => '\\',
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            _ => return None,
        });
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sandbox;

    fn rel(s: &str) -> Rel {
        Rel::parse(s).unwrap()
    }

    #[test]
    fn fingerprints_cover_content_and_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let sb = sandbox();
        let a = sb.path().join("a");
        let b = sb.path().join("b");
        std::fs::write(&a, "same").unwrap();
        std::fs::write(&b, "same").unwrap();
        let fp = |p: &Path| fingerprint(&fsx::lstat(p).unwrap().unwrap()).unwrap();
        assert_eq!(fp(&a), fp(&b));
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(fp(&a), fp(&b));
        std::fs::write(&b, "other").unwrap();
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(fp(&a), fp(&b));
        let link = sb.path().join("link");
        std::os::unix::fs::symlink("same", &link).unwrap();
        assert_ne!(fp(&link), fp(&a), "a link to x is not a file holding x");
    }

    #[test]
    fn saves_and_loads_with_odd_names() {
        let sb = sandbox();
        let store = sb.path().join("store");
        let mut index = Index::load(sb.path(), &store);
        assert!(!index.existed());
        let fp = Fp([7; 16]);
        let odd = rel(".config/tab\tnew\nline\\back");
        index.synced(&odd, fp);
        index.import_legacy([rel(".zshrc")]);
        index.records.get_mut(&odd).unwrap().home = Some(Cached {
            stat: Stat {
                kind: b'f',
                len: 3,
                mtime_ns: -5,
                ctime_ns: 1_700_000_000_123_456_789,
                dev: 1,
                ino: 2,
                mode: 0o644,
            },
            fp,
        });
        index.changed = true;
        index.save().unwrap();

        let loaded = Index::load(sb.path(), &store);
        assert!(loaded.existed());
        assert_eq!(loaded.records, index.records);
        assert_eq!(loaded.base(&rel(".zshrc")), Base::Seen);
        assert_eq!(loaded.base(&rel(".nothing")), Base::None);
        // Another store has its own index.
        assert!(!Index::load(sb.path(), &sb.path().join("other")).existed());
    }

    #[test]
    fn a_damaged_index_starts_over() {
        let sb = sandbox();
        let store = sb.path().join("store");
        let path = Index::path_for(sb.path(), &store);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not an index\n").unwrap();
        assert!(!Index::load(sb.path(), &store).existed());
        std::fs::write(&path, format!("{HEADER}\tx\ngarbage\n.zshrc\t?\t-\t-\n")).unwrap();
        let index = Index::load(sb.path(), &store);
        assert_eq!(index.records.len(), 1);
    }

    #[test]
    fn recent_changes_are_not_trusted() {
        let now = 10 * RACY_NS;
        let stat = |t: i64| Stat {
            kind: b'f',
            len: 1,
            mtime_ns: t,
            ctime_ns: t,
            dev: 1,
            ino: 1,
            mode: 0o644,
        };
        assert!(stat(now - 1).is_racy(now));
        assert!(!stat(now - RACY_NS).is_racy(now));
    }
}
