//! `PluginManifestSchema` (`manifest.ts`) trims `description` with zod's
//! `.trim()` and rejects a build argument whose `trim().length` is 0. Both are
//! `String.prototype.trim`, which strips U+FEFF and U+3000 and keeps U+0085;
//! node v22.20.0 printed `"﻿".trim().length` 0 and `"\u0085".trim().length`
//! 1. `str::trim` does the opposite on both.

use std::fs;

use spocky_plugin_pilot::{PluginError, load_manifest};

fn load(manifest: &str) -> Result<Option<String>, PluginError> {
    let directory = std::env::temp_dir().join(format!(
        "spocky-plugin-manifest-trim-{}-{:x}",
        std::process::id(),
        manifest.bytes().fold(0_u64, |hash, byte| hash
            .wrapping_mul(31)
            .wrapping_add(u64::from(byte)))
    ));
    fs::create_dir_all(&directory).expect("create directory");
    fs::write(directory.join("paseo-plugin.json"), manifest).expect("write manifest");
    fs::write(directory.join("index.server.ts"), "").expect("write entry");
    let loaded = load_manifest(&directory);
    fs::remove_dir_all(&directory).expect("remove directory");
    loaded.map(|loaded| loaded.manifest.description)
}

#[test]
fn description_is_trimmed_like_zod() {
    assert!(matches!(
        load(r#"{"id":"demo","description":"﻿"}"#),
        Err(PluginError::InvalidCandidate)
    ));
    assert!(matches!(
        load(r#"{"id":"demo","description":"　  "}"#),
        Err(PluginError::InvalidCandidate)
    ));
    assert_eq!(
        load(r#"{"id":"demo","description":"\u0085"}"#).expect("loads"),
        Some("\u{85}".to_owned())
    );
    assert_eq!(
        load(r#"{"id":"demo","description":"﻿hello　"}"#).expect("loads"),
        Some("hello".to_owned())
    );
}

#[test]
fn build_arguments_are_checked_like_trim_length() {
    assert!(matches!(
        load(r#"{"id":"demo","build":[["npm","﻿"]]}"#),
        Err(PluginError::InvalidCandidate)
    ));
    assert!(load(r#"{"id":"demo","build":[["npm","\u0085"]]}"#).is_ok());
}
