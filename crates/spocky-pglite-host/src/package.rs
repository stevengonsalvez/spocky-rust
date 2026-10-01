//! Loads the pinned `@electric-sql/pglite` 0.5.4 package bytes unchanged.
//!
//! The Wasm modules and the file package are used as shipped. The file
//! package offsets and directory list are read from the distributed glue
//! (`dist/index.js`), which is the code the retained Node host runs.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const PACKAGE_NAME: &str = "@electric-sql/pglite";
pub const PACKAGE_VERSION: &str = "0.5.4";

/// SHA-256 of the pinned files, equal to
/// `evidence/phase2/hub-embedded-retained-package-sha256.txt`.
pub const PINNED_DIGESTS: [(&str, &str); 4] = [
    (
        "dist/pglite.wasm",
        "a20a37e2eb30553ae44f728001f0ead1b32cdcd53ab21f81fa2117b2947ef599",
    ),
    (
        "dist/initdb.wasm",
        "aa134fde5c96733ff9ab9644f409d9337ae7b3f42a66914a4dad2beec03824df",
    ),
    (
        "dist/pglite.data",
        "e39943c245ec32c36ed89bd5000229c8f3874983db93b7081d949155aaefaebf",
    ),
    (
        "dist/index.js",
        "d346708dbb8a67e6b1e27ae7187b3172c3c51c23fdfe820c9a1e3f5c6f8e170f",
    ),
];

#[derive(Debug)]
pub enum PackageError {
    Io(PathBuf, std::io::Error),
    Digest {
        file: &'static str,
        expected: &'static str,
        actual: String,
    },
    Glue(String),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, error) => write!(formatter, "{}: {error}", path.display()),
            Self::Digest {
                file,
                expected,
                actual,
            } => write!(
                formatter,
                "{file} SHA-256 is {actual}; pinned package requires {expected}"
            ),
            Self::Glue(message) => write!(formatter, "pinned glue metadata: {message}"),
        }
    }
}

impl std::error::Error for PackageError {}

/// One file of `pglite.data` with its byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedFile {
    pub path: String,
    pub start: usize,
    pub end: usize,
}

pub struct PinnedPackage {
    pub root: PathBuf,
    pub pglite_wasm: Vec<u8>,
    pub initdb_wasm: Vec<u8>,
    pub data: Vec<u8>,
    /// `FS_createPath(parent, name, true, true)` calls in glue order.
    pub directories: Vec<(String, String)>,
    pub files: Vec<PackagedFile>,
    pub package_json: serde_json::Value,
}

impl PinnedPackage {
    /// Reads and verifies the pinned package rooted at `root`
    /// (`node_modules/@electric-sql/pglite`).
    ///
    /// # Errors
    ///
    /// Fails when a file is missing, its digest differs from the pinned
    /// digest, or the glue metadata cannot be parsed.
    pub fn load(root: &Path) -> Result<Self, PackageError> {
        let read = |relative: &str| {
            let path = root.join(relative);
            fs::read(&path).map_err(|error| PackageError::Io(path, error))
        };
        let mut contents = Vec::new();
        for (file, expected) in PINNED_DIGESTS {
            let bytes = read(file)?;
            let actual = hex(&Sha256::digest(&bytes));
            if actual != expected {
                return Err(PackageError::Digest {
                    file,
                    expected,
                    actual,
                });
            }
            contents.push(bytes);
        }
        let glue = String::from_utf8(contents.pop().unwrap_or_default())
            .map_err(|_| PackageError::Glue("index.js is not UTF-8".into()))?;
        let data = contents.pop().unwrap_or_default();
        let initdb_wasm = contents.pop().unwrap_or_default();
        let pglite_wasm = contents.pop().unwrap_or_default();
        let directories = parse_create_paths(&glue)?;
        let files = parse_files(&glue)?;
        let size = parse_number_after(&glue, "remote_package_size:")?;
        if size != data.len() {
            return Err(PackageError::Glue(format!(
                "Invalid FS bundle size: {} !== {size}",
                data.len()
            )));
        }
        if files
            .iter()
            .any(|file| file.end > data.len() || file.start > file.end)
        {
            return Err(PackageError::Glue("file range outside pglite.data".into()));
        }
        let package_json = serde_json::from_slice(&read("package.json")?)
            .map_err(|error| PackageError::Glue(format!("package.json: {error}")))?;
        Ok(Self {
            root: root.to_path_buf(),
            pglite_wasm,
            initdb_wasm,
            data,
            directories,
            files,
            package_json,
        })
    }

    #[must_use]
    pub fn file_bytes(&self, file: &PackagedFile) -> &[u8] {
        &self.data[file.start..file.end]
    }
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn quoted_at(text: &str, start: usize) -> Result<(String, usize), PackageError> {
    let rest = &text[start..];
    let body = rest
        .strip_prefix('"')
        .ok_or_else(|| PackageError::Glue("expected a string literal".into()))?;
    let end = body
        .find('"')
        .ok_or_else(|| PackageError::Glue("unterminated string literal".into()))?;
    let value = &body[..end];
    if value.contains('\\') {
        return Err(PackageError::Glue(format!("escaped path {value}")));
    }
    Ok((value.to_owned(), start + 1 + end + 1))
}

fn parse_create_paths(glue: &str) -> Result<Vec<(String, String)>, PackageError> {
    const MARKER: &str = "FS_createPath(";
    let mut paths = Vec::new();
    let mut offset = 0;
    while let Some(found) = glue[offset..].find(MARKER) {
        let start = offset + found + MARKER.len();
        offset = start;
        if !glue[start..].starts_with('"') {
            continue;
        }
        let (parent, next) = quoted_at(glue, start)?;
        let next = next + 1;
        let (name, next) = quoted_at(glue, next)?;
        if !glue[next..].starts_with(",!0,!0)") {
            return Err(PackageError::Glue(format!(
                "unexpected createPath flags for {name}"
            )));
        }
        paths.push((parent, name));
    }
    if paths.is_empty() {
        return Err(PackageError::Glue("no FS_createPath calls".into()));
    }
    Ok(paths)
}

fn parse_files(glue: &str) -> Result<Vec<PackagedFile>, PackageError> {
    const MARKER: &str = "{filename:";
    let mut files = Vec::new();
    let mut offset = 0;
    while let Some(found) = glue[offset..].find(MARKER) {
        let start = offset + found + MARKER.len();
        let (path, next) = quoted_at(glue, start)?;
        let rest = &glue[next..];
        let rest = rest
            .strip_prefix(",start:")
            .ok_or_else(|| PackageError::Glue(format!("missing start for {path}")))?;
        let start_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let rest = &rest[start_digits.len()..];
        let rest = rest
            .strip_prefix(",end:")
            .ok_or_else(|| PackageError::Glue(format!("missing end for {path}")))?;
        let end_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let rest = &rest[end_digits.len()..];
        if !rest.starts_with('}') {
            return Err(PackageError::Glue(format!(
                "unexpected file entry for {path}"
            )));
        }
        files.push(PackagedFile {
            path,
            start: start_digits
                .parse()
                .map_err(|_| PackageError::Glue("bad start".into()))?,
            end: end_digits
                .parse()
                .map_err(|_| PackageError::Glue("bad end".into()))?,
        });
        offset = glue.len() - rest.len();
    }
    if files.is_empty() {
        return Err(PackageError::Glue("no packaged files".into()));
    }
    Ok(files)
}

fn parse_number_after(glue: &str, marker: &str) -> Result<usize, PackageError> {
    let start = glue
        .find(marker)
        .ok_or_else(|| PackageError::Glue(format!("missing {marker}")))?
        + marker.len();
    let digits: String = glue[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse()
        .map_err(|_| PackageError::Glue(format!("bad number after {marker}")))
}
