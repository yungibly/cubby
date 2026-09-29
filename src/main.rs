mod cli;
mod config;
mod diff;
mod fsx;
mod ignore;
mod index;
mod lock;
mod manifest;
mod paths;
mod perms;
mod plan;
mod runs;
mod scan;
mod secrets;
mod testutil;
mod tomlx;
mod ui;

fn main() {
    // Let `cubby list | head` end quietly instead of panicking on a closed pipe.
    // SAFETY: resetting a signal disposition at startup, before any threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    std::process::exit(cli::run());
}
