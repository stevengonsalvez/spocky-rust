//! Agent creation inputs: `AgentSessionConfigSchema`, `GitSetupOptionsSchema`,
//! and `CreateAgentWorktreeTargetSchema`.

use serde::{Deserialize, Serialize};

use crate::field::{Nullable, optional};
use crate::json::{JsRecord, JsonValue, ZodJson, deserialize_tagged};
use crate::literal::string_literal;
use crate::number::PositiveInt;
use crate::text::{JsText, NonEmptyString, TrimmedString};

/// `MAX_EXPLICIT_AGENT_TITLE_CHARS` in `agent-title-limits.ts`.
pub const MAX_EXPLICIT_AGENT_TITLE_CHARS: usize = 200;

/// `z.string().trim().min(1).max(MAX_EXPLICIT_AGENT_TITLE_CHARS)`.
pub type ExplicitAgentTitle = TrimmedString<1, MAX_EXPLICIT_AGENT_TITLE_CHARS>;

/// `McpStdioServerConfigSchema` fields after `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpStdioServerConfig {
    pub command: JsText,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub args: Option<Vec<JsText>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub env: Option<JsRecord<JsText>>,
    #[serde(
        rename = "alwaysLoad",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub always_load: Option<bool>,
}

/// `McpHttpServerConfigSchema` and `McpSseServerConfigSchema` fields after
/// `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpRemoteServerConfig {
    pub url: JsText,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub headers: Option<JsRecord<JsText>>,
    #[serde(
        rename = "alwaysLoad",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub always_load: Option<bool>,
}

/// `McpServerConfigSchema`, discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum McpServerConfig {
    #[serde(rename = "stdio")]
    Stdio(McpStdioServerConfig),
    #[serde(rename = "http")]
    Http(McpRemoteServerConfig),
    #[serde(rename = "sse")]
    Sse(McpRemoteServerConfig),
}

deserialize_tagged!(McpServerConfig, "type", {
    "stdio" => |input| McpStdioServerConfig::deserialize(input).map(McpServerConfig::Stdio),
    "http" => |input| McpRemoteServerConfig::deserialize(input).map(McpServerConfig::Http),
    "sse" => |input| McpRemoteServerConfig::deserialize(input).map(McpServerConfig::Sse),
});

string_literal!(McpKind = "mcp");

/// `McpToolRefSchema`: strict, with trimmed non-empty names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolRef {
    pub kind: McpKind,
    pub server: TrimmedString<1, { usize::MAX }>,
    pub tool: TrimmedString<1, { usize::MAX }>,
}

/// `ToolPolicySchema`: strict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicy {
    pub preapproved: Vec<McpToolRef>,
}

/// `AgentSessionConfigSchema` in zod output order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionConfig {
    pub provider: JsText,
    pub cwd: JsText,
    #[serde(
        rename = "modeId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub mode_id: Option<JsText>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub model: Option<JsText>,
    #[serde(
        rename = "thinkingOptionId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub thinking_option_id: Option<JsText>,
    #[serde(
        rename = "featureValues",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub feature_values: Option<JsRecord<JsonValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub title: Option<Nullable<ExplicitAgentTitle>>,
    #[serde(
        rename = "providerOptions",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub provider_options: Option<JsRecord<ZodJson>>,
    #[serde(
        rename = "toolPolicy",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub tool_policy: Option<ToolPolicy>,
    #[serde(
        rename = "systemPrompt",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub system_prompt: Option<JsText>,
    #[serde(
        rename = "mcpServers",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub mcp_servers: Option<JsRecord<McpServerConfig>>,
}

/// `GitSetupOptionsSchema.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitSetupAction {
    #[serde(rename = "branch-off")]
    BranchOff,
    #[serde(rename = "checkout")]
    Checkout,
}

string_literal!(ChangeRequestKind = "change_request");

/// `ChangeRequestCheckoutSourceSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeRequestCheckoutSource {
    pub kind: ChangeRequestKind,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub forge: Option<JsText>,
    pub number: PositiveInt,
    #[serde(
        rename = "projectPath",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub project_path: Option<JsText>,
}

/// `GitSetupOptionsSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitSetupOptions {
    #[serde(
        rename = "baseBranch",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub base_branch: Option<JsText>,
    #[serde(
        rename = "createNewBranch",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub create_new_branch: Option<bool>,
    #[serde(
        rename = "newBranchName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub new_branch_name: Option<JsText>,
    #[serde(
        rename = "createWorktree",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub create_worktree: Option<bool>,
    #[serde(
        rename = "worktreeSlug",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub worktree_slug: Option<JsText>,
    #[serde(
        rename = "refName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub ref_name: Option<NonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub action: Option<GitSetupAction>,
    #[serde(
        rename = "checkoutSource",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub checkout_source: Option<ChangeRequestCheckoutSource>,
    #[serde(
        rename = "githubPrNumber",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub github_pr_number: Option<PositiveInt>,
}

/// `CreateAgentWorktreeTargetSchema`, discriminated by `mode`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "mode")]
pub enum CreateAgentWorktreeTarget {
    #[serde(rename = "branch-off")]
    BranchOff {
        #[serde(rename = "newBranch")]
        new_branch: NonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
        base: Option<NonEmptyString>,
    },
    #[serde(rename = "checkout-branch")]
    CheckoutBranch { branch: NonEmptyString },
    #[serde(rename = "checkout-pr")]
    CheckoutPr {
        #[serde(rename = "prNumber")]
        pr_number: PositiveInt,
    },
}

deserialize_tagged!(CreateAgentWorktreeTarget, "mode", {
    "branch-off" => |input| {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(rename = "newBranch")]
            new_branch: NonEmptyString,
            #[serde(default, with = "optional")]
            base: Option<NonEmptyString>,
        }
        Fields::deserialize(input).map(|fields| CreateAgentWorktreeTarget::BranchOff {
            new_branch: fields.new_branch,
            base: fields.base,
        })
    },
    "checkout-branch" => |input| {
        #[derive(Deserialize)]
        struct Fields {
            branch: NonEmptyString,
        }
        Fields::deserialize(input).map(|fields| CreateAgentWorktreeTarget::CheckoutBranch {
            branch: fields.branch,
        })
    },
    "checkout-pr" => |input| {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(rename = "prNumber")]
            pr_number: PositiveInt,
        }
        Fields::deserialize(input).map(|fields| CreateAgentWorktreeTarget::CheckoutPr {
            pr_number: fields.pr_number,
        })
    },
});
