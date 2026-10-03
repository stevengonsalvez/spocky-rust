//! Differential check of the default rewind SDK: the pinned
//! `forkSession` of `@anthropic-ai/claude-agent-sdk` and `fork_session_in` fork
//! the same transcripts and write the same file. Normalized: the fresh uuids
//! of the fork (`<new-N>`, numbered by first appearance) and the fork's clock
//! (`<now>`); fixtures use uuids starting `11111111-` and timestamps in 2001,
//! which stay as they are.

mod support;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_provider_claude::fork_session::{fork_session_in, is_control_or_format};

const CASES: &str = include_str!("fork_session_cases.json");

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "../../../../node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
        "bf86ef08eff553cb8e64262ab1575c76f9a7f4a7a3b4d5dc723e4f3bc6e568af",
    ),
    (
        "../../../../node_modules/@anthropic-ai/claude-agent-sdk/package.json",
        "e629045710dfe1308691ac81a42573508c18d9062a2f4c8676a24e116689714c",
    ),
];

const NODE_SCRIPT: &str = r#"
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
const [dist, casesFile, configDir] = process.argv.slice(1);
const sdk = await import(pathToFileURL(resolve(dist, "../../../../node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs")).href);
const cases = JSON.parse(readFileSync(casesFile, "utf8"));
const lines = [];
for (const c of cases) {
  try {
    const result = await sdk.forkSession(c.sessionId, c.upTo === undefined ? {} : { upToMessageId: c.upTo });
    let file = null;
    for (const project of readdirSync(join(configDir, "projects"))) {
      const candidate = join(configDir, "projects", project, `${result.sessionId}.jsonl`);
      if (existsSync(candidate)) file = readFileSync(candidate, "utf8");
    }
    lines.push(JSON.stringify({ ok: result.sessionId, file }));
  } catch (error) {
    lines.push(JSON.stringify({ error: error instanceof Error ? error.message : String(error) }));
  }
}
process.stdout.write(lines.join("\n") + "\n");
"#;

fn write_fixtures(files: &[JsValue], config_dir: &Path) {
    for file in files {
        let path = config_dir.join(file.get("path").and_then(JsValue::as_str).expect("path"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let bytes: Vec<u8> = if let Some(hex) = file.get("hex").and_then(JsValue::as_str) {
            (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                .collect()
        } else if let Some(text) = file.get("text").and_then(JsValue::as_str) {
            text.as_bytes().to_vec()
        } else {
            file.get("lines")
                .and_then(JsValue::as_array)
                .unwrap_or_default()
                .iter()
                .fold(String::new(), |mut all, line| {
                    all.push_str(&stringify(line));
                    all.push('\n');
                    all
                })
                .into_bytes()
        };
        std::fs::write(path, bytes).expect("fixture");
    }
}

/// Fresh uuids become `<new-N>`, a clock in the present `<now>`.
fn normalize(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut seen: Vec<String> = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        if let Some(candidate) = uuid_at(&characters, index) {
            if candidate.starts_with("11111111-") {
                out.push_str(&candidate);
            } else {
                let position = seen
                    .iter()
                    .position(|known| *known == candidate)
                    .unwrap_or_else(|| {
                        seen.push(candidate.clone());
                        seen.len() - 1
                    });
                let _ = write!(out, "<new-{position}>");
            }
            index += 36;
        } else if let Some(stamp) = timestamp_at(&characters, index) {
            if stamp.starts_with("2001-") {
                out.push_str(&stamp);
            } else {
                out.push_str("<now>");
            }
            index += stamp.chars().count();
        } else {
            out.push(characters[index]);
            index += 1;
        }
    }
    out
}

fn uuid_at(characters: &[char], at: usize) -> Option<String> {
    let slice = characters.get(at..at + 36)?;
    let candidate: String = slice.iter().collect();
    let shaped = candidate.chars().enumerate().all(|(index, character)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            character == '-'
        } else {
            character.is_ascii_hexdigit()
        }
    });
    shaped.then_some(candidate)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn timestamp_at(characters: &[char], at: usize) -> Option<String> {
    let slice = characters.get(at..at + 24)?;
    let candidate: String = slice.iter().collect();
    let bytes = candidate.as_bytes();
    let shaped = bytes.iter().enumerate().all(|(index, byte)| match index {
        4 | 7 => *byte == b'-',
        10 => *byte == b'T',
        13 | 16 => *byte == b':',
        19 => *byte == b'.',
        23 => *byte == b'Z',
        _ => byte.is_ascii_digit(),
    });
    shaped.then_some(candidate)
}

fn find_fork(config_dir: &Path, session_id: &str) -> Option<String> {
    let projects = config_dir.join("projects");
    for project in std::fs::read_dir(projects).ok()? {
        let candidate = project.ok()?.path().join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            return std::fs::read_to_string(candidate).ok();
        }
    }
    None
}

fn rust_line(config_dir: &Path, case: &JsValue) -> String {
    let session_id = case.get("sessionId").and_then(JsValue::as_str).expect("id");
    let up_to = case.get("upTo").and_then(JsValue::as_str);
    let mut object = spocky_contracts::js_value::JsObject::new();
    match fork_session_in(&config_dir.to_string_lossy(), session_id, up_to) {
        Ok(forked) => {
            object.insert("ok", JsValue::String(forked.clone()));
            object.insert(
                "file",
                find_fork(config_dir, &forked).map_or(JsValue::Null, JsValue::String),
            );
        }
        Err(error) => object.insert("error", JsValue::String(error.message)),
    }
    stringify(&JsValue::Object(object))
}

#[test]
fn forks_match_the_pinned_sdk() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let parsed = parse(CASES).expect("cases");
    let files = parsed
        .get("files")
        .and_then(JsValue::as_array)
        .expect("files");
    let cases = parsed
        .get("cases")
        .and_then(JsValue::as_array)
        .expect("cases");
    let scratch = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp")
        .join(format!("spocky-fork-diff-{}", std::process::id()));
    let node_config = scratch.join("node-config");
    let rust_config = scratch.join("rust-config");
    for config in [&node_config, &rust_config] {
        std::fs::create_dir_all(config).expect("config dir");
        write_fixtures(files, config);
    }
    let cases_file = scratch.join("cases.json");
    std::fs::write(&cases_file, stringify(&JsValue::Array(cases.to_vec()))).expect("cases file");
    let output = Command::new(timeout_binary())
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(&cases_file)
        .arg(&node_config)
        .env("CLAUDE_CONFIG_DIR", &node_config)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<String> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(expected.len(), cases.len(), "one line per case");
    let mut failures = Vec::new();
    for (case, node_line) in cases.iter().zip(&expected) {
        let name = case
            .get("name")
            .and_then(JsValue::as_str)
            .unwrap_or_default();
        let node_text = normalize(node_line);
        let rust_text = normalize(&rust_line(&rust_config, case));
        if node_text != rust_text {
            failures.push(format!("{name}\n  node: {node_text}\n  rust: {rust_text}"));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} forks differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

#[test]
fn control_and_format_characters_match_unicode() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let script = r#"
const ranges = [];
let start = -1;
for (let code = 0; code <= 0x110000; code++) {
  const hit = code <= 0x10ffff && !(code >= 0xd800 && code <= 0xdfff) &&
    /[\p{Cc}\p{Cf}\u2028\u2029]/u.test(String.fromCodePoint(code));
  if (hit && start < 0) start = code;
  if (!hit && start >= 0) { ranges.push(`${start.toString(16)}-${(code - 1).toString(16)}`); start = -1; }
}
process.stdout.write(ranges.join("\n") + "\n");
"#;
    let output = support::run_node(&node, &dist, script, &[]);
    let mut rust = Vec::new();
    let mut start: Option<u32> = None;
    for code in 0..=0x11_0000_u32 {
        let hit = char::from_u32(code).is_some_and(is_control_or_format);
        match (hit, start) {
            (true, None) => start = Some(code),
            (false, Some(first)) => {
                rust.push(format!("{first:x}-{:x}", code - 1));
                start = None;
            }
            _ => {}
        }
    }
    let node_ranges: Vec<&str> = output.lines().collect();
    assert_eq!(
        rust.iter().map(String::as_str).collect::<Vec<_>>(),
        node_ranges
    );
}
