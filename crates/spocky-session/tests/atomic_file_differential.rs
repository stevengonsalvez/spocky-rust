//! Differential check of `spocky_store::atomic::write_file_atomic` against
//! the pinned build's `writeJsonFileAtomic`: the same writes into the same
//! prepared trees must succeed or fail alike, with node's error `code` and
//! message text, leave no temp file behind, and write the same bytes.
//!
//! Normalized: the disposable home directory (`<home>`) and the temp file
//! suffix `.<pid>.<millis>.<uuid>.tmp`, nothing else.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use spocky_store::atomic::write_file_atomic;
use spocky_store::js_value::{JsObject, JsValue, stringify};

/// `[case, target relative to the case directory]`.
fn cases() -> Vec<(&'static str, String)> {
    let long = "x".repeat(300);
    vec![
        ("parentIsFile", "file/sub/a.json".to_owned()),
        ("directoryIsFile", "file/a.json".to_owned()),
        ("lockedDeep", "ro/x/y/a.json".to_owned()),
        ("lockedOpen", "ro/a.json".to_owned()),
        ("targetIsDirectory", "d/a.json".to_owned()),
        ("longName", format!("d/{long}.json")),
        ("longDirectory", format!("{long}/y/a.json")),
        ("symlinkLoop", "loop/a.json".to_owned()),
        ("fresh", "new/a/b/a.json".to_owned()),
    ]
}

/// The tree every case starts from.
fn prepare(directory: &Path) {
    std::fs::create_dir_all(directory.join("ro")).expect("ro");
    std::fs::set_permissions(directory.join("ro"), std::fs::Permissions::from_mode(0o500))
        .expect("lock ro");
    std::fs::write(directory.join("file"), "x").expect("file");
    std::fs::create_dir_all(directory.join("d/a.json")).expect("blocking directory");
    std::os::unix::fs::symlink(directory.join("loop"), directory.join("loop")).expect("loop");
}

fn unlock(directory: &Path) {
    let _ = std::fs::set_permissions(directory.join("ro"), std::fs::Permissions::from_mode(0o700));
}

const NODE_SCRIPT: &str = r#"
const [dist, home, casesJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { writeJsonFileAtomic } = await import(`${dist}/server/atomic-file.js`);
const fs = await import("node:fs");
const path = await import("node:path");
const out = {};
for (const [name, relative] of JSON.parse(casesJson)) {
  const target = path.join(home, name, relative);
  let result;
  try {
    await writeJsonFileAtomic(target, { a: 1 });
    result = { ok: fs.readFileSync(target, "utf8") };
  } catch (error) {
    result = { code: error.code, message: error.message };
  }
  let temps = null;
  try {
    temps = fs.readdirSync(path.dirname(target)).filter((entry) => entry.endsWith(".tmp")).length;
  } catch {}
  out[name] = { result, temps };
}
process.stdout.write(JSON.stringify(out));
"#;

fn rust_output(home: &Path) -> String {
    let mut out = JsObject::new();
    for (name, relative) in cases() {
        let target = home.join(name).join(&relative);
        let mut result = JsObject::new();
        match write_file_atomic(&target, "{\n  \"a\": 1\n}") {
            Ok(()) => result.insert(
                "ok",
                JsValue::String(std::fs::read_to_string(&target).expect("written")),
            ),
            Err(error) => {
                result.insert("code", JsValue::String(error.code()));
                result.insert("message", JsValue::String(error.to_string()));
            }
        }
        let temps =
            std::fs::read_dir(target.parent().expect("parent")).map_or(JsValue::Null, |entries| {
                let count = entries
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                    .count();
                JsValue::Number(f64::from(u32::try_from(count).expect("count")))
            });
        let mut case = JsObject::new();
        case.insert("result", JsValue::Object(result));
        case.insert("temps", temps);
        out.insert(name, JsValue::Object(case));
    }
    stringify(&JsValue::Object(out))
}

/// Replaces `home` with `<home>` and each temp suffix
/// `.<digits>.<digits>.<uuid>.tmp` with `.<pid>.<millis>.<uuid>.tmp`.
fn normalize(text: &str, home: &str) -> String {
    let text = text.replace(home, "<home>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(index) = rest.find(".tmp") {
        let (head, tail) = rest.split_at(index);
        match temp_suffix_start(head) {
            Some(start) => {
                out.push_str(&head[..start]);
                out.push_str(".<pid>.<millis>.<uuid>");
            }
            None => out.push_str(head),
        }
        out.push_str(".tmp");
        rest = &tail[4..];
    }
    out.push_str(rest);
    out
}

/// Where `.<digits>.<digits>.<uuid>` starts, when `head` ends with it.
fn temp_suffix_start(head: &str) -> Option<usize> {
    let uuid_start = head.len().checked_sub(36)?;
    let uuid = &head[uuid_start..];
    let is_uuid = uuid.char_indices().all(|(index, character)| {
        if [8, 13, 18, 23].contains(&index) {
            character == '-'
        } else {
            character.is_ascii_hexdigit()
        }
    });
    if !is_uuid || !head[..uuid_start].ends_with('.') {
        return None;
    }
    let mut end = uuid_start - 1;
    for _ in 0..2 {
        let digits = head[..end]
            .bytes()
            .rev()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits == 0 || !head[..end - digits].ends_with('.') {
            return None;
        }
        end -= digits + 1;
    }
    Some(end)
}

#[test]
fn normalize_only_touches_home_and_temp_suffixes() {
    let text = "EISDIR: x, rename '/h/d/.a.json.81063.1790891361759.dbc10825-60a8-4a41-a4c1-ef3fcd3ba69e.tmp' -> '/h/d/a.json' .b.1.x.tmp 12.tmp";
    assert_eq!(
        normalize(text, "/h"),
        "EISDIR: x, rename '<home>/d/.a.json.<pid>.<millis>.<uuid>.tmp' -> '<home>/d/a.json' .b.1.x.tmp 12.tmp"
    );
}

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/atomic-file.js",
    "835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25",
)];

fn assert_pinned_modules(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    for (path, expected) in PINNED_MODULES {
        let bytes = std::fs::read(Path::new(dist).join(path)).expect("pinned module");
        let actual = Sha256::digest(&bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_eq!(&actual, expected, "{path} is not the pinned build");
    }
}

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        for (name, _) in cases() {
            unlock(&self.0.join(name));
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(name: &str) -> Home {
    let path = std::env::temp_dir().join(format!("spocky-atomic-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("disposable home");
    let path = std::fs::canonicalize(&path).expect("canonical home");
    for (case, _) in cases() {
        prepare(&path.join(case));
    }
    Home(path)
}

#[test]
fn atomic_writes_fail_like_the_pinned_build() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: atomic file differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let node_home = home("node");
    let rust_home = home("rust");
    let cases_json = stringify(&JsValue::Array(
        cases()
            .into_iter()
            .map(|(name, relative)| {
                JsValue::Array(vec![
                    JsValue::String(name.to_owned()),
                    JsValue::String(relative),
                ])
            })
            .collect(),
    ));
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(&node_home.0)
        .arg(&cases_json)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = normalize(
        &String::from_utf8_lossy(&output.stdout),
        &node_home.0.to_string_lossy(),
    );
    let actual = normalize(&rust_output(&rust_home.0), &rust_home.0.to_string_lossy());
    assert_eq!(actual, expected);
}
