//! The daemon's own MCP server in agent configs, from pinned Paseo
//! `agent/runtime-mcp-config.ts`.

use spocky_store::js_value::{JsObject, JsValue};
use url::Url;

use crate::js::{js_string, spread, spread_into, truthy};

const PASEO_MCP_SERVER_NAME: &str = "paseo";
const PASEO_MCP_PATHNAME: &str = "/mcp/agents";

/// `isInternalPaseoMcpServer`: an HTTP or SSE server whose URL path is the
/// daemon's agent MCP route.
fn is_internal_paseo_mcp_server(config: &JsValue) -> bool {
    let kind = config.get("type").and_then(JsValue::as_str);
    if kind != Some("http") && kind != Some("sse") {
        return false;
    }
    // `new URL(config.url)` stringifies a non-string url first.
    Url::parse(&js_string(config.get("url"))).is_ok_and(|url| url.path() == PASEO_MCP_PATHNAME)
}

/// `stripInternalPaseoMcpServer`.
#[must_use]
pub fn strip_internal_paseo_mcp_server(config: &JsValue) -> JsValue {
    let Some(mcp_servers) = config
        .get("mcpServers")
        .filter(|servers| truthy(Some(servers)))
    else {
        return config.clone();
    };
    let Some(paseo) = mcp_servers.get(PASEO_MCP_SERVER_NAME) else {
        return config.clone();
    };
    if !truthy(Some(paseo)) || !is_internal_paseo_mcp_server(paseo) {
        return config.clone();
    }
    let mut next_servers = JsObject::new();
    for (name, server) in spread(Some(mcp_servers)).iter() {
        if name != PASEO_MCP_SERVER_NAME {
            next_servers.insert(name, server.clone());
        }
    }
    let mut next = JsObject::new();
    for (key, value) in spread(Some(config)).iter() {
        if key == "mcpServers" {
            // `next.mcpServers = ...` keeps the slot; `delete` removes it.
            if !next_servers.is_empty() {
                next.insert(key, JsValue::Object(next_servers.clone()));
            }
        } else {
            next.insert(key, value.clone());
        }
    }
    JsValue::Object(next)
}

/// `withRuntimePaseoMcpServer`.
#[must_use]
pub fn with_runtime_paseo_mcp_server(
    config: &JsValue,
    agent_id: &str,
    mcp_base_url: Option<&str>,
    mcp_auth_token: Option<&str>,
) -> JsValue {
    let stored = strip_internal_paseo_mcp_server(config);
    let has_paseo = stored
        .get("mcpServers")
        .and_then(|servers| servers.get(PASEO_MCP_SERVER_NAME))
        .is_some_and(|server| truthy(Some(server)));
    let Some(base_url) = mcp_base_url.filter(|url| !url.is_empty()) else {
        return stored;
    };
    if has_paseo {
        return stored;
    }
    let mut server = JsObject::new();
    server.insert("type", JsValue::String("http".to_owned()));
    server.insert(
        "url",
        JsValue::String(format!("{base_url}?callerAgentId={agent_id}")),
    );
    if let Some(token) = mcp_auth_token.filter(|token| !token.is_empty()) {
        let mut headers = JsObject::new();
        headers.insert("Authorization", JsValue::String(format!("Bearer {token}")));
        server.insert("headers", JsValue::Object(headers));
    }
    let mut servers = JsObject::new();
    servers.insert(PASEO_MCP_SERVER_NAME, JsValue::Object(server));
    spread_into(&mut servers, stored.get("mcpServers"));
    let mut next = spread(Some(&stored));
    next.insert("mcpServers", JsValue::Object(servers));
    JsValue::Object(next)
}
