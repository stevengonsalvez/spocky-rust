#![cfg(target_os = "macos")]

use std::{
    fs,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

const DEEP_LINK: &str = "paseo://h/server%2Fmain/agent/agent%20123";

#[test]
fn embedded_host_exercises_real_webview_boundaries() {
    let output = run_host(&["--self-test", "--deep-link", DEEP_LINK]);

    assert!(output.status.success(), "{}", stderr(&output));
    let events = events(&output);
    assert_event(&events, "host.started", |event| {
        event["engine"] == "WKWebView" && event["resumed"] == false
    });
    assert_event(&events, "guest.loaded", |event| {
        event["hostId"] == "host-a" && event["uiRendered"] == true
    });
    assert_event(&events, "navigation.denied", |event| {
        event["url"]
            .as_str()
            .is_some_and(|url| url.contains("/host-b/private"))
    });
    assert_event(&events, "navigation.allowed", |event| {
        event["url"]
            .as_str()
            .is_some_and(|url| url.contains("/host-a/next"))
    });
    assert_event(&events, "deep_link.delivered", |event| {
        event["serverId"] == "server/main" && event["agentId"] == "agent 123"
    });
    assert_event(&events, "download.completed", |event| {
        event["success"] == true && event["bytes"] == 18
    });
    assert_event(&events, "host.ready", |_| true);
}

#[test]
fn embedded_host_recovers_after_process_failure() {
    let checkpoint = unique_checkpoint();
    let checkpoint_arg = checkpoint.to_string_lossy().into_owned();

    let failed = run_host(&[
        "--self-test",
        "--deep-link",
        DEEP_LINK,
        "--checkpoint",
        &checkpoint_arg,
        "--fail-before-ready",
    ]);
    assert!(!failed.status.success());
    assert_event(&events(&failed), "host.failed", |event| {
        event["reason"] == "injected_failure"
    });
    assert!(checkpoint.exists());

    let restarted = run_host(&["--self-test", "--checkpoint", &checkpoint_arg]);
    assert!(restarted.status.success(), "{}", stderr(&restarted));
    assert_event(&events(&restarted), "host.started", |event| {
        event["resumed"] == true
    });
    assert_event(&events(&restarted), "host.ready", |_| true);

    fs::remove_file(checkpoint).expect("checkpoint cleans up");
}

fn run_host(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_paseo-browser-host"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("embedded host starts");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().expect("host status is readable").is_some() {
            return child.wait_with_output().expect("host output is readable");
        }
        if Instant::now() >= deadline {
            child.kill().expect("timed-out host terminates");
            let output = child
                .wait_with_output()
                .expect("timed-out output is readable");
            panic!("embedded host timed out: {}", stderr(&output));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn events(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .expect("host stdout is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("host output is JSON lines"))
        .collect()
}

fn assert_event(events: &[Value], operation: &str, predicate: impl Fn(&Value) -> bool) {
    assert!(
        events
            .iter()
            .any(|event| event["operation"] == operation && predicate(event)),
        "missing {operation} in {events:#?}"
    );
}

fn unique_checkpoint() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "paseo-browser-host-{}-{nonce}.json",
        std::process::id()
    ))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
