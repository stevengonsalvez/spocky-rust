//! Golden parity against the pinned Paseo validators.
//!
//! `tests/fixtures/g1-golden.json` is written by
//! `scripts/phase3/contracts-capture.mjs` from Paseo `5de45e2` with node
//! v22.20.0. For every case:
//!
//! - inbound: Rust accepts exactly when the daemon's zod parse accepts, and
//!   writes byte-for-byte what zod outputs (key order, defaults, stripping);
//! - outbound: pinned server code (`toAgentPayload`, `buildStoredAgentPayload`,
//!   `checkoutFromPersistedWorkspacePlacement`) builds the snapshot parts, the
//!   pinned client's zod-aot validator accepts the frame and returns it
//!   unchanged, and the Rust value the Spocky daemon would emit writes exactly
//!   the captured validator output. Outbound types are emit-only.

#[path = "support/outbound_frames.rs"]
mod outbound_frames;

use std::fs;
use std::path::Path;

use serde_json::Value;
use spocky_contracts::frame::{WsInbound, WsOutbound, frame_text, parse_frame};
use spocky_contracts::number::Int;
use spocky_contracts::session::{SessionOutbound, StatusPayload};
use spocky_contracts::ws::{
    DaemonPermission, HelloRejected, HelloRejectedReason, ServerCapabilities,
    ServerCapabilityState, ServerFeatureGates, ServerFeatures, ServerId, ServerInfo,
    ServerVoiceCapabilities, WS_PROTOCOL_VERSION, WsControlOutbound,
};

/// Raised only by recapturing; a lower count fails the run.
const EXPECTED_CASES: usize = 117;

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/g1-golden.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string at {pointer}"))
}

fn flag(value: &Value, pointer: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("missing bool at {pointer}"))
}

fn check_inbound(case: &Value, id: &str, input: &str, failures: &mut Vec<String>) {
    let accepted = flag(case, "/zod/success");
    match (parse_frame::<WsInbound>(input), accepted) {
        (Ok(frame), true) => {
            let written = frame_text(&frame).unwrap();
            let expected = text(case, "/zod/output");
            if written != expected {
                failures.push(format!("{id}: wrote\n  {written}\nzod\n  {expected}"));
            }
        }
        (Err(_), false) => {}
        (Ok(frame), false) => failures.push(format!("{id}: zod rejects, Rust accepted {frame:?}")),
        (Err(error), true) => failures.push(format!("{id}: zod accepts, Rust rejected: {error}")),
    }
}

fn server_info(gates: ServerFeatureGates, capabilities: Option<ServerCapabilities>) -> WsOutbound {
    let info = ServerInfo {
        protocol_version: Int::new(WS_PROTOCOL_VERSION).unwrap(),
        server_id: ServerId::new("srv_golden").unwrap(),
        hostname: "golden-host".into(),
        version: "0.10.0".into(),
        permissions: DaemonPermission::ALL.to_vec(),
        desktop_managed: gates.desktop_managed,
        capabilities,
        features: ServerFeatures::advertised(gates),
    };
    WsOutbound::Session(Box::new(SessionOutbound::Status {
        payload: StatusPayload::ServerInfo(Box::new(info)),
    }))
}

const ALL_GATES_ON: ServerFeatureGates = ServerFeatureGates {
    workspace_labels: true,
    daemon_status_rpc: true,
    relay_config: true,
    desktop_managed: false,
};

/// The Rust value the Spocky daemon emits for each outbound golden case.
fn outbound_frame(id: &str) -> Option<WsOutbound> {
    let state = |enabled: bool, reason: &str| ServerCapabilityState {
        enabled,
        reason: reason.into(),
    };
    Some(match id {
        "ws.pong" => WsOutbound::Control(WsControlOutbound::Pong),
        "ws.hello_rejected.password_required" => {
            WsOutbound::Control(WsControlOutbound::HelloRejected(HelloRejected::new(
                HelloRejectedReason::PasswordRequired,
            )))
        }
        "ws.hello_rejected.incompatible_protocol" => {
            WsOutbound::Control(WsControlOutbound::HelloRejected(HelloRejected::new(
                HelloRejectedReason::IncompatibleProtocol,
            )))
        }
        "ws.server_info.default" => server_info(ALL_GATES_ON, None),
        "ws.server_info.desktop_managed_ungated" => server_info(
            ServerFeatureGates {
                workspace_labels: false,
                daemon_status_rpc: false,
                relay_config: false,
                desktop_managed: true,
            },
            None,
        ),
        "ws.server_info.voice_capabilities" => server_info(
            ALL_GATES_ON,
            Some(ServerCapabilities {
                voice: ServerVoiceCapabilities {
                    dictation: state(true, ""),
                    voice: state(false, "Voice is disabled"),
                },
            }),
        ),
        _ => return outbound_frames::frame(id),
    })
}

fn check_outbound(case: &Value, id: &str, input: &str, failures: &mut Vec<String>) {
    if !flag(case, "/aot/success") {
        failures.push(format!("{id}: the pinned client rejects this daemon frame"));
        return;
    }
    if text(case, "/aot/output") != input {
        failures.push(format!("{id}: client validator changed the daemon text"));
    }
    let Some(frame) = outbound_frame(id) else {
        failures.push(format!("{id}: no Rust value for this outbound case"));
        return;
    };
    // Expected bytes are what the pinned client validator returned, captured
    // from the frame the pinned server code built.
    let expected = text(case, "/aot/output");
    let written = frame_text(&frame).unwrap();
    if written != expected {
        failures.push(format!("{id}: wrote\n  {written}\npinned\n  {expected}"));
    }
}

#[test]
fn fixture_provenance_is_pinned() {
    let fixture = fixture();
    assert_eq!(
        text(&fixture, "/provenance/paseoCommit"),
        "5de45e208690b0efc51c59a585ae9729325a9204"
    );
    assert_eq!(text(&fixture, "/provenance/node"), "v22.20.0");
    assert_eq!(
        text(&fixture, "/provenance/nodeBinarySha256"),
        "1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931"
    );
    assert_eq!(text(&fixture, "/provenance/zod"), "4.4.3");
    assert_eq!(text(&fixture, "/provenance/zodAot"), "0.20.4");
    for module in ["agentProjectionsJsSha256", "workspaceRegistryModelJsSha256"] {
        let digest = text(&fixture, &format!("/provenance/pinnedServer/{module}"));
        assert_eq!(digest.len(), 64, "{module} digest recorded");
    }
}

#[test]
fn every_case_matches_pinned_validators() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().expect("cases array");
    assert_eq!(cases.len(), EXPECTED_CASES, "fixture case count changed");
    let mut failures = Vec::new();
    for case in cases {
        let id = text(case, "/id");
        let input = text(case, "/input");
        match text(case, "/direction") {
            "inbound" => check_inbound(case, id, input, &mut failures),
            "outbound" => check_outbound(case, id, input, &mut failures),
            other => failures.push(format!("{id}: unknown direction {other}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
