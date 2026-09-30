use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use paseo_differential::{
    DifferentialFailureManifest, DifferentialManifest, DifferentialRun, RunPlan,
    run_differential_preserving_evidence,
};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("paseo-differential: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let plan_path = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: paseo-differential <plan.json> <manifest.json>")?,
    );
    let manifest_path = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: paseo-differential <plan.json> <manifest.json>")?,
    );
    if arguments.next().is_some() {
        return Err("usage: paseo-differential <plan.json> <manifest.json>".into());
    }

    let plan: RunPlan = serde_json::from_slice(&fs::read(&plan_path)?)?;
    match run_differential_preserving_evidence(&plan)? {
        DifferentialRun::Compared(report) => {
            let bytes = DifferentialManifest::to_bytes(&report)?;
            fs::write(&manifest_path, bytes)?;
            Ok(report.equivalent)
        }
        DifferentialRun::ComparisonFailed(failure) => {
            let bytes = DifferentialFailureManifest::to_bytes(&failure)?;
            fs::write(&manifest_path, bytes)?;
            Err(std::io::Error::other(failure.error).into())
        }
    }
}
