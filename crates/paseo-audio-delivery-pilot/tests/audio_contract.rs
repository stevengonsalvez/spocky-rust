use std::collections::BTreeSet;

use paseo_audio_delivery_pilot::{
    AudioCapability, AudioEvent, AudioPilot, AudioState, Outcome, Permission,
};

#[test]
fn audio_permission_capture_playback_and_interruption_are_stateful() {
    let mut pilot = AudioPilot::new([
        AudioCapability::Capture,
        AudioCapability::Playback,
        AudioCapability::Realtime,
        AudioCapability::SpeechToText,
        AudioCapability::TextToSpeech,
        AudioCapability::LocalModel,
        AudioCapability::DeviceChange,
        AudioCapability::Interruption,
    ]);

    assert_eq!(pilot.start_capture(), Outcome::Denied("microphone".into()));
    pilot.set_microphone_permission(Permission::Granted);
    assert_eq!(pilot.start_capture(), Outcome::Supported);
    assert_eq!(pilot.state(), AudioState::Capturing);
    assert_eq!(pilot.playback(3200, 16_000), Outcome::Supported);
    assert_eq!(pilot.realtime_voice(), Outcome::Supported);
    assert_eq!(pilot.speech_to_text(), Outcome::Supported);
    assert_eq!(pilot.text_to_speech(), Outcome::Supported);
    assert_eq!(pilot.local_model_ready(true), Outcome::Supported);
    assert_eq!(pilot.device_changed("wired-headset"), Outcome::Supported);
    assert_eq!(pilot.interrupt(), Outcome::Supported);
    assert_eq!(pilot.state(), AudioState::Interrupted);

    assert_eq!(
        pilot
            .events()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=10).collect::<Vec<_>>()
    );
    assert!(pilot.events().contains(&AudioEvent::new(
        10,
        "interruption",
        Outcome::Supported,
        AudioState::Interrupted,
    )));
}

#[test]
fn audio_absent_capabilities_are_explicitly_unsupported() {
    let mut pilot = AudioPilot::new([AudioCapability::Playback]);

    assert_eq!(
        pilot.realtime_voice(),
        Outcome::Unsupported("audio.realtime".into())
    );
    assert_eq!(
        pilot.speech_to_text(),
        Outcome::Unsupported("speech.stt".into())
    );
    assert_eq!(
        pilot.local_model_ready(false),
        Outcome::Unsupported("speech.local_model".into())
    );
    assert_eq!(
        pilot.capabilities(),
        &BTreeSet::from([AudioCapability::Playback])
    );
}
