//! An append-only log of what cubby did: one tab-separated line per action
//! with an RFC 3339 timestamp, the operation, and the path.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::paths::Rel;

pub struct History {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub time: jiff::Timestamp,
    pub op: String,
    pub rel: String,
}

impl History {
    pub fn new(state_dir: &Path) -> History {
        History {
            path: state_dir.join("history.log"),
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, op: &str, rel: &Rel) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("cannot open {}", self.path.display()))?;
        writeln!(file, "{}\t{}\t{}", jiff::Timestamp::now(), op, rel.as_str())
            .with_context(|| format!("cannot write {}", self.path.display()))
    }

    /// All records, oldest first. Lines that cannot be parsed are skipped.
    pub fn read(&self) -> Result<Vec<Record>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", self.path.display()));
            }
        };
        Ok(text.lines().filter_map(parse_line).collect())
    }
}

fn parse_line(line: &str) -> Option<Record> {
    let mut parts = line.splitn(3, '\t');
    let time = parts.next()?.parse().ok()?;
    let op = parts.next()?.to_owned();
    let rel = parts.next()?.to_owned();
    Some(Record { time, op, rel })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sandbox;

    #[test]
    fn record_and_read() {
        let sb = sandbox();
        let h = History::new(&sb.path().join("state"));
        assert!(h.read().unwrap().is_empty());
        h.record("save", &Rel::parse(".zshrc").unwrap()).unwrap();
        h.record("restore", &Rel::parse(".config/nvim/init.lua").unwrap())
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(h.path())
            .unwrap()
            .write_all(b"garbage line\n")
            .unwrap();
        let records = h.read().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].op, "save");
        assert_eq!(records[1].rel, ".config/nvim/init.lua");
    }
}
