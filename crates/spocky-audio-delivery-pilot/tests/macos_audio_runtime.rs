#![cfg(target_os = "macos")]

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_audio_delivery_pilot::MacOsAudioAdapter;

#[test]
fn system_tts_produces_parsed_audio_and_playback_engine_consumes_it() {
    let root = std::env::temp_dir().join(format!(
        "spocky-macos-audio-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows Unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create macOS audio fixture");
    let output = root.join("speech.aiff");
    let adapter = MacOsAudioAdapter::system();

    let evidence = adapter
        .synthesize_and_play_muted("Spocky runtime test", &output)
        .expect("system speech and playback execute");

    assert_eq!(evidence.synthesis_exit_code, Some(0));
    assert_eq!(evidence.probe_exit_code, Some(0));
    assert_eq!(evidence.playback_exit_code, Some(0));
    assert!(evidence.file_bytes > 4_096);
    assert!(evidence.probe_output.contains("1 ch"));
    assert!(evidence.probe_output.contains("lpcm"));
    assert!(output.is_file());

    fs::remove_dir_all(root).expect("remove macOS audio fixture");
}

#[test]
fn system_tts_rejects_empty_text_before_process_launch() {
    let output = std::env::temp_dir().join("spocky-empty-speech.aiff");
    let error = MacOsAudioAdapter::system()
        .synthesize_and_play_muted("  ", &output)
        .expect_err("empty speech is rejected");

    assert_eq!(error.to_string(), "speech text must not be empty");
}
