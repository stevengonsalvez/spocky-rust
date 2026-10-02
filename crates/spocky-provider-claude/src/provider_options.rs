//! `ClaudeProviderOptionsSchema` from `providers/claude/options.ts`, built on
//! the contracts zod port so issue text and parsed output follow zod 4.4.3.

use std::sync::OnceLock;

use spocky_contracts::js_value::{JsValue, stringify_pretty};
use spocky_contracts::zod::{NumberCheck, Schema, UnknownKeys, Verdict, verdict};

fn string() -> Schema {
    Schema::String(Vec::new())
}

fn optional(schema: Schema) -> Schema {
    Schema::Optional(Box::new(schema))
}

fn string_array() -> Schema {
    Schema::Array(Box::new(string()))
}

fn boolean() -> Schema {
    Schema::Boolean
}

fn strict(shape: Vec<(&'static str, Schema)>) -> Schema {
    Schema::Object(shape, UnknownKeys::Strict)
}

fn positive_int() -> Schema {
    Schema::Number(vec![NumberCheck::Int, NumberCheck::Gt(0.0)])
}

fn permission_rules() -> Schema {
    strict(vec![
        ("allow", optional(string_array())),
        ("ask", optional(string_array())),
        ("deny", optional(string_array())),
    ])
}

fn sandbox_network() -> Schema {
    strict(vec![
        ("allowedDomains", optional(string_array())),
        ("deniedDomains", optional(string_array())),
        ("strictAllowlist", optional(boolean())),
        ("allowManagedDomainsOnly", optional(boolean())),
        ("allowUnixSockets", optional(string_array())),
        ("allowAllUnixSockets", optional(boolean())),
        ("allowLocalBinding", optional(boolean())),
        ("allowMachLookup", optional(string_array())),
        ("httpProxyPort", optional(positive_int())),
        ("socksProxyPort", optional(positive_int())),
        (
            "tlsTerminate",
            optional(strict(vec![
                ("caCertPath", optional(string())),
                ("caKeyPath", optional(string())),
            ])),
        ),
    ])
}

fn sandbox_filesystem() -> Schema {
    strict(vec![
        ("allowWrite", optional(string_array())),
        ("denyWrite", optional(string_array())),
        ("denyRead", optional(string_array())),
        ("allowRead", optional(string_array())),
        ("allowManagedReadPathsOnly", optional(boolean())),
        ("disabled", optional(boolean())),
    ])
}

/// The options-level `sandbox` shape.
fn sandbox() -> Schema {
    strict(vec![
        ("enabled", optional(boolean())),
        ("failIfUnavailable", optional(boolean())),
        ("autoAllowBashIfSandboxed", optional(boolean())),
        ("excludedCommands", optional(string_array())),
        ("allowUnsandboxedCommands", optional(boolean())),
        ("network", optional(sandbox_network())),
        ("filesystem", optional(sandbox_filesystem())),
        (
            "ignoreViolations",
            optional(Schema::Record(Box::new(string()), Box::new(string_array()))),
        ),
        ("enableWeakerNestedSandbox", optional(boolean())),
        (
            "ripgrep",
            optional(strict(vec![
                ("command", string()),
                ("args", optional(string_array())),
            ])),
        ),
    ])
}

/// The `sandbox` shape inside `settings`.
fn settings_sandbox() -> Schema {
    strict(vec![
        ("enabled", optional(boolean())),
        ("failIfUnavailable", optional(boolean())),
        ("autoAllowBashIfSandboxed", optional(boolean())),
        ("excludedCommands", optional(string_array())),
        ("allowUnsandboxedCommands", optional(boolean())),
        ("network", optional(sandbox_network())),
        ("filesystem", optional(sandbox_filesystem())),
    ])
}

fn build() -> Schema {
    strict(vec![
        ("allowedTools", optional(string_array())),
        ("disallowedTools", optional(string_array())),
        ("additionalDirectories", optional(string_array())),
        (
            "extraArgs",
            optional(Schema::Record(
                Box::new(string()),
                Box::new(Schema::Nullable(Box::new(string()))),
            )),
        ),
        ("sandbox", optional(sandbox())),
        (
            "settings",
            optional(strict(vec![
                ("permissions", optional(permission_rules())),
                ("sandbox", optional(settings_sandbox())),
            ])),
        ),
    ])
}

/// `ClaudeProviderOptionsSchema`.
#[must_use]
pub fn claude_provider_options_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(build)
}

/// `ClaudeProviderOptionsSchema.safeParse(value)`: the parsed options, or the
/// `ZodError` message (`JSON.stringify(issues, null, 2)`).
///
/// # Errors
///
/// The issues of a rejected value; a throw or an unmodeled shape reports its
/// own text.
pub fn parse_claude_provider_options(value: &JsValue) -> Result<JsValue, String> {
    match verdict(claude_provider_options_schema(), value) {
        Verdict::Valid(parsed) => Ok(parsed),
        Verdict::Invalid(issues) => Err(stringify_pretty(&JsValue::Array(issues))),
        Verdict::Throws(message) => Err(message),
        Verdict::Unmodeled | Verdict::TooDeep => Err("Maximum call stack size exceeded".to_owned()),
    }
}
