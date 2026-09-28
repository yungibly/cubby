//! Permissions git cannot keep.
//!
//! Git records one bit of a file's mode: whether it is executable. A clone
//! gets 644 or 755 whatever the file had, so a `~/.netrc` restored from a
//! freshly cloned store would be readable by every user on the machine. So
//! cubby records, in the manifest, the files and directories that group and
//! others cannot read, and applies those records when it writes.
//!
//! Only a record's group and other bits are enforced, and only ever to take
//! permissions away: the owner's bits follow the usual rules. A record gets
//! stricter on its own but never looser, so a machine whose files were
//! loosened (say, by an older cubby's restore) cannot erase another
//! machine's record just by saving. Loosening one takes `save --force`.

/// Whether group or others lack read access, which git cannot keep.
pub fn is_private(mode: u32) -> bool {
    mode & 0o044 != 0o044
}

/// `mode` without the group and other permissions `record` does not grant.
pub fn restrict(mode: u32, record: u32) -> u32 {
    (mode & !0o077) | (mode & record & 0o077)
}

/// The record a save leaves for a path whose home copy has mode `home`.
/// With `force`, home's permissions are taken as they are.
pub fn after_save(home: u32, recorded: Option<u32>, force: bool) -> Option<u32> {
    let home = home & 0o777;
    match recorded {
        Some(r) if !force => Some(restrict(home, r)),
        _ => is_private(home).then_some(home),
    }
}

/// The mode restore leaves on a home file or directory that has `home`.
pub fn after_restore(home: u32, recorded: Option<u32>) -> u32 {
    let home = home & 0o777;
    recorded.map_or(home, |r| restrict(home, r))
}

/// `600`, the way modes are written.
pub fn show(mode: u32) -> String {
    format!("{:03o}", mode & 0o7777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_means_group_or_others_cannot_read() {
        assert!(is_private(0o600));
        assert!(is_private(0o640));
        assert!(is_private(0o700));
        assert!(is_private(0o400));
        assert!(!is_private(0o644));
        assert!(!is_private(0o755));
        assert!(!is_private(0o664));
        assert!(!is_private(0o444));
    }

    #[test]
    fn saving_records_private_modes_and_only_tightens() {
        assert_eq!(after_save(0o600, None, false), Some(0o600));
        assert_eq!(after_save(0o644, None, false), None);
        // A loosened copy does not loosen the record...
        assert_eq!(after_save(0o644, Some(0o600), false), Some(0o600));
        // ...unless forced.
        assert_eq!(after_save(0o644, Some(0o600), true), None);
        // Owner changes (say, chmod +x) follow home.
        assert_eq!(after_save(0o700, Some(0o600), false), Some(0o700));
        // A stricter home copy tightens the record.
        assert_eq!(after_save(0o600, Some(0o640), false), Some(0o600));
    }

    #[test]
    fn restoring_takes_permissions_away_but_never_adds_them() {
        assert_eq!(after_restore(0o644, Some(0o600)), 0o600);
        assert_eq!(after_restore(0o755, Some(0o700)), 0o700);
        assert_eq!(after_restore(0o644, Some(0o640)), 0o640);
        assert_eq!(after_restore(0o600, Some(0o644)), 0o600);
        assert_eq!(after_restore(0o644, None), 0o644);
        assert_eq!(show(0o600), "600");
        assert_eq!(show(0o4755), "4755");
    }
}
