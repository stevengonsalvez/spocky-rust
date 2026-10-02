//! Home files the baseline bootstrap writes before the daemon listens.
//!
//! `createAgentProviderRuntime` starts the `OpenCode` bridge unconditionally
//! (`bootstrap.ts`), and `OpenCodeBridge.start` materializes its plugin
//! (`agent/providers/opencode/bridge.ts` `materializePlugin`): the built
//! `bridge-plugin.bundle.mjs`, written atomically to
//! `$PASEO_HOME/runtime/opencode/paseo-<sha256 of the bytes>.mjs`.
//!
//! `assets/opencode-bridge-plugin.bundle.mjs` is that file copied byte for
//! byte from the pinned Paseo `5de45e2` build
//! (`packages/server/dist/server/server/agent/providers/opencode/`), produced
//! by `packages/server/scripts/build-opencode-bridge-plugin.mjs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use spocky_session::clock::random_uuid;

/// The pinned `OpenCode` bridge plugin bundle.
const OPENCODE_BRIDGE_PLUGIN: &[u8] = include_bytes!("../assets/opencode-bridge-plugin.bundle.mjs");

/// SHA-256 of the pinned bundle, as the pinned daemon names the file.
pub const OPENCODE_BRIDGE_PLUGIN_SHA256: &str =
    "a88cef53578dcb32cfa4af17e13e44c84751a30e5c19f847733904dd84eda872";

fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// `writeFileAtomic` (`atomic-file.ts`): create the parent directories,
/// write `.<name>.<pid>.<Date.now()>.<uuid>.tmp` beside the target, then
/// rename it over the target; the temporary file is removed on failure.
fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = directory.join(format!(
        ".{name}.{}.{millis}.{}.tmp",
        std::process::id(),
        random_uuid()
    ));
    let written = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// `materializePlugin()`: writes the `OpenCode` bridge plugin and returns its
/// path.
///
/// # Errors
///
/// A failure to create the directories or write the file, which fails the
/// baseline's bootstrap too.
pub fn materialize_opencode_bridge_plugin(paseo_home: &Path) -> io::Result<PathBuf> {
    let digest = hex_sha256(OPENCODE_BRIDGE_PLUGIN);
    let destination = paseo_home
        .join("runtime")
        .join("opencode")
        .join(format!("paseo-{digest}.mjs"));
    write_file_atomic(&destination, OPENCODE_BRIDGE_PLUGIN)?;
    Ok(destination)
}

/// The schedule store's directory. `ScheduleService.start` recovers
/// interrupted runs by listing the store, and `ScheduleStore` creates
/// `$PASEO_HOME/schedules` on first use (`schedule/store.ts`). The schedule
/// service itself is outside the slice; this is its bootstrap effect on the
/// home.
///
/// # Errors
///
/// A failure to create the directory, which fails the baseline's bootstrap.
pub fn ensure_schedule_store_dir(paseo_home: &Path) -> io::Result<()> {
    fs::create_dir_all(paseo_home.join("schedules"))
}

#[cfg(test)]
mod tests {
    use super::{
        OPENCODE_BRIDGE_PLUGIN, OPENCODE_BRIDGE_PLUGIN_SHA256, ensure_schedule_store_dir,
        hex_sha256, materialize_opencode_bridge_plugin,
    };

    #[test]
    fn embedded_bundle_is_the_pinned_build() {
        assert_eq!(OPENCODE_BRIDGE_PLUGIN.len(), 547_817);
        assert_eq!(
            hex_sha256(OPENCODE_BRIDGE_PLUGIN),
            OPENCODE_BRIDGE_PLUGIN_SHA256
        );
    }

    #[test]
    fn plugin_lands_under_its_digest_with_no_temporary_left() {
        let home = std::env::temp_dir().join(format!(
            "spocky-daemon-app-bootstrap-{}-{}",
            std::process::id(),
            spocky_session::clock::random_uuid()
        ));
        let path = materialize_opencode_bridge_plugin(&home).expect("materialize");
        assert_eq!(
            path,
            home.join("runtime")
                .join("opencode")
                .join(format!("paseo-{OPENCODE_BRIDGE_PLUGIN_SHA256}.mjs"))
        );
        assert_eq!(std::fs::read(&path).unwrap(), OPENCODE_BRIDGE_PLUGIN);
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        // A second start rewrites the same file.
        materialize_opencode_bridge_plugin(&home).expect("rewrite");
        ensure_schedule_store_dir(&home).expect("schedules");
        ensure_schedule_store_dir(&home).expect("schedules again");
        assert!(home.join("schedules").is_dir());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
