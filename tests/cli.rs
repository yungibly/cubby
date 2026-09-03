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
        Command::new(env!("CARGO_BIN_EXE_cubby"))
            .args(args)
            .env("CUBBY_HOME", &self.home)
            .env_remove("CUBBY_STORE")
            .env_remove("CUBBY_PAGER")
            .env("PAGER", "cat")
            .env("NO_COLOR", "1")
            .current_dir(&self.home)
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

    fn backups(&self) -> Vec<PathBuf> {
        let dir = self.home.join(".local/state/cubby/backups");
        let mut sets: Vec<PathBuf> = match fs::read_dir(dir) {
            Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
            Err(_) => Vec::new(),
        };
        sets.sort();
        sets
    }

    fn history(&self) -> String {
        fs::read_to_string(self.home.join(".local/state/cubby/history.log")).unwrap_or_default()
    }
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
    assert!(sb.history().contains("\tsave\t.zshrc"));
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
            .contains("\tremove\t.config/nvim/lua/keymaps.lua")
    );

    // The removed file was backed up.
    let sets = sb.backups();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert!(
        sets[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-save")
    );
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
    assert!(sb.history().contains("\trestore\t.zshrc"));

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
    assert!(sb.history().contains("\tuntrack\t.zshrc"));
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
    assert!(text.contains("(symlink)"), "{text}");
    assert!(text.contains("- ../real/theme.conf"), "{text}");
    assert!(text.contains("+ ../real/other.conf"), "{text}");

    sb.ok(&["restore", "-y"]);
    assert_eq!(
        fs::read_link(sb.home_path(".config/theme.conf")).unwrap(),
        PathBuf::from("../real/theme.conf")
    );
}

#[test]
fn conflicts_are_skipped_unless_forced() {
    let sb = Sandbox::ready();
    sb.write_home(".zshrc", "file\n");
    sb.ok(&["~/.zshrc", "-y"]);
    fs::remove_file(sb.home_path(".zshrc")).unwrap();
    std::os::unix::fs::symlink("elsewhere", sb.home_path(".zshrc")).unwrap();

    let text = sb.ok(&["-y"]);
    assert!(text.contains("nothing to save"), "{text}");
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
    assert!(text.contains("~/.zshrc"), "{text}");
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
    assert!(sb.history().is_empty());

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
        text.contains("is ignored (pattern \"lazy-lock.json\""),
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
fn history_lists_operations() {
    let sb = Sandbox::ready();
    let text = sb.ok(&["history"]);
    assert!(text.contains("no history yet"), "{text}");
    sb.write_home(".zshrc", "z\n");
    sb.ok(&["~/.zshrc", "-y"]);
    sb.write_store(".zshrc", "changed\n");
    sb.ok(&["restore", "-y"]);
    let text = sb.ok(&["history"]);
    assert!(text.contains("save      .zshrc"), "{text}");
    assert!(text.contains("restore   .zshrc"), "{text}");
    assert!(text.contains("2 entries shown"), "{text}");
    let text = sb.ok(&["history", "--op", "restore"]);
    assert!(!text.contains("save "), "{text}");
    let text = sb.ok(&["history", "-c", "1"]);
    assert!(text.contains("1 entry shown of 2"), "{text}");
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
    assert!(text.contains("does not exist"), "{text}");
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
