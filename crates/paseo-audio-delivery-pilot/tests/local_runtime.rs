use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use paseo_audio_delivery_pilot::{
    LocalDeliveryRuntime, ProcessCommand, create_pcm16_wav, create_unsigned_package,
};

fn temp_directory(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "paseo-audio-delivery-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock follows Unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("temporary directory is created");
    path
}

fn cleanup(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("temporary directory is removed");
    }
}

#[test]
fn process_adapter_captures_real_stdout_stderr_and_exit_status() {
    let result = ProcessCommand::new("/bin/sh")
        .args(["-c", "printf runtime-out; printf runtime-err >&2; exit 7"])
        .run()
        .expect("shell launches");

    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.stdout, b"runtime-out");
    assert_eq!(result.stderr, b"runtime-err");
}

#[test]
fn process_adapter_drains_large_stdout_and_stderr_without_deadlock() {
    let result = ProcessCommand::new("/bin/sh")
        .args([
            "-c",
            "dd if=/dev/zero bs=1024 count=128 2>/dev/null; dd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null",
        ])
        .timeout(Duration::from_secs(2))
        .run()
        .expect("large output process completes");

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.len(), 128 * 1024);
    assert_eq!(result.stderr.len(), 128 * 1024);
}

#[test]
fn process_adapter_times_out_and_reaps_the_spawned_process_group() {
    let root = temp_directory("process-timeout");
    let child_pid = root.join("child.pid");
    let script = format!(
        "sleep 30 & child=$!; printf %s $child > '{}'; wait $child",
        child_pid.display()
    );
    let started = Instant::now();
    let error = ProcessCommand::new("/bin/sh")
        .args(["-c", &script])
        .timeout(Duration::from_millis(150))
        .run()
        .expect_err("long-running process times out");

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "process timed out after 150 ms: /bin/sh");
    assert!(started.elapsed() < Duration::from_secs(2));

    let pid = fs::read_to_string(&child_pid).expect("child pid is recorded");
    let probe = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("process probe runs");
    assert!(!probe.success(), "timed-out child process remains alive");

    cleanup(&root);
}

#[test]
fn pcm_runtime_writes_a_real_wav_that_the_host_audio_tool_parses() {
    let root = temp_directory("pcm");
    let wav = root.join("silence.wav");

    let metadata = create_pcm16_wav(&wav, 16_000, &[0; 1_600]).expect("wav is written");
    let probe = ProcessCommand::new("/usr/bin/afinfo")
        .arg(&wav)
        .run()
        .expect("afinfo launches");

    assert_eq!(metadata.sample_rate, 16_000);
    assert_eq!(metadata.channels, 1);
    assert_eq!(metadata.samples, 1_600);
    assert_eq!(metadata.file_bytes, 3_244);
    assert_eq!(probe.exit_code, Some(0));
    assert!(String::from_utf8_lossy(&probe.stdout).contains("16000 Hz"));

    cleanup(&root);
}

#[test]
fn unsigned_packages_update_and_rollback_on_the_real_filesystem() {
    let root = temp_directory("delivery");
    let package_100 = root.join("paseo-1.0.0-unsigned");
    let package_110 = root.join("paseo-1.1.0-unsigned");
    let corrupt_120 = root.join("paseo-1.2.0-unsigned");
    create_unsigned_package(&package_100, "1.0.0", b"payload-v1")
        .expect("first package is created");
    create_unsigned_package(&package_110, "1.1.0", b"payload-v2")
        .expect("second package is created");
    create_unsigned_package(&corrupt_120, "1.2.0", b"payload-v3")
        .expect("corrupt package starts valid");
    fs::write(corrupt_120.join("payload.bin"), b"tampered").expect("package is corrupted");

    let mut runtime = LocalDeliveryRuntime::new(root.join("installation"));
    let installed = runtime
        .install(&package_100)
        .expect("first install succeeds");
    assert_eq!(installed.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(fs::read(runtime.active_payload()).unwrap(), b"payload-v1");

    let failed = runtime
        .update(&corrupt_120)
        .expect_err("corrupt update fails validation");
    assert_eq!(failed.to_string(), "payload checksum mismatch");
    assert_eq!(runtime.active_version().unwrap().as_deref(), Some("1.0.0"));
    assert_eq!(fs::read(runtime.active_payload()).unwrap(), b"payload-v1");

    let updated = runtime.update(&package_110).expect("valid update succeeds");
    assert_eq!(updated.active_version.as_deref(), Some("1.1.0"));
    assert_eq!(fs::read(runtime.active_payload()).unwrap(), b"payload-v2");

    let rolled_back = runtime.rollback().expect("rollback succeeds");
    assert_eq!(rolled_back.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(fs::read(runtime.active_payload()).unwrap(), b"payload-v1");

    let uninstalled = runtime.uninstall(true).expect("uninstall succeeds");
    assert_eq!(uninstalled.active_version, None);
    assert_eq!(
        fs::read(runtime.retained_state_path()).unwrap(),
        b"payload-v1"
    );
    assert!(!runtime.active_payload().exists());

    cleanup(&root);
}
