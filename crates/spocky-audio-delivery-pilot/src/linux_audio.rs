//! Linux audio qualification through ALSA null devices.
//!
//! `PulseAudio` and `PipeWire` probes inventory installed client tools. They do not claim a running
//! server graph. ALSA playback and capture use the kernel-independent `null` PCM, so this module
//! never claims physical microphone input or audible output.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_runtime::{ProcessCommand, ProcessResult, RuntimeError, create_pcm16_wav};

pub const LINUX_AUDIO_BASELINE: &str = "paseo@5de45e208690b0efc51c59a585ae9729325a9204";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioToolInventory {
    pub available: bool,
    pub exit_code: Option<i32>,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxAudioInventory {
    pub alsa: AudioToolInventory,
    pub pulseaudio: AudioToolInventory,
    pub pipewire: AudioToolInventory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxAudioArtifact {
    pub artifact: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: usize,
    pub file_bytes: usize,
    pub exit_code: Option<i32>,
    pub matches_generated_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxAudioEvidenceReport {
    pub schema_version: u32,
    pub baseline: String,
    pub contract_id: String,
    pub claim: String,
    pub inventory: LinuxAudioInventory,
    pub playback: LinuxAudioArtifact,
    pub capture: LinuxAudioArtifact,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxAudioAdapter {
    aplay: PathBuf,
    arecord: PathBuf,
    pactl: PathBuf,
    pw_cli: PathBuf,
}

impl LinuxAudioAdapter {
    #[must_use]
    pub fn system() -> Self {
        Self {
            aplay: PathBuf::from("aplay"),
            arecord: PathBuf::from("arecord"),
            pactl: PathBuf::from("pactl"),
            pw_cli: PathBuf::from("pw-cli"),
        }
    }

    /// Runs generated playback and capture through ALSA null PCMs and inventories audio clients.
    ///
    /// # Errors
    ///
    /// Returns an error when the output root cannot be created, ALSA tools are unavailable, a
    /// process fails, or the captured WAV does not contain the requested mono PCM shape.
    pub fn qualify(&self, root: &Path) -> Result<LinuxAudioEvidenceReport, RuntimeError> {
        fs::create_dir_all(root)?;
        let inventory = LinuxAudioInventory {
            alsa: probe_tool(&self.aplay, ["--version"]),
            pulseaudio: probe_tool(&self.pactl, ["--version"]),
            pipewire: probe_tool(&self.pw_cli, ["--version"]),
        };
        if !inventory.alsa.available {
            return Err(RuntimeError::ProcessFailed(
                "ALSA aplay version probe failed".to_owned(),
            ));
        }

        let playback_path = root.join("generated-playback.wav");
        let playback_samples = generated_square_wave();
        let playback_metadata = create_pcm16_wav(&playback_path, 16_000, &playback_samples)?;
        let playback_result = ProcessCommand::new(&self.aplay)
            .args([OsStr::new("-q"), OsStr::new("-D"), OsStr::new("null")])
            .arg(&playback_path)
            .run()?;
        require_process_success("ALSA null playback", &playback_result)?;

        let capture_samples = generated_capture_wave();
        let capture_input = root.join("generated-capture-input.raw");
        let capture_input_bytes = capture_samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>();
        fs::write(&capture_input, capture_input_bytes)?;
        let alsa_config = root.join("alsa-null-capture.conf");
        fs::write(
            &alsa_config,
            format!(
                "</usr/share/alsa/alsa.conf>\npcm.spocky_capture {{\n  type file\n  slave.pcm null\n  file \"/dev/null\"\n  infile \"{}\"\n  format raw\n}}\n",
                escape_alsa_string(&capture_input)
            ),
        )?;
        let capture_path = root.join("generated-capture.wav");
        let capture_result = ProcessCommand::new(&self.arecord)
            .args([
                OsStr::new("-q"),
                OsStr::new("-D"),
                OsStr::new("spocky_capture"),
                OsStr::new("-t"),
                OsStr::new("wav"),
                OsStr::new("-f"),
                OsStr::new("S16_LE"),
                OsStr::new("-r"),
                OsStr::new("16000"),
                OsStr::new("-c"),
                OsStr::new("1"),
                OsStr::new("-d"),
                OsStr::new("1"),
            ])
            .arg(&capture_path)
            .env("ALSA_CONFIG_PATH", &alsa_config)
            .run()?;
        require_process_success("ALSA null capture", &capture_result)?;
        let capture = read_pcm16_wav(&capture_path, capture_result.exit_code, &capture_samples)?;
        if capture.sample_rate != 16_000 || capture.channels != 1 || capture.samples != 16_000 {
            return Err(RuntimeError::ProcessFailed(format!(
                "ALSA null capture shape mismatch: {} Hz, {} channels, {} samples",
                capture.sample_rate, capture.channels, capture.samples
            )));
        }

        Ok(LinuxAudioEvidenceReport {
            schema_version: 1,
            baseline: LINUX_AUDIO_BASELINE.to_owned(),
            contract_id: "P2-AUDIO-01".to_owned(),
            claim: "linux_alsa_null_runtime".to_owned(),
            inventory,
            playback: LinuxAudioArtifact {
                artifact: playback_path,
                sample_rate: playback_metadata.sample_rate,
                channels: playback_metadata.channels,
                samples: playback_metadata.samples,
                file_bytes: playback_metadata.file_bytes,
                exit_code: playback_result.exit_code,
                matches_generated_input: true,
            },
            capture,
            limitations: [
                "no_physical_microphone_capture",
                "no_audible_output_assertion",
                "pulseaudio_cli_inventory_only",
                "pipewire_cli_inventory_only",
                "no_speech_to_text_or_text_to_speech",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        })
    }
}

fn generated_square_wave() -> Vec<i16> {
    (0..1_600)
        .map(|index| if index % 16 < 8 { 8_192 } else { -8_192 })
        .collect()
}

fn generated_capture_wave() -> Vec<i16> {
    (0..16_000)
        .map(|index| i16::try_from(index % 257).expect("generated sample fits i16") - 128)
        .collect()
}

fn escape_alsa_string(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn probe_tool<const N: usize>(program: &Path, args: [&str; N]) -> AudioToolInventory {
    match ProcessCommand::new(program).args(args).run() {
        Ok(result) => AudioToolInventory {
            available: result.exit_code == Some(0),
            exit_code: result.exit_code,
            version: combined_output(&result),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => AudioToolInventory {
            available: false,
            exit_code: None,
            version: String::new(),
        },
        Err(error) => AudioToolInventory {
            available: false,
            exit_code: None,
            version: error.to_string(),
        },
    }
}

fn combined_output(result: &ProcessResult) -> String {
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    format!("{stdout}{stderr}").trim().to_owned()
}

fn require_process_success(label: &str, result: &ProcessResult) -> Result<(), RuntimeError> {
    if result.exit_code == Some(0) {
        return Ok(());
    }
    Err(RuntimeError::ProcessFailed(format!(
        "{label} failed with {:?}: {}",
        result.exit_code,
        combined_output(result)
    )))
}

fn read_pcm16_wav(
    path: &Path,
    exit_code: Option<i32>,
    expected_samples: &[i16],
) -> Result<LinuxAudioArtifact, RuntimeError> {
    let bytes = fs::read(path)?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(RuntimeError::ProcessFailed(
            "ALSA capture did not produce a RIFF/WAVE artifact".to_owned(),
        ));
    }
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let bits_per_sample = u16::from_le_bytes([bytes[34], bytes[35]]);
    let data_bytes = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    if bits_per_sample != 16 || bytes.len() != 44 + data_bytes || !data_bytes.is_multiple_of(2) {
        return Err(RuntimeError::ProcessFailed(
            "ALSA capture WAV layout is not canonical PCM16".to_owned(),
        ));
    }
    let matches_generated_input = bytes[44..]
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .eq(expected_samples.iter().copied());
    Ok(LinuxAudioArtifact {
        artifact: path.to_owned(),
        sample_rate,
        channels,
        samples: data_bytes / 2,
        file_bytes: bytes.len(),
        exit_code,
        matches_generated_input,
    })
}
