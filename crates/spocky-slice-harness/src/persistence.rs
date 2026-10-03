//! The scoped `persistence-enrichment-timing` transform (g4-retry only).
//!
//! The pinned daemon fills an agent snapshot's `persistence` handle in two
//! steps: a minimal `{provider, sessionId, metadata: {cwd}}` first, then the
//! full handle (`nativeHandle` and the whole metadata). When the second step
//! lands depends on timing: in a run under load it was already there at
//! `prompt_started`, in the usual run only from the first
//! `wait_for_finish_response`. This module accepts exactly two shapes in the
//! early snapshots (`agent_ready`, `prompt_started`, `completed` and
//! `agent.create.response`, and the stored creation record's
//! `/snapshot/agent/persistence`): the minimal one,
//! or, byte for byte, the full handle the same side emits at its first
//! `wait_for_finish_response`. The second is rewritten to the minimal one so
//! both compare equal; any other shape is left as it is and so fails.

use std::path::Path;

use serde_json::{Value, json};

use crate::side::SideRun;

/// Id of the persistence enrichment transform.
pub const PERSISTENCE_TRANSFORM: &str = "persistence-enrichment-timing";

/// The only gate this transform applies to.
pub const PERSISTENCE_GATE: &str = "g4-retry";

/// Where a stored creation record keeps the agent snapshot's handle.
const RECORD_HANDLE: &str = "/snapshot/agent/persistence";

const EARLY_PHASES: [&str; 3] = ["agent_ready", "prompt_started", "completed"];

fn serialized(value: &Value) -> String {
    value.to_string()
}

fn session_message(frame: &Value) -> Option<&Value> {
    (frame.get("type")? == "session")
        .then(|| frame.get("message"))
        .flatten()
}

/// The full handle: the `persistence` of the FIRST `wait_for_finish_response`
/// in the probe's wire. The first line that mentions that message decides:
/// when it is not UTF-8, not JSON, not a session message of that type, does
/// not serialize back to itself byte for byte, or carries no `nativeHandle`,
/// there is no full handle and the transform does not apply. A later line
/// never stands in for it.
fn full_handle(stdout: &[u8]) -> Option<Value> {
    const MESSAGE: &[u8] = b"wait_for_finish_response";
    for line in stdout.split(|byte| *byte == b'\n') {
        if !line.windows(MESSAGE.len()).any(|window| window == MESSAGE) {
            continue;
        }
        let text = std::str::from_utf8(line).ok()?;
        let frame = serde_json::from_str::<Value>(text).ok()?;
        let message = session_message(&frame)?;
        if message.get("type") != Some(&Value::from("wait_for_finish_response")) {
            return None;
        }
        // It must not change under a rewrite.
        if serialized(&frame) != text {
            return None;
        }
        let handle = message.pointer("/payload/final/persistence")?;
        handle.get("nativeHandle")?;
        return Some(handle.clone());
    }
    None
}

/// The minimal handle that goes with a full one.
fn minimal_handle(full: &Value) -> Option<Value> {
    Some(json!({
        "provider": full.get("provider")?,
        "sessionId": full.get("sessionId")?,
        "metadata": {"cwd": full.pointer("/metadata/cwd")?},
    }))
}

/// Whether the frame is an early agent snapshot of a create.
fn early_snapshot(frame: &Value) -> bool {
    let Some(message) = session_message(frame) else {
        return false;
    };
    match message.get("type").and_then(Value::as_str) {
        Some("agent.create.update") => message
            .pointer("/payload/phase")
            .and_then(Value::as_str)
            .is_some_and(|phase| EARLY_PHASES.contains(&phase)),
        Some("agent.create.response") => true,
        _ => false,
    }
}

/// The probe wire as bytes with every early full handle rewritten to the
/// minimal one, or `None` when nothing changed. A line that is not UTF-8, or
/// not JSON, or does not serialize back to itself, keeps its bytes.
fn rewritten_wire(stdout: &[u8], full: &Value, minimal: &Value) -> Option<Vec<u8>> {
    let full_text = full.to_string();
    let mut changed = false;
    let lines: Vec<Vec<u8>> = stdout
        .split(|byte| *byte == b'\n')
        .map(|line| {
            let Ok(text) = std::str::from_utf8(line) else {
                return line.to_vec();
            };
            let Ok(mut frame) = serde_json::from_str::<Value>(text) else {
                return line.to_vec();
            };
            // Rewriting must change nothing but the handle.
            if serialized(&frame) != text || !early_snapshot(&frame) {
                return line.to_vec();
            }
            match frame.pointer_mut("/message/payload/agent/persistence") {
                Some(handle) if serialized(handle) == full_text => {
                    *handle = minimal.clone();
                    changed = true;
                    frame.to_string().into_bytes()
                }
                _ => line.to_vec(),
            }
        })
        .collect();
    changed.then(|| lines.join(&b'\n'))
}

/// A stored creation record with its early full handle rewritten, or `None`
/// when nothing changed or the record would not serialize back to itself.
fn rewritten_record(text: &str, full: &Value, minimal: &Value) -> Option<String> {
    let body = text.trim_end_matches('\n');
    let suffix = &text[body.len()..];
    let mut record = serde_json::from_str::<Value>(body).ok()?;
    // Rewriting must change nothing but the handle.
    if serde_json::to_string_pretty(&record).ok()? != body {
        return None;
    }
    // Only at the one path a creation record keeps the agent's handle.
    let handle = record.pointer_mut(RECORD_HANDLE)?;
    if serialized(handle) != serialized(full) {
        return None;
    }
    *handle = minimal.clone();
    serde_json::to_string_pretty(&record)
        .ok()
        .map(|text| format!("{text}{suffix}"))
}

/// The side with the early full handles of the probe wire and of the stored
/// creation records rewritten to the minimal one, and the names of the
/// compared artifacts that changed (`step-<nn>-<name>/stdout` and state file
/// paths).
#[must_use]
pub fn without_enrichment_race(side: &SideRun) -> (SideRun, Vec<String>) {
    let mut side = side.clone();
    let mut names = Vec::new();
    let full = side.steps.iter().find_map(|step| full_handle(&step.stdout));
    let Some(minimal) = full.as_ref().and_then(minimal_handle) else {
        return (side, names);
    };
    let full = full.unwrap_or(Value::Null);
    for (index, step) in side.steps.iter_mut().enumerate() {
        if let Some(new) = rewritten_wire(&step.stdout, &full, &minimal) {
            step.stdout = new;
            names.push(format!("step-{:02}-{}/stdout", index + 1, step.name));
        }
    }
    for file in &mut side.state {
        let is_json = Path::new(&file.path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if !file.path.contains("/creations/") || !is_json {
            continue;
        }
        let Ok(text) = String::from_utf8(file.bytes.clone()) else {
            continue;
        };
        if let Some(new) = rewritten_record(&text, &full, &minimal) {
            file.bytes = new.into_bytes();
            names.push(file.path.clone());
        }
    }
    (side, names)
}

/// Id of the stored persistence enrichment race class.
pub const STORED_RACE_TRANSFORM: &str = "stored-persistence-enrichment-race";

/// Whether a gate id is a G4 gate, the only ones the stored race class
/// applies to.
#[must_use]
pub fn is_g4_gate(id: &str) -> bool {
    id.starts_with("g4-")
}

fn keys(handle: &Value) -> Option<Vec<&str>> {
    Some(handle.as_object()?.keys().map(String::as_str).collect())
}

/// Exactly `{provider, sessionId, metadata: {cwd}}`, in that order.
fn is_minimal_shape(handle: &Value) -> bool {
    keys(handle).is_some_and(|keys| keys == ["provider", "sessionId", "metadata"])
        && handle.get("provider").is_some_and(Value::is_string)
        && handle.get("sessionId").is_some_and(Value::is_string)
        && handle
            .get("metadata")
            .and_then(Value::as_object)
            .is_some_and(|metadata| {
                metadata.len() == 1 && metadata.get("cwd").is_some_and(Value::is_string)
            })
}

/// `{provider, sessionId, nativeHandle, metadata}` in that order, the native
/// handle equal to the session id and the metadata carrying the same
/// provider and a `cwd`.
fn is_full_shape(handle: &Value) -> bool {
    let metadata = handle.get("metadata");
    keys(handle).is_some_and(|keys| keys == ["provider", "sessionId", "nativeHandle", "metadata"])
        && handle.get("provider").is_some_and(Value::is_string)
        && handle.get("sessionId").is_some_and(Value::is_string)
        && handle.get("nativeHandle") == handle.get("sessionId")
        && metadata.is_some_and(Value::is_object)
        && metadata
            .and_then(|m| m.get("cwd"))
            .is_some_and(Value::is_string)
        && metadata.and_then(|m| m.get("provider")) == handle.get("provider")
}

/// One stored creation record that has an agent handle.
struct Stored {
    file: usize,
    created: String,
    record: Value,
    suffix: String,
}

/// The side's stored creation records with an agent handle, oldest first by
/// the agent's `createdAt`. `None` when any of them does not serialize back
/// to itself or has no `createdAt`, so the class then does not apply.
fn stored_records(side: &SideRun) -> Option<Vec<Stored>> {
    let mut found = Vec::new();
    for (file, captured) in side.state.iter().enumerate() {
        let is_json = Path::new(&captured.path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if !captured.path.contains("/creations/") || !is_json {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&captured.bytes) else {
            continue;
        };
        let body = text.trim_end_matches('\n');
        let Ok(record) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        if record.pointer(RECORD_HANDLE).is_none() {
            continue;
        }
        if serde_json::to_string_pretty(&record).ok()? != body {
            return None;
        }
        let created = record
            .pointer("/snapshot/agent/createdAt")?
            .as_str()?
            .to_owned();
        found.push(Stored {
            file,
            created,
            record,
            suffix: text[body.len()..].to_owned(),
        });
    }
    found.sort_by(|a, b| a.created.cmp(&b.created));
    Some(found)
}

/// The record's handle rewritten to the minimal shape of its own provider,
/// session id and `cwd`.
fn minimized(stored: &Stored) -> Option<Vec<u8>> {
    let mut record = stored.record.clone();
    let handle = record.pointer_mut(RECORD_HANDLE)?;
    let minimal = json!({
        "provider": handle.get("provider")?,
        "sessionId": handle.get("sessionId")?,
        "metadata": {"cwd": handle.pointer("/metadata/cwd")?},
    });
    *handle = minimal;
    let text = serde_json::to_string_pretty(&record).ok()?;
    Some(format!("{text}{}", stored.suffix).into_bytes())
}

/// The stored-persistence-enrichment-race class (every G4 gate). A stored
/// creation record's `/snapshot/agent/persistence` may be the minimal handle
/// or a full handle; where one side stored the full handle and the other the
/// minimal one, the full one is rewritten to the minimal shape of its own
/// provider, session id and `cwd`, so the normal comparison then fails on any
/// difference in those three fields. Records are paired oldest first by
/// `createdAt`; a different count, or any other shape, is left raw. Returns
/// the sides and, per side, the stored file paths rewritten.
#[must_use]
pub fn without_stored_race(
    left: &SideRun,
    right: &SideRun,
) -> ((SideRun, Vec<String>), (SideRun, Vec<String>)) {
    let mut sides = [(left.clone(), Vec::new()), (right.clone(), Vec::new())];
    let (Some(l), Some(r)) = (stored_records(left), stored_records(right)) else {
        let [a, b] = sides;
        return (a, b);
    };
    if l.len() == r.len() {
        for (mine, theirs) in l.iter().zip(&r) {
            for (side, (own, other)) in [(0, (mine, theirs)), (1, (theirs, mine))] {
                let handle = own.record.pointer(RECORD_HANDLE);
                let against = other.record.pointer(RECORD_HANDLE);
                if handle.is_some_and(is_full_shape)
                    && against.is_some_and(is_minimal_shape)
                    && let Some(bytes) = minimized(own)
                {
                    let (run, names) = &mut sides[side];
                    names.push(run.state[own.file].path.clone());
                    run.state[own.file].bytes = bytes;
                }
            }
        }
    }
    let [a, b] = sides;
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{"provider":"codex","sessionId":"s1","metadata":{"cwd":"/p"}}"#;
    const FULL: &str = r#"{"provider":"codex","sessionId":"s1","nativeHandle":"s1","metadata":{"provider":"codex","cwd":"/p","title":null,"threadId":"s1"}}"#;

    fn frame(kind: &str, phase: &str, handle: &str) -> String {
        format!(
            r#"{{"type":"session","message":{{"type":"{kind}","payload":{{"phase":"{phase}","agent":{{"id":"a","persistence":{handle}}}}}}}}}"#
        )
    }

    fn later(handle: &str) -> String {
        format!(
            r#"{{"type":"session","message":{{"type":"wait_for_finish_response","payload":{{"final":{{"id":"a","persistence":{handle}}}}}}}}}"#
        )
    }

    fn wire(early: &[&str], later_handle: &str) -> String {
        let mut lines = vec![
            "{\"outcomes\":[]}".to_owned(),
            "# recording client".to_owned(),
        ];
        for (index, handle) in early.iter().enumerate() {
            let phase = EARLY_PHASES[index.min(2)];
            lines.push(frame("agent.create.update", phase, handle));
        }
        lines.push(frame("agent.create.response", "", early[0]));
        lines.push(later(later_handle));
        lines.join("\n")
    }

    fn rewritten(stdout: &str) -> String {
        let full = full_handle(stdout.as_bytes()).unwrap();
        let minimal = minimal_handle(&full).unwrap();
        rewritten_wire(stdout.as_bytes(), &full, &minimal).map_or_else(
            || stdout.to_owned(),
            |bytes| String::from_utf8(bytes).unwrap(),
        )
    }

    #[test]
    fn an_early_full_handle_becomes_the_minimal_one() {
        let usual = wire(&[MINIMAL, MINIMAL, MINIMAL], FULL);
        let raced = wire(&[MINIMAL, FULL, FULL], FULL);
        assert_eq!(rewritten(&raced), usual);
        // The usual wire has nothing to rewrite.
        assert_eq!(rewritten(&usual), usual);
    }

    #[test]
    fn a_third_shape_is_left_alone() {
        let third =
            r#"{"provider":"codex","sessionId":"s1","nativeHandle":"s1","metadata":{"cwd":"/p"}}"#;
        let odd = wire(&[MINIMAL, third, MINIMAL], FULL);
        assert_eq!(rewritten(&odd), odd);
    }

    #[test]
    fn a_full_handle_that_differs_from_the_later_one_is_left_alone() {
        let other = FULL.replace("threadId\":\"s1", "threadId\":\"s2");
        let odd = wire(&[MINIMAL, &other, MINIMAL], FULL);
        assert_eq!(rewritten(&odd), odd);
        // Same fields in another order is not the same bytes either.
        let reordered = r#"{"provider":"codex","nativeHandle":"s1","sessionId":"s1","metadata":{"provider":"codex","cwd":"/p","title":null,"threadId":"s1"}}"#;
        let odd = wire(&[MINIMAL, reordered, MINIMAL], FULL);
        assert_eq!(rewritten(&odd), odd);
    }

    #[test]
    fn a_stored_record_is_rewritten_the_same_way() {
        let record = |handle: &str| {
            let value: Value = serde_json::from_str(&format!(
                r#"{{"fingerprint":"f","snapshot":{{"agent":{{"id":"a","persistence":{handle}}}}}}}"#
            ))
            .unwrap();
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
        };
        let stdout = wire(&[MINIMAL], FULL);
        let full = full_handle(stdout.as_bytes()).unwrap();
        let minimal = minimal_handle(&full).unwrap();
        assert_eq!(
            rewritten_record(&record(FULL), &full, &minimal).unwrap(),
            record(MINIMAL)
        );
        assert!(rewritten_record(&record(MINIMAL), &full, &minimal).is_none());
        // Not byte-stable under a rewrite: left alone.
        let compact = record(FULL).replace("\n  ", "\n    ");
        assert!(rewritten_record(&compact, &full, &minimal).is_none());
    }

    #[test]
    fn only_the_first_wait_for_finish_response_can_supply_the_full_handle() {
        let mut lines = vec![frame("agent.create.update", "prompt_started", FULL)];
        lines.push(later(MINIMAL));
        lines.push(later(FULL));
        let stdout = lines.join("\n");
        // The first frame has no nativeHandle: no full handle, the class
        // does not apply, and the later frame does not stand in for it.
        assert!(full_handle(stdout.as_bytes()).is_none());
    }

    #[test]
    fn a_record_is_rewritten_only_at_the_snapshot_agent_path() {
        let stdout = wire(&[MINIMAL], FULL);
        let full = full_handle(stdout.as_bytes()).unwrap();
        let minimal = minimal_handle(&full).unwrap();
        let elsewhere: Value = serde_json::from_str(&format!(
            r#"{{"fingerprint":"f","other":{{"agent":{{"persistence":{FULL}}}}},"snapshot":{{"agent":{{"persistence":{MINIMAL}}}}}}}"#
        ))
        .unwrap();
        let text = format!("{}\n", serde_json::to_string_pretty(&elsewhere).unwrap());
        assert!(rewritten_record(&text, &full, &minimal).is_none());
    }

    #[test]
    fn a_first_wait_line_that_does_not_round_trip_supplies_nothing() {
        let first = later(FULL).replacen("\"type\":\"session\"", "\"type\": \"session\"", 1);
        assert_ne!(first, later(FULL));
        let stdout = [first, later(FULL)].join("\n");
        // The first line decides and it would change under a rewrite.
        assert!(full_handle(stdout.as_bytes()).is_none());
        let stdout = [later(FULL), later(FULL)].join("\n");
        assert!(full_handle(stdout.as_bytes()).is_some());
    }

    #[test]
    fn bytes_that_are_not_utf8_survive_a_rewrite() {
        let raced = wire(&[FULL], FULL);
        let full = full_handle(raced.as_bytes()).unwrap();
        let minimal = minimal_handle(&full).unwrap();
        let mut bytes = b"\xff\xfe raw \xc3(\n".to_vec();
        bytes.extend_from_slice(raced.as_bytes());
        bytes.extend_from_slice(b"\n\xe2\x28\xa1 tail");
        let new = rewritten_wire(&bytes, &full, &minimal).unwrap();
        assert!(new.starts_with(b"\xff\xfe raw \xc3(\n"));
        assert!(new.ends_with(b"\n\xe2\x28\xa1 tail"));
        let count = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .matches("nativeHandle")
                .count()
        };
        // The two early full handles are gone; the one at wait_for_finish stays.
        assert_eq!((count(&bytes), count(&new)), (3, 1));
    }

    #[test]
    fn an_unparseable_first_wait_line_means_the_class_does_not_apply() {
        let good = later(FULL);
        // Broken JSON mentioning the message, then a good line.
        let broken = r#"{"type":"session","message":{"type":"wait_for_finish_response","#;
        assert!(full_handle([broken, good.as_str()].join("\n").as_bytes()).is_none());
        // Not UTF-8 mentioning the message.
        let mut bytes = b"\xff wait_for_finish_response\n".to_vec();
        bytes.extend_from_slice(good.as_bytes());
        assert!(full_handle(&bytes).is_none());
        // Sanity: the good line alone supplies the handle.
        assert!(full_handle(good.as_bytes()).is_some());
    }
}
