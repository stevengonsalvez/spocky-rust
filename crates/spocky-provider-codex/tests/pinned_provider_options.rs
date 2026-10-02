//! The provider-option fallbacks of `ensureThread` against the pinned Paseo
//! build. A session with no `modeId` takes its approval policy and sandbox
//! from `providerOptions`, and `shouldPromoteThreadResponseToAutoReview`
//! switches it to `auto-review` when Codex answers with an auto-review
//! reviewer. A recorded real `thread/start` response from a Codex started in
//! auto-review mode (`tests/fixtures/provider_options.json`) is replayed to
//! the Rust provider and to the pinned `CodexAppServerAgentClient`, which
//! must report the same runtime info (raw JSON text) for every case.
//!
//! Pinned validates `providerOptions` with a strict zod schema before any of
//! that: `sandbox_mode` is one of three strings and `approval_policy` a
//! string enum or a `{granular}` object. A value outside it (an array, a
//! number, a boolean, another object) never reaches the fallbacks; the pinned
//! constructor throws a `ZodError`. The second test has the Rust constructor
//! reject each such value with the same `ZodError` message, compared as text.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::{
    CodexProvider, ProviderCommand, ProviderRuntimeSettings, SessionConfig,
};
use support::DisposableRoot;

const FIXTURE: &str = "provider_options.json";
const SCENARIO: &str = "auto_review_thread_start";
const MODEL: &str = "gpt-6-astra";

/// (name, providerOptions) the schema accepts, with the mode each yields when
/// the replayed `thread/start` answers `approvalsReviewer: "auto_review"`.
fn valid_cases() -> Vec<(&'static str, Option<Value>, &'static str)> {
    let ws = "workspace-write";
    vec![
        (
            "on-request and workspace-write",
            Some(json!({"approval_policy": "on-request", "sandbox_mode": ws})),
            "auto-review",
        ),
        (
            "untrusted policy",
            Some(json!({"approval_policy": "untrusted", "sandbox_mode": ws})),
            "auto",
        ),
        (
            "never policy",
            Some(json!({"approval_policy": "never", "sandbox_mode": ws})),
            "auto",
        ),
        (
            "read-only sandbox",
            Some(json!({"approval_policy": "on-request", "sandbox_mode": "read-only"})),
            "auto",
        ),
        (
            "danger-full-access sandbox",
            Some(json!({"approval_policy": "on-request", "sandbox_mode": "danger-full-access"})),
            "auto",
        ),
        (
            "no sandbox option",
            Some(json!({"approval_policy": "on-request"})),
            "auto",
        ),
        (
            "no policy option",
            Some(json!({"sandbox_mode": ws})),
            "auto",
        ),
        (
            "network proxy policy",
            Some(
                json!({"features": {"network_proxy": {"enabled": true, "proxy_url": "http://127.0.0.1:1", "domains": {"a.com": "allow"}, "unix_sockets": {}}, "multi_agent_v2": false}, "web_search": "cached", "sandbox_workspace_write": {"writable_roots": ["/a"], "exclude_slash_tmp": true}}),
            ),
            "auto",
        ),
        (
            "network proxy flag",
            Some(json!({"features": {"network_proxy": true}})),
            "auto",
        ),
        (
            "reordered keys",
            Some(
                json!({"web_search": "live", "features": {"multi_agent_v2": true, "network_proxy": true}, "sandbox_workspace_write": {"exclude_tmpdir_env_var": false, "network_access": true, "writable_roots": ["/b", "/a"]}, "sandbox_mode": "workspace-write", "approval_policy": "on-request"}),
            ),
            "auto-review",
        ),
        (
            "reordered granular policy",
            Some(
                json!({"sandbox_mode": "read-only", "approval_policy": {"granular": {"skill_approval": true, "rules": false, "sandbox_approval": true, "request_permissions": false, "mcp_elicitations": true}}}),
            ),
            "auto",
        ),
        (
            "reordered network policy",
            Some(
                json!({"features": {"network_proxy": {"unix_sockets": {"/z": "deny", "/a": "allow"}, "domains": {"z.com": "deny", "a.com": "allow"}, "allow_upstream_proxy": false, "socks_url": "socks5://127.0.0.1:2", "enabled": true}}}),
            ),
            "auto",
        ),
        ("null options", Some(Value::Null), "auto"),
        (
            "writable root with U+10FFFF",
            Some(
                json!({"sandbox_workspace_write": {"writable_roots": ["/a\u{10FFFF}b", "\u{10FFFF}", "\u{10FFFF}\u{10FFFF}", "\u{10FFFF}\u{F0000}"]}}),
            ),
            "auto",
        ),
        ("empty options", Some(json!({})), "auto"),
        ("absent options", None, "auto"),
        (
            "granular policy object",
            Some(json!({"approval_policy": {"granular": {"rules": true}}, "sandbox_mode": ws})),
            "auto",
        ),
        (
            "workspace-write extras",
            Some(
                json!({"approval_policy": "on-request", "sandbox_mode": ws, "sandbox_workspace_write": {"network_access": true}}),
            ),
            "auto-review",
        ),
    ]
}

/// providerOptions the schema rejects, where the fallbacks would otherwise
/// have been written against `String()` or a raw comparison.
fn rejected_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("string options", json!("workspace-write")),
        ("array options", json!([{"sandbox_mode": "read-only"}])),
        ("number options", json!(7)),
        ("boolean options", json!(false)),
        (
            "array policy",
            json!({"approval_policy": ["on-request"], "sandbox_mode": "workspace-write"}),
        ),
        (
            "float policy",
            json!({"approval_policy": 1.5, "sandbox_mode": "workspace-write"}),
        ),
        (
            "boolean policy",
            json!({"approval_policy": true, "sandbox_mode": "workspace-write"}),
        ),
        (
            "object policy",
            json!({"approval_policy": {"on": "request"}, "sandbox_mode": "workspace-write"}),
        ),
        (
            "null policy",
            json!({"approval_policy": null, "sandbox_mode": "workspace-write"}),
        ),
        (
            "array sandbox",
            json!({"approval_policy": "on-request", "sandbox_mode": ["workspace-write"]}),
        ),
        (
            "float sandbox",
            json!({"approval_policy": "on-request", "sandbox_mode": 1.5}),
        ),
        (
            "boolean sandbox",
            json!({"approval_policy": "on-request", "sandbox_mode": true}),
        ),
        (
            "object sandbox",
            json!({"approval_policy": "on-request", "sandbox_mode": {"a": 1}}),
        ),
        (
            "unknown sandbox string",
            json!({"approval_policy": "on-request", "sandbox_mode": "workspace_write"}),
        ),
        (
            "unknown option key",
            json!({"approval_policy": "on-request", "sandbox_mode": "workspace-write", "extra": 1}),
        ),
    ]
}

/// Nested objects of the schema, each strict, and several issues at once.
fn rejected_nested_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("two unknown option keys", json!({"extra": 1, "other": [2]})),
        (
            "unknown policy string",
            json!({"approval_policy": "on_request"}),
        ),
        (
            "granular with unknown key",
            json!({"approval_policy": {"granular": {"rules": true, "bogus": 1}}}),
        ),
        (
            "granular with non-boolean",
            json!({"approval_policy": {"granular": {"rules": "yes"}}}),
        ),
        ("granular missing in object", json!({"approval_policy": {}})),
        (
            "granular next to extra key",
            json!({"approval_policy": {"granular": {}, "other": 1}}),
        ),
        (
            "workspace-write not an object",
            json!({"sandbox_workspace_write": "yes"}),
        ),
        (
            "workspace-write unknown key",
            json!({"sandbox_workspace_write": {"network": true}}),
        ),
        (
            "writable roots not strings",
            json!({"sandbox_workspace_write": {"writable_roots": ["/a", 2, null]}}),
        ),
        (
            "writable roots not an array",
            json!({"sandbox_workspace_write": {"writable_roots": "/a"}}),
        ),
        (
            "workspace-write non-boolean flags",
            json!({"sandbox_workspace_write": {"network_access": 1, "exclude_slash_tmp": "x", "exclude_tmpdir_env_var": null}}),
        ),
        ("unknown web search", json!({"web_search": "on"})),
        ("web search number", json!({"web_search": 1})),
        ("features not an object", json!({"features": []})),
        (
            "features unknown key",
            json!({"features": {"multi_agent_v3": true}}),
        ),
        (
            "multi agent not boolean",
            json!({"features": {"multi_agent_v2": "true"}}),
        ),
        (
            "network proxy string",
            json!({"features": {"network_proxy": "on"}}),
        ),
        (
            "network proxy unknown key",
            json!({"features": {"network_proxy": {"enabled": true, "proxy": "x"}}}),
        ),
        (
            "network proxy bad field types",
            json!({"features": {"network_proxy": {"enabled": "yes", "proxy_url": 3, "enable_socks5": 0}}}),
        ),
        (
            "network proxy bad domains",
            json!({"features": {"network_proxy": {"domains": {"a.com": "maybe", "b.com": 1}}}}),
        ),
        (
            "network proxy domains not a record",
            json!({"features": {"network_proxy": {"unix_sockets": ["allow"]}}}),
        ),
        (
            "several bad options at once",
            json!({"approval_policy": 3, "sandbox_mode": "x", "web_search": false, "features": {"multi_agent_v2": 1}, "extra": true}),
        ),
    ]
}

/// The raw `thread/start` request line a client sent the replay server.
fn thread_start_line(log: &Path) -> String {
    let text = std::fs::read_to_string(log).expect("client log");
    let mut lines = text
        .lines()
        .filter(|line| line.contains(r#""method":"thread/start""#));
    let line = lines.next().expect("a thread/start request");
    assert!(lines.next().is_none(), "one thread/start request");
    line.to_owned()
}

fn replay_command(root: &DisposableRoot, log: &Path) -> Vec<String> {
    support::replay_argv(&support::pinned_paseo(), FIXTURE, SCENARIO, root, log)
}

fn rust_info(options: Option<&Value>, root: &DisposableRoot) -> Result<String, String> {
    let provider = CodexProvider::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: replay_command(root, &root.join("rust-client.jsonl")),
            }),
            env: None,
        }),
        None,
        vec![
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("HOME".into(), root.join("home").into_os_string()),
        ],
    );
    let session = provider.create_session(
        SessionConfig {
            cwd: root.project(),
            model: Some(MODEL.to_owned()),
            provider_options: options.cloned(),
            ..SessionConfig::default()
        },
        None,
        false,
    )?;
    let info = session.runtime_info();
    session.close()?;
    info.map(|info| serde_json::to_string(&info).expect("info JSON"))
}

fn pinned_info(options: Option<&Value>, root: &DisposableRoot) -> String {
    let pinned = support::pinned_paseo();
    let argv = replay_command(root, &root.join("pinned-client.jsonl"));
    support::run_pinned_node(
        &pinned,
        "tests/support/pinned_provider_options.mjs",
        &[
            pinned.module.to_string_lossy().into_owned(),
            serde_json::to_string(&argv).unwrap(),
            root.project(),
            MODEL.to_owned(),
            options.map_or_else(|| "absent".to_owned(), Value::to_string),
        ],
        root,
        Duration::from_secs(60),
    )
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn provider_option_fallbacks_match_pinned() {
    support::assert_fixture_digest(FIXTURE);
    for (name, options, mode) in valid_cases() {
        let root = DisposableRoot::new("provider-options");
        let rust =
            rust_info(options.as_ref(), &root).unwrap_or_else(|error| panic!("{name}: {error}"));
        let pinned = pinned_info(options.as_ref(), &root);
        assert_eq!(rust, pinned, "{name}: runtime info differs from pinned");
        // The inner config goes out in the schema's key order, not the
        // input's: the raw `thread/start` request lines must be equal.
        assert_eq!(
            thread_start_line(&root.join("rust-client.jsonl")),
            thread_start_line(&root.join("pinned-client.jsonl")),
            "{name}: thread/start line differs from pinned"
        );
        let info: Value = serde_json::from_str(&rust).expect("info JSON");
        assert_eq!(
            info["modeId"],
            json!(mode),
            "{name}: the mode the options lead to"
        );
    }
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn rejected_provider_options_match_pinned_zod_error() {
    support::assert_fixture_digest(FIXTURE);
    for (name, options) in rejected_cases().into_iter().chain(rejected_nested_cases()) {
        let root = DisposableRoot::new("provider-options-rejected");
        let pinned = pinned_info(Some(&options), &root);
        let line: Value =
            serde_json::from_str(&pinned).unwrap_or_else(|_| panic!("{name}: {pinned}"));
        assert_eq!(line["error"]["name"], json!("ZodError"), "{name}: {pinned}");
        let rust = rust_info(Some(&options), &root).expect_err(name);
        // The message is the issue list as zod prints it, compared as text.
        assert_eq!(
            Value::String(rust),
            line["error"]["message"],
            "{name}: createSession error differs from pinned"
        );
    }
}
