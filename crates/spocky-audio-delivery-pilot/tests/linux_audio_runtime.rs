#![cfg(target_os = "linux")]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use spocky_audio_delivery_pilot::{LinuxAudioAdapter, ProcessCommand};

fn temp_directory(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "spocky-linux-audio-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
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
fn process_adapter_passes_explicit_audio_environment() {
    let result = ProcessCommand::new("/usr/bin/env")
        .env("SPOCKY_AUDIO_CAPTURE", "deterministic")
        .run()
        .expect("environment probe runs");

    assert_eq!(result.exit_code, Some(0));
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .lines()
            .any(|line| line == "SPOCKY_AUDIO_CAPTURE=deterministic")
    );
}

#[test]
fn alsa_null_runtime_plays_and_captures_generated_pcm() {
    let root = temp_directory("alsa-null");
    let report = LinuxAudioAdapter::system()
        .qualify(&root)
        .expect("Linux null-device audio runtime qualifies");

    assert_eq!(report.schema_version, 1);
    assert_eq!(
        report.baseline,
        "paseo@5de45e208690b0efc51c59a585ae9729325a9204"
    );
    assert_eq!(report.contract_id, "P2-AUDIO-01");
    assert_eq!(report.claim, "linux_alsa_null_runtime");
    assert_eq!(report.playback.sample_rate, 16_000);
    assert_eq!(report.playback.channels, 1);
    assert_eq!(report.playback.samples, 1_600);
    assert_eq!(report.playback.exit_code, Some(0));
    assert_eq!(report.capture.sample_rate, 16_000);
    assert_eq!(report.capture.channels, 1);
    assert_eq!(report.capture.samples, 16_000);
    assert_eq!(report.capture.exit_code, Some(0));
    assert!(report.capture.matches_generated_input);
    assert!(report.inventory.alsa.available);
    assert!(report.inventory.pulseaudio.available);
    assert!(report.inventory.pipewire.available);
    assert_eq!(
        report.limitations,
        [
            "no_physical_microphone_capture",
            "no_audible_output_assertion",
            "pulseaudio_cli_inventory_only",
            "pipewire_cli_inventory_only",
            "no_speech_to_text_or_text_to_speech",
        ]
    );

    assert!(report.playback.artifact.is_file());
    assert!(report.capture.artifact.is_file());
    cleanup(&root);
}

#[test]
fn timeout_reaps_linux_audio_adapter_process_group() {
    let root = temp_directory("timeout");
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
fn exited_parent_reaps_linux_audio_adapter_descendant() {
    let root = temp_directory("parent-exit");
    let child_pid = root.join("child.pid");
    let script = format!(
        "sleep 30 & child=$!; printf %s $child > '{}'; exit 0",
        child_pid.display()
    );
    let started = Instant::now();
    let result = ProcessCommand::new("/bin/sh")
        .args(["-c", &script])
        .timeout(Duration::from_millis(150))
        .run()
        .expect("exited parent returns without inherited-pipe hang");

    assert_eq!(result.exit_code, Some(0));
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid = fs::read_to_string(&child_pid).expect("child pid is recorded");
    let probe = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("process probe runs");
    assert!(
        !probe.success(),
        "exited parent's child process remains alive"
    );
    cleanup(&root);
}
