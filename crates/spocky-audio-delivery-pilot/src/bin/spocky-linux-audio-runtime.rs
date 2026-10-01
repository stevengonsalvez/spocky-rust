#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use spocky_audio_delivery_pilot::LinuxAudioAdapter;

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), &'static str> {
    Err("Linux host required for audio runtime execution")
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output_root = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: spocky-linux-audio-runtime OUTPUT_ROOT")?,
    );
    if arguments.next().is_some() {
        return Err("usage: spocky-linux-audio-runtime OUTPUT_ROOT".into());
    }
    fs::create_dir_all(&output_root)?;
    let report = LinuxAudioAdapter::system().qualify(&output_root)?;
    let report_path = output_root.join("linux-audio-report.json");
    let bytes = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, [bytes.as_slice(), b"\n"].concat())?;
    println!("Linux audio report: {}", report_path.display());
    Ok(())
}
