//! Gate definitions. Only gates whose scenario is fully specified here exist;
//! the gate runner rejects any other id.

use serde_json::json;

use crate::side::{Arg, Check, GateSpec, StepSpec};
use crate::stub::{Script, ScriptedReply};

/// The fixed G1 prompt.
pub const G1_PROMPT: &str = "Reply with the single word READY.";
/// The scripted assistant reply for G1.
pub const G1_REPLY: &str = "READY";

fn completed_turn(response_id: &str, message_id: &str, text: &str) -> ScriptedReply {
    ScriptedReply {
        status: 200,
        events: vec![
            json!({"type": "response.created", "response": {"id": response_id}}),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "id": message_id,
                    "content": [{"type": "output_text", "text": text}]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "usage": {
                        "input_tokens": 0,
                        "input_tokens_details": null,
                        "output_tokens": 0,
                        "output_tokens_details": null,
                        "total_tokens": 0
                    }
                }
            }),
        ],
        json: None,
    }
}

/// G1: real Codex happy path. `workspace create`, `run` with full access in
/// that workspace, then `logs`, `ls -a`, and `inspect` of the new agent.
#[must_use]
pub fn g1() -> GateSpec {
    use Arg::{Captured, Host, Lit, Project};
    GateSpec {
        id: "g1",
        script: Script {
            responses: vec![completed_turn("resp_g1_1", "msg_g1_1", G1_REPLY)],
        },
        steps: vec![
            StepSpec {
                name: "workspace-create",
                args: vec![
                    Lit("workspace"),
                    Lit("create"),
                    Host,
                    Lit("--json"),
                    Lit("--isolation"),
                    Lit("local"),
                    Lit("--path"),
                    Project,
                ],
                capture: Some(("workspace", "/workspaceId")),
            },
            StepSpec {
                name: "run",
                args: vec![
                    Lit("run"),
                    Host,
                    Lit("--json"),
                    Lit("--provider"),
                    Lit("codex"),
                    Lit("--mode"),
                    Lit("full-access"),
                    Lit("--workspace"),
                    Captured("workspace"),
                    Lit(G1_PROMPT),
                ],
                capture: Some(("agent", "/agentId")),
            },
            StepSpec {
                name: "logs",
                args: vec![Lit("logs"), Host, Lit("--json"), Captured("agent")],
                capture: None,
            },
            StepSpec {
                name: "ls",
                args: vec![Lit("ls"), Host, Lit("--json"), Lit("-a")],
                capture: None,
            },
            StepSpec {
                name: "inspect",
                args: vec![Lit("inspect"), Host, Lit("--json"), Captured("agent")],
                capture: None,
            },
        ],
        checks: vec![
            Check::AllExitZero,
            Check::JsonString {
                step: "run",
                pointer: "/status",
                expected: "completed",
            },
            Check::StdoutContains {
                step: "logs",
                needle: G1_REPLY,
            },
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
    }
}

/// Looks up a gate by id.
#[must_use]
pub fn by_id(id: &str) -> Option<GateSpec> {
    match id {
        "g1" => Some(g1()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::validate_script;

    #[test]
    fn g1_script_is_valid_and_steps_capture_before_use() {
        let gate = g1();
        assert!(validate_script(&gate.script).is_ok());
        let mut captured: Vec<&str> = Vec::new();
        for step in &gate.steps {
            for arg in &step.args {
                if let Arg::Captured(key) = arg {
                    assert!(
                        captured.contains(key),
                        "{} uses {key} before capture",
                        step.name
                    );
                }
            }
            if let Some((key, _)) = step.capture {
                captured.push(key);
            }
        }
        assert!(by_id("g2").is_none());
    }
}
