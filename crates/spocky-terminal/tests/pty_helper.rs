//! The PTY child helper starts its target with default signal dispositions:
//! a `SIGINT` that the helper's own parent ignores (a background job, `nohup`)
//! must still kill the target, as it does under node-pty.

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

#[test]
fn the_target_does_not_inherit_an_ignored_sigint() {
    let helper = env!("CARGO_BIN_EXE_spocky-pty-helper");
    // `trap '' INT` makes the shell ignore SIGINT, and exec keeps it ignored
    // for the helper; the helper must undo that before it execs the target.
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "trap '' INT; exec \"$0\" . /bin/sh -c 'kill -INT $$; echo survived'",
            helper,
        ])
        .output()
        .expect("run helper");
    assert_eq!(output.status.signal(), Some(2), "target must die of SIGINT");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("survived"));
}
