use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = spocky_audio_delivery_pilot::run_pilot().to_json_pretty()?;
    let mut output = io::stdout().lock();
    output.write_all(&report)?;
    output.write_all(b"\n")?;
    Ok(())
}
