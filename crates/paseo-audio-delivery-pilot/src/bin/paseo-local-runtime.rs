use std::fs;
use std::path::PathBuf;

use paseo_audio_delivery_pilot::{
    AndroidDeviceAdapter, LocalDeliveryRuntime, NativeCapability, NativeRuntimeEvidence,
    ProcessCommand, create_pcm16_wav, create_unsigned_package,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalRuntimeReport {
    schema_version: u32,
    baseline: &'static str,
    claim: &'static str,
    host: HostEvidence,
    process: ProcessEvidence,
    audio: AudioEvidence,
    native: NativeEvidence,
    delivery: DeliveryEvidence,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostEvidence {
    os: &'static str,
    architecture: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessEvidence {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioEvidence {
    artifact: PathBuf,
    sample_rate: u32,
    channels: u16,
    samples: usize,
    file_bytes: usize,
    probe_exit_code: Option<i32>,
    probe_output: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeEvidence {
    status: &'static str,
    identity: Option<paseo_audio_delivery_pilot::AndroidDeviceIdentity>,
    commands: Vec<NativeRuntimeEvidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryEvidence {
    signing: &'static str,
    installation_root: PathBuf,
    steps: Vec<String>,
    active_version: Option<String>,
    retained_state_bytes: u64,
}

struct Arguments {
    root: PathBuf,
    afinfo: PathBuf,
    adb: Option<PathBuf>,
    serial: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = parse_arguments()?;
    let report_path = arguments.root.join("runtime-report.json");
    let report = run(arguments)?;
    let report_json = format!("{}\n", serde_json::to_string_pretty(&report)?);
    fs::write(report_path, &report_json)?;
    print!("{report_json}");
    Ok(())
}

fn run(arguments: Arguments) -> Result<LocalRuntimeReport, Box<dyn std::error::Error>> {
    fs::create_dir_all(&arguments.root)?;

    let process = ProcessCommand::new("/bin/sh")
        .args(["-c", "printf runtime-out; printf runtime-err >&2; exit 7"])
        .run()?;
    let process = ProcessEvidence {
        exit_code: process.exit_code,
        stdout: String::from_utf8_lossy(&process.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&process.stderr).into_owned(),
    };

    let audio_path = arguments.root.join("audio").join("silence-16khz.wav");
    fs::create_dir_all(audio_path.parent().expect("audio artifact has a parent"))?;
    let audio_metadata = create_pcm16_wav(&audio_path, 16_000, &[0; 1_600])?;
    let audio_probe = ProcessCommand::new(&arguments.afinfo)
        .arg(&audio_path)
        .run()?;
    let audio = AudioEvidence {
        artifact: audio_path,
        sample_rate: audio_metadata.sample_rate,
        channels: audio_metadata.channels,
        samples: audio_metadata.samples,
        file_bytes: audio_metadata.file_bytes,
        probe_exit_code: audio_probe.exit_code,
        probe_output: String::from_utf8_lossy(&audio_probe.stdout).into_owned(),
    };

    let packages = arguments.root.join("packages");
    let package_100 = packages.join("paseo-1.0.0-unsigned");
    let package_110 = packages.join("paseo-1.1.0-unsigned");
    let package_120 = packages.join("paseo-1.2.0-unsigned-corrupt");
    create_unsigned_package(&package_100, "1.0.0", b"runtime-state-v1")?;
    create_unsigned_package(&package_110, "1.1.0", b"runtime-state-v2")?;
    create_unsigned_package(&package_120, "1.2.0", b"runtime-state-v3")?;
    fs::write(package_120.join("payload.bin"), b"tampered")?;

    let installation_root = arguments.root.join("installation");
    let mut delivery_runtime = LocalDeliveryRuntime::new(installation_root.clone());
    delivery_runtime.install(&package_100)?;
    let mut delivery_steps = vec!["install:1.0.0".to_owned()];
    match delivery_runtime.update(&package_120) {
        Err(error) if error.to_string() == "payload checksum mismatch" => {}
        Ok(_) | Err(_) => return Err("corrupt update produced an unexpected result".into()),
    }
    delivery_steps.push("failed_update:checksum_mismatch".to_owned());
    delivery_runtime.update(&package_110)?;
    delivery_steps.push("update:1.1.0".to_owned());
    delivery_runtime.rollback()?;
    delivery_steps.push("rollback:1.0.0".to_owned());
    delivery_runtime.uninstall(true)?;
    delivery_steps.push("uninstall:retained_state".to_owned());
    let retained_state_bytes = fs::metadata(delivery_runtime.retained_state_path())?.len();
    let delivery = DeliveryEvidence {
        signing: "unsigned",
        installation_root,
        steps: delivery_steps,
        active_version: delivery_runtime.active_version()?,
        retained_state_bytes,
    };

    let native = match (arguments.adb, arguments.serial) {
        (Some(adb), Some(serial)) => {
            let adapter = AndroidDeviceAdapter::new(adb, serial);
            let identity = adapter.identity()?;
            let commands = NativeCapability::ALL
                .into_iter()
                .map(|capability| adapter.invoke(capability))
                .collect::<Result<Vec<_>, _>>()?;
            NativeEvidence {
                status: "android_avd_executed",
                identity: Some(identity),
                commands,
            }
        }
        (None, None) => NativeEvidence {
            status: "not_requested",
            identity: None,
            commands: Vec::new(),
        },
        _ => return Err("--adb and --serial must be supplied together".into()),
    };

    Ok(LocalRuntimeReport {
        schema_version: 1,
        baseline: "paseo@5de45e208690b0efc51c59a585ae9729325a9204",
        claim: "local_unsigned_runtime",
        host: HostEvidence {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
        },
        process,
        audio,
        native,
        delivery,
        limitations: vec![
            "no_real_microphone_capture",
            "no_audible_playback_assertion",
            "no_speech_service_credentials",
            "no_ios_simulator_or_device",
            "no_signed_package_artifact",
            "no_production_update_execution",
        ],
    })
}

fn parse_arguments() -> Result<Arguments, Box<dyn std::error::Error>> {
    let mut root = None;
    let mut afinfo = None;
    let mut adb = None;
    let mut serial = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--afinfo" => afinfo = Some(PathBuf::from(value)),
            "--adb" => adb = Some(PathBuf::from(value)),
            "--serial" => serial = Some(value),
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(Arguments {
        root: root.ok_or("--root is required")?,
        afinfo: afinfo.ok_or("--afinfo is required")?,
        adb,
        serial,
    })
}
