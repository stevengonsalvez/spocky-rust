//! Finishes PTY child setup between `/usr/bin/env -i` and the target
//! program: `spocky-pty-helper <cwd> <file> [args...]`. See
//! `spocky_terminal::pty`.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(spocky_terminal::pty::run_helper(&args));
}
