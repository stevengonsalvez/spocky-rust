//! `CodexProviderOptionsSchema` (pinned `codex/options.ts`), run on the
//! contracts zod engine. The pinned session constructor calls
//! `CodexProviderOptionsSchema.parse(config.providerOptions ?? {})`, so a
//! value outside the schema makes `createSession` reject with the `ZodError`,
//! whose `message` is the issue list as zod 4 prints it.

use std::sync::OnceLock;

use serde_json::{Map, Value};
use spocky_contracts::js_value::{self, JsValue};
use spocky_contracts::zod::{self, Schema, UnknownKeys, Verdict};

use crate::transport::to_js_value;

fn optional(schema: Schema) -> Schema {
    Schema::Optional(Box::new(schema))
}

fn boolean_option() -> Schema {
    optional(Schema::Boolean)
}

fn string_option() -> Schema {
    optional(Schema::String(vec![]))
}

/// `z.record(z.string(), z.enum(["allow", "deny"])).optional()`.
fn permission_record() -> Schema {
    optional(Schema::Record(
        Box::new(Schema::String(vec![])),
        Box::new(Schema::Enum(&["allow", "deny"])),
    ))
}

fn approval_policy() -> Schema {
    Schema::Union(vec![
        Schema::Enum(&["untrusted", "on-request", "never"]),
        Schema::Object(
            vec![(
                "granular",
                Schema::Object(
                    vec![
                        ("sandbox_approval", boolean_option()),
                        ("rules", boolean_option()),
                        ("mcp_elicitations", boolean_option()),
                        ("request_permissions", boolean_option()),
                        ("skill_approval", boolean_option()),
                    ],
                    UnknownKeys::Strict,
                ),
            )],
            UnknownKeys::Strict,
        ),
    ])
}

fn network_policy() -> Schema {
    Schema::Object(
        vec![
            ("enabled", boolean_option()),
            ("proxy_url", string_option()),
            ("socks_url", string_option()),
            ("enable_socks5", boolean_option()),
            ("enable_socks5_udp", boolean_option()),
            ("allow_local_binding", boolean_option()),
            ("allow_upstream_proxy", boolean_option()),
            ("dangerously_allow_all_unix_sockets", boolean_option()),
            ("dangerously_allow_non_loopback_proxy", boolean_option()),
            ("domains", permission_record()),
            ("unix_sockets", permission_record()),
        ],
        UnknownKeys::Strict,
    )
}

fn provider_options_schema() -> Schema {
    Schema::Object(
        vec![
            ("approval_policy", optional(approval_policy())),
            (
                "sandbox_mode",
                optional(Schema::Enum(&[
                    "read-only",
                    "workspace-write",
                    "danger-full-access",
                ])),
            ),
            (
                "sandbox_workspace_write",
                optional(Schema::Object(
                    vec![
                        (
                            "writable_roots",
                            optional(Schema::Array(Box::new(Schema::String(vec![])))),
                        ),
                        ("network_access", boolean_option()),
                        ("exclude_slash_tmp", boolean_option()),
                        ("exclude_tmpdir_env_var", boolean_option()),
                    ],
                    UnknownKeys::Strict,
                )),
            ),
            (
                "web_search",
                optional(Schema::Enum(&["disabled", "cached", "indexed", "live"])),
            ),
            (
                "features",
                optional(Schema::Object(
                    vec![
                        (
                            "network_proxy",
                            optional(Schema::Union(vec![Schema::Boolean, network_policy()])),
                        ),
                        ("multi_agent_v2", boolean_option()),
                    ],
                    UnknownKeys::Strict,
                )),
            ),
        ],
        UnknownKeys::Strict,
    )
}

/// `CodexProviderOptionsSchema.parse(providerOptions ?? {})`: the parsed
/// output, which holds the schema's keys in the schema's order (zod builds a
/// new object from its shape), not the input's.
///
/// # Errors
/// The `ZodError` message pinned throws for options outside the schema.
pub fn parse_provider_options(
    options: Option<&Map<String, Value>>,
) -> Result<Map<String, Value>, String> {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    let schema = SCHEMA.get_or_init(provider_options_schema);
    let value = to_js_value(&Value::Object(options.cloned().unwrap_or_default()));
    match zod::verdict(schema, &value) {
        Verdict::Valid(output) => match serde_json::from_str(&js_value::stringify(&output)) {
            Ok(Value::Object(parsed)) => Ok(parsed),
            _ => Err("providerOptions could not be validated".to_owned()),
        },
        Verdict::Invalid(issues) => Err(js_value::stringify_pretty(&JsValue::Array(issues))),
        // The schema has no unported option, no `z.lazy()`, and no transform,
        // so only a failure to start the deep-check thread lands here.
        Verdict::Unmodeled | Verdict::TooDeep | Verdict::Throws(_) => {
            Err("providerOptions could not be validated".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: &Value) -> Result<Map<String, Value>, String> {
        parse_provider_options(value.as_object())
    }

    #[test]
    fn accepts_absent_and_valid_options() {
        assert_eq!(parse_provider_options(None), Ok(Map::new()));
        let options = json!({
            "approval_policy": {"granular": {"rules": true}},
            "sandbox_mode": "workspace-write",
            "features": {"network_proxy": {"enabled": true, "domains": {"a.com": "allow"}}},
        });
        assert_eq!(parse(&options).as_ref(), Ok(options.as_object().unwrap()));
    }

    #[test]
    fn output_follows_the_schema_key_order() {
        let options = json!({
            "web_search": "live",
            "sandbox_mode": "read-only",
            "approval_policy": {"granular": {"rules": true, "sandbox_approval": false}},
        });
        let parsed = parse(&options).unwrap();
        assert_eq!(
            Value::Object(parsed).to_string(),
            r#"{"approval_policy":{"granular":{"sandbox_approval":false,"rules":true}},"sandbox_mode":"read-only","web_search":"live"}"#
        );
    }

    #[test]
    fn rejects_an_unknown_key_with_the_zod_issue_list() {
        let message = parse(&json!({"extra": 1})).unwrap_err();
        assert_eq!(
            message,
            "[\n  {\n    \"code\": \"unrecognized_keys\",\n    \"keys\": [\n      \"extra\"\n    ],\n    \"path\": [],\n    \"message\": \"Unrecognized key: \\\"extra\\\"\"\n  }\n]"
        );
    }
}
