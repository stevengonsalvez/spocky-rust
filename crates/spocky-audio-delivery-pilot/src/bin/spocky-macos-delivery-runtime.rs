#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use spocky_audio_delivery_pilot::run_macos_delivery_feasibility;

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), &'static str> {
    Err("macOS host required for app bundle execution")
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_root = parse_output_root()?;
    fs::create_dir_all(&output_root)?;
    let report = run_macos_delivery_feasibility(&output_root.join("lifecycle"))?;
    let report_path = output_root.join("delivery-runtime-report.json");
    let bytes = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, [bytes.as_slice(), b"\n"].concat())?;
    println!("delivery runtime report: {}", report_path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn parse_output_root() -> Result<PathBuf, &'static str> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let output = arguments
        .next()
        .ok_or("usage: spocky-macos-delivery-runtime OUTPUT_ROOT")?;
    if arguments.next().is_some() {
        return Err("usage: spocky-macos-delivery-runtime OUTPUT_ROOT");
    }
    Ok(output.into())
}
