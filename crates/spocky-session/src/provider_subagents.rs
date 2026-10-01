//! `ProviderSubagentStore` from pinned Paseo
//! `agent/provider-subagents/store.ts`: the provider's own child agents of
//! each managed agent, with a timeline per child.
//!
//! Input events and descriptors stay JavaScript values, so fields a
//! provider sends keep their exact shape. Descriptors keep the baseline's
//! object key order, and `list` sorts with `localeCompare` (ASCII port,
//! see [`spocky_store::collate`]).

use spocky_store::collate::locale_compare;
use spocky_store::js_value::{JsObject, JsValue};

use crate::clock::now_iso;
use crate::timeline::{
    FetchDirection, TimelineCursor, TimelineError, TimelineFetch, TimelineStore,
};
use crate::timeline_content::limit_agent_timeline_item_content;
use spocky_contracts::js::js_string;

/// `storeKey(parentAgentId, subagentId)`.
fn store_key(parent_agent_id: &str, subagent_id: &str) -> String {
    format!("{parent_agent_id}\0{subagent_id}")
}

/// `stickyField(next, previous)`: an omitted value keeps the stored one,
/// any given value (`null` included) replaces it.
fn sticky_field(next: Option<&JsValue>, previous: Option<&JsValue>) -> JsValue {
    match next {
        None | Some(JsValue::Undefined) => match previous {
            None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Null,
            Some(previous) => previous.clone(),
        },
        Some(next) => next.clone(),
    }
}

/// `value ?? fallback` for an optional property.
fn nullish(value: Option<&JsValue>) -> Option<&JsValue> {
    value.filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
}

fn string(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn remove_event(parent_agent_id: &str, subagent_id: JsValue) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", string("remove"));
    event.insert("parentAgentId", string(parent_agent_id));
    event.insert("subagentId", subagent_id);
    JsValue::Object(event)
}

/// `ProviderSubagentStore`.
#[derive(Default)]
pub struct ProviderSubagentStore {
    /// `descriptors`, in `Map` insertion order.
    descriptors: Vec<(String, JsValue)>,
    timelines: TimelineStore,
}

impl ProviderSubagentStore {
    fn descriptor(&self, key: &str) -> Option<&JsValue> {
        self.descriptors
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, descriptor)| descriptor)
    }

    fn delete(&mut self, key: &str) {
        self.descriptors.retain(|(existing, _)| existing != key);
        self.timelines.delete(key);
    }

    fn ensure_timeline(&mut self, key: &str) -> Result<(), TimelineError> {
        if !self.timelines.has(key) {
            self.timelines
                .initialize(key, Vec::new(), None, None, None)?;
        }
        Ok(())
    }

    /// `apply(parentAgentId, provider, event)`: the store event the input
    /// produced (`upsert`, `timeline` or `remove`).
    ///
    /// # Errors
    ///
    /// Returns the baseline's `TypeError` from limiting or projecting a
    /// timeline item.
    pub fn apply(
        &mut self,
        parent_agent_id: &str,
        provider: &str,
        event: &JsValue,
    ) -> Result<JsValue, TimelineError> {
        let id = event.get("id").cloned().unwrap_or(JsValue::Undefined);
        let key = store_key(parent_agent_id, &js_string(Some(&id)));
        match event.get("type").and_then(JsValue::as_str) {
            Some("remove") => {
                self.delete(&key);
                Ok(remove_event(parent_agent_id, id))
            }
            Some("timeline") => {
                self.ensure_timeline(&key)?;
                let item = limit_agent_timeline_item_content(
                    event.get("item").cloned().unwrap_or(JsValue::Undefined),
                )?;
                let timestamp = nullish(event.get("timestamp")).map(|value| js_string(Some(value)));
                let row = self.timelines.append(&key, item, timestamp, None, None)?;
                let mut out = JsObject::new();
                out.insert("type", string("timeline"));
                out.insert("parentAgentId", string(parent_agent_id));
                out.insert("subagentId", id);
                out.insert("provider", string(provider));
                out.insert("row", row.to_js());
                out.insert("epoch", string(self.timelines.epoch(&key)?));
                Ok(JsValue::Object(out))
            }
            _ => {
                let previous = self.descriptor(&key).cloned();
                let previous = previous.as_ref();
                self.ensure_timeline(&key)?;
                let timestamp = nullish(event.get("timestamp"))
                    .cloned()
                    .unwrap_or_else(|| JsValue::String(now_iso()));
                let field =
                    |name: &str| sticky_field(event.get(name), previous.and_then(|p| p.get(name)));
                let mut subagent = JsObject::new();
                subagent.insert("id", id);
                subagent.insert("parentAgentId", string(parent_agent_id));
                subagent.insert("provider", string(provider));
                subagent.insert("title", field("title"));
                subagent.insert("description", field("description"));
                subagent.insert(
                    "status",
                    nullish(event.get("status"))
                        .or_else(|| previous.and_then(|p| nullish(p.get("status"))))
                        .cloned()
                        .unwrap_or_else(|| string("running")),
                );
                subagent.insert(
                    "createdAt",
                    previous
                        .and_then(|p| nullish(p.get("createdAt")))
                        .cloned()
                        .unwrap_or_else(|| timestamp.clone()),
                );
                subagent.insert("updatedAt", timestamp);
                subagent.insert("toolCallId", field("toolCallId"));
                subagent.insert("cwd", field("cwd"));
                subagent.insert("subtitle", field("subtitle"));
                subagent.insert("parentSubagentId", field("parentSubagentId"));
                let subagent = JsValue::Object(subagent);
                match self
                    .descriptors
                    .iter_mut()
                    .find(|(existing, _)| *existing == key)
                {
                    Some(slot) => slot.1 = subagent.clone(),
                    None => self.descriptors.push((key, subagent.clone())),
                }
                let mut out = JsObject::new();
                out.insert("type", string("upsert"));
                out.insert("subagent", subagent);
                Ok(JsValue::Object(out))
            }
        }
    }

    /// `list(parentAgentId)`: the parent's children by `createdAt`
    /// (`localeCompare`, stable).
    #[must_use]
    pub fn list(&self, parent_agent_id: &str) -> Vec<JsValue> {
        let mut children: Vec<JsValue> = self
            .descriptors
            .iter()
            .map(|(_, descriptor)| descriptor)
            .filter(|descriptor| {
                descriptor.get("parentAgentId").and_then(JsValue::as_str) == Some(parent_agent_id)
            })
            .cloned()
            .collect();
        children.sort_by(|left, right| {
            locale_compare(
                &js_string(left.get("createdAt")),
                &js_string(right.get("createdAt")),
            )
        });
        children
    }

    /// `listAll()`.
    #[must_use]
    pub fn list_all(&self) -> Vec<JsValue> {
        self.descriptors
            .iter()
            .map(|(_, descriptor)| descriptor.clone())
            .collect()
    }

    /// `get(parentAgentId, subagentId)`.
    #[must_use]
    pub fn get(&self, parent_agent_id: &str, subagent_id: &str) -> Option<JsValue> {
        self.descriptor(&store_key(parent_agent_id, subagent_id))
            .cloned()
    }

    /// `fetchTimeline(parentAgentId, subagentId, options)`.
    ///
    /// # Errors
    ///
    /// As [`TimelineStore::fetch`], including an unknown child.
    pub fn fetch_timeline(
        &self,
        parent_agent_id: &str,
        subagent_id: &str,
        direction: FetchDirection,
        cursor: Option<&TimelineCursor>,
        limit: Option<usize>,
    ) -> Result<TimelineFetch, TimelineError> {
        self.timelines.fetch(
            &store_key(parent_agent_id, subagent_id),
            direction,
            cursor,
            limit,
        )
    }

    /// `deleteParent(parentAgentId)`: a `remove` event per child, in `list`
    /// order.
    pub fn delete_parent(&mut self, parent_agent_id: &str) -> Vec<JsValue> {
        let mut events = Vec::new();
        for subagent in self.list(parent_agent_id) {
            let id = subagent.get("id").cloned().unwrap_or(JsValue::Undefined);
            self.delete(&store_key(parent_agent_id, &js_string(Some(&id))));
            events.push(remove_event(parent_agent_id, id));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use spocky_store::js_value::{JsValue, parse, stringify};

    use super::ProviderSubagentStore;
    use crate::timeline::FetchDirection;

    fn event(text: &str) -> JsValue {
        parse(text).expect("event")
    }

    #[test]
    fn children_and_timelines_stay_scoped_to_the_parent() {
        let mut store = ProviderSubagentStore::default();
        store
            .apply(
                "parent-a",
                "codex",
                &event(r#"{"type":"upsert","id":"child-1","title":"Explore","cwd":"/workspace/child","status":"running","timestamp":"2026-07-12T10:00:00.000Z"}"#),
            )
            .expect("upsert");
        store
            .apply(
                "parent-a",
                "codex",
                &event(r#"{"type":"timeline","id":"child-1","item":{"type":"assistant_message","text":"Found it."},"timestamp":"2026-07-12T10:00:01.000Z"}"#),
            )
            .expect("timeline");
        store
            .apply(
                "parent-a",
                "codex",
                &event(r#"{"type":"upsert","id":"child-1","status":"completed","timestamp":"2026-07-12T10:00:02.000Z"}"#),
            )
            .expect("status");
        store
            .apply(
                "parent-b",
                "claude",
                &event(r#"{"type":"upsert","id":"child-1","title":"Review","status":"running","timestamp":"2026-07-12T10:00:03.000Z"}"#),
            )
            .expect("other parent");
        assert_eq!(
            stringify(&JsValue::Array(store.list("parent-a"))),
            r#"[{"id":"child-1","parentAgentId":"parent-a","provider":"codex","title":"Explore","description":null,"status":"completed","createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:02.000Z","toolCallId":null,"cwd":"/workspace/child","subtitle":null,"parentSubagentId":null}]"#
        );
        let page = store
            .fetch_timeline("parent-a", "child-1", FetchDirection::Tail, None, None)
            .expect("fetch");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(
            stringify(&JsValue::Array(store.delete_parent("parent-a"))),
            r#"[{"type":"remove","parentAgentId":"parent-a","subagentId":"child-1"}]"#
        );
        assert!(store.list("parent-a").is_empty());
        assert_eq!(store.list("parent-b").len(), 1);
    }
}
