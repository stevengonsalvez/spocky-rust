//! The `fetch_agents` listing helpers `session.ts` uses: the agent
//! `SortablePager` (`pagination/sortable-pager.ts`, `pagination/cursor.ts`),
//! `getAgentStatusPriority` (`protocol/agent-state-bucket.ts`),
//! `matchesAgentUpdatesFilter` (`session/agent-updates/agent-updates-service.ts`),
//! and `checkoutFromPersistedWorkspacePlacement`
//! (`workspace-registry-model.ts`). Agents and placements are the baseline's
//! payload objects as [`JsValue`].

use std::cmp::Ordering;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use spocky_contracts::js::{date_parse, js_string};
use spocky_contracts::js_value::{self, JsObject, JsValue};
use spocky_contracts::request::{AgentDirectoryFilter, AgentSort, AgentSortKey, SortDirection};
use spocky_contracts::text::{JsText, js_to_lowercase, js_trim};
use spocky_store::collate::locale_compare;
use spocky_store::registry::{PersistedWorkspaceRecord, WorkspaceKind};

/// A `SortablePager` configuration: the cursor label, the valid sort keys,
/// the default sort, and `getSortValue`. Items are keyed by their `id`.
pub struct Pager {
    pub label: &'static str,
    pub keys: &'static [&'static str],
    pub default_sort: SortSpec,
    pub value: fn(&JsValue, &str) -> SortValue,
}

/// The agents pager (`FETCH_AGENTS_SORT_KEYS`, `updated_at desc`).
pub const AGENTS: Pager = Pager {
    label: "fetch_agents",
    keys: &["status_priority", "created_at", "updated_at", "title"],
    default_sort: SortSpec {
        key: "updated_at",
        ascending: false,
    },
    value: sort_value,
};

/// `SortSpec` with the wire key and direction text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub key: &'static str,
    pub ascending: bool,
}

/// `CursorSortValue`: `string | number | null`.
#[derive(Debug, Clone, PartialEq)]
pub enum SortValue {
    Text(String),
    Number(f64),
    Null,
}

/// `CursorError`: an invalid or mismatched cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorError(pub String);

fn key_text(key: AgentSortKey) -> &'static str {
    match key {
        AgentSortKey::StatusPriority => "status_priority",
        AgentSortKey::CreatedAt => "created_at",
        AgentSortKey::UpdatedAt => "updated_at",
        AgentSortKey::Title => "title",
    }
}

/// The request's agent sort as specs.
#[must_use]
pub fn agent_sort(sort: Option<&[AgentSort]>) -> Vec<SortSpec> {
    sort.unwrap_or_default()
        .iter()
        .map(|entry| SortSpec {
            key: key_text(entry.key),
            ascending: matches!(entry.direction, SortDirection::Asc),
        })
        .collect()
}

impl Pager {
    /// `normalizeSort`: the default sort when empty, else the request's
    /// entries with repeated keys dropped.
    #[must_use]
    pub fn normalize_sort(&self, sort: &[SortSpec]) -> Vec<SortSpec> {
        let mut deduped: Vec<SortSpec> = Vec::new();
        for spec in sort {
            if !deduped.iter().any(|seen| seen.key == spec.key) {
                deduped.push(*spec);
            }
        }
        if deduped.is_empty() {
            vec![self.default_sort]
        } else {
            deduped
        }
    }
}

fn text_field<'a>(agent: &'a JsValue, key: &str) -> Option<&'a str> {
    agent.get(key).and_then(JsValue::as_str)
}

/// `getAgentStatusPriority`.
#[must_use]
pub fn status_priority(agent: &JsValue) -> u8 {
    let pending = agent
        .get("pendingPermissions")
        .and_then(JsValue::as_array)
        .map_or(0, <[JsValue]>::len);
    let attention = text_field(agent, "attentionReason");
    let status = text_field(agent, "status");
    if pending > 0 || attention == Some("permission") {
        0
    } else if status == Some("error") || attention == Some("error") {
        1
    } else if status == Some("running") {
        2
    } else if status == Some("initializing") {
        3
    } else {
        4
    }
}

/// `Date.parse` of a payload timestamp; `NaN` when absent or invalid.
fn date_value(agent: &JsValue, key: &str) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    text_field(agent, key)
        .and_then(date_parse)
        .map_or(f64::NAN, |millis| millis as f64)
}

/// The agents pager's `getSortValue`.
#[must_use]
pub fn sort_value(agent: &JsValue, key: &str) -> SortValue {
    match key {
        "status_priority" => SortValue::Number(f64::from(status_priority(agent))),
        "created_at" => SortValue::Number(date_value(agent, "createdAt")),
        "updated_at" => SortValue::Number(date_value(agent, "updatedAt")),
        // `agent.title?.toLocaleLowerCase() ?? ""`. ponytail: node's
        // `toLowerCase` tables, which the default locale shares unless it is
        // tr, az or lt; the locale variant replaces this when contracts has it.
        _ => SortValue::Text(
            text_field(agent, "title")
                .map(js_to_lowercase)
                .unwrap_or_default(),
        ),
    }
}

fn ordering_number(ordering: Ordering) -> i32 {
    match ordering {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `compareValues`. Numbers compare with JavaScript `===` and `<`, so exact
/// float equality is intended.
#[must_use]
#[allow(clippy::float_cmp)]
pub fn compare_values(left: &SortValue, right: &SortValue) -> i32 {
    match (left, right) {
        // `left === right`: `NaN` never equals itself.
        (SortValue::Number(l), SortValue::Number(r)) if l == r => 0,
        (SortValue::Text(l), SortValue::Text(r)) if l == r => 0,
        (SortValue::Null, SortValue::Null) => 0,
        (SortValue::Null, _) => -1,
        (_, SortValue::Null) => 1,
        (SortValue::Number(l), SortValue::Number(r)) => {
            if l < r {
                -1
            } else {
                1
            }
        }
        (l, r) => ordering_number(locale_compare(&sort_value_string(l), &sort_value_string(r))),
    }
}

/// `String(value)` for a sort value.
fn sort_value_string(value: &SortValue) -> String {
    js_string(Some(&match value {
        SortValue::Text(text) => JsValue::String(text.clone()),
        SortValue::Number(number) => JsValue::Number(*number),
        SortValue::Null => JsValue::Null,
    }))
}

fn agent_id(agent: &JsValue) -> &str {
    text_field(agent, "id").unwrap_or_default()
}

fn directed(base: i32, spec: SortSpec) -> i32 {
    if spec.ascending { base } else { -base }
}

/// `compare(left, right, sort)`: the specs in order, then id.
#[must_use]
pub fn compare(pager: &Pager, left: &JsValue, right: &JsValue, sort: &[SortSpec]) -> Ordering {
    for spec in sort {
        let base = compare_values(
            &(pager.value)(left, spec.key),
            &(pager.value)(right, spec.key),
        );
        if base != 0 {
            return directed(base, *spec).cmp(&0);
        }
    }
    locale_compare(agent_id(left), agent_id(right))
}

/// A decoded cursor: the sort values by key and the item id.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub values: Vec<(String, SortValue)>,
    pub id: String,
}

/// `compareWithCursor(item, cursor, sort)`.
#[must_use]
pub fn compare_with_cursor(
    pager: &Pager,
    agent: &JsValue,
    cursor: &Cursor,
    sort: &[SortSpec],
) -> i32 {
    for spec in sort {
        let right = cursor
            .values
            .iter()
            .find(|(key, _)| key == spec.key)
            .map_or(SortValue::Null, |(_, value)| value.clone());
        let base = compare_values(&(pager.value)(agent, spec.key), &right);
        if base != 0 {
            return directed(base, *spec);
        }
    }
    ordering_number(locale_compare(agent_id(agent), &cursor.id))
}

fn sort_value_js(value: &SortValue) -> JsValue {
    match value {
        SortValue::Text(text) => JsValue::String(text.clone()),
        SortValue::Number(number) => JsValue::Number(*number),
        SortValue::Null => JsValue::Null,
    }
}

/// `encodeCursor`: base64url of `JSON.stringify({ sort, values, id })`.
#[must_use]
pub fn encode_cursor(pager: &Pager, agent: &JsValue, sort: &[SortSpec]) -> String {
    let mut sort_list = Vec::new();
    let mut values = JsObject::new();
    for spec in sort {
        let mut entry = JsObject::new();
        entry.insert("key", JsValue::String(spec.key.to_owned()));
        entry.insert(
            "direction",
            JsValue::String(if spec.ascending { "asc" } else { "desc" }.to_owned()),
        );
        sort_list.push(JsValue::Object(entry));
        values.insert(spec.key, sort_value_js(&(pager.value)(agent, spec.key)));
    }
    let mut payload = JsObject::new();
    payload.insert("sort", JsValue::Array(sort_list));
    payload.insert("values", JsValue::Object(values));
    payload.insert("id", JsValue::String(agent_id(agent).to_owned()));
    let text = js_value::stringify(&JsValue::Object(payload));
    URL_SAFE_NO_PAD.encode(spocky_contracts::json::js_wire_text(&text))
}

/// `decodeCursor(token, sort, validKeys, label)`.
///
/// # Errors
///
/// `Invalid <label> cursor` for an undecodable or malformed token, and
/// `<label> cursor does not match current sort` for another sort.
pub fn decode_cursor(pager: &Pager, token: &str, sort: &[SortSpec]) -> Result<Cursor, CursorError> {
    let label = pager.label;
    let invalid = || CursorError(format!("Invalid {label} cursor"));
    // `Buffer.from(token, "base64url")` ignores padding and stops at the
    // first character outside the alphabet.
    let alphabet = |c: &char| c.is_ascii_alphanumeric() || *c == '-' || *c == '_';
    let clean: String = token.chars().take_while(alphabet).collect();
    let bytes = URL_SAFE_NO_PAD
        .decode(clean.trim_end_matches('='))
        .map_err(|_| invalid())?;
    let parsed = js_value::parse(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid())?;
    let payload = parsed.as_object().ok_or_else(invalid)?;
    let (Some(JsValue::Array(raw_sort)), Some(JsValue::String(id))) =
        (payload.get("sort"), payload.get("id"))
    else {
        return Err(invalid());
    };
    let values = match payload.get("values") {
        Some(JsValue::Object(values)) => values
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    JsValue::String(text) => SortValue::Text(text.clone()),
                    JsValue::Number(number) => SortValue::Number(*number),
                    _ => SortValue::Null,
                };
                (key.to_owned(), value)
            })
            .collect(),
        Some(JsValue::Array(_)) => Vec::new(),
        _ => return Err(invalid()),
    };
    let mut cursor_sort = Vec::new();
    for item in raw_sort {
        let item = item.as_object().ok_or_else(invalid)?;
        let (Some(JsValue::String(key)), Some(JsValue::String(direction))) =
            (item.get("key"), item.get("direction"))
        else {
            return Err(invalid());
        };
        if !pager.keys.contains(&key.as_str()) || (direction != "asc" && direction != "desc") {
            return Err(invalid());
        }
        cursor_sort.push((key.clone(), direction == "asc"));
    }
    let matches = cursor_sort.len() == sort.len()
        && cursor_sort
            .iter()
            .zip(sort)
            .all(|((key, ascending), spec)| key == spec.key && *ascending == spec.ascending);
    if !matches {
        return Err(CursorError(format!(
            "{label} cursor does not match current sort"
        )));
    }
    Ok(Cursor {
        values,
        id: id.clone(),
    })
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn nullable(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::Null, text)
}

/// `checkoutFromPersistedWorkspacePlacement`.
#[must_use]
pub fn checkout_from_persisted_workspace_placement(
    workspace: &PersistedWorkspaceRecord,
    fallback_branch: Option<&str>,
    fallback_worktree_root: Option<&str>,
) -> JsValue {
    let mut checkout = JsObject::new();
    checkout.insert("cwd", text(&workspace.cwd));
    if workspace.kind == WorkspaceKind::Directory {
        checkout.insert("isGit", JsValue::Bool(false));
        checkout.insert("currentBranch", JsValue::Null);
        checkout.insert("remoteUrl", JsValue::Null);
        checkout.insert("worktreeRoot", JsValue::Null);
        checkout.insert("isPaseoOwnedWorktree", JsValue::Bool(false));
        checkout.insert("mainRepoRoot", JsValue::Null);
        return JsValue::Object(checkout);
    }
    checkout.insert(
        "currentBranch",
        nullable(workspace.branch.as_deref().or(fallback_branch)),
    );
    checkout.insert("remoteUrl", JsValue::Null);
    checkout.insert(
        "worktreeRoot",
        text(
            workspace
                .worktree_root
                .as_deref()
                .or(fallback_worktree_root)
                .unwrap_or(&workspace.cwd),
        ),
    );
    checkout.insert("isGit", JsValue::Bool(true));
    let owned_root = workspace
        .main_repo_root
        .as_deref()
        .filter(|root| workspace.is_paseo_owned_worktree && !root.is_empty());
    checkout.insert("isPaseoOwnedWorktree", JsValue::Bool(owned_root.is_some()));
    checkout.insert(
        "mainRepoRoot",
        nullable(owned_root.or(workspace.main_repo_root.as_deref())),
    );
    JsValue::Object(checkout)
}

fn labels_match(agent: &JsValue, filter: &AgentDirectoryFilter) -> bool {
    filter.labels.as_ref().is_none_or(|labels| {
        labels.iter().all(|(key, value)| {
            agent
                .get("labels")
                .and_then(|labels| labels.get(key))
                .and_then(JsValue::as_str)
                == Some(value.as_str())
        })
    })
}

/// `agentThinkingOptionMatchesFilter`. Payloads from `toAgentPayload` always
/// carry `effectiveThinkingOptionId`, computed by the same resolution the
/// baseline falls back to, so the payload value is the resolved one.
fn thinking_matches(agent: &JsValue, filter: &AgentDirectoryFilter) -> bool {
    let Some(expected) = filter.thinking_option_id.as_ref() else {
        return true;
    };
    let expected = expected
        .as_option()
        .map(|id| js_trim(id.as_str()))
        .filter(|id| !id.is_empty());
    text_field(agent, "effectiveThinkingOptionId") == expected
}

/// `matchesAgentStructuralFilter`.
fn structural_matches(agent: &JsValue, project: &JsValue, filter: &AgentDirectoryFilter) -> bool {
    if let Some(statuses) = filter.statuses.as_ref().filter(|s| !s.is_empty()) {
        let status = text_field(agent, "status");
        let listed = statuses.iter().any(|candidate| {
            serde_json::to_value(candidate)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .as_deref()
                == status
        });
        if !listed {
            return false;
        }
    }
    if let Some(required) = filter.requires_attention {
        let requires = agent
            .get("requiresAttention")
            .and_then(JsValue::as_bool)
            .unwrap_or(false);
        if requires != required {
            return false;
        }
    }
    if let Some(keys) = filter.project_keys.as_ref().filter(|k| !k.is_empty()) {
        let keys: Vec<&str> = keys
            .iter()
            .map(JsText::as_str)
            .filter(|key| !js_trim(key).is_empty())
            .collect();
        if !keys.is_empty() && !keys.contains(&text_field(project, "projectKey").unwrap_or("")) {
            return false;
        }
    }
    true
}

/// `matchesAgentUpdatesFilter({ agent, project, filter })`.
#[must_use]
pub fn matches_agent_updates_filter(
    agent: &JsValue,
    project: &JsValue,
    filter: Option<&AgentDirectoryFilter>,
) -> bool {
    if filter.is_some_and(|filter| !labels_match(agent, filter)) {
        return false;
    }
    let include_archived = filter.and_then(|f| f.include_archived).unwrap_or(false);
    let archived = agent.get("archivedAt").is_some_and(|at| match at {
        JsValue::String(text) => !text.is_empty(),
        JsValue::Null | JsValue::Undefined => false,
        _ => true,
    });
    if !include_archived && archived {
        return false;
    }
    filter.is_none_or(|filter| {
        thinking_matches(agent, filter) && structural_matches(agent, project, filter)
    })
}

#[cfg(test)]
mod tests {
    use spocky_contracts::js_value::parse;
    use spocky_store::registry::PersistedWorkspaceRecord;

    use super::*;

    fn agent(json: &str) -> JsValue {
        parse(json).unwrap()
    }

    #[test]
    fn status_priority_follows_the_bucket_order() {
        assert_eq!(
            status_priority(&agent(r#"{"status":"idle","pendingPermissions":[{}]}"#)),
            0
        );
        assert_eq!(
            status_priority(&agent(r#"{"status":"idle","attentionReason":"error"}"#)),
            1
        );
        assert_eq!(status_priority(&agent(r#"{"status":"running"}"#)), 2);
        assert_eq!(status_priority(&agent(r#"{"status":"initializing"}"#)), 3);
        assert_eq!(status_priority(&agent(r#"{"status":"idle"}"#)), 4);
    }

    #[test]
    fn default_sort_is_updated_desc_then_id() {
        let sort = AGENTS.normalize_sort(&agent_sort(None));
        let older = agent(r#"{"id":"b","updatedAt":"2026-10-01T00:00:00.000Z"}"#);
        let newer = agent(r#"{"id":"a","updatedAt":"2026-10-02T00:00:00.000Z"}"#);
        assert_eq!(compare(&AGENTS, &newer, &older, &sort), Ordering::Less);
        let tie = agent(r#"{"id":"c","updatedAt":"2026-10-02T00:00:00.000Z"}"#);
        assert_eq!(compare(&AGENTS, &newer, &tie, &sort), Ordering::Less);
    }

    #[test]
    fn cursor_round_trips_and_rejects_another_sort() {
        let sort = AGENTS.normalize_sort(&agent_sort(None));
        let item = agent(r#"{"id":"a","updatedAt":"2026-10-02T00:00:00.000Z"}"#);
        let token = encode_cursor(&AGENTS, &item, &sort);
        let decoded = decode_cursor(&AGENTS, &token, &sort).expect("cursor");
        assert_eq!(decoded.id, "a");
        assert_eq!(compare_with_cursor(&AGENTS, &item, &decoded, &sort), 0);
        let other = [SortSpec {
            key: "title",
            ascending: true,
        }];
        assert_eq!(
            decode_cursor(&AGENTS, &token, &other),
            Err(CursorError(
                "fetch_agents cursor does not match current sort".to_owned()
            ))
        );
        assert_eq!(
            decode_cursor(&AGENTS, "@@@", &sort),
            Err(CursorError("Invalid fetch_agents cursor".to_owned()))
        );
    }

    fn workspace(kind: WorkspaceKind) -> PersistedWorkspaceRecord {
        PersistedWorkspaceRecord {
            workspace_id: "wks_0".to_owned(),
            project_id: "prj_0".to_owned(),
            cwd: "/p".to_owned(),
            kind,
            display_name: "main".to_owned(),
            title: None,
            branch: Some("main".to_owned()),
            worktree_root: Some("/p".to_owned()),
            base_branch: None,
            is_paseo_owned_worktree: false,
            main_repo_root: None,
            created_at: String::new(),
            updated_at: String::new(),
            archived_at: None,
            auto_archived_change_request_url: None,
            pinned_at: None,
            labels: None,
            untrusted_source: None,
        }
    }

    #[test]
    fn local_checkout_placement_matches_the_baseline_shape() {
        // The G1 creation snapshot's `project.checkout`.
        assert_eq!(
            js_value::stringify(&checkout_from_persisted_workspace_placement(
                &workspace(WorkspaceKind::LocalCheckout),
                None,
                None
            )),
            r#"{"cwd":"/p","currentBranch":"main","remoteUrl":null,"worktreeRoot":"/p","isGit":true,"isPaseoOwnedWorktree":false,"mainRepoRoot":null}"#
        );
        assert_eq!(
            js_value::stringify(&checkout_from_persisted_workspace_placement(
                &workspace(WorkspaceKind::Directory),
                None,
                None
            )),
            r#"{"cwd":"/p","isGit":false,"currentBranch":null,"remoteUrl":null,"worktreeRoot":null,"isPaseoOwnedWorktree":false,"mainRepoRoot":null}"#
        );
    }

    #[test]
    fn archived_agents_need_include_archived() {
        let archived = agent(r#"{"id":"a","archivedAt":"2026-10-01T00:00:00.000Z","labels":{}}"#);
        let project = agent(r#"{"projectKey":"prj_0"}"#);
        assert!(!matches_agent_updates_filter(&archived, &project, None));
        let filter: AgentDirectoryFilter =
            serde_json::from_str(r#"{"includeArchived":true}"#).unwrap();
        assert!(matches_agent_updates_filter(
            &archived,
            &project,
            Some(&filter)
        ));
    }

    #[test]
    fn timestamps_parse_as_date_parse_does() {
        // Printed by node 22: Date.parse("Fri, 02 Oct 2026 12:00:00 GMT"),
        // Date.parse("Oct 2 2026 12:00:00 GMT+0100"), Date.parse("not a date").
        let value = |text: &str| {
            date_value(
                &parse(&format!(r#"{{"updatedAt":"{text}"}}"#)).unwrap(),
                "updatedAt",
            )
        };
        assert_eq!(
            value("Fri, 02 Oct 2026 12:00:00 GMT").to_bits(),
            1_790_942_400_000.0_f64.to_bits()
        );
        assert_eq!(
            value("Oct 2 2026 12:00:00 GMT+0100").to_bits(),
            1_790_938_800_000.0_f64.to_bits()
        );
        assert!(value("not a date").is_nan());
    }

    #[test]
    fn titles_lowercase_as_to_locale_lower_case_does() {
        // Printed by node 22: "\u{c9}COLE", "\u{130}stanbul" and "\u{391}\u{3a3}" through
        // toLocaleLowerCase (identical to toLowerCase in the default locale).
        let title = |text: &str| sort_value(&agent(&format!(r#"{{"title":"{text}"}}"#)), "title");
        assert_eq!(
            title("\u{c9}COLE"),
            SortValue::Text("\u{e9}cole".to_owned())
        );
        assert_eq!(
            title("\u{130}stanbul"),
            SortValue::Text("i\u{307}stanbul".to_owned())
        );
        // The final sigma of a word is the final form.
        assert_eq!(
            title("\u{391}\u{3a3}"),
            SortValue::Text("\u{3b1}\u{3c2}".to_owned())
        );
    }
}
