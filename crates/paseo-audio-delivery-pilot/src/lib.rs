use std::collections::BTreeSet;

mod local_runtime;

pub use local_runtime::{
    AndroidDeviceAdapter, AndroidDeviceIdentity, DeliverySnapshot, LocalDeliveryRuntime,
    NativeRuntimeEvidence, PcmFileMetadata, ProcessCommand, ProcessResult, RuntimeError,
    create_pcm16_wav, create_unsigned_package,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    Supported,
    Unsupported(String),
    Denied(String),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCapability {
    Capture,
    Playback,
    Realtime,
    SpeechToText,
    TextToSpeech,
    LocalModel,
    DeviceChange,
    Interruption,
}

impl AudioCapability {
    const fn contract_name(self) -> &'static str {
        match self {
            Self::Capture => "audio.capture",
            Self::Playback => "audio.playback",
            Self::Realtime => "audio.realtime",
            Self::SpeechToText => "speech.stt",
            Self::TextToSpeech => "speech.tts",
            Self::LocalModel => "speech.local_model",
            Self::DeviceChange => "audio.device_change",
            Self::Interruption => "audio.interruption",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Unknown,
    Granted,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioState {
    Idle,
    Capturing,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioEvent {
    pub sequence: u64,
    pub operation: String,
    pub outcome: Outcome,
    pub state: AudioState,
}

impl AudioEvent {
    #[must_use]
    pub fn new(
        sequence: u64,
        operation: impl Into<String>,
        outcome: Outcome,
        state: AudioState,
    ) -> Self {
        Self {
            sequence,
            operation: operation.into(),
            outcome,
            state,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioPilot {
    capabilities: BTreeSet<AudioCapability>,
    microphone_permission: Permission,
    state: AudioState,
    events: Vec<AudioEvent>,
}

impl AudioPilot {
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = AudioCapability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
            microphone_permission: Permission::Unknown,
            state: AudioState::Idle,
            events: Vec::new(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> AudioState {
        self.state
    }

    #[must_use]
    pub const fn capabilities(&self) -> &BTreeSet<AudioCapability> {
        &self.capabilities
    }

    #[must_use]
    pub fn events(&self) -> &[AudioEvent] {
        &self.events
    }

    pub fn set_microphone_permission(&mut self, permission: Permission) {
        self.microphone_permission = permission;
        let outcome = match permission {
            Permission::Granted => Outcome::Supported,
            Permission::Unknown | Permission::Denied => Outcome::Denied("microphone".into()),
        };
        self.record("permission.microphone", outcome);
    }

    pub fn start_capture(&mut self) -> Outcome {
        let outcome = self.require(AudioCapability::Capture).and_then(|()| {
            if self.microphone_permission == Permission::Granted {
                self.state = AudioState::Capturing;
                Ok(())
            } else {
                Err(Outcome::Denied("microphone".into()))
            }
        });
        self.finish("capture.start", outcome)
    }

    pub fn playback(&mut self, pcm_bytes: usize, sample_rate: u32) -> Outcome {
        let outcome = self.require(AudioCapability::Playback).and_then(|()| {
            if pcm_bytes == 0 || sample_rate == 0 {
                Err(Outcome::Failed("invalid_pcm".into()))
            } else {
                Ok(())
            }
        });
        self.finish("playback", outcome)
    }

    pub fn realtime_voice(&mut self) -> Outcome {
        self.finish("realtime_voice", self.require(AudioCapability::Realtime))
    }

    pub fn speech_to_text(&mut self) -> Outcome {
        self.finish(
            "speech_to_text",
            self.require(AudioCapability::SpeechToText),
        )
    }

    pub fn text_to_speech(&mut self) -> Outcome {
        self.finish(
            "text_to_speech",
            self.require(AudioCapability::TextToSpeech),
        )
    }

    pub fn local_model_ready(&mut self, ready: bool) -> Outcome {
        let outcome = self.require(AudioCapability::LocalModel).and_then(|()| {
            if ready {
                Ok(())
            } else {
                Err(Outcome::Failed("model_not_ready".into()))
            }
        });
        self.finish("local_model.readiness", outcome)
    }

    pub fn device_changed(&mut self, device_id: &str) -> Outcome {
        let outcome = self.require(AudioCapability::DeviceChange).and_then(|()| {
            if device_id.is_empty() {
                Err(Outcome::Failed("missing_device".into()))
            } else {
                Ok(())
            }
        });
        self.finish("device_change", outcome)
    }

    pub fn interrupt(&mut self) -> Outcome {
        let outcome = self.require(AudioCapability::Interruption).map(|()| {
            self.state = AudioState::Interrupted;
        });
        self.finish("interruption", outcome)
    }

    fn require(&self, capability: AudioCapability) -> Result<(), Outcome> {
        if self.capabilities.contains(&capability) {
            Ok(())
        } else {
            Err(Outcome::Unsupported(capability.contract_name().into()))
        }
    }

    fn finish(&mut self, operation: &str, result: Result<(), Outcome>) -> Outcome {
        let outcome = match result {
            Ok(()) => Outcome::Supported,
            Err(outcome) => outcome,
        };
        self.record(operation, outcome.clone());
        outcome
    }

    fn record(&mut self, operation: &str, outcome: Outcome) {
        self.events.push(AudioEvent::new(
            self.events.len() as u64 + 1,
            operation,
            outcome,
            self.state,
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Ios,
    Android,
    Web,
}

impl Platform {
    const fn contract_name(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
            Self::Web => "web",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCapability {
    Push,
    Camera,
    FilePicker,
    Haptics,
    Notifications,
    Background,
    DeepLink,
}

impl NativeCapability {
    pub const ALL: [Self; 7] = [
        Self::Push,
        Self::Camera,
        Self::FilePicker,
        Self::Haptics,
        Self::Notifications,
        Self::Background,
        Self::DeepLink,
    ];

    const fn contract_name(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Camera => "camera",
            Self::FilePicker => "file_picker",
            Self::Haptics => "haptics",
            Self::Notifications => "notifications",
            Self::Background => "background",
            Self::DeepLink => "deep_link",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeEvent {
    pub sequence: u64,
    pub platform: Platform,
    pub operation: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub struct NativePilot {
    platform: Platform,
    capabilities: BTreeSet<NativeCapability>,
    events: Vec<NativeEvent>,
}

impl NativePilot {
    #[must_use]
    pub fn new(
        platform: Platform,
        capabilities: impl IntoIterator<Item = NativeCapability>,
    ) -> Self {
        Self {
            platform,
            capabilities: capabilities.into_iter().collect(),
            events: Vec::new(),
        }
    }

    pub fn invoke(&mut self, capability: NativeCapability) -> Outcome {
        let operation = capability.contract_name();
        let outcome = if self.capabilities.contains(&capability) {
            Outcome::Supported
        } else {
            Outcome::Unsupported(format!(
                "native.{operation}@{}",
                self.platform.contract_name()
            ))
        };
        self.events.push(NativeEvent {
            sequence: self.events.len() as u64 + 1,
            platform: self.platform,
            operation: operation.into(),
            outcome: outcome.clone(),
        });
        outcome
    }

    #[must_use]
    pub fn events(&self) -> &[NativeEvent] {
        &self.events
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryState {
    Absent,
    Installed {
        version: String,
    },
    UpdateFailed {
        active_version: String,
        target_version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryEvent {
    pub sequence: u64,
    pub operation: String,
    pub outcome: Outcome,
    pub state: DeliveryState,
    pub retained_state_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeliveryPilot {
    supports_updates: bool,
    supports_rollback: bool,
    state: DeliveryState,
    retained_state_digest: Option<String>,
    events: Vec<DeliveryEvent>,
}

impl DeliveryPilot {
    #[must_use]
    pub const fn new(supports_updates: bool, supports_rollback: bool) -> Self {
        Self {
            supports_updates,
            supports_rollback,
            state: DeliveryState::Absent,
            retained_state_digest: None,
            events: Vec::new(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> &DeliveryState {
        &self.state
    }

    #[must_use]
    pub fn retained_state_digest(&self) -> Option<&str> {
        self.retained_state_digest.as_deref()
    }

    #[must_use]
    pub fn events(&self) -> &[DeliveryEvent] {
        &self.events
    }

    pub fn install(&mut self, version: &str, state_digest: &str) -> Outcome {
        let outcome = if matches!(self.state, DeliveryState::Absent) {
            self.state = DeliveryState::Installed {
                version: version.into(),
            };
            self.retained_state_digest = Some(state_digest.into());
            Outcome::Supported
        } else {
            Outcome::Failed("already_installed".into())
        };
        self.record("install", outcome.clone());
        outcome
    }

    pub fn upgrade(&mut self, target_version: &str, succeeds: bool) -> Outcome {
        let outcome = if !self.supports_updates {
            Outcome::Unsupported("delivery.upgrade".into())
        } else if let DeliveryState::Installed { version } = &self.state {
            if succeeds {
                self.state = DeliveryState::Installed {
                    version: target_version.into(),
                };
                Outcome::Supported
            } else {
                self.state = DeliveryState::UpdateFailed {
                    active_version: version.clone(),
                    target_version: target_version.into(),
                };
                Outcome::Failed("update_failed".into())
            }
        } else {
            Outcome::Failed("not_installed".into())
        };
        self.record("upgrade", outcome.clone());
        outcome
    }

    pub fn rollback(&mut self) -> Outcome {
        let outcome = if !self.supports_rollback {
            Outcome::Unsupported("delivery.rollback".into())
        } else if let DeliveryState::UpdateFailed { active_version, .. } = &self.state {
            self.state = DeliveryState::Installed {
                version: active_version.clone(),
            };
            Outcome::Supported
        } else {
            Outcome::Failed("rollback_unavailable".into())
        };
        self.record("rollback", outcome.clone());
        outcome
    }

    pub fn uninstall(&mut self, retain_state: bool) -> Outcome {
        let outcome = if matches!(self.state, DeliveryState::Absent) {
            Outcome::Failed("not_installed".into())
        } else {
            self.state = DeliveryState::Absent;
            if !retain_state {
                self.retained_state_digest = None;
            }
            Outcome::Supported
        };
        self.record("uninstall", outcome.clone());
        outcome
    }

    fn record(&mut self, operation: &str, outcome: Outcome) {
        self.events.push(DeliveryEvent {
            sequence: self.events.len() as u64 + 1,
            operation: operation.into(),
            outcome,
            state: self.state.clone(),
            retained_state_digest: self.retained_state_digest.clone(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PilotTrace {
    pub contract_id: String,
    pub evidence: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PilotReport {
    pub schema_version: u32,
    pub baseline: String,
    pub claim: String,
    pub traces: Vec<PilotTrace>,
    pub limitations: Vec<String>,
}

impl PilotReport {
    /// Serializes the evidence report with stable field and event ordering.
    ///
    /// # Errors
    ///
    /// Returns an error if report serialization fails.
    pub fn to_json_pretty(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }
}

#[must_use]
pub fn run_pilot() -> PilotReport {
    let audio_capabilities = [
        AudioCapability::Capture,
        AudioCapability::Playback,
        AudioCapability::Realtime,
        AudioCapability::SpeechToText,
        AudioCapability::TextToSpeech,
        AudioCapability::LocalModel,
        AudioCapability::DeviceChange,
        AudioCapability::Interruption,
    ];
    let mut audio = AudioPilot::new(audio_capabilities);
    audio.start_capture();
    audio.set_microphone_permission(Permission::Granted);
    audio.start_capture();
    audio.playback(3_200, 16_000);
    audio.realtime_voice();
    audio.speech_to_text();
    audio.text_to_speech();
    audio.local_model_ready(false);
    audio.local_model_ready(true);
    audio.device_changed("wired-headset");
    audio.interrupt();
    let mut unsupported_audio = AudioPilot::new([]);
    unsupported_audio.realtime_voice();
    unsupported_audio.speech_to_text();
    unsupported_audio.text_to_speech();
    let audio_evidence = serialize_events(audio.events())
        .into_iter()
        .chain(serialize_events(unsupported_audio.events()))
        .collect();

    let mut native = NativePilot::new(Platform::Ios, NativeCapability::ALL);
    for capability in NativeCapability::ALL {
        native.invoke(capability);
    }
    let mut web = NativePilot::new(Platform::Web, []);
    for capability in NativeCapability::ALL {
        web.invoke(capability);
    }
    let native_evidence = serialize_events(native.events())
        .into_iter()
        .chain(serialize_events(web.events()))
        .collect();

    let mut delivery = DeliveryPilot::new(true, true);
    delivery.install("1.0.0", "sha256:state-a");
    delivery.upgrade("1.1.0", false);
    delivery.rollback();
    delivery.upgrade("1.1.0", true);
    delivery.uninstall(true);
    let mut limited_delivery = DeliveryPilot::new(false, false);
    limited_delivery.install("1.0.0", "sha256:state-b");
    limited_delivery.upgrade("1.1.0", true);
    limited_delivery.rollback();
    limited_delivery.uninstall(false);
    let delivery_evidence = serialize_events(delivery.events())
        .into_iter()
        .chain(serialize_events(limited_delivery.events()))
        .collect();

    PilotReport {
        schema_version: 1,
        baseline: "paseo@5de45e2".into(),
        claim: "contract_pilot_only".into(),
        traces: vec![
            PilotTrace {
                contract_id: "P2-AUDIO-01".into(),
                evidence: audio_evidence,
            },
            PilotTrace {
                contract_id: "P2-NATIVE-01".into(),
                evidence: native_evidence,
            },
            PilotTrace {
                contract_id: "P2-DELIVERY-01".into(),
                evidence: delivery_evidence,
            },
        ],
        limitations: [
            "no_full_parity_claim",
            "no_real_device_recording",
            "no_signed_package_artifact",
            "no_production_update_execution",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    }
}

fn serialize_events<T: Serialize>(events: &[T]) -> Vec<serde_json::Value> {
    events
        .iter()
        .map(|event| serde_json::to_value(event).expect("event serialization is infallible"))
        .collect()
}
