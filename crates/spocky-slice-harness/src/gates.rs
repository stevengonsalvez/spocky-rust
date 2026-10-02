//! Gate definitions. Only gates whose scenario is fully specified here exist;
//! the gate runner rejects any other id.

use std::collections::BTreeMap;

use serde_json::json;

use crate::side::{Arg, Check, GateSpec, HomeOrigin, StepSpec};
use crate::stub::{Script, ScriptedReply};

/// The fixed G1 prompt.
pub const G1_PROMPT: &str = "Reply with the single word READY.";
/// The scripted assistant reply for G1.
pub const G1_REPLY: &str = "READY";

fn completed(response_id: &str) -> serde_json::Value {
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
    })
}

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
            completed(response_id),
        ],
        json: None,
        hold_ms: None,
        delay_ms: None,
    }
}

/// A turn whose only output is a code-mode `exec` cell running one shell
/// command with `sandbox_permissions: "require_escalated"`, which codex must
/// route to the user for approval in `auto` mode (approval policy
/// `on-request`). The cell waits up to 300 s instead of yielding early, and
/// returns a fixed text so no timing reaches the next request.
fn approval_turn(
    response_id: &str,
    call_id: &str,
    command: &str,
    justification: &str,
) -> ScriptedReply {
    let source = format!(
        "// @exec: {{\"yield_time_ms\": 300000}}\n\
         await tools.exec_command({{cmd: {command}, sandbox_permissions: \"require_escalated\", justification: {justification}}});\n\
         text(\"done\");",
        command = serde_json::Value::from(command),
        justification = serde_json::Value::from(justification),
    );
    ScriptedReply {
        status: 200,
        events: vec![
            json!({"type": "response.created", "response": {"id": response_id}}),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "custom_tool_call",
                    "id": format!("ctc_{call_id}"),
                    "status": "completed",
                    "call_id": call_id,
                    "name": "exec",
                    "input": source
                }
            }),
            completed(response_id),
        ],
        json: None,
        hold_ms: None,
        delay_ms: None,
    }
}

/// A turn that starts and then stays in flight, so `stop` cancels it mid-turn.
fn held_turn(response_id: &str) -> ScriptedReply {
    ScriptedReply {
        status: 200,
        events: vec![json!({"type": "response.created", "response": {"id": response_id}})],
        json: None,
        hold_ms: Some(120_000),
        delay_ms: None,
    }
}

fn step(
    name: &'static str,
    args: Vec<Arg>,
    capture: Option<(&'static str, &'static str)>,
    wait_for_stub_requests: Option<usize>,
) -> StepSpec {
    StepSpec {
        name,
        args,
        capture,
        wait_for_stub_requests,
        daemon_restart: false,
        disconnect_at_stub_requests: None,
        node_script: None,
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
                wait_for_stub_requests: None,
                daemon_restart: false,
                disconnect_at_stub_requests: None,
                node_script: None,
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
                wait_for_stub_requests: None,
                daemon_restart: false,
                disconnect_at_stub_requests: None,
                node_script: None,
            },
            StepSpec {
                name: "logs",
                args: vec![Lit("logs"), Host, Lit("--json"), Captured("agent")],
                capture: None,
                wait_for_stub_requests: None,
                daemon_restart: false,
                disconnect_at_stub_requests: None,
                node_script: None,
            },
            StepSpec {
                name: "ls",
                args: vec![Lit("ls"), Host, Lit("--json"), Lit("-a")],
                capture: None,
                wait_for_stub_requests: None,
                daemon_restart: false,
                disconnect_at_stub_requests: None,
                node_script: None,
            },
            StepSpec {
                name: "inspect",
                args: vec![Lit("inspect"), Host, Lit("--json"), Captured("agent")],
                capture: None,
                wait_for_stub_requests: None,
                daemon_restart: false,
                disconnect_at_stub_requests: None,
                node_script: None,
            },
        ],
        checks: vec![
            Check::AllExitZero,
            Check::JsonString {
                step: "run",
                pointer: "/status",
                expected: "completed",
            },
            // A whole-line match: the prompt echo also contains the word.
            Check::StdoutLine {
                step: "logs",
                line: G1_REPLY,
            },
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
        preimages: g1_preimages,
        codex_present: true,
        home_origin: HomeOrigin::Same,
    }
}

/// Creation request fingerprint preimages, exactly as `creation/index.ts`
/// digests them: the request minus `requestId`, `type`, `subscribe`, and
/// `idempotencyKey`, keys sorted at every level.
fn creation_preimages(
    captured: &BTreeMap<&'static str, String>,
    mode: &str,
    prompt: &str,
) -> Vec<(&'static str, String)> {
    let mut preimages = Vec::new();
    let Some(project) = captured.get("project") else {
        return preimages;
    };
    preimages.push((
        "workspace-create-request",
        json!({"source": {"kind": "directory", "path": project}}).to_string(),
    ));
    if let Some(workspace) = captured.get("workspace") {
        preimages.push((
            "agent-create-request",
            json!({
                "config": {"cwd": project, "modeId": mode, "provider": "codex"},
                "initialPrompt": prompt,
                "labels": {},
                "workspaceId": workspace
            })
            .to_string(),
        ));
    }
    preimages
}

/// G1 creation fingerprint preimages.
#[must_use]
pub fn g1_preimages(captured: &BTreeMap<&'static str, String>) -> Vec<(&'static str, String)> {
    creation_preimages(captured, "full-access", G1_PROMPT)
}

/// The G2 prompt whose shell call is allowed.
pub const G2_PROMPT_ALLOW: &str = "Run the G2 allow probe.";
/// The G2 prompt whose shell call is denied.
pub const G2_PROMPT_DENY: &str = "Run the G2 deny probe.";
/// The G2 prompt whose turn is cancelled mid-flight.
pub const G2_PROMPT_HOLD: &str = "Start the G2 cancel probe.";
/// Scripted reply after the allowed call.
pub const G2_REPLY_ALLOWED: &str = "ALLOWED";
/// Scripted reply after the denied call.
pub const G2_REPLY_DENIED: &str = "DENIED";

/// G2 creation fingerprint preimages.
#[must_use]
pub fn g2_preimages(captured: &BTreeMap<&'static str, String>) -> Vec<(&'static str, String)> {
    creation_preimages(captured, "auto", G2_PROMPT_ALLOW)
}

/// G2: permission and cancel in `--mode auto`. A scripted shell call needs
/// approval: `permit ls` then `permit allow`; a second needs approval:
/// `permit ls` then `permit deny`; a third turn is held in flight and
/// cancelled with `stop`. Then `logs`, `ls -a`, and `inspect`.
#[must_use]
pub fn g2() -> GateSpec {
    GateSpec {
        id: "g2",
        script: g2_script(),
        steps: g2_steps(),
        checks: g2_checks(),
        preimages: g2_preimages,
        codex_present: true,
        home_origin: HomeOrigin::Same,
    }
}

fn g2_script() -> Script {
    Script {
        responses: vec![
            approval_turn(
                "resp_g2_1",
                "call_g2_allow",
                "printf G2-ALLOW",
                "G2 allow probe",
            ),
            completed_turn("resp_g2_2", "msg_g2_2", G2_REPLY_ALLOWED),
            approval_turn(
                "resp_g2_3",
                "call_g2_deny",
                "printf G2-DENY",
                "G2 deny probe",
            ),
            completed_turn("resp_g2_4", "msg_g2_4", G2_REPLY_DENIED),
            held_turn("resp_g2_5"),
        ],
    }
}

/// `<command words> --host <host> --json`, then `rest`.
fn cli(command: &[&'static str], rest: Vec<Arg>) -> Vec<Arg> {
    let mut args: Vec<Arg> = command.iter().map(|word| Arg::Lit(word)).collect();
    args.push(Arg::Host);
    args.push(Arg::Lit("--json"));
    args.extend(rest);
    args
}

fn g2_steps() -> Vec<StepSpec> {
    use Arg::{Captured, Lit, Project};
    let agent = || Captured("agent");
    vec![
        step(
            "workspace-create",
            cli(
                &["workspace", "create"],
                vec![Lit("--isolation"), Lit("local"), Lit("--path"), Project],
            ),
            Some(("workspace", "/workspaceId")),
            None,
        ),
        step(
            "run",
            cli(
                &["run"],
                vec![
                    Lit("--provider"),
                    Lit("codex"),
                    Lit("--mode"),
                    Lit("auto"),
                    Lit("--workspace"),
                    Captured("workspace"),
                    Lit(G2_PROMPT_ALLOW),
                ],
            ),
            Some(("agent", "/agentId")),
            None,
        ),
        step(
            "permit-ls-allow",
            cli(&["permit", "ls"], vec![]),
            Some(("permission_allow", "/0/id")),
            None,
        ),
        step(
            "permit-allow",
            cli(
                &["permit", "allow"],
                vec![agent(), Captured("permission_allow")],
            ),
            None,
            // Codex has sent the allowed tool output.
            Some(2),
        ),
        step(
            "wait-allowed",
            cli(&["wait"], vec![Lit("--timeout"), Lit("120"), agent()]),
            None,
            None,
        ),
        step(
            "send-deny",
            cli(&["send"], vec![agent(), Lit(G2_PROMPT_DENY)]),
            None,
            None,
        ),
        step(
            "permit-ls-deny",
            cli(&["permit", "ls"], vec![]),
            Some(("permission_deny", "/0/id")),
            None,
        ),
        step(
            "permit-deny",
            cli(
                &["permit", "deny"],
                vec![agent(), Captured("permission_deny")],
            ),
            None,
            // Codex has sent the denied tool output.
            Some(4),
        ),
        step(
            "wait-denied",
            cli(&["wait"], vec![Lit("--timeout"), Lit("120"), agent()]),
            None,
            None,
        ),
        step(
            "send-hold",
            cli(
                &["send"],
                vec![Lit("--no-wait"), agent(), Lit(G2_PROMPT_HOLD)],
            ),
            None,
            // Cancel mid-turn: stop runs once codex is streaming the held reply.
            Some(5),
        ),
        step("stop", cli(&["stop"], vec![agent()]), None, None),
        step("logs", cli(&["logs"], vec![agent()]), None, None),
        step("ls", cli(&["ls"], vec![Lit("-a")]), None, None),
        step("inspect", cli(&["inspect"], vec![agent()]), None, None),
    ]
}

fn g2_checks() -> Vec<Check> {
    vec![
        Check::AllExitZero,
        Check::JsonString {
            step: "run",
            pointer: "/status",
            expected: "permission",
        },
        Check::JsonString {
            step: "send-deny",
            pointer: "/status",
            expected: "permission",
        },
        Check::StdoutLine {
            step: "logs",
            line: G2_REPLY_ALLOWED,
        },
        Check::StdoutLine {
            step: "logs",
            line: G2_REPLY_DENIED,
        },
        Check::StubExactlyConsumed,
        Check::DaemonExit(0),
    ]
}

/// G4 error-reply body: what the scripted upstream returns with HTTP 500.
fn upstream_failure() -> ScriptedReply {
    ScriptedReply {
        status: 500,
        events: Vec::new(),
        json: Some(json!({"error": {"message": "stub upstream failure", "type": "server_error"}})),
        hold_ms: None,
        delay_ms: None,
    }
}

fn g4_run_step() -> StepSpec {
    use Arg::{Captured, Lit};
    step(
        "run",
        cli(
            &["run"],
            vec![
                Lit("--provider"),
                Lit("codex"),
                Lit("--mode"),
                Lit("full-access"),
                Lit("--workspace"),
                Captured("workspace"),
                Lit(G1_PROMPT),
            ],
        ),
        Some(("agent", "/agentId")),
        None,
    )
}

fn g4_workspace_step() -> StepSpec {
    use Arg::{Lit, Project};
    step(
        "workspace-create",
        cli(
            &["workspace", "create"],
            vec![Lit("--isolation"), Lit("local"), Lit("--path"), Project],
        ),
        Some(("workspace", "/workspaceId")),
        None,
    )
}

fn g4_preimages(captured: &BTreeMap<&'static str, String>) -> Vec<(&'static str, String)> {
    creation_preimages(captured, "full-access", G1_PROMPT)
}

/// G4 failure: the Responses upstream answers HTTP 500 to the first turn.
/// The CLI's `run` and the agent list must report the failure the same way.
#[must_use]
pub fn g4_http500() -> GateSpec {
    use Arg::Lit;
    GateSpec {
        id: "g4-http500",
        script: Script {
            responses: vec![upstream_failure()],
        },
        steps: vec![
            g4_workspace_step(),
            g4_run_step(),
            step("ls", cli(&["ls"], vec![Lit("-a")]), None, None),
        ],
        checks: vec![
            Check::AllExitZero,
            // The pinned CLI reports the failed turn in the run result, with
            // exit code 0.
            Check::JsonString {
                step: "run",
                pointer: "/status",
                expected: "error",
            },
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
        preimages: g4_preimages,
        codex_present: true,
        home_origin: HomeOrigin::Same,
    }
}

/// G4 failure: no `codex` binary on the daemon's `PATH`.
#[must_use]
pub fn g4_nocodex() -> GateSpec {
    use Arg::Lit;
    GateSpec {
        id: "g4-nocodex",
        script: Script {
            responses: Vec::new(),
        },
        steps: vec![
            g4_workspace_step(),
            g4_run_step(),
            step("ls", cli(&["ls"], vec![Lit("-a")]), None, None),
        ],
        checks: vec![
            Check::ExitsAre(&[("run", 1)]),
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
        preimages: g4_preimages,
        codex_present: false,
        home_origin: HomeOrigin::Same,
    }
}

/// The G4 turn the client abandons.
pub const G4_PROMPT_DROPPED: &str = "Reply with the single word DROPPED.";
/// The G4 turn sent again after the client reconnects.
pub const G4_PROMPT_RETRY: &str = "Reply with the single word RETRIED.";
/// Scripted reply to the abandoned turn.
pub const G4_REPLY_DROPPED: &str = "DROPPED";
/// Scripted reply to the retried turn.
pub const G4_REPLY_RETRIED: &str = "RETRIED";

/// G4 client disconnect and retry: the client is killed while a delayed turn
/// is in flight. The daemon must finish the turn, a new client sees it, and
/// a further `send` works.
#[must_use]
pub fn g4_disconnect() -> GateSpec {
    use Arg::{Captured, Lit};
    let mut dropped = completed_turn("resp_g4_2", "msg_g4_2", G4_REPLY_DROPPED);
    dropped.delay_ms = Some(4_000);
    GateSpec {
        id: "g4-disconnect",
        script: Script {
            responses: vec![
                completed_turn("resp_g4_1", "msg_g4_1", G1_REPLY),
                dropped,
                completed_turn("resp_g4_3", "msg_g4_3", G4_REPLY_RETRIED),
            ],
        },
        steps: vec![
            g4_workspace_step(),
            g4_run_step(),
            StepSpec {
                disconnect_at_stub_requests: Some(2),
                ..step(
                    "send-dropped",
                    cli(&["send"], vec![Captured("agent"), Lit(G4_PROMPT_DROPPED)]),
                    None,
                    None,
                )
            },
            step(
                "wait",
                cli(
                    &["wait"],
                    vec![Lit("--timeout"), Lit("120"), Captured("agent")],
                ),
                None,
                None,
            ),
            step("logs", cli(&["logs"], vec![Captured("agent")]), None, None),
            step(
                "send-retry",
                cli(&["send"], vec![Captured("agent"), Lit(G4_PROMPT_RETRY)]),
                None,
                None,
            ),
            step(
                "logs-retry",
                cli(&["logs"], vec![Captured("agent")]),
                None,
                None,
            ),
        ],
        checks: vec![
            Check::ExitsAre(&[("send-dropped", -9)]),
            Check::JsonString {
                step: "send-retry",
                pointer: "/status",
                expected: "completed",
            },
            Check::StdoutLine {
                step: "logs",
                line: G4_REPLY_DROPPED,
            },
            Check::StdoutLine {
                step: "logs-retry",
                line: G4_REPLY_RETRIED,
            },
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
        preimages: g4_preimages,
        codex_present: true,
        home_origin: HomeOrigin::Same,
    }
}

/// The turn the socket-drop probe holds open.
pub const G4_PROMPT_HOLD: &str = "Hold the G4 turn open.";

/// The probe script: two clients, the first dropped mid-wait.
const G4_SUBSCRIBER: &str = include_str!("../../../scripts/phase3/g4-subscriber.mjs");

fn g4_socketdrop_preimages(
    captured: &BTreeMap<&'static str, String>,
) -> Vec<(&'static str, String)> {
    creation_preimages(captured, "full-access", G4_PROMPT_HOLD)
}

/// G4 client disconnect at the wire: client A is killed (SIGKILL, so its
/// TCP connection drops with no close handshake) while a held turn is in
/// flight; client B then fetches the agent (still running, no error frames)
/// and cancels the held turn.
#[must_use]
pub fn g4_socketdrop() -> GateSpec {
    use Arg::{Host, Lit, Project};
    GateSpec {
        id: "g4-socketdrop",
        script: Script {
            responses: vec![held_turn("resp_g4_hold")],
        },
        steps: vec![StepSpec {
            node_script: Some(G4_SUBSCRIBER),
            ..step(
                "probe",
                vec![Host, Project, Lit(G4_PROMPT_HOLD)],
                Some(("workspace", "/workspaceId")),
                None,
            )
        }],
        checks: vec![
            Check::AllExitZero,
            // Client A is killed, not closed: the OS drops its connection
            // with no WebSocket close handshake.
            Check::FirstLineField {
                step: "probe",
                pointer: "/dropped",
                expected: "SIGKILL",
            },
            // The daemon must not turn A's drop into an error for B.
            Check::FirstLineField {
                step: "probe",
                pointer: "/errorFrames",
                expected: "0",
            },
            Check::FirstLineField {
                step: "probe",
                pointer: "/status",
                expected: "running",
            },
            Check::StdoutContains {
                step: "probe",
                needle: "cancel_agent_response",
            },
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
        preimages: g4_socketdrop_preimages,
        codex_present: true,
        home_origin: HomeOrigin::Same,
    }
}

/// G4 old state: the original daemon makes the home, the side's daemon opens it.
#[must_use]
pub fn g4_oldstate() -> GateSpec {
    GateSpec {
        id: "g4-oldstate",
        home_origin: HomeOrigin::OriginalThenSide,
        ..g3()
    }
}

/// G4 new state: the side's daemon makes the home, the original opens it.
#[must_use]
pub fn g4_newstate() -> GateSpec {
    GateSpec {
        id: "g4-newstate",
        home_origin: HomeOrigin::SideThenOriginal,
        ..g3()
    }
}

/// The G3 prompt answered before the daemon restarts.
pub const G3_PROMPT_BEFORE: &str = "Reply with the single word BEFORE.";
/// The G3 prompt sent after the daemon restarts.
pub const G3_PROMPT_AFTER: &str = "Reply with the single word AFTER.";
/// Scripted reply to the first G3 turn.
pub const G3_REPLY_BEFORE: &str = "BEFORE";
/// Scripted reply to the G3 turn after the restart.
pub const G3_REPLY_AFTER: &str = "AFTER";

/// G3 creation fingerprint preimages.
#[must_use]
pub fn g3_preimages(captured: &BTreeMap<&'static str, String>) -> Vec<(&'static str, String)> {
    creation_preimages(captured, "full-access", G3_PROMPT_BEFORE)
}

/// G3: daemon stop and start on the same home. One completed turn, a
/// restart, then `ls -a`, `inspect`, `send` of a second turn to the same
/// agent, and `logs`, whose first turn the restarted daemon can only rebuild
/// from the persisted Codex thread (`thread/read`).
#[must_use]
pub fn g3() -> GateSpec {
    use Arg::{Captured, Lit, Project};
    let agent = || Captured("agent");
    GateSpec {
        id: "g3",
        script: Script {
            responses: vec![
                completed_turn("resp_g3_1", "msg_g3_1", G3_REPLY_BEFORE),
                completed_turn("resp_g3_2", "msg_g3_2", G3_REPLY_AFTER),
            ],
        },
        steps: vec![
            step(
                "workspace-create",
                cli(
                    &["workspace", "create"],
                    vec![Lit("--isolation"), Lit("local"), Lit("--path"), Project],
                ),
                Some(("workspace", "/workspaceId")),
                None,
            ),
            step(
                "run",
                cli(
                    &["run"],
                    vec![
                        Lit("--provider"),
                        Lit("codex"),
                        Lit("--mode"),
                        Lit("full-access"),
                        Lit("--workspace"),
                        Captured("workspace"),
                        Lit(G3_PROMPT_BEFORE),
                    ],
                ),
                Some(("agent", "/agentId")),
                None,
            ),
            StepSpec {
                daemon_restart: true,
                ..step("daemon-restart", Vec::new(), None, None)
            },
            step("ls", cli(&["ls"], vec![Lit("-a")]), None, None),
            step("inspect", cli(&["inspect"], vec![agent()]), None, None),
            step(
                "send",
                cli(&["send"], vec![agent(), Lit(G3_PROMPT_AFTER)]),
                None,
                None,
            ),
            step("logs", cli(&["logs"], vec![agent()]), None, None),
        ],
        checks: vec![
            Check::AllExitZero,
            Check::JsonString {
                step: "run",
                pointer: "/status",
                expected: "completed",
            },
            Check::JsonString {
                step: "send",
                pointer: "/status",
                expected: "completed",
            },
            Check::StdoutLine {
                step: "logs",
                line: G3_REPLY_BEFORE,
            },
            Check::StdoutLine {
                step: "logs",
                line: G3_REPLY_AFTER,
            },
            Check::StubExactlyConsumed,
            Check::DaemonExit(0),
        ],
        preimages: g3_preimages,
        codex_present: true,
        home_origin: HomeOrigin::Same,
    }
}

/// Looks up a gate by id.
#[must_use]
pub fn by_id(id: &str) -> Option<GateSpec> {
    match id {
        "g1" => Some(g1()),
        "g2" => Some(g2()),
        "g3" => Some(g3()),
        "g4-http500" => Some(g4_http500()),
        "g4-nocodex" => Some(g4_nocodex()),
        "g4-disconnect" => Some(g4_disconnect()),
        "g4-socketdrop" => Some(g4_socketdrop()),
        "g4-oldstate" => Some(g4_oldstate()),
        "g4-newstate" => Some(g4_newstate()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::validate_script;

    #[test]
    fn g1_script_is_valid_and_steps_capture_before_use() {
        for gate in [g1(), g2()] {
            assert_capture_before_use(&gate);
        }
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
        assert!(by_id("g4").is_none());
        assert!(G1_PROMPT.contains(G1_REPLY));
        assert!(gate.checks.contains(&Check::StdoutLine {
            step: "logs",
            line: G1_REPLY
        }));
        assert!(
            !gate
                .checks
                .iter()
                .any(|check| matches!(check, Check::StdoutContains { .. }))
        );
    }

    fn assert_capture_before_use(gate: &GateSpec) {
        assert!(validate_script(&gate.script).is_ok(), "{}", gate.id);
        let mut captured: Vec<&str> = vec!["project"];
        for step in &gate.steps {
            for arg in &step.args {
                if let Arg::Captured(key) = arg {
                    assert!(
                        captured.contains(key),
                        "{} {} uses {key} before capture",
                        gate.id,
                        step.name
                    );
                }
            }
            if let Some((key, _)) = step.capture {
                captured.push(key);
            }
        }
    }

    #[test]
    fn g4_gates_are_valid_and_capture_before_use() {
        for id in [
            "g4-http500",
            "g4-nocodex",
            "g4-disconnect",
            "g4-socketdrop",
            "g4-oldstate",
            "g4-newstate",
        ] {
            let gate = by_id(id).unwrap();
            assert_eq!(gate.id, id);
            assert_capture_before_use(&gate);
        }
        assert_eq!(g4_http500().script.responses[0].status, 500);
        assert!(!g4_nocodex().codex_present);
        assert!(g4_nocodex().script.responses.is_empty());
        let disconnect = g4_disconnect();
        let dropping: Vec<_> = disconnect
            .steps
            .iter()
            .filter(|step| step.disconnect_at_stub_requests.is_some())
            .collect();
        assert_eq!(dropping.len(), 1);
        // The dropped turn is the second request and is still in flight then.
        assert_eq!(dropping[0].disconnect_at_stub_requests, Some(2));
        assert!(disconnect.script.responses[1].delay_ms.is_some());
        assert_eq!(g4_oldstate().home_origin, HomeOrigin::OriginalThenSide);
        assert_eq!(g4_newstate().home_origin, HomeOrigin::SideThenOriginal);
        assert_eq!(g4_oldstate().steps.len(), g3().steps.len());
    }

    #[test]
    fn g3_restarts_between_the_two_turns() {
        let gate = g3();
        assert_capture_before_use(&gate);
        let names: Vec<&str> = gate.steps.iter().map(|step| step.name).collect();
        assert_eq!(
            names,
            [
                "workspace-create",
                "run",
                "daemon-restart",
                "ls",
                "inspect",
                "send",
                "logs"
            ]
        );
        let restarts: Vec<&StepSpec> = gate
            .steps
            .iter()
            .filter(|step| step.daemon_restart)
            .collect();
        assert_eq!(restarts.len(), 1);
        assert!(restarts[0].args.is_empty());
        assert_eq!(gate.script.responses.len(), 2);
        assert!(by_id("g3").is_some());
        let preimages = g3_preimages(&BTreeMap::from([
            ("project", "/p".to_owned()),
            ("workspace", "wks_0123456789abcdef".to_owned()),
        ]));
        assert!(preimages[1].1.contains(G3_PROMPT_BEFORE));
    }

    #[test]
    fn g2_scripts_approvals_and_a_held_turn_and_stops_mid_turn() {
        let gate = g2();
        let holds: Vec<Option<u64>> = gate
            .script
            .responses
            .iter()
            .map(|reply| reply.hold_ms)
            .collect();
        assert_eq!(holds, vec![None, None, None, None, Some(120_000)]);
        let calls: Vec<String> = gate
            .script
            .responses
            .iter()
            .flat_map(|reply| &reply.events)
            .filter(|event| event["item"]["type"] == "custom_tool_call")
            .map(|event| event["item"]["input"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|source| source.contains("sandbox_permissions: \"require_escalated\""))
        );
        assert!(
            calls[0].contains("\"printf G2-ALLOW\"") && calls[1].contains("\"printf G2-DENY\"")
        );
        let settled: Vec<(&str, usize)> = gate
            .steps
            .iter()
            .filter_map(|step| Some((step.name, step.wait_for_stub_requests?)))
            .collect();
        assert_eq!(
            settled,
            [
                ("permit-allow", 2),
                ("permit-deny", 4),
                ("send-hold", gate.script.responses.len())
            ]
        );
        let preimages = g2_preimages(&BTreeMap::from([
            ("project", "/p".to_owned()),
            ("workspace", "wks_0123456789abcdef".to_owned()),
        ]));
        assert!(preimages[1].1.contains(r#""modeId":"auto""#));
        assert!(preimages[1].1.contains(G2_PROMPT_ALLOW));
    }

    #[test]
    fn g1_preimages_match_paseo_sorted_stringify() {
        let captured = BTreeMap::from([
            ("project", "/p".to_owned()),
            ("workspace", "wks_0123456789abcdef".to_owned()),
        ]);
        assert_eq!(
            g1_preimages(&captured),
            vec![
                (
                    "workspace-create-request",
                    r#"{"source":{"kind":"directory","path":"/p"}}"#.to_owned()
                ),
                (
                    "agent-create-request",
                    r#"{"config":{"cwd":"/p","modeId":"full-access","provider":"codex"},"initialPrompt":"Reply with the single word READY.","labels":{},"workspaceId":"wks_0123456789abcdef"}"#
                        .to_owned()
                ),
            ]
        );
    }
}
