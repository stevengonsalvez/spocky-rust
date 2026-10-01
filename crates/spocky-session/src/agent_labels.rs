//! Agent label helpers from pinned Paseo `protocol/src/agent-labels.ts`.

use spocky_store::js_value::JsValue;

use crate::text::js_trim;

/// `PARENT_AGENT_ID_LABEL`.
pub const PARENT_AGENT_ID_LABEL: &str = "paseo.parent-agent-id";
const OPEN_AGENT_TAB_LABEL_PREFIX: &str = "paseo.open-agent-tab.";

/// `getOpenAgentTabLabel`.
#[must_use]
pub fn open_agent_tab_label(client_id: &str) -> String {
    format!("{OPEN_AGENT_TAB_LABEL_PREFIX}{client_id}")
}

/// `isOpenAgentTabLabel`.
#[must_use]
pub fn is_open_agent_tab_label(label: &str) -> bool {
    label.starts_with(OPEN_AGENT_TAB_LABEL_PREFIX)
}

/// `getParentAgentIdFromLabels`: the trimmed parent id, if non-empty.
#[must_use]
pub fn parent_agent_id_from_labels(labels: Option<&JsValue>) -> Option<String> {
    labels
        .and_then(|labels| labels.get(PARENT_AGENT_ID_LABEL))
        .and_then(JsValue::as_str)
        .map(js_trim)
        .filter(|parent| !parent.is_empty())
        .map(str::to_owned)
}

/// `isDelegatedAgent`.
#[must_use]
pub fn is_delegated_agent(labels: Option<&JsValue>) -> bool {
    parent_agent_id_from_labels(labels).is_some()
}

/// `hasOpenAgentTab`.
#[must_use]
pub fn has_open_agent_tab(labels: Option<&JsValue>) -> bool {
    labels.and_then(JsValue::as_object).is_some_and(|labels| {
        labels
            .iter()
            .any(|(label, value)| is_open_agent_tab_label(label) && value.as_str() == Some("true"))
    })
}
