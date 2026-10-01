#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use spocky_audio_delivery_pilot::{
    DISPOSABLE_ROOT_ENV, LinuxDebLifecycleConfig, run_linux_delivery_qualification,
};

#[cfg(target_os = "linux")]
const USAGE: &str = "usage: spocky-linux-delivery-runtime OUTPUT_ROOT [WORK_ROOT]";

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), &'static str> {
    Err("Linux host required for dpkg delivery execution")
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output_root = PathBuf::from(arguments.next().ok_or(USAGE)?);
    let work_root = arguments
        .next()
        .map_or_else(|| output_root.join("lifecycle"), PathBuf::from);
    if arguments.next().is_some() {
        return Err(USAGE.into());
    }
    fs::create_dir_all(&output_root)?;
    let config = LinuxDebLifecycleConfig {
        root: work_root,
        user: "spocky-delivery".to_owned(),
        uid: 1000,
        gid: 1000,
        home: PathBuf::from("/home/spocky-delivery"),
        disposable_root_acknowledged: std::env::var(DISPOSABLE_ROOT_ENV).as_deref() == Ok("1"),
    };
    let report = run_linux_delivery_qualification(&config)?;
    let report_path = output_root.join("linux-delivery-report.json");
    let bytes = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, [bytes.as_slice(), b"\n"].concat())?;
    println!("linux delivery report: {}", report_path.display());
    Ok(())
}
