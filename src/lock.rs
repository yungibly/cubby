//! One cubby at a time: commands that change files hold an exclusive lock
//! on a file in the state directory. Commands that only look (a prompt
//! running `status --quiet`, say) never wait for it; they just skip
//! updating the index while another cubby holds it.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result};

pub struct Lock {
    // Closing the file releases the lock.
    _file: File,
}

impl Lock {
    /// Take the lock, or `None` when another process holds it.
    pub fn try_acquire(state_dir: &Path) -> Result<Option<Lock>> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("cannot create {}", state_dir.display()))?;
        let path = state_dir.join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        // SAFETY: flock on a descriptor this function owns.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(Lock { _file: file }));
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(err).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sandbox;

    #[test]
    fn a_second_lock_is_refused_until_the_first_is_dropped() {
        let sb = sandbox();
        let first = Lock::try_acquire(sb.path()).unwrap();
        assert!(first.is_some());
        assert!(Lock::try_acquire(sb.path()).unwrap().is_none());
        drop(first);
        assert!(Lock::try_acquire(sb.path()).unwrap().is_some());
    }
}
