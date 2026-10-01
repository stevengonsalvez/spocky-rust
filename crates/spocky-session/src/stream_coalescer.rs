//! Agent stream coalescing from pinned Paseo
//! `agent/agent-stream-coalescer.ts`: assistant and reasoning text chunks
//! and tool-call updates are buffered per agent and flushed at most once per
//! window, with a leading-edge flush after an idle window.
//!
//! The coalescer is a state machine: the caller supplies the clock (`now`,
//! epoch milliseconds as `Date.now()` returns) and runs timers. When a
//! buffer needs its trailing flush, a call returns a [`TimerRequest`]; the
//! caller waits `delay_ms` and calls [`AgentStreamCoalescer::fire`] with the
//! request's token. A cleared timer's token no longer matches, so a stale
//! wake-up does nothing, as `clearTimeout` would. Flushes come back as
//! [`CoalescerFlush`] values in the order the baseline calls `onFlush`.

use spocky_store::js_value::JsValue;

use crate::js::{js_string, spread};

/// `AGENT_STREAM_COALESCE_DEFAULT_WINDOW_MS`.
pub const AGENT_STREAM_COALESCE_DEFAULT_WINDOW_MS: f64 = 60.0;

/// An `onFlush` payload: `{ agentId, item, provider, turnId? }`.
#[derive(Debug, Clone, PartialEq)]
pub struct CoalescerFlush {
    pub agent_id: String,
    pub item: JsValue,
    pub provider: JsValue,
    pub turn_id: Option<JsValue>,
}

/// A trailing flush to schedule: after `delay_ms`, call
/// [`AgentStreamCoalescer::fire`] with `agent_id` and `token`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimerRequest {
    pub agent_id: String,
    pub token: u64,
    pub delay_ms: f64,
}

/// What [`AgentStreamCoalescer::handle`] did with an event.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HandleOutcome {
    /// `handle`'s return value: the coalescer took the event.
    pub coalesced: bool,
    pub flushes: Vec<CoalescerFlush>,
    pub timer: Option<TimerRequest>,
}

/// A `Map` key with `SameValueZero` equality, for tool-call ids.
#[derive(Debug, Clone, PartialEq)]
enum CallKey {
    Undefined,
    Null,
    Bool(bool),
    /// The number's bits, with `-0` folded into `0` and every NaN equal.
    Number(u64),
    Text(String),
    /// An object or array: equal only to itself.
    Unique(u64),
}

#[derive(Debug, Clone)]
enum Entry {
    Text {
        item: JsValue,
        text: JsValue,
        provider: JsValue,
        turn_id: Option<JsValue>,
    },
    ToolCall {
        item: JsValue,
        provider: JsValue,
        turn_id: Option<JsValue>,
    },
}

#[derive(Debug)]
struct Buffer {
    agent_id: String,
    entries: Vec<Entry>,
    tool_call_indexes: Vec<(CallKey, usize)>,
    timer: Option<u64>,
    last_flush_at: Option<f64>,
}

/// `AgentStreamCoalescer`.
#[derive(Debug)]
pub struct AgentStreamCoalescer {
    /// In `Map` insertion order.
    buffers: Vec<Buffer>,
    window_ms: f64,
    next_token: u64,
}

impl Default for AgentStreamCoalescer {
    fn default() -> Self {
        Self::new(AGENT_STREAM_COALESCE_DEFAULT_WINDOW_MS)
    }
}

fn item_type(item: &JsValue) -> Option<&str> {
    item.get("type").and_then(JsValue::as_str)
}

fn is_text_item(item: &JsValue) -> bool {
    matches!(item_type(item), Some("assistant_message" | "reasoning"))
}

/// `===` for the values these entries compare; objects are never equal to
/// another value read from a different event.
fn strict_equals(left: Option<&JsValue>, right: Option<&JsValue>) -> bool {
    fn normalize(value: Option<&JsValue>) -> Option<&JsValue> {
        value.filter(|value| !matches!(value, JsValue::Undefined))
    }
    match (normalize(left), normalize(right)) {
        (None, None) | (Some(JsValue::Null), Some(JsValue::Null)) => true,
        (Some(JsValue::Bool(a)), Some(JsValue::Bool(b))) => a == b,
        #[allow(clippy::float_cmp, reason = "JavaScript === on numbers")]
        (Some(JsValue::Number(a)), Some(JsValue::Number(b))) => a == b,
        (Some(JsValue::String(a)), Some(JsValue::String(b))) => a == b,
        _ => false,
    }
}

/// `ToNumber` of a primitive.
fn to_number(value: &JsValue) -> f64 {
    match value {
        JsValue::Null => 0.0,
        JsValue::Bool(flag) => f64::from(u8::from(*flag)),
        JsValue::Number(number) => *number,
        _ => f64::NAN,
    }
}

/// `left += right`: string concatenation when either side is a string or an
/// object (whose primitive is a string), numeric addition otherwise.
fn js_plus(left: &JsValue, right: &JsValue) -> JsValue {
    let stringy = |value: &JsValue| {
        matches!(
            value,
            JsValue::String(_) | JsValue::Array(_) | JsValue::Object(_)
        )
    };
    if stringy(left) || stringy(right) {
        JsValue::String(format!(
            "{}{}",
            js_string(Some(left)),
            js_string(Some(right))
        ))
    } else {
        JsValue::Number(to_number(left) + to_number(right))
    }
}

impl AgentStreamCoalescer {
    /// `new AgentStreamCoalescer({ windowMs })`.
    #[must_use]
    pub const fn new(window_ms: f64) -> Self {
        Self {
            buffers: Vec::new(),
            window_ms,
            next_token: 0,
        }
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    fn call_key(&mut self, value: Option<&JsValue>) -> CallKey {
        match value {
            None | Some(JsValue::Undefined) => CallKey::Undefined,
            Some(JsValue::Null) => CallKey::Null,
            Some(JsValue::Bool(flag)) => CallKey::Bool(*flag),
            Some(JsValue::Number(number)) => {
                let folded = if *number == 0.0 { 0.0 } else { *number };
                CallKey::Number(if folded.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    folded.to_bits()
                })
            }
            Some(JsValue::String(text)) => CallKey::Text(text.clone()),
            Some(JsValue::Array(_) | JsValue::Object(_)) => CallKey::Unique(self.token()),
        }
    }

    fn position(&self, agent_id: &str) -> Option<usize> {
        self.buffers
            .iter()
            .position(|buffer| buffer.agent_id == agent_id)
    }

    /// `handle(agentId, event)` at time `now`.
    pub fn handle(&mut self, agent_id: &str, event: &JsValue, now: f64) -> HandleOutcome {
        let Some(item) = event
            .get("item")
            .filter(|_| event.get("type").and_then(JsValue::as_str) == Some("timeline"))
            .filter(|item| {
                matches!(
                    item_type(item),
                    Some("assistant_message" | "reasoning" | "tool_call")
                )
            })
        else {
            return HandleOutcome::default();
        };
        let coalesced = HandleOutcome {
            coalesced: true,
            ..HandleOutcome::default()
        };
        if is_text_item(item) && item.get("text").and_then(JsValue::as_str) == Some("") {
            return coalesced;
        }
        let provider = event.get("provider").cloned().unwrap_or(JsValue::Undefined);
        let turn_id = event
            .get("turnId")
            .filter(|turn| !matches!(turn, JsValue::Undefined))
            .cloned();
        let index = self.position(agent_id).unwrap_or_else(|| {
            self.buffers.push(Buffer {
                agent_id: agent_id.to_owned(),
                entries: Vec::new(),
                tool_call_indexes: Vec::new(),
                timer: None,
                last_flush_at: None,
            });
            self.buffers.len() - 1
        });
        if is_text_item(item) {
            self.buffers[index].entries.push(Entry::Text {
                item: item.clone(),
                text: item.get("text").cloned().unwrap_or(JsValue::Undefined),
                provider,
                turn_id,
            });
        } else {
            let key = self.call_key(item.get("callId"));
            let entry = Entry::ToolCall {
                item: item.clone(),
                provider,
                turn_id,
            };
            let buffer = &mut self.buffers[index];
            let existing = buffer
                .tool_call_indexes
                .iter()
                .find(|(existing, _)| *existing == key)
                .map(|(_, position)| *position);
            if let Some(position) = existing {
                buffer.entries[position] = entry;
            } else {
                buffer.tool_call_indexes.push((key, buffer.entries.len()));
                buffer.entries.push(entry);
            }
        }
        let terminal_tool_call = item_type(item) == Some("tool_call")
            && matches!(
                item.get("status").and_then(JsValue::as_str),
                Some("completed" | "failed" | "canceled")
            );
        if terminal_tool_call {
            return HandleOutcome {
                flushes: self.flush_at(index, now),
                ..coalesced
            };
        }
        if self.buffers[index].timer.is_none() {
            let elapsed = self.buffers[index]
                .last_flush_at
                .map_or(f64::INFINITY, |last| now - last);
            if elapsed >= self.window_ms {
                return HandleOutcome {
                    flushes: self.flush_at(index, now),
                    ..coalesced
                };
            }
            let token = self.token();
            self.buffers[index].timer = Some(token);
            return HandleOutcome {
                timer: Some(TimerRequest {
                    agent_id: agent_id.to_owned(),
                    token,
                    delay_ms: self.window_ms,
                }),
                ..coalesced
            };
        }
        coalesced
    }

    /// The timer `token` for `agent_id` fired at `now`.
    pub fn fire(&mut self, agent_id: &str, token: u64, now: f64) -> Vec<CoalescerFlush> {
        match self.position(agent_id) {
            Some(index) if self.buffers[index].timer == Some(token) => self.flush_at(index, now),
            _ => Vec::new(),
        }
    }

    /// `flushFor(agentId)`.
    pub fn flush_for(&mut self, agent_id: &str, now: f64) -> Vec<CoalescerFlush> {
        match self.position(agent_id) {
            Some(index) => self.flush_at(index, now),
            None => Vec::new(),
        }
    }

    /// `flushAll()`, buffers in insertion order.
    pub fn flush_all(&mut self, now: f64) -> Vec<CoalescerFlush> {
        (0..self.buffers.len())
            .flat_map(|index| self.flush_at(index, now))
            .collect()
    }

    /// `flushAndDiscard(agentId)`.
    pub fn flush_and_discard(&mut self, agent_id: &str, now: f64) -> Vec<CoalescerFlush> {
        let flushes = self.flush_for(agent_id, now);
        if let Some(index) = self.position(agent_id) {
            self.buffers.remove(index);
        }
        flushes
    }

    /// `flushBuffer`: clears the timer, then emits the collapsed entries.
    fn flush_at(&mut self, index: usize, now: f64) -> Vec<CoalescerFlush> {
        let buffer = &mut self.buffers[index];
        buffer.timer = None;
        if buffer.entries.is_empty() {
            return Vec::new();
        }
        let entries = std::mem::take(&mut buffer.entries);
        buffer.tool_call_indexes.clear();
        buffer.last_flush_at = Some(now);
        let agent_id = buffer.agent_id.clone();
        collapse(entries)
            .into_iter()
            .map(|entry| match entry {
                Entry::Text {
                    item,
                    text,
                    provider,
                    turn_id,
                } => {
                    let mut copy = spread(Some(&item));
                    copy.insert("text", text);
                    CoalescerFlush {
                        agent_id: agent_id.clone(),
                        item: JsValue::Object(copy),
                        provider,
                        turn_id,
                    }
                }
                Entry::ToolCall {
                    item,
                    provider,
                    turn_id,
                } => CoalescerFlush {
                    agent_id: agent_id.clone(),
                    item,
                    provider,
                    turn_id,
                },
            })
            .collect()
    }
}

/// `collapseEntries`: adjacent text chunks of one stream join.
fn collapse(entries: Vec<Entry>) -> Vec<Entry> {
    let mut collapsed: Vec<Entry> = Vec::with_capacity(entries.len());
    for entry in entries {
        if let (
            Some(Entry::Text {
                item: previous_item,
                text: previous_text,
                provider: previous_provider,
                turn_id: previous_turn,
            }),
            Entry::Text {
                item,
                text,
                provider,
                turn_id,
            },
        ) = (collapsed.last_mut(), &entry)
        {
            let same_stream = item_type(previous_item) == item_type(item)
                && (item_type(item) != Some("assistant_message")
                    || strict_equals(previous_item.get("messageId"), item.get("messageId")));
            if same_stream
                && strict_equals(Some(previous_provider), Some(provider))
                && strict_equals(previous_turn.as_ref(), turn_id.as_ref())
            {
                *previous_text = js_plus(previous_text, text);
                continue;
            }
        }
        collapsed.push(entry);
    }
    collapsed
}
