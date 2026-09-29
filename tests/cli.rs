//! End-to-end tests that run the real binary against a sandboxed home
//! directory created under `target/`. Nothing here touches the real home.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    store: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(root).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("cubby-e2e-")
            .tempdir_in(root)
            .unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let store = home.join(".dotfiles");
        Sandbox {
            _dir: dir,
            home,
            store,
        }
    }

    /// A sandbox with the store initialised.
    fn ready() -> Sandbox {
        let sb = Sandbox::new();
        sb.ok(&["init"]);
        sb
    }

    fn cmd(&self, args: &[&str]) -> Output {
        sb_command(self, args)
            .output()
            .expect("failed to run cubby")
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = self.cmd(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (success, text) = self.run(args);
        assert!(success, "expected success for {args:?}:\n{text}");
        text
    }

    fn fail(&self, args: &[&str]) -> String {
        let (success, text) = self.run(args);
        assert!(!success, "expected failure for {args:?}:\n{text}");
        text
    }

    fn home_path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn store_path(&self, rel: &str) -> PathBuf {
        self.store.join(rel)
    }

    fn write_home(&self, rel: &str, content: &str) {
        write(&self.home_path(rel), content);
    }

    fn write_store(&self, rel: &str, content: &str) {
        write(&self.store_path(rel), content);
    }

    fn read_home(&self, rel: &str) -> String {
        fs::read_to_string(self.home_path(rel)).unwrap_or_else(|e| panic!("read ~/{rel}: {e}"))
    }

    fn read_store(&self, rel: &str) -> String {
        fs::read_to_string(self.store_path(rel)).unwrap_or_else(|e| panic!("read store/{rel}: {e}"))
    }

    fn manifest(&self) -> String {
        self.read_store(".cubby.toml")
    }

    /// The backup directories of recorded runs, oldest first.
    fn backups(&self) -> Vec<PathBuf> {
        let dir = self.home.join(".local/state/cubby/runs");
        let mut sets: Vec<PathBuf> = match fs::read_dir(dir) {
            Ok(rd) => rd
                .map(|e| e.unwrap().path().join("backup"))
                .filter(|b| b.exists())
                .collect(),
            Err(_) => Vec::new(),
        };
        sets.sort();
        sets
    }

    /// Every run, with the files each touched.
    fn history(&self) -> String {
        self.ok(&["history", "-v", "--all"])
    }
}

/// cubby, set up to run in the sandbox.
fn sb_command(sb: &Sandbox, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cubby"));
    cmd.args(args)
        .env("CUBBY_HOME", &sb.home)
        .env_remove("CUBBY_STORE")
        .env_remove("CUBBY_PAGER")
        .env("PAGER", "cat")
        .env("NO_COLOR", "1")
        .envs(GIT_ENV)
        .current_dir(&sb.home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

/// Keep git away from the real configuration, and give it an identity.
const GIT_ENV: [(&str, &str); 6] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "cubby test"),
    ("GIT_AUTHOR_EMAIL", "test@example.com"),
    ("GIT_COMMITTER_NAME", "cubby test"),
    ("GIT_COMMITTER_EMAIL", "test@example.com"),
];

/// Run git in `dir` with a predictable umask; panics on failure.
fn git(dir: &Path, args: &str) -> String {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("umask 022 && git {args}"))
        .current_dir(dir)
        .envs(GIT_ENV)
        .output()
        .expect("failed to run git");
    assert!(
        out.status.success(),
        "git {args}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn init_creates_config_store_and_manifest() {
    let sb = Sandbox::new();
    let text = sb.fail(&["status"]);
    assert!(text.contains("no store at ~/.dotfiles"), "{text}");

    let text = sb.ok(&["init"]);
    assert!(text.contains("wrote ~/.config/cubby/config.toml"), "{text}");
    assert!(text.contains("created ~/.dotfiles"), "{text}");
    let config = fs::read_to_string(sb.home.join(".config/cubby/config.toml")).unwrap();
    assert!(config.contains("store = \"~/.dotfiles\""), "{config}");
    assert!(sb.manifest().contains("dirs = ["));

    // Running again is harmless.
    let text = sb.ok(&["init"]);
    assert!(text.contains("already"), "{text}");
    // Pointing at another store needs --force.
    let text = sb.fail(&["init", "~/other"]);
    assert!(text.contains("--force"), "{text}");
    sb.ok(&["init", "~/other", "--force"]);
    let config = fs::read_to_string(sb.home.join(".config/cubby/config.toml")).unwrap();
    assert!(config.contains("store = \"~/other\""), "{config}");
    assert!(sb.home.join("other/.cubby.toml").exists());

    let text = sb.ok(&["status"]);
    assert!(text.contains("0 tracked files up to date"), "{text}");
}

#[test]
fn save_a_file_then_nothing_to_do() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "export EDITOR=nvim\n");

    let text = sb.ok(&["save", "~/.zshrc", "-y"]);
    assert!(text.contains("+ .zshrc"), "{text}");
    assert!(text.contains("saved 1 change"), "{text}");
    assert_eq!(sb.read_store(".zshrc"), "export EDITOR=nvim\n");

    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing to save"), "{text}");

    sb.write_home(".zshrc", "export EDITOR=vim\n");
    let text = sb.ok(&["status"]);
    assert!(text.contains("modified"), "{text}");
    assert!(text.contains("~ .zshrc"), "{text}");
    let text = sb.ok(&["-y"]);
    assert!(text.contains("~ .zshrc"), "{text}");
    assert_eq!(sb.read_store(".zshrc"), "export EDITOR=vim\n");
    assert!(sb.history().contains("~ .zshrc overwrite in the store"));
}

#[test]
fn bare_paths_are_save_and_relative_paths_work() {
    let sb = Sandbox::ready();
    sb.write_home(".config/kitty/kitty.conf", "font_size 12\n");
    let text = sb.ok(&[".config/kitty/kitty.conf", "-y"]);
    assert!(text.contains("+ .config/kitty/kitty.conf"), "{text}");
    assert!(sb.store_path(".config/kitty/kitty.conf").exists());
    // A file is not a tracked directory.
    assert!(!sb.manifest().contains("kitty"));
}

#[test]
fn tracked_directory_picks_up_new_files_and_mirrors_deletions() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "-- init\n");
    sb.write_home(".config/nvim/lua/keymaps.lua", "-- keys\n");

    let text = sb.ok(&["~/.config/nvim", "-y"]);
    assert!(
        text.contains("tracking ~/.config/nvim as a directory"),
        "{text}"
    );
    assert!(text.contains("2 files to copy"), "{text}");
    assert!(
        sb.manifest().contains("\"~/.config/nvim\""),
        "{}",
        sb.manifest()
    );

    // A new file appears, an old one is deleted, one is edited.
    sb.write_home(".config/nvim/lua/options.lua", "-- opts\n");
    fs::remove_file(sb.home_path(".config/nvim/lua/keymaps.lua")).unwrap();
    sb.write_home(".config/nvim/init.lua", "-- init v2\n");

    let text = sb.ok(&["status"]);
    assert!(text.contains("+ .config/nvim/lua/options.lua"), "{text}");
    assert!(text.contains("- .config/nvim/lua/keymaps.lua"), "{text}");
    assert!(text.contains("deleted at home"), "{text}");
    assert!(text.contains("~ .config/nvim/init.lua"), "{text}");

    let text = sb.ok(&["-y"]);
    assert!(text.contains("1 file to remove from the store"), "{text}");
    assert!(sb.store_path(".config/nvim/lua/options.lua").exists());
    assert!(!sb.store_path(".config/nvim/lua/keymaps.lua").exists());
    assert_eq!(sb.read_store(".config/nvim/init.lua"), "-- init v2\n");
    assert!(
        sb.history()
            .contains("- .config/nvim/lua/keymaps.lua remove in the store")
    );

    // The removed file was backed up.
    let sets = sb.backups();
    assert_eq!(sets.len(), 1, "{sets:?}");
    let run = sets[0].parent().unwrap().file_name().unwrap();
    assert!(run.to_str().unwrap().contains("-save"), "{run:?}");
    assert!(sets[0].join(".config/nvim/lua/keymaps.lua").exists());
    assert!(
        sets[0].join(".config/nvim/init.lua").exists(),
        "the overwritten copy too"
    );

    // Adding a parent directory absorbs the child.
    sb.write_home(".config/fish/config.fish", "set -x X 1\n");
    let text = sb.ok(&["~/.config", "-y"]);
    assert!(text.contains("it now covers ~/.config/nvim"), "{text}");
    let manifest = sb.manifest();
    assert!(manifest.contains("\"~/.config\""), "{manifest}");
    assert!(!manifest.contains("\"~/.config/nvim\""), "{manifest}");
}

#[test]
fn absent_tracked_directory_is_never_deleted_from_store() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "-- init\n");
    sb.ok(&["~/.config/nvim", "-y"]);
    fs::remove_dir_all(sb.home_path(".config/nvim")).unwrap();

    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing to save"), "{text}");
    assert!(text.contains("does not exist at home"), "{text}");
    assert!(sb.store_path(".config/nvim/init.lua").exists());

    let text = sb.ok(&["status"]);
    assert!(
        text.contains("tracked directory, does not exist at home"),
        "{text}"
    );

    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("+ .config/nvim/init.lua"), "{text}");
    assert_eq!(sb.read_home(".config/nvim/init.lua"), "-- init\n");
}

#[test]
fn restore_creates_and_overwrites_with_backups() {
    let sb = Sandbox::ready();
    sb.write_store(".zshrc", "from store\n");
    sb.write_store(".config/git/config", "[user]\n\tname = me\n");
    sb.write_home(".zshrc", "local edits\n");

    let text = sb.ok(&["restore", "-n"]);
    assert!(text.contains("dry run"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "local edits\n");
    assert!(!sb.home_path(".config/git/config").exists());

    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("~ .zshrc"), "{text}");
    assert!(text.contains("+ .config/git/config"), "{text}");
    assert!(
        text.contains("home copy is newer") || text.contains("modified"),
        "{text}"
    );
    assert!(text.contains("restored 2 changes"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "from store\n");
    assert_eq!(sb.read_home(".config/git/config"), "[user]\n\tname = me\n");
    let sets = sb.backups();
    assert_eq!(sets.len(), 1);
    assert_eq!(
        fs::read_to_string(sets[0].join(".zshrc")).unwrap(),
        "local edits\n"
    );
    assert!(sb.history().contains("~ .zshrc overwrite at home"));

    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("nothing to restore"), "{text}");

    // Restoring a path with nothing in the store is an error.
    let text = sb.fail(&["restore", "~/.nothing", "-y"]);
    assert!(
        text.contains("nothing in the store at ~/.nothing"),
        "{text}"
    );
}

#[test]
fn restore_never_deletes_extra_files_at_home() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.ok(&["~/.config/nvim", "-y"]);
    sb.write_home(".config/nvim/scratch.lua", "not saved\n");
    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("nothing to restore"), "{text}");
    assert!(
        text.contains("1 file at home is not in the store yet"),
        "{text}"
    );
    assert!(sb.home_path(".config/nvim/scratch.lua").exists());
}

#[test]
fn no_backup_flag_and_config() {
    let sb = Sandbox::ready();
    sb.write_store(".zshrc", "store\n");
    sb.write_home(".zshrc", "home\n");
    sb.ok(&["restore", "-y", "--no-backup"]);
    assert!(sb.backups().is_empty());
    sb.write_home(".zshrc", "home again\n");
    let config = sb.home.join(".config/cubby/config.toml");
    fs::write(&config, "store = \"~/.dotfiles\"\nbackups = false\n").unwrap();
    sb.ok(&["restore", "-y"]);
    assert!(sb.backups().is_empty());
}

#[test]
fn status_reports_every_kind_of_difference() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.ok(&["~/.config/nvim", "-y"]);
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    sb.write_store(".gitconfig", "only in store\n");
    sb.write_home(".config/nvim/init.lua", "b\n");
    sb.write_home(".config/nvim/new.lua", "n\n");
    sb.write_store(".vimrc", "file in store\n");
    fs::create_dir_all(sb.home_path(".vimrc")).unwrap();

    let text = sb.ok(&["status"]);
    assert!(
        text.contains("modified\n  ~ .config/nvim/init.lua"),
        "{text}"
    );
    assert!(
        text.contains("new at home, not saved yet\n  + .config/nvim/new.lua"),
        "{text}"
    );
    assert!(
        text.contains("in the store, missing at home\n  - .gitconfig"),
        "{text}"
    );
    assert!(text.contains("conflicts\n  ! .vimrc"), "{text}");
    assert!(
        text.contains("home has a directory, store has a file"),
        "{text}"
    );
    assert!(
        text.contains("1 modified · 1 new · 1 missing · 1 conflict · 1 up to date"),
        "{text}"
    );

    let text = sb.ok(&["status", "-v"]);
    assert!(text.contains("up to date\n  = .zshrc"), "{text}");

    let text = sb.ok(&["status", "~/.nope"]);
    assert!(
        text.contains("? .nope") && text.contains("nothing tracked here"),
        "{text}"
    );
    assert!(!text.contains("up to date"), "{text}");
    let text = sb.fail(&["diff", "~/.nope"]);
    assert!(text.contains("nothing in the store at ~/.nope"), "{text}");
    let text = sb.ok(&["status", "~/.zshrc"]);
    assert!(!text.contains(".gitconfig"), "{text}");
}

#[test]
fn untrack_files_and_directories() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.write_home(".config/nvim/lua/x.lua", "x\n");
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.config/nvim", "~/.zshrc", "-y"]);

    let text = sb.fail(&["untrack", "~/.config/nvim/init.lua", "-y"]);
    assert!(
        text.contains("inside the tracked directory ~/.config/nvim"),
        "{text}"
    );
    assert!(sb.store_path(".config/nvim/init.lua").exists());

    let text = sb.fail(&["untrack", "~/.bashrc", "-y"]);
    assert!(text.contains("~/.bashrc is not tracked"), "{text}");

    let text = sb.ok(&["untrack", "~/.zshrc", "-y"]);
    assert!(text.contains("removed 1 file from the store"), "{text}");
    assert!(!sb.store_path(".zshrc").exists());
    assert_eq!(sb.read_home(".zshrc"), "z\n", "home is untouched");

    let text = sb.ok(&["untrack", "~/.config/nvim", "-y"]);
    assert!(text.contains("removed 2 files from the store"), "{text}");
    assert!(
        !sb.store_path(".config").exists(),
        "empty directories are pruned"
    );
    assert!(!sb.manifest().contains("nvim"));
    assert!(sb.home_path(".config/nvim/lua/x.lua").exists());
    let sets = sb.backups();
    assert_eq!(sets.len(), 2);
    assert!(sets[1].join(".config/nvim/lua/x.lua").exists());
    let history = sb.history();
    assert!(
        history.contains("untrack") && history.contains("- .zshrc remove in the store"),
        "{history}"
    );
}

#[test]
fn symlinks_are_copied_as_symlinks() {
    let sb = Sandbox::ready();
    sb.write_home("real/theme.conf", "dark\n");
    std::os::unix::fs::symlink("../real/theme.conf", sb.home_path(".config/theme.conf")).unwrap();
    fs::create_dir_all(sb.home_path(".config")).unwrap();

    sb.ok(&["~/.config/theme.conf", "-y"]);
    let stored = sb.store_path(".config/theme.conf");
    assert!(
        fs::symlink_metadata(&stored)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(&stored).unwrap(),
        PathBuf::from("../real/theme.conf")
    );

    let text = sb.ok(&["list"]);
    assert!(text.contains("theme.conf -> ../real/theme.conf"), "{text}");

    fs::remove_file(sb.home_path(".config/theme.conf")).unwrap();
    std::os::unix::fs::symlink("../real/other.conf", sb.home_path(".config/theme.conf")).unwrap();
    let text = sb.ok(&["diff"]);
    assert!(text.contains("(changed at home; symlink)"), "{text}");
    assert!(text.contains("- ../real/theme.conf"), "{text}");
    assert!(text.contains("+ ../real/other.conf"), "{text}");

    // The link changed at home: restore keeps that unless forced.
    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("changed at home; left alone"), "{text}");
    assert_eq!(
        fs::read_link(sb.home_path(".config/theme.conf")).unwrap(),
        PathBuf::from("../real/other.conf")
    );
    sb.ok(&["restore", "--force", "-y"]);
    assert_eq!(
        fs::read_link(sb.home_path(".config/theme.conf")).unwrap(),
        PathBuf::from("../real/theme.conf")
    );
}

#[test]
fn naming_a_symlinked_directory_explains_what_is_saved() {
    let sb = Sandbox::ready();
    sb.write_home("src/nvim-config/init.lua", "vim.o.number = true\n");
    fs::create_dir_all(sb.home_path(".config")).unwrap();
    std::os::unix::fs::symlink("../src/nvim-config", sb.home_path(".config/nvim")).unwrap();

    let text = sb.ok(&["~/.config/nvim", "-y"]);
    assert!(
        text.contains("~/.config/nvim is a symlink to a directory (../src/nvim-config)"),
        "{text}"
    );
    assert!(text.contains("`cubby ~/src/nvim-config`"), "{text}");
    assert!(
        text.contains("link → ../src/nvim-config (a directory; the link is saved"),
        "{text}"
    );
    assert!(
        fs::symlink_metadata(sb.store_path(".config/nvim"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn nothing_is_written_through_links_in_the_store() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.ok(&["~/.config/nvim", "-y"]);
    // In the store, lua/ is a link (another machine saved it as one) that
    // here leads out of the store.
    let outside = sb.home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, sb.store_path(".config/nvim/lua")).unwrap();
    sb.write_home(".config/nvim/lua/x.lua", "x\n");

    let text = sb.fail(&["-y"]);
    assert!(
        text.contains(
            ".config/nvim/lua in the store is a symlink; cubby will not write through it"
        ),
        "{text}"
    );
    sb.fail(&["~/.config/nvim/lua/x.lua", "-y"]);
    sb.fail(&["sync", "-y"]);
    assert!(!outside.join("x.lua").exists());
}

#[test]
fn stow_links_are_left_alone_even_when_forced() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    fs::remove_file(sb.home_path(".zshrc")).unwrap();
    std::os::unix::fs::symlink(".dotfiles/.zshrc", sb.home_path(".zshrc")).unwrap();

    let text = sb.fail(&["save", "--force", "-y"]);
    assert!(
        text.contains("a symlink into the store (as stow makes)"),
        "{text}"
    );
    let stored = fs::symlink_metadata(sb.store_path(".zshrc")).unwrap();
    assert!(stored.file_type().is_file());
    assert_eq!(sb.read_store(".zshrc"), "z\n");
}

#[test]
fn a_directory_that_became_a_file_or_a_link() {
    let sb = Sandbox::ready();
    sb.write_home(".config/app/a.conf", "a\n");
    sb.write_home(".config/app/sub/x.conf", "x\n");
    sb.ok(&["~/.config/app", "-y"]);

    // Became a file: the old files leave the store and the file takes the
    // directory's place, in one run.
    fs::remove_dir_all(sb.home_path(".config/app/sub")).unwrap();
    sb.write_home(".config/app/sub", "now a file\n");
    let text = sb.ok(&["-y"]);
    assert!(text.contains("- .config/app/sub/x.conf"), "{text}");
    assert_eq!(sb.read_store(".config/app/sub"), "now a file\n");

    // Became a link to a directory that holds the same file: a conflict
    // that --force cannot settle, reported as such rather than as "new".
    fs::remove_file(sb.home_path(".config/app/sub")).unwrap();
    fs::remove_file(sb.store_path(".config/app/sub")).unwrap();
    sb.write_store(".config/app/sub/x.conf", "x\n");
    sb.write_home("elsewhere/x.conf", "x\n");
    std::os::unix::fs::symlink("../../elsewhere", sb.home_path(".config/app/sub")).unwrap();
    let text = sb.ok(&["status"]);
    assert!(
        text.contains("home has a symlink, store has a directory"),
        "{text}"
    );
    let text = sb.fail(&["save", "--force", "-y"]);
    assert!(text.contains("cubby never replaces a directory"), "{text}");
}

#[test]
fn conflicts_are_skipped_unless_forced() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "file\n");
    sb.ok(&["~/.zshrc", "-y"]);
    fs::remove_file(sb.home_path(".zshrc")).unwrap();
    std::os::unix::fs::symlink("elsewhere", sb.home_path(".zshrc")).unwrap();

    // A skipped conflict means the run did not do everything: exit 1.
    let text = sb.fail(&["-y"]);
    assert!(text.contains("nothing saved; 1 path skipped"), "{text}");
    assert!(text.contains("! .zshrc"), "{text}");
    assert!(
        text.contains("home has a symlink, store has a file; use --force"),
        "{text}"
    );
    assert!(
        !fs::symlink_metadata(sb.store_path(".zshrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let text = sb.ok(&["-y", "--force"]);
    assert!(text.contains("replacing the store copy"), "{text}");
    assert!(
        fs::symlink_metadata(sb.store_path(".zshrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn refuses_dangerous_paths() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);

    let text = sb.fail(&["/etc/hosts", "-y"]);
    assert!(text.contains("outside your home directory"), "{text}");
    let text = sb.fail(&["~", "-y"]);
    assert!(text.contains("home directory itself"), "{text}");
    let text = sb.fail(&["~/.dotfiles/.zshrc", "-y"]);
    assert!(text.contains("inside the store"), "{text}");
    let text = sb.fail(&["~/.dotfiles", "-y"]);
    assert!(text.contains("inside the store"), "{text}");
    let text = sb.fail(&["~/.missing", "-y"]);
    assert!(text.contains("does not exist"), "{text}");

    // A stow-style symlink into the store must not be tracked: copying it
    // would replace the store file with a link to itself.
    std::os::unix::fs::symlink(sb.store_path(".zshrc"), sb.home_path(".linked")).unwrap();
    let text = sb.fail(&["~/.linked", "-y"]);
    assert!(text.contains("symlink into the store"), "{text}");
    assert_eq!(sb.read_store(".zshrc"), "z\n");

    // Tracking home's parent-ish tricks.
    let text = sb.fail(&["../", "-y"]);
    assert!(
        text.contains("outside your home directory") || text.contains("home directory itself"),
        "{text}"
    );
}

#[test]
fn store_inside_a_tracked_directory_is_skipped() {
    let sb = Sandbox::new();
    sb.ok(&["init", "~/.config/dotfiles"]);
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.ok(&["~/.config", "-y"]);
    assert!(
        sb.home
            .join(".config/dotfiles/.config/nvim/init.lua")
            .exists()
    );
    assert!(
        !sb.home.join(".config/dotfiles/.config/dotfiles").exists(),
        "the store must not copy itself"
    );
    let text = sb.ok(&["status"]);
    assert!(text.contains("up to date"), "{text}");
}

#[test]
fn diff_shows_unified_output_in_both_directions() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "line one\nline two\n");
    sb.ok(&["~/.zshrc", "-y"]);
    sb.write_home(".zshrc", "line one\nline 2\n");

    let text = sb.ok(&["diff"]);
    assert!(text.contains("~/.zshrc (changed at home)"), "{text}");
    assert!(text.contains("--- store\n+++ home\n"), "{text}");
    assert!(text.contains("-line two\n+line 2\n"), "{text}");
    let text = sb.ok(&["diff", "-R"]);
    assert!(text.contains("--- home\n+++ store\n"), "{text}");
    assert!(text.contains("-line 2\n+line two\n"), "{text}");

    sb.write_store(".bin", "\u{0}\u{1}\u{2}");
    fs::write(sb.home_path(".bin"), b"\x00\x01\x03").unwrap();
    let text = sb.ok(&["diff", "~/.bin"]);
    assert!(text.contains("binary files differ"), "{text}");

    sb.ok(&["-y"]);
    let text = sb.ok(&["diff", "~/.zshrc"]);
    assert!(text.contains("no differences"), "{text}");

    // Each direction shows only what that command would change.
    sb.write_store(".only-in-store", "from the store\n");
    let text = sb.ok(&["diff"]);
    assert!(!text.contains("from the store"), "{text}");
    assert!(
        text.contains("1 file only in the store not shown: saving leaves it alone"),
        "{text}"
    );
    let text = sb.ok(&["diff", "-R"]);
    assert!(text.contains("+from the store"), "{text}");
    sb.write_home(".config/app/a.conf", "a\n");
    sb.ok(&["~/.config/app", "-y"]);
    sb.write_home(".config/app/b.conf", "only at home\n");
    // Permissions show in the direction that would change them.
    fs::set_permissions(sb.home_path(".zshrc"), fs::Permissions::from_mode(0o600)).unwrap();
    let text = sb.ok(&["diff", "~/.zshrc"]);
    assert!(
        text.contains("~/.zshrc (permissions: record 600)"),
        "{text}"
    );
    let text = sb.ok(&["diff", "-R", "~/.config/app"]);
    assert!(!text.contains("only at home\n"), "{text}");
    assert!(
        text.contains("1 file only at home not shown: restore never deletes"),
        "{text}"
    );
    let text = sb.ok(&["diff", "~/.config/app"]);
    assert!(text.contains("+only at home"), "{text}");
}

#[test]
fn list_tree_and_plain() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.config/nvim", "~/.zshrc", "-y"]);
    fs::create_dir_all(sb.store_path(".git")).unwrap();
    sb.write_store("README.md", "# dotfiles\n");

    let text = sb.ok(&["list"]);
    assert!(
        text.contains(
            "├── .config/\n│   └── nvim/ (tracked directory)\n│       └── init.lua\n└── .zshrc\n"
        ),
        "{text}"
    );
    assert!(text.contains("2 files · 1 tracked directory"), "{text}");
    assert!(!text.contains("README"), "{text}");

    let text = sb.ok(&["list", "--plain"]);
    assert_eq!(text, ".config/nvim/init.lua\n.zshrc\n");
}

#[test]
fn dry_run_changes_nothing_and_prompts_need_a_tty() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    let text = sb.ok(&["~/.zshrc", "-n"]);
    assert!(text.contains("dry run"), "{text}");
    assert!(!sb.store_path(".zshrc").exists());
    assert!(sb.history().contains("no history yet"));

    // Without --yes and without a terminal, cubby refuses to guess.
    let text = sb.fail(&["~/.zshrc"]);
    assert!(text.contains("stdin is not a terminal"), "{text}");
    assert!(!sb.store_path(".zshrc").exists());
}

#[test]
fn ignore_patterns_are_honoured() {
    let sb = Sandbox::ready();
    let manifest = sb.manifest().replace(
        "ignore = [\n",
        "ignore = [\n  \"lazy-lock.json\",\n  \"~/.config/nvim/secret/**\",\n",
    );
    fs::write(sb.store_path(".cubby.toml"), manifest).unwrap();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.write_home(".config/nvim/lazy-lock.json", "{}\n");
    sb.write_home(".config/nvim/secret/token", "hunter2\n");
    sb.write_home(".config/nvim/init.lua.swp", "swap\n");
    sb.write_home(".config/nvim/.git/HEAD", "ref\n");
    sb.write_home(".config/nvim/.DS_Store", "junk\n");

    let text = sb.ok(&["~/.config/nvim", "-y"]);
    assert!(text.contains("1 file to copy"), "{text}");
    assert!(sb.store_path(".config/nvim/init.lua").exists());
    assert!(!sb.store_path(".config/nvim/lazy-lock.json").exists());
    assert!(!sb.store_path(".config/nvim/secret").exists());
    assert!(!sb.store_path(".config/nvim/init.lua.swp").exists());
    assert!(!sb.store_path(".config/nvim/.git").exists());

    let text = sb.fail(&["~/.config/nvim/lazy-lock.json", "-y"]);
    assert!(
        text.contains("is ignored: pattern \"lazy-lock.json\" in .cubby.toml"),
        "{text}"
    );

    // An ignored file already in the store is left alone but never restored.
    sb.write_store(".config/nvim/lazy-lock.json", "stale\n");
    let text = sb.ok(&["status"]);
    assert!(!text.contains("lazy-lock"), "{text}");
}

#[test]
fn executable_bit_propagates_and_permissions_are_kept() {
    let sb = Sandbox::ready();
    sb.write_home(".local/bin/hello", "#!/bin/sh\necho hi\n");
    fs::set_permissions(
        sb.home_path(".local/bin/hello"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    sb.ok(&["~/.local/bin/hello", "-y"]);
    assert_eq!(mode(&sb.store_path(".local/bin/hello")), 0o755);

    // Losing the bit is a change worth saving.
    fs::set_permissions(
        sb.home_path(".local/bin/hello"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let text = sb.ok(&["status"]);
    assert!(text.contains("~ .local/bin/hello"), "{text}");
    sb.ok(&["-y"]);
    assert_eq!(mode(&sb.store_path(".local/bin/hello")), 0o644);

    // Restoring over a locked-down file keeps it locked down.
    sb.write_store(".secret", "new\n");
    sb.write_home(".secret", "old\n");
    fs::set_permissions(sb.home_path(".secret"), fs::Permissions::from_mode(0o600)).unwrap();
    sb.ok(&["restore", "~/.secret", "-y"]);
    assert_eq!(sb.read_home(".secret"), "new\n");
    assert_eq!(mode(&sb.home_path(".secret")), 0o600);
}

#[test]
fn private_permissions_survive_a_fresh_clone() {
    let a = Sandbox::ready();
    a.write_home(".ssh/config", "Host x\n");
    a.write_home(".netrc", "machine api password hunter2\n");
    a.write_home(".bin/tool", "#!/bin/sh\n");
    chmod(&a.home_path(".ssh"), 0o700);
    chmod(&a.home_path(".ssh/config"), 0o600);
    chmod(&a.home_path(".netrc"), 0o600);
    chmod(&a.home_path(".bin/tool"), 0o700);
    let text = a.ok(&[
        "~/.ssh/config",
        "~/.netrc",
        "~/.bin/tool",
        "-y",
        "--allow-secrets",
    ]);
    assert!(text.contains("~ .ssh/ "), "{text}");
    assert!(text.contains("record permissions 700"), "{text}");
    let m = a.manifest();
    for line in [
        "\"~/.bin/tool\" = \"700\"",
        "\"~/.netrc\" = \"600\"",
        "\"~/.ssh\" = \"700\"",
        "\"~/.ssh/config\" = \"600\"",
    ] {
        assert!(m.contains(line), "{line} in\n{m}");
    }
    assert!(!m.contains(".bin\""), "{m}");
    // Private at home, private in the store.
    assert_eq!(mode(&a.store_path(".netrc")), 0o600);
    assert_eq!(mode(&a.store_path(".ssh")), 0o700);
    let text = a.ok(&["list"]);
    assert!(text.contains(".netrc mode 600"), "{text}");
    git(&a.store, "init -q && git add -A && git commit -qm dots");

    // Another machine clones the store: git kept only the executable bit.
    let b = Sandbox::new();
    git(
        &b.home,
        &format!("clone -q {} .dotfiles", a.store.display()),
    );
    b.ok(&["init"]);
    assert_eq!(mode(&b.store_path(".netrc")), 0o644);
    b.ok(&["restore", "-y"]);
    // The store's own copies are private again too.
    assert_eq!(mode(&b.store_path(".netrc")), 0o600);
    assert_eq!(mode(&b.store_path(".ssh")), 0o700);
    assert_eq!(mode(&b.home_path(".netrc")), 0o600);
    assert_eq!(mode(&b.home_path(".ssh")), 0o700);
    assert_eq!(mode(&b.home_path(".ssh/config")), 0o600);
    assert_eq!(mode(&b.home_path(".bin/tool")), 0o700);
    let text = b.ok(&["status"]);
    assert!(text.contains("up to date"), "{text}");

    // Loosened at home: status says so, saving keeps the record, and
    // restore tightens it again.
    chmod(&b.home_path(".netrc"), 0o644);
    chmod(&b.home_path(".ssh"), 0o755);
    let text = b.ok(&["status"]);
    assert!(
        text.contains("permissions\n  ~ .netrc") && text.contains("644 at home, 600 recorded"),
        "{text}"
    );
    assert!(text.contains("~ .ssh/"), "{text}");
    assert_eq!(b.cmd(&["status", "-q"]).status.code(), Some(1));
    b.ok(&["-y"]);
    assert!(b.manifest().contains("\"~/.netrc\" = \"600\""));
    let text = b.ok(&["restore", "-y"]);
    assert!(text.contains("permissions 644 → 600"), "{text}");
    assert!(text.contains("permissions 755 → 700"), "{text}");
    assert_eq!(mode(&b.home_path(".netrc")), 0o600);
    assert_eq!(mode(&b.home_path(".ssh")), 0o700);

    // Loosening a record on purpose takes --force.
    chmod(&b.home_path(".netrc"), 0o644);
    let text = b.ok(&["save", "--force", "~/.netrc", "-y"]);
    assert!(text.contains("forget recorded permissions 600"), "{text}");
    assert!(!b.manifest().contains("netrc"));

    // Untracking drops the records that go with it.
    b.ok(&["untrack", "~/.ssh/config", "-y"]);
    let m = b.manifest();
    assert!(!m.contains(".ssh"), "{m}");
}

#[test]
fn files_that_look_secret_need_a_second_yes() {
    let sb = Sandbox::ready();
    let token = |s: &str| format!("github.com:\n  oauth_token: ghp_{}\n", s.repeat(9));
    sb.write_home(".config/gh/hosts.yml", &token("a1B2"));
    sb.write_home(".config/gh/config.yml", "editor: vim\n");
    sb.write_home(
        ".ssh/work_key",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n",
    );

    // --yes alone does not answer this question.
    let text = sb.fail(&["~/.config/gh", "~/.ssh/work_key", "-y"]);
    assert!(
        text.contains("looks secret: it holds what looks like a GitHub token"),
        "{text}"
    );
    assert!(
        text.contains("looks secret: it holds a private key"),
        "{text}"
    );
    assert!(text.contains("2 files above look like secrets"), "{text}");
    assert!(
        text.contains("left out 2 files that look secret; `--allow-secrets` saves them"),
        "{text}"
    );
    assert!(sb.store_path(".config/gh/config.yml").exists());
    assert!(!sb.store_path(".config/gh/hosts.yml").exists());
    assert!(!sb.store_path(".ssh/work_key").exists());

    // Allowed, they go in, and later changes are not asked about again.
    sb.ok(&["~/.ssh/work_key", "-y", "--allow-secrets"]);
    sb.ok(&["sync", "-y", "--allow-secrets"]);
    assert!(sb.store_path(".config/gh/hosts.yml").exists());
    assert!(sb.store_path(".ssh/work_key").exists());
    sb.write_home(".config/gh/hosts.yml", &token("c3D4"));
    let text = sb.ok(&["-y"]);
    assert!(!text.contains("looks secret"), "{text}");

    // A new store ignores the usual names of private keys.
    sb.write_home(".ssh/id_ed25519", "-----BEGIN OPENSSH PRIVATE KEY-----\n");
    let text = sb.fail(&["~/.ssh/id_ed25519", "-y"]);
    assert!(
        text.contains("is ignored: pattern \"id_ed25519\""),
        "{text}"
    );
}

#[test]
fn skip_leaves_paths_alone_on_this_machine() {
    let sb = Sandbox::ready();
    // Another machine saved its macOS-only settings.
    sb.write_store(".config/aerospace/aerospace.toml", "gaps = 8\n");
    sb.write_store(".zshrc", "z\n");
    sb.write_home(".zshrc", "z\n");
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(1));

    let text = sb.ok(&["ignore", "--here", "~/.config/aerospace"]);
    assert!(
        text.contains("skipping \"~/.config/aerospace\" on this machine"),
        "{text}"
    );
    let config = fs::read_to_string(sb.home.join(".config/cubby/config.toml")).unwrap();
    assert!(
        config.contains("skip = [\n  \"~/.config/aerospace\",\n]"),
        "{config}"
    );
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(0));
    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("nothing to restore"), "{text}");
    assert!(!sb.home_path(".config/aerospace").exists());
    let text = sb.fail(&["restore", "~/.config/aerospace", "-y"]);
    assert!(text.contains("skipped on this machine"), "{text}");
    let text = sb.ok(&["status", "~/.config/aerospace"]);
    assert!(
        text.contains("skipped on this machine (skip pattern"),
        "{text}"
    );

    // Still in the store: listed, and untrackable from here.
    let text = sb.ok(&["list"]);
    assert!(
        text.contains("aerospace.toml skipped on this machine"),
        "{text}"
    );
    assert!(text.contains("1 skipped on this machine"), "{text}");
    let text = sb.ok(&["ignore"]);
    assert!(
        text.contains("skipped on this machine") && text.contains("~/.config/aerospace"),
        "{text}"
    );
    sb.ok(&["ignore", "--here", "--remove", "~/.config/aerospace"]);
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(1));
    sb.ok(&["ignore", "--here", "~/.config/aerospace"]);
    sb.ok(&["untrack", "~/.config/aerospace/aerospace.toml", "-y"]);
    assert!(!sb.store_path(".config/aerospace").exists());
}

#[test]
fn ignore_adds_patterns_and_offers_to_drop_matching_files() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.write_home(".config/nvim/lazy-lock.json", "{}\n");
    sb.ok(&["~/.config/nvim", "-y"]);
    let text = sb.fail(&["untrack", "~/.config/nvim/lazy-lock.json", "-y"]);
    assert!(
        text.contains("run `cubby ignore ~/.config/nvim/lazy-lock.json`"),
        "{text}"
    );

    let text = sb.ok(&["ignore", "lazy-lock.json", "-y"]);
    assert!(
        text.contains("ignoring \"lazy-lock.json\" on every machine"),
        "{text}"
    );
    assert!(
        text.contains("1 file already in the store is ignored now"),
        "{text}"
    );
    assert!(text.contains("removed 1 file from the store"), "{text}");
    assert!(!sb.store_path(".config/nvim/lazy-lock.json").exists());
    assert!(sb.home_path(".config/nvim/lazy-lock.json").exists());
    assert!(sb.manifest().contains("  \"lazy-lock.json\",\n]"));
    let text = sb.ok(&["status"]);
    assert!(!text.contains("lazy-lock"), "{text}");

    // A path the shell expanded is written relative to home.
    let abs = sb.home_path(".config/nvim/scratch.lua");
    sb.ok(&["ignore", abs.to_str().unwrap(), "-y"]);
    assert!(sb.manifest().contains("\"~/.config/nvim/scratch.lua\""));
    sb.ok(&["ignore", "--remove", "~/.config/nvim/scratch.lua"]);
    assert!(!sb.manifest().contains("scratch.lua"));
    let text = sb.fail(&["ignore", "--remove", "never-there"]);
    assert!(text.contains("is not an ignore pattern"), "{text}");
    let text = sb.fail(&["ignore", "["]);
    assert!(text.contains("invalid ignore pattern"), "{text}");
}

#[test]
fn the_last_sync_decides_which_side_changed() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "one\n");
    sb.write_home(".vimrc", "set nu\n");
    sb.write_home(".config/app/a.conf", "a\n");
    sb.ok(&["~/.zshrc", "~/.vimrc", "~/.config/app", "-y"]);

    // Another machine's changes arrive in the store (a git pull, say), and
    // this machine has an edit of its own.
    sb.write_store(".zshrc", "two\n");
    sb.write_store(".config/app/b.conf", "b\n");
    sb.write_home(".vimrc", "set nonu\n");
    let text = sb.ok(&["status"]);
    assert!(
        text.contains(".zshrc") && text.contains("changed in the store"),
        "{text}"
    );
    assert!(
        text.contains(".vimrc") && text.contains("changed at home"),
        "{text}"
    );
    assert!(
        text.contains("- .config/app/b.conf") && text.contains("new in the store"),
        "{text}"
    );

    // Saving takes home's edit and leaves the store's alone; the file from
    // the other machine is not mistaken for one deleted at home.
    let text = sb.ok(&["-y"]);
    assert!(text.contains("~ .vimrc"), "{text}");
    assert!(text.contains("changed in the store; left alone"), "{text}");
    assert_eq!(sb.read_store(".vimrc"), "set nonu\n");
    assert_eq!(sb.read_store(".zshrc"), "two\n");
    assert!(sb.store_path(".config/app/b.conf").exists());

    // Restoring brings the store's changes home.
    sb.ok(&["restore", "-y"]);
    assert_eq!(sb.read_home(".zshrc"), "two\n");
    assert_eq!(sb.read_home(".config/app/b.conf"), "b\n");
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(0));

    // Changed on both sides: a conflict until a side is picked.
    sb.write_home(".zshrc", "three at home\n");
    sb.write_store(".zshrc", "three in the store\n");
    let text = sb.ok(&["status"]);
    assert!(
        text.contains("conflicts\n  ! .zshrc") && text.contains("changed at home and in the store"),
        "{text}"
    );
    let text = sb.fail(&["-y"]);
    assert!(text.contains("use --force"), "{text}");
    sb.fail(&["restore", "-y"]);
    assert_eq!(sb.read_home(".zshrc"), "three at home\n");
    sb.ok(&["save", "--force", "~/.zshrc", "-y"]);
    assert_eq!(sb.read_store(".zshrc"), "three at home\n");

    // Deleted from the store elsewhere: not saved back unless forced.
    fs::remove_file(sb.store_path(".config/app/a.conf")).unwrap();
    let text = sb.ok(&["status"]);
    assert!(
        text.contains("deleted from the store, still at home\n  - .config/app/a.conf"),
        "{text}"
    );
    let text = sb.ok(&["-y"]);
    assert!(
        text.contains("deleted from the store; not added back"),
        "{text}"
    );
    assert!(!sb.store_path(".config/app/a.conf").exists());
    sb.ok(&["save", "--force", "~/.config/app/a.conf", "-y"]);
    assert!(sb.store_path(".config/app/a.conf").exists());

    // Deleted at home after changing in the store: a conflict, not a removal.
    fs::remove_file(sb.home_path(".config/app/b.conf")).unwrap();
    sb.write_store(".config/app/b.conf", "b, edited elsewhere\n");
    let text = sb.fail(&["-y"]);
    assert!(
        text.contains("deleted at home, changed in the store"),
        "{text}"
    );
    assert!(sb.store_path(".config/app/b.conf").exists());
}

#[test]
fn sync_copies_each_change_the_way_it_went() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "one\n");
    sb.write_home(".vimrc", "set nu\n");
    sb.write_home(".config/app/a.conf", "a\n");
    sb.write_home(".config/app/gone.conf", "g\n");
    sb.ok(&["~/.zshrc", "~/.vimrc", "~/.config/app", "-y"]);

    sb.write_store(".zshrc", "two\n");
    sb.write_store(".config/app/b.conf", "b\n");
    sb.write_home(".vimrc", "set nonu\n");
    sb.write_home(".config/app/c.conf", "c\n");
    fs::remove_file(sb.home_path(".config/app/gone.conf")).unwrap();
    let text = sb.ok(&["status"]);
    assert!(text.contains("`cubby sync` does both"), "{text}");

    let text = sb.ok(&["sync", "-n"]);
    assert!(
        text.contains("home → store") && text.contains("store → home"),
        "{text}"
    );
    assert_eq!(sb.read_home(".zshrc"), "one\n");
    let text = sb.ok(&["sync", "-y"]);
    assert!(text.contains("synced 5 changes"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "two\n");
    assert_eq!(sb.read_home(".config/app/b.conf"), "b\n");
    assert_eq!(sb.read_store(".vimrc"), "set nonu\n");
    assert_eq!(sb.read_store(".config/app/c.conf"), "c\n");
    assert!(!sb.store_path(".config/app/gone.conf").exists());
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(0));
    let text = sb.ok(&["sync", "-y"]);
    assert!(text.contains("nothing to sync"), "{text}");

    // Changed on both sides, or different with no record of the last sync:
    // left for the person to decide.
    sb.write_home(".zshrc", "home\n");
    sb.write_store(".zshrc", "store\n");
    sb.write_store(".unknown", "s\n");
    sb.write_home(".unknown", "h\n");
    let text = sb.fail(&["sync", "-y"]);
    assert!(
        text.contains("! .zshrc") && text.contains("changed at home and in the store"),
        "{text}"
    );
    assert!(
        text.contains("! .unknown") && text.contains("no record of the last sync"),
        "{text}"
    );
    assert!(text.contains("2 paths left for you"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "home\n");
    assert_eq!(sb.read_store(".zshrc"), "store\n");
}

#[test]
fn a_new_machine_never_mistakes_store_files_for_deletions() {
    let sb = Sandbox::ready();
    // A store cloned from another machine, tracking ~/.config/app...
    let manifest = sb
        .manifest()
        .replace("dirs = [\n]", "dirs = [\n  \"~/.config/app\",\n]");
    fs::write(sb.store_path(".cubby.toml"), manifest).unwrap();
    sb.write_store(".config/app/settings.conf", "mine\n");
    // ...where the app has already written a default config of its own.
    sb.write_home(".config/app/default.conf", "default\n");

    let text = sb.ok(&["-y"]);
    assert!(text.contains("+ .config/app/default.conf"), "{text}");
    assert!(!text.contains("- .config/app/settings.conf"), "{text}");
    assert!(sb.store_path(".config/app/settings.conf").exists());
    sb.ok(&["restore", "-y"]);
    assert_eq!(sb.read_home(".config/app/settings.conf"), "mine\n");
}

#[test]
fn a_store_made_again_does_not_inherit_the_old_one() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    fs::remove_dir_all(&sb.store).unwrap();
    sb.ok(&["init"]);
    let text = sb.ok(&["~/.zshrc", "-y"]);
    assert!(text.contains("+ .zshrc"), "{text}");
    assert_eq!(sb.read_store(".zshrc"), "z\n");

    // cubby 2's log does not seed a store it never saved to.
    let other = Sandbox::ready();
    other.write_home(".vimrc", "v\n");
    other.write_home(
        ".local/state/cubby/history.log",
        "2026-01-01T00:00:00Z\tsave\t.vimrc\n",
    );
    let text = other.ok(&["~/.vimrc", "-y"]);
    assert!(text.contains("+ .vimrc"), "{text}");
}

#[test]
fn a_file_deleted_from_the_store_is_saved_again_by_name() {
    let sb = Sandbox::ready();
    sb.write_home(".config/app/a.conf", "a\n");
    sb.write_home(".config/app/b.conf", "b\n");
    sb.ok(&["~/.config/app", "-y"]);
    // Another machine removed b.conf from the store.
    fs::remove_file(sb.store_path(".config/app/b.conf")).unwrap();
    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing saved; 1 path left alone"), "{text}");
    assert!(!text.contains("up to date"), "{text}");
    assert!(!sb.store_path(".config/app/b.conf").exists());
    let text = sb.ok(&["~/.config/app/b.conf", "-y"]);
    assert!(text.contains("adding it back"), "{text}");
    assert!(sb.store_path(".config/app/b.conf").exists());

    // Naming a path does not save over a change from another machine.
    sb.write_store(".config/app/a.conf", "from elsewhere\n");
    let text = sb.ok(&["~/.config/app/a.conf", "-y"]);
    assert!(text.contains("changed in the store; left alone"), "{text}");
    assert_eq!(sb.read_store(".config/app/a.conf"), "from elsewhere\n");
}

#[test]
fn upgrading_from_cubby_2_keeps_deletions_working() {
    let sb = Sandbox::ready();
    sb.write_home(".config/app/a.conf", "a\n");
    sb.write_home(".config/app/b.conf", "b\n");
    sb.ok(&["~/.config/app", "-y"]);
    // What cubby 2 leaves behind: the store and a history log, no index.
    let state = sb.home.join(".local/state/cubby");
    fs::remove_dir_all(state.join("index")).unwrap();
    fs::write(
        state.join("history.log"),
        "2026-01-01T00:00:00Z\tsave\t.config/app/a.conf\n\
         2026-01-01T00:00:00Z\tsave\t.config/app/b.conf\n",
    )
    .unwrap();

    fs::remove_file(sb.home_path(".config/app/b.conf")).unwrap();
    let text = sb.ok(&["-y"]);
    assert!(
        text.contains("- .config/app/b.conf") && text.contains("deleted at home"),
        "{text}"
    );
    assert!(!sb.store_path(".config/app/b.conf").exists());
}

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[test]
fn one_cubby_changes_files_at_a_time() {
    use std::os::fd::AsRawFd;
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    let state = sb.home.join(".local/state/cubby");
    fs::create_dir_all(&state).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("lock"))
        .unwrap();
    // LOCK_EX | LOCK_NB, the same on Linux and macOS.
    assert_eq!(unsafe { flock(lock.as_raw_fd(), 2 | 4) }, 0);

    let text = sb.fail(&["~/.zshrc", "-y"]);
    assert!(text.contains("another cubby is running"), "{text}");
    assert!(!sb.store_path(".zshrc").exists());
    // Looking, and dry runs, do not wait.
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(0));
    sb.ok(&["~/.zshrc", "-n"]);
    drop(lock);
    sb.ok(&["~/.zshrc", "-y"]);
    assert!(sb.store_path(".zshrc").exists());
}

#[test]
fn git_is_watched_but_never_driven() {
    let sb = Sandbox::ready();
    let text = sb.ok(&["status"]);
    assert!(!text.contains("store:"), "no nagging without git: {text}");
    git(&sb.store, "init -q");
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    let text = sb.ok(&["status"]);
    assert!(text.contains("store: 2 changes to commit"), "{text}");
    sb.ok(&["git", "add", "-A"]);
    sb.ok(&["git", "commit", "-qm", "dots"]);
    let text = sb.ok(&["status"]);
    assert!(
        text.contains("store: committed (no upstream to push to)"),
        "{text}"
    );
    assert_eq!(sb.ok(&["git", "log", "-n", "1", "--format=%s"]), "dots\n");

    // A global gitignore saved into the store hides other dotfiles from git.
    sb.write_home(".gitignore", "*.local\n");
    sb.write_home(".zshrc.local", "alias x=y\n");
    let text = sb.ok(&["~/.gitignore", "~/.zshrc.local", "-y"]);
    assert!(
        text.contains(".gitignore sits at the root of the store"),
        "{text}"
    );
    assert!(text.contains("git ignores 1 file just saved"), "{text}");
    assert!(
        text.contains(".zshrc.local  *.local in .gitignore:1"),
        "{text}"
    );

    // A store inside a larger repository counts only its own part of it.
    let outer = Sandbox::new();
    git(&outer.home, "init -q");
    outer.ok(&["init"]);
    outer.write_home("notes.txt", "unrelated\n");
    outer.write_home(".zshrc", "z\n");
    outer.ok(&["~/.zshrc", "-y"]);
    let text = outer.ok(&["status"]);
    assert!(text.contains("store: 2 changes to commit"), "{text}");
}

#[test]
fn git_ignore_checks_handle_negations_and_many_files() {
    let sb = Sandbox::ready();
    git(&sb.store, "init -q");
    fs::write(sb.store_path(".gitignore"), "*.conf\n!keep.conf\n").unwrap();
    sb.write_home(".config/app/keep.conf", "k\n");
    sb.write_home(".config/app/other.conf", "o\n");
    let text = sb.ok(&["~/.config/app", "-y"]);
    assert!(text.contains("git ignores 1 file just saved"), "{text}");
    assert!(
        text.contains("other.conf  *.conf in .gitignore:1"),
        "{text}"
    );
    assert!(
        !text.contains("keep.conf in"),
        "a negated rule keeps it: {text}"
    );

    // Enough ignored paths to fill both pipes: this used to hang.
    let long = "a-rather-long-file-name-to-fill-the-pipe-quickly";
    for i in 0..2500 {
        sb.write_home(&format!(".config/many/{long}-{i:04}.conf"), "x\n");
    }
    let mut child = sb_command(&sb, &["~/.config/many", "-y"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("cubby hung checking git's ignore rules");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn init_can_clone_a_store() {
    let a = Sandbox::ready();
    a.write_home(".zshrc", "z\n");
    a.ok(&["~/.zshrc", "-y"]);
    git(&a.store, "init -q && git add -A && git commit -qm dots");

    let b = Sandbox::new();
    let url = format!("file://{}", a.store.display());
    let text = b.ok(&["init", &url]);
    assert!(text.contains("cloned"), "{text}");
    assert!(
        text.contains("+ .zshrc") && text.contains("new in the store"),
        "{text}"
    );
    assert!(text.contains("dry run"), "{text}");
    assert!(!b.home_path(".zshrc").exists());
    let text = b.ok(&["status"]);
    assert!(
        text.contains("store: committed, up to date with origin/"),
        "{text}"
    );
    b.ok(&["restore", "-y"]);
    assert_eq!(b.read_home(".zshrc"), "z\n");

    let text = b.fail(&["init", &url, "--force"]);
    assert!(text.contains("already exists and is not empty"), "{text}");
    let text = b.fail(&["init", "~/a", "~/b"]);
    assert!(text.contains("is not a repository URL"), "{text}");
}

#[test]
fn history_lists_operations() {
    let sb = Sandbox::ready();
    let text = sb.ok(&["history"]);
    assert!(text.contains("no history yet"), "{text}");
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    sb.write_store(".zshrc", "changed\n");
    sb.ok(&["restore", "-y"]);
    let text = sb.ok(&["history"]);
    assert!(text.contains("save      1 change"), "{text}");
    assert!(text.contains("restore   1 change · 1 backed up"), "{text}");
    assert!(text.contains("2 runs shown"), "{text}");
    let text = sb.ok(&["history", "--op", "restore"]);
    assert!(!text.contains("save "), "{text}");
    let text = sb.ok(&["history", "-c", "1"]);
    assert!(text.contains("1 run shown of 2"), "{text}");
    let text = sb.ok(&["history", "-v"]);
    assert!(text.contains("~ .zshrc overwrite at home"), "{text}");
    let text = sb.fail(&["history", "--op", "sav"]);
    assert!(text.contains("possible values"), "{text}");

    // cubby 2's log is pointed to, not lost.
    fs::write(
        sb.home.join(".local/state/cubby/history.log"),
        "2026-01-01T00:00:00Z\tsave\t.zshrc\n",
    )
    .unwrap();
    let text = sb.ok(&["history"]);
    assert!(text.contains("earlier history, from cubby 2"), "{text}");
}

#[test]
fn undo_reverses_the_last_run() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "mine\n");
    sb.write_home(".config/app/a.conf", "a\n");
    sb.ok(&["~/.zshrc", "~/.config/app", "-y"]);

    // A forced restore overwrites a local edit and creates a file...
    sb.write_home(".zshrc", "local edit\n");
    sb.write_store(".zshrc", "from elsewhere\n");
    sb.write_store(".config/app/new.conf", "n\n");
    let text = sb.ok(&["restore", "--force", "-y"]);
    assert!(text.contains("`cubby undo` reverses this"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "from elsewhere\n");

    // ...and undo puts the edit back and takes the new file away.
    let text = sb.ok(&["undo", "-y"]);
    assert!(text.contains("put back at home"), "{text}");
    assert!(text.contains("created at home by that run"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "local edit\n");
    assert!(!sb.home_path(".config/app/new.conf").exists());
    assert!(
        sb.home_path(".config/app").exists(),
        "home directories stay"
    );
    // The last sync is as it was, so the state is as it was.
    let text = sb.ok(&["status"]);
    assert!(text.contains("changed at home and in the store"), "{text}");
    assert!(text.contains("new in the store"), "{text}");

    // Undoing a save that tracked a directory untracks it again.
    sb.write_home(".config/fish/config.fish", "set x\n");
    sb.ok(&["~/.config/fish", "-y"]);
    assert!(sb.manifest().contains("fish"));
    sb.ok(&["undo", "-y"]);
    assert!(!sb.manifest().contains("fish"), "{}", sb.manifest());
    assert!(!sb.store_path(".config/fish").exists());

    // What changed again since is left alone.
    sb.write_home(".vimrc", "v1\n");
    sb.ok(&["~/.vimrc", "-y"]);
    sb.write_store(".vimrc", "v2\n");
    let text = sb.fail(&["undo", "-y"]);
    assert!(text.contains("changed since"), "{text}");
    assert_eq!(sb.read_store(".vimrc"), "v2\n");

    let text = sb.ok(&["history"]);
    assert!(text.contains("undone"), "{text}");
    assert!(text.contains("reverses"), "{text}");
    let text = sb.fail(&["undo", "nope"]);
    assert!(text.contains("no run nope"), "{text}");
}

#[test]
fn backups_are_listed_and_found_by_path() {
    let sb = Sandbox::ready();
    let text = sb.ok(&["backups"]);
    assert!(text.contains("no backups yet"), "{text}");
    sb.write_home(".zshrc", "one\n");
    sb.ok(&["~/.zshrc", "-y"]);
    sb.write_home(".zshrc", "two\n");
    sb.ok(&["-y"]);
    sb.write_home(".zshrc", "three\n");
    sb.ok(&["-y"]);
    // cubby 2 kept its copies elsewhere; they are found too.
    sb.write_home(
        ".local/state/cubby/backups/20200101-000000-restore/.zshrc",
        "ancient\n",
    );

    let text = sb.ok(&["backups"]);
    assert_eq!(
        text.lines().filter(|l| l.contains("  save  ")).count(),
        2,
        "{text}"
    );
    assert!(text.contains("1 file, 4 B"), "{text}");
    assert!(text.contains("20200101-000000-restore"), "{text}");
    let text = sb.ok(&["backups", "~/.zshrc"]);
    assert!(
        text.contains("3 copies of ~/.zshrc, newest first"),
        "{text}"
    );
    let newest = text
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .last()
        .unwrap();
    let newest = sb.home.join(newest.strip_prefix("~/").unwrap());
    assert_eq!(fs::read_to_string(newest).unwrap(), "two\n");
}

#[test]
fn store_can_be_chosen_by_flag_and_env() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    let alt = sb.home.join("alt-store");
    fs::create_dir_all(&alt).unwrap();
    sb.ok(&["--store", alt.to_str().unwrap(), "~/.zshrc", "-y"]);
    assert!(alt.join(".zshrc").exists());
    assert!(!sb.store_path(".zshrc").exists());

    let out = Command::new(env!("CARGO_BIN_EXE_cubby"))
        .args(["list", "--plain"])
        .env("CUBBY_HOME", &sb.home)
        .env("CUBBY_STORE", &alt)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), ".zshrc\n");

    let text = sb.fail(&["--store", "~/nowhere", "status"]);
    assert!(text.contains("store ~/nowhere"), "{text}");
    assert!(text.contains("does not exist"), "{text}");
    // A quoted tilde is expanded like an unquoted one.
    let text = sb.ok(&["--store", "~/alt-store", "list", "--plain"]);
    assert_eq!(text, ".zshrc\n");
}

#[test]
fn init_twice_with_a_symlinked_store() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home_path("Dropbox/dots")).unwrap();
    std::os::unix::fs::symlink("Dropbox/dots", sb.home_path(".dotfiles")).unwrap();
    sb.ok(&["init"]);
    let text = sb.ok(&["init"]);
    assert!(text.contains("config already at"), "{text}");
    let config = fs::read_to_string(sb.home.join(".config/cubby/config.toml")).unwrap();
    assert!(config.contains("store = \"~/.dotfiles\""), "{config}");
}

#[test]
fn relative_store_in_config_is_relative_to_home() {
    let sb = Sandbox::new();
    write(
        &sb.home.join(".config/cubby/config.toml"),
        "store = \"dots\"\n",
    );
    sb.write_home("dots/.zshrc", "z\n");
    fs::create_dir_all(sb.home_path("elsewhere")).unwrap();
    // Even with CUBBY_STORE set but empty, and run from another directory.
    let out = Command::new(env!("CARGO_BIN_EXE_cubby"))
        .args(["list", "--plain"])
        .env("CUBBY_HOME", &sb.home)
        .env("CUBBY_STORE", "")
        .env("NO_COLOR", "1")
        .current_dir(sb.home_path("elsewhere"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), ".zshrc\n");
}

#[test]
fn large_previews_are_capped() {
    let sb = Sandbox::ready();
    for i in 0..45 {
        sb.write_home(&format!(".config/many/file{i:02}"), "x\n");
    }
    let text = sb.ok(&["~/.config/many", "-n"]);
    assert!(text.contains("and 5 more"), "{text}");
    let text = sb.ok(&["~/.config/many", "-n", "-v"]);
    assert!(text.contains("file44"), "{text}");
    assert!(!text.contains("more"), "{text}");
}

#[test]
fn global_flags_work_before_the_command() {
    let sb = Sandbox::ready();
    sb.write_store(".zshrc", "store\n");
    sb.write_home(".zshrc", "home\n");
    let text = sb.ok(&["-n", "restore"]);
    assert!(text.contains("dry run"), "{text}");
    assert_eq!(sb.read_home(".zshrc"), "home\n");
    let text = sb.ok(&["-v", "--color", "never", "status"]);
    assert!(text.contains("~ .zshrc"), "{text}");
    let text = sb.ok(&["--store", "~/.dotfiles", "list", "--plain"]);
    assert_eq!(text, ".zshrc\n");

    let text = sb.fail(&["~/.zshrc", "status"]);
    assert!(text.contains("~/status does not exist"), "{text}");
    let text = sb.fail(&["--force", "status"]);
    assert!(text.contains("go after the command"), "{text}");
    let text = sb.fail(&["statu"]);
    assert!(text.contains("did you mean `cubby status`?"), "{text}");
    let text = sb.fail(&["~/statu"]);
    assert!(!text.contains("did you mean"), "{text}");
}

#[test]
fn large_files_are_pointed_out() {
    let sb = Sandbox::ready();
    sb.write_home(".config/app/settings.json", "{}\n");
    sb.write_home(".config/app/cache.db", &"x".repeat(6 * 1024 * 1024));
    let text = sb.ok(&["~/.config/app", "-n"]);
    assert!(
        text.contains("cache.db") && text.contains("large: 6.0 MiB"),
        "{text}"
    );
    assert!(text.contains("1 file over 5.0 MiB marked above"), "{text}");
    let small = text.lines().find(|l| l.contains("settings.json")).unwrap();
    assert!(!small.contains("large"), "{text}");
}

#[test]
fn doctor_finds_what_status_cannot() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    // Problems an older cubby, or a person, could have left behind.
    sb.write_store(".local/state/cubby/history.log", "old\n");
    sb.write_store(
        ".config/gh/hosts.yml",
        &format!("oauth_token: ghp_{}\n", "a1B2".repeat(9)),
    );
    sb.write_store(".cache.db", &"x".repeat(6 * 1024 * 1024));
    sb.write_store(".netrc", "machine x password y\n");
    let manifest = sb
        .manifest()
        .replace("dirs = [\n]", "dirs = [\n  \"~/.config/gone\",\n]")
        + "\n[modes]\n\"~/.netrc\" = \"600\"\n";
    fs::write(sb.store_path(".cubby.toml"), manifest).unwrap();

    let text = sb.fail(&["doctor"]);
    for expected in [
        "the store is not versioned with git",
        "the store holds cubby's own files",
        "1 file in the store looks like a secret",
        ".config/gh/hosts.yml  it holds what looks like a GitHub token",
        "1 file in the store is over 5.0 MiB",
        "copies of private files in the store can be read by other users",
        "`chmod 600 ~/.dotfiles/.netrc`",
        "tracked directories with nothing in them anywhere",
        "~/.config/gone",
    ] {
        assert!(text.contains(expected), "{expected:?} in\n{text}");
    }

    // A tidy store, committed and pushed, passes.
    let tidy = Sandbox::ready();
    tidy.write_home(".zshrc", "z\n");
    tidy.ok(&["~/.zshrc", "-y"]);
    git(&tidy.home, "init -q --bare remote.git");
    git(
        &tidy.store,
        "init -q && git add -A && git commit -qm dots && git remote add origin ../remote.git && git push -q -u origin HEAD",
    );
    let text = tidy.ok(&["doctor"]);
    assert!(text.contains("committed and pushed"), "{text}");
    assert!(text.contains("no problems found"), "{text}");
}

#[test]
fn completion_and_version() {
    let sb = Sandbox::new();
    let text = sb.ok(&["completion", "zsh"]);
    assert!(text.contains("#compdef cubby"), "{text}");
    let text = sb.ok(&["--version"]);
    assert!(text.starts_with("cubby "), "{text}");
    let text = sb.ok(&["--help"]);
    assert!(text.contains("restore"), "{text}");
    assert!(!text.contains("completion"), "hidden: {text}");
}

#[test]
fn empty_tracked_directory_is_treated_as_absent() {
    let sb = Sandbox::ready();
    sb.write_home(".config/app/a.conf", "a\n");
    sb.write_home(".config/app/b.conf", "b\n");
    sb.ok(&["~/.config/app", "-y"]);
    // An unmounted volume or a wiped config leaves the directory in place but empty.
    fs::remove_file(sb.home_path(".config/app/a.conf")).unwrap();
    fs::remove_file(sb.home_path(".config/app/b.conf")).unwrap();

    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing to save"), "{text}");
    assert!(
        text.contains("~/.config/app exists at home but has no files"),
        "{text}"
    );
    assert!(sb.store_path(".config/app/a.conf").exists());
    assert!(sb.store_path(".config/app/b.conf").exists());

    let text = sb.ok(&["status"]);
    assert!(
        text.contains("tracked directory, empty at home; store copy kept"),
        "{text}"
    );
    assert!(!text.contains("deleted at home"), "{text}");

    // Deleting some files is still mirrored; only a completely empty directory is suspect.
    sb.write_home(".config/app/a.conf", "a\n");
    let text = sb.ok(&["-y"]);
    assert!(text.contains("- .config/app/b.conf"), "{text}");
    assert!(!sb.store_path(".config/app/b.conf").exists());
    assert!(sb.store_path(".config/app/a.conf").exists());

    // Restore still brings everything back into an empty directory.
    fs::remove_file(sb.home_path(".config/app/a.conf")).unwrap();
    let text = sb.ok(&["restore", "-y"]);
    assert!(text.contains("+ .config/app/a.conf"), "{text}");
}

#[test]
fn paths_are_tracked_as_typed_through_symlinked_parents() {
    let sb = Sandbox::ready();
    sb.write_home("Dropbox/myapp/sub/a.conf", "a\n");
    std::os::unix::fs::symlink("Dropbox/myapp", sb.home_path(".myapp")).unwrap();

    let text = sb.ok(&["~/.myapp/sub", "-y"]);
    assert!(
        text.contains("tracking ~/.myapp/sub as a directory"),
        "{text}"
    );
    assert!(text.contains("+ .myapp/sub/a.conf"), "{text}");
    assert!(sb.store_path(".myapp/sub/a.conf").exists());
    assert!(!sb.store_path("Dropbox").exists());

    // Round trip through the same typed path, and a lone file through the link.
    sb.write_home("Dropbox/myapp/sub/a.conf", "edited\n");
    sb.write_home("Dropbox/myapp/top.conf", "t\n");
    sb.ok(&["~/.myapp/top.conf", "-y"]);
    assert!(sb.store_path(".myapp/top.conf").exists());
    let text = sb.ok(&["status"]);
    assert!(text.contains("~ .myapp/sub/a.conf"), "{text}");
    // A restore over the symlinked path lands in the real directory (forced:
    // the file changed on both sides).
    sb.write_store(".myapp/sub/a.conf", "from store\n");
    let text = sb.fail(&["restore", "-y", "~/.myapp/sub/a.conf"]);
    assert!(text.contains("changed at home and in the store"), "{text}");
    sb.ok(&["restore", "--force", "-y", "~/.myapp/sub/a.conf"]);
    assert_eq!(sb.read_home("Dropbox/myapp/sub/a.conf"), "from store\n");

    // The real location is still what the store check looks at.
    std::os::unix::fs::symlink(".dotfiles", sb.home_path("dots")).unwrap();
    let text = sb.fail(&["~/dots/.myapp/top.conf", "-y"]);
    assert!(text.contains("inside the store"), "{text}");
    std::os::unix::fs::symlink("dots/.myapp/top.conf", sb.home_path(".linked")).unwrap();
    let text = sb.fail(&["~/.linked", "-y"]);
    assert!(text.contains("symlink into the store"), "{text}");
    assert!(!sb.store_path(".linked").exists());
}

#[test]
fn unreadable_files_are_errors_not_new() {
    if unsafe { libc_geteuid() } == 0 {
        return; // root can read anything
    }
    let sb = Sandbox::ready();
    sb.write_home(".config/app/ok.conf", "ok\n");
    sb.ok(&["~/.config/app", "-y"]);
    sb.write_home(".config/app/secret", "s\n");
    fs::set_permissions(
        sb.home_path(".config/app/secret"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let text = sb.ok(&["status"]);
    assert!(text.contains("errors\n  ! .config/app/secret"), "{text}");
    assert!(text.contains("cannot read"), "{text}");
    assert!(!text.contains("new at home"), "{text}");

    let text = sb.fail(&["-y"]);
    assert!(text.contains("nothing saved; 1 path skipped"), "{text}");
    assert!(text.contains("! .config/app/secret"), "{text}");
    assert!(!sb.store_path(".config/app/secret").exists());
    fs::set_permissions(
        sb.home_path(".config/app/secret"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
}

unsafe extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[test]
fn unicode_normalization_forms_are_one_file() {
    let sb = Sandbox::ready();
    let nfc = "caf\u{e9}";
    let nfd = "cafe\u{301}";
    sb.write_home(".tracked/plain", "p\n");
    sb.ok(&["~/.tracked", "-y"]);
    // The store (say, from git on Linux) has the composed name; home (say,
    // an older macOS app) has the decomposed one.
    sb.write_store(&format!(".tracked/{nfc}"), "store\n");
    sb.write_home(&format!(".tracked/{nfd}"), "home\n");

    let text = sb.ok(&["status"]);
    assert_eq!(
        text.matches("caf").count(),
        1,
        "one entry, not two:\n{text}"
    );
    assert!(text.contains(&format!("~ .tracked/{nfc}")), "{text}");

    sb.ok(&["-y"]);
    let text = sb.ok(&["list", "--plain"]);
    assert_eq!(text.matches("caf").count(), 1, "{text}");
    assert_eq!(sb.read_store(&format!(".tracked/{nfc}")), "home\n");

    // Restore writes back to the name that exists at home.
    sb.write_store(&format!(".tracked/{nfc}"), "store2\n");
    sb.ok(&["restore", "-y"]);
    let text = sb.ok(&["list", "--plain"]);
    assert_eq!(text.matches("caf").count(), 1, "{text}");
    let home_files: Vec<String> = fs::read_dir(sb.home_path(".tracked"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("caf"))
        .collect();
    assert_eq!(home_files.len(), 1, "{home_files:?}");
    assert_eq!(
        fs::read_to_string(sb.home_path(&format!(".tracked/{nfd}"))).unwrap(),
        "store2\n"
    );
}

#[test]
fn non_utf8_names_are_skipped_not_fatal() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let sb = Sandbox::ready();
    sb.write_home(".config/app/ok.conf", "ok\n");
    sb.ok(&["~/.config/app", "-y"]);
    let bad = sb
        .home_path(".config/app")
        .join(OsStr::from_bytes(b"bad\xff.conf"));
    if fs::write(&bad, "x").is_err() {
        return; // the filesystem refuses such names (APFS does)
    }
    let text = sb.ok(&["status"]);
    assert!(text.contains("skipped"), "{text}");
    assert!(text.contains("not valid UTF-8"), "{text}");
    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing to save"), "{text}");
    fs::write(
        sb.store_path(".config/app")
            .join(OsStr::from_bytes(b"bad\xff.conf")),
        "x",
    )
    .unwrap();
    let text = sb.ok(&["list", "--plain"]);
    assert_eq!(text, ".config/app/ok.conf\n");
}

#[test]
fn cubbys_own_config_and_state_are_never_tracked() {
    let sb = Sandbox::ready();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.write_home(".local/bin/tool", "#!/bin/sh\n");
    let text = sb.ok(&["~/.config", "~/.local", "-y"]);
    assert!(!text.contains("cubby/"), "{text}");
    assert!(!sb.store_path(".config/cubby").exists());

    // Saving writes history and backups under ~/.local/state/cubby; none of
    // that may feed the next run.
    sb.write_home(".config/nvim/init.lua", "b\n");
    sb.ok(&["-y"]);
    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing to save"), "{text}");
    assert!(!sb.store_path(".local/state").exists());
    let text = sb.ok(&["status"]);
    assert!(text.contains("up to date"), "{text}");

    let text = sb.fail(&["~/.config/cubby/config.toml", "-y"]);
    assert!(text.contains("stays on this machine"), "{text}");
}

#[test]
fn manifest_comments_survive_and_version_is_added() {
    let sb = Sandbox::ready();
    // A cubby 2 manifest, with notes someone wrote.
    fs::write(
        sb.store_path(".cubby.toml"),
        "# my notes\ndirs = [\n]\n\nignore = [\n  # neovim plugin lock file\n  \"lazy-lock.json\",\n]\n",
    )
    .unwrap();
    sb.write_home(".config/nvim/init.lua", "a\n");
    sb.ok(&["~/.config/nvim", "-y"]);
    let m = sb.manifest();
    assert!(m.starts_with("# my notes\nversion = 2\n"), "{m}");
    assert!(
        m.contains("# neovim plugin lock file\n  \"lazy-lock.json\""),
        "{m}"
    );
    assert!(m.contains("dirs = [\n  \"~/.config/nvim\",\n]"), "{m}");

    // A store written by a newer cubby asks for an upgrade.
    fs::write(sb.store_path(".cubby.toml"), "version = 9\nshiny = 1\n").unwrap();
    let text = sb.fail(&["status"]);
    assert!(text.contains("upgrade cubby"), "{text}");
}

#[test]
fn unusual_directory_names_keep_the_manifest_readable() {
    let sb = Sandbox::ready();
    let name = ".config/notes-\u{1F469}\u{200D}\u{1F4BB} \"quoted\"";
    sb.write_home(&format!("{name}/a.md"), "hi\n");
    sb.ok(&[&format!("~/{name}"), "-y"]);
    let text = sb.ok(&["status"]);
    assert!(text.contains("1 tracked file up to date"), "{text}");
    let text = sb.ok(&["list"]);
    assert!(text.contains("(tracked directory)"), "{text}");
}

#[test]
fn status_quiet_uses_exit_codes() {
    let sb = Sandbox::new();
    let out = sb.cmd(&["status", "-q"]);
    assert_eq!(out.status.code(), Some(2), "no store is an error");
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());

    sb.ok(&["init"]);
    let out = sb.cmd(&["status", "-q"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());

    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    assert_eq!(sb.cmd(&["status", "--quiet"]).status.code(), Some(0));

    sb.write_home(".zshrc", "changed\n");
    let out = sb.cmd(&["status", "-q"]);
    assert_eq!(out.status.code(), Some(1), "modified");
    assert!(out.stdout.is_empty() && out.stderr.is_empty());

    sb.ok(&["-y"]);
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(0));
    fs::remove_file(sb.home_path(".zshrc")).unwrap();
    assert_eq!(
        sb.cmd(&["status", "-q"]).status.code(),
        Some(1),
        "missing at home"
    );
    sb.ok(&["restore", "-y"]);

    // An untracked file named explicitly is not "dirty"; a bad path is an error.
    sb.write_home(".other", "o\n");
    assert_eq!(sb.cmd(&["status", "-q", "~/.other"]).status.code(), Some(0));
    assert_eq!(
        sb.cmd(&["status", "-q", "/etc/hosts"]).status.code(),
        Some(2)
    );

    // Anything that stops cubby from telling is 2, not "differs".
    let manifest = sb.manifest();
    fs::write(sb.store_path(".cubby.toml"), "<<<<<<< HEAD\n").unwrap();
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(2));
    fs::write(sb.store_path(".cubby.toml"), "version = 9\n").unwrap();
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(2));
    fs::write(sb.store_path(".cubby.toml"), manifest).unwrap();

    // A tracked directory missing at home counts as dirty.
    sb.write_home(".config/app/a.conf", "a\n");
    sb.ok(&["~/.config/app", "-y"]);
    fs::remove_dir_all(sb.home_path(".config/app")).unwrap();
    assert_eq!(sb.cmd(&["status", "-q"]).status.code(), Some(1));
}
