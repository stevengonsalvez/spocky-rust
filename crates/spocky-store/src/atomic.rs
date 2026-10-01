//! `writeFileAtomic` from pinned Paseo `atomic-file.ts`: create the parent
//! directory, write a hidden temp file beside the target, rename it over the
//! target, and remove the temp file on failure.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::StoreError;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Writes `contents` to `path` atomically.
///
/// # Errors
///
/// Returns an error if directory creation, writing, or rename fails.
pub fn write_json_atomic(path: &Path, contents: &str) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| StoreError::Io {
        operation: "create registry directory",
        source,
    })?;
    let base = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let temporary = parent.join(format!(
        ".{base}.{}.{millis}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = fs::write(&temporary, contents)
        .map_err(|source| StoreError::Io {
            operation: "write temporary file",
            source,
        })
        .and_then(|()| {
            fs::rename(&temporary, path).map_err(|source| StoreError::Io {
                operation: "rename temporary file",
                source,
            })
        });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
