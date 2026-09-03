mod backup;
mod cli;
mod config;
mod diff;
mod fsx;
mod history;
mod ignore;
mod manifest;
mod paths;
mod plan;
mod scan;
mod testutil;
mod ui;

fn main() {
    // Let `cubby list | head` end quietly instead of panicking on a closed pipe.
    // SAFETY: resetting a signal disposition at startup, before any threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    std::process::exit(cli::run());
}
