//! `providers/provider-image-output.ts`: base64 image tool results written
//! to a private temp directory and rendered as assistant markdown.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::js_trim;

const PROVIDER_IMAGE_ATTACHMENT_DIR: &str = "paseo-attachments";

/// `ProviderImageOutput`: the base64 form Claude tool results carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderImageOutput {
    pub data: String,
    pub mime_type: Option<String>,
}

/// The process-wide materialization directory (`materializedImageAttachmentDir`).
static ATTACHMENT_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn set_private(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

fn reusable(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
        && set_private(dir, 0o700).is_ok()
}

/// `fs.mkdtempSync(path.join(os.tmpdir(), "paseo-attachments-"))`.
fn mkdtemp() -> std::io::Result<PathBuf> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let base = std::env::temp_dir();
    loop {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let suffix: String = bytes
            .iter()
            .take(6)
            .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
            .collect();
        let candidate = base.join(format!("{PROVIDER_IMAGE_ATTACHMENT_DIR}-{suffix}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

fn attachment_dir() -> std::io::Result<PathBuf> {
    let mut slot = ATTACHMENT_DIR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(dir) = slot.as_ref()
        && reusable(dir)
    {
        return Ok(dir.clone());
    }
    let dir = mkdtemp()?;
    set_private(&dir, 0o700)?;
    *slot = Some(dir.clone());
    Ok(dir)
}

/// `Buffer.from(text, "base64")`: characters outside the (URL-safe
/// extended) alphabet are skipped and `=` ends the data.
#[must_use]
pub fn node_base64_decode(text: &str) -> Vec<u8> {
    let mut bits = 0_u32;
    let mut count = 0;
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push(u8::try_from((bits >> count) & 0xff).unwrap_or(0));
        }
    }
    bytes
}

fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "bin",
    }
}

/// `normalizeImageData(mimeType, data)`: a `data:<type>;base64,<data>` URI
/// splits into its parts.
fn normalize_image_data(mime_type: &str, data: &str) -> (String, String) {
    if let Some(rest) = data.strip_prefix("data:")
        && let Some((kind, payload)) = rest.split_once(";base64,")
        && !kind.is_empty()
        && !kind.contains(';')
        && !payload.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
    {
        return (kind.to_owned(), payload.to_owned());
    }
    (mime_type.to_owned(), data.to_owned())
}

/// `materializeProviderImage({ data, mimeType })`: the written file path.
///
/// # Errors
///
/// The filesystem error, which the baseline's caller catches.
pub fn materialize_provider_image(data: &str, mime_type: Option<&str>) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::OpenOptionsExt;
    let dir = attachment_dir()?;
    let (mime_type, payload) = normalize_image_data(mime_type.unwrap_or("image/png"), data);
    let bytes = node_base64_decode(&payload);
    let hash = Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let path = dir.join(format!("{hash}.{}", image_extension(&mime_type)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    set_private(&path, 0o600)?;
    Ok(path.to_string_lossy().into_owned())
}

/// `isProviderImageMarkdown(text)`.
#[must_use]
pub fn is_provider_image_markdown(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("![") else {
        return false;
    };
    let Some(close) = rest.find(']') else {
        return false;
    };
    let Some(source_and_rest) = rest[close + 1..].strip_prefix('(') else {
        return false;
    };
    // `[^)]*` then the attachments dir: try every occurrence before `)`.
    let source = source_and_rest.split(')').next().unwrap_or_default();
    if !source_and_rest[source.len()..].starts_with(')') {
        return false;
    }
    let mut search = 0;
    while let Some(found) = source[search..].find(PROVIDER_IMAGE_ATTACHMENT_DIR) {
        let start = search + found + PROVIDER_IMAGE_ATTACHMENT_DIR.len();
        if image_tail_matches(&source[start..]) {
            return true;
        }
        search = search + found + 1;
    }
    false
}

/// `(?:-[^/\\)]+)?[/\\]+(?:[^/\\)]+[/\\]+)?[0-9a-f]{64}\.[a-z0-9]+` over the
/// whole remaining source.
fn image_tail_matches(tail: &str) -> bool {
    let is_separator = |character: char| character == '/' || character == '\\';
    let mut candidates = vec![tail];
    if let Some(suffix) = tail.strip_prefix('-') {
        let segment = suffix.find(is_separator).unwrap_or(suffix.len());
        for end in 1..=segment {
            if suffix.is_char_boundary(end) {
                candidates.push(&suffix[end..]);
            }
        }
    }
    candidates.into_iter().any(|rest| {
        let separators = rest
            .chars()
            .take_while(|character| is_separator(*character))
            .count();
        if separators == 0 {
            return false;
        }
        let after = &rest[separators..];
        if file_name_matches(after) {
            return true;
        }
        // One optional `[^/\\)]+[/\\]+` directory segment.
        let segment = after.find(is_separator).unwrap_or(after.len());
        if segment == 0 || segment == after.len() {
            return false;
        }
        let rest = &after[segment..];
        let separators = rest
            .chars()
            .take_while(|character| is_separator(*character))
            .count();
        file_name_matches(&rest[separators..])
    })
}

/// `[0-9a-f]{64}\.[a-z0-9]+` covering the whole text.
fn file_name_matches(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 65
        && bytes[..64]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        && bytes[64] == b'.'
        && bytes[65..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `encodeURIComponent(segment)`.
fn encode_uri_component(segment: &str) -> String {
    let mut out = String::new();
    for character in segment.chars() {
        if character.is_ascii_alphanumeric() || "-_.!~*'()".contains(character) {
            out.push(character);
        } else {
            let mut buffer = [0_u8; 4];
            for byte in character.encode_utf8(&mut buffer).bytes() {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn markdown_image_source(value: &str) -> String {
    if value.starts_with('/') {
        let encoded = value
            .split('/')
            .map(encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        return format!("file://{encoded}");
    }
    value.to_owned()
}

fn escape_alt(value: &str) -> String {
    value.replace('\\', "\\\\").replace(']', "\\]")
}

fn escape_source(value: &str) -> String {
    markdown_image_source(value)
        .replace('\\', "\\\\")
        .replace(')', "\\)")
}

fn assistant_message(text: String) -> JsValue {
    let mut item = JsObject::new();
    item.insert("type", JsValue::String("assistant_message".to_owned()));
    item.insert("text", JsValue::String(text));
    JsValue::Object(item)
}

/// `renderProviderImageOutputAsAssistantMarkdown(image, { materialize:
/// materializeProviderImage })` for a base64 image.
#[must_use]
pub fn render_provider_image_output(image: &ProviderImageOutput) -> Option<JsValue> {
    let data = non_empty(Some(&image.data))?;
    let materialized =
        materialize_provider_image(&data, non_empty(image.mime_type.as_deref()).as_deref()).ok();
    let Some(path) = materialized.filter(|path| {
        !path.is_empty() && !js_trim(path).to_lowercase().starts_with("data:image/")
    }) else {
        return Some(assistant_message(
            "Image output was omitted because it was not available as a file path or URL."
                .to_owned(),
        ));
    };
    Some(assistant_message(format!(
        "![{}]({})",
        escape_alt("Image"),
        escape_source(&path)
    )))
}
