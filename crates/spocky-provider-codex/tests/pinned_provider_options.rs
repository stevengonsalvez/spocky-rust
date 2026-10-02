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
//! constructor throws. The second test pins that down against the pinned
//! build. The Rust provider does not validate `providerOptions` yet, so those
//! inputs are not compared here.

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
            provider_options: options.and_then(Value::as_object).cloned(),
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
fn pinned_rejects_provider_options_outside_its_schema() {
    support::assert_fixture_digest(FIXTURE);
    for (name, options) in rejected_cases() {
        let root = DisposableRoot::new("provider-options-rejected");
        let pinned = pinned_info(Some(&options), &root);
        let line: Value =
            serde_json::from_str(&pinned).unwrap_or_else(|_| panic!("{name}: {pinned}"));
        assert_eq!(line["error"]["name"], json!("ZodError"), "{name}: {pinned}");
    }
}
