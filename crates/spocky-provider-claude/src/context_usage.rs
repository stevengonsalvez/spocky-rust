//! `ClaudeContextUsageState` from `providers/claude/agent.ts`: context
//! window usage from stream events, results, and compaction.

use spocky_contracts::js_value::{JsObject, JsValue};

use crate::transcript::{
    extract_context_window_size, read_active_usage_tokens, read_legacy_result_usage_tokens,
    read_stream_request_input_tokens, read_stream_request_output_tokens, read_trimmed_string,
};

/// `ClaudeContextUsageState`.
#[derive(Debug, Default)]
pub struct ContextUsageState {
    context_window_max_tokens: Option<f64>,
    stream_request_input_tokens: Option<f64>,
    stream_request_output_tokens: Option<f64>,
    compacted_context_window_used_tokens: Option<f64>,
    completed_result_turns: u64,
}

fn usage_event(usage: JsObject) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", JsValue::String("usage_updated".to_owned()));
    event.insert("provider", JsValue::String("claude".to_owned()));
    event.insert("usage", JsValue::Object(usage));
    JsValue::Object(event)
}

impl ContextUsageState {
    /// `new ClaudeContextUsageState(initialContextWindowMaxTokens)`.
    #[must_use]
    pub fn new(initial: Option<f64>) -> Self {
        Self {
            context_window_max_tokens: initial,
            ..Self::default()
        }
    }

    /// `beginTurn()`.
    pub fn begin_turn(&mut self) {
        self.stream_request_input_tokens = None;
        self.stream_request_output_tokens = None;
        self.compacted_context_window_used_tokens = None;
    }

    /// `setInitialContextWindowMaxTokens(tokens)`.
    pub fn set_initial_context_window_max_tokens(&mut self, tokens: Option<f64>) {
        self.context_window_max_tokens = tokens;
    }

    fn record_model_usage(&mut self, model_usage: Option<&JsValue>) -> Option<f64> {
        if let Some(window) = extract_context_window_size(model_usage) {
            self.context_window_max_tokens = Some(window);
        }
        self.context_window_max_tokens
    }

    fn stream_used_tokens(&self) -> Option<f64> {
        let used = self.stream_request_input_tokens? + self.stream_request_output_tokens?;
        (used > 0.0).then_some(used)
    }

    /// `buildStreamUsageEvent(event)`.
    pub fn build_stream_usage_event(&mut self, event: Option<&JsValue>) -> Option<JsValue> {
        let event = event?.as_object()?;
        match read_trimmed_string(event.get("type")).as_deref() {
            Some("message_start") => {
                let input = read_stream_request_input_tokens(event)?;
                self.stream_request_input_tokens = Some(input);
                self.stream_request_output_tokens = Some(0.0);
            }
            Some("message_delta") => {
                self.stream_request_output_tokens = Some(read_stream_request_output_tokens(event)?);
            }
            _ => return None,
        }
        let used = self.stream_used_tokens()?;
        let mut usage = JsObject::new();
        usage.insert("contextWindowUsedTokens", JsValue::Number(used));
        if let Some(max) = self.context_window_max_tokens {
            usage.insert("contextWindowMaxTokens", JsValue::Number(max));
        }
        Some(usage_event(usage))
    }

    /// `buildResultUsage(message, modelUsage)`: the `AgentUsage`, `None`
    /// without `message.usage`.
    pub fn build_result_usage(&mut self, message: &JsValue) -> Option<JsValue> {
        let result = (|| {
            let raw_usage = message
                .get("usage")
                .filter(|usage| spocky_contracts::js::truthy(Some(usage)))?;
            let member = |key: &str| raw_usage.get(key).cloned().unwrap_or(JsValue::Undefined);
            let mut usage = JsObject::new();
            usage.insert("inputTokens", member("input_tokens"));
            usage.insert("cachedInputTokens", member("cache_read_input_tokens"));
            usage.insert("outputTokens", member("output_tokens"));
            usage.insert(
                "totalCostUsd",
                message
                    .get("total_cost_usd")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
            let model_window = self.record_model_usage(message.get("modelUsage"));
            if let Some(max) = self.context_window_max_tokens.or(model_window) {
                usage.insert("contextWindowMaxTokens", JsValue::Number(max));
            }
            let active = read_active_usage_tokens(Some(raw_usage)).or_else(|| {
                (self.completed_result_turns == 0)
                    .then(|| read_legacy_result_usage_tokens(Some(raw_usage)))
                    .flatten()
            });
            if let Some(used) = self
                .stream_used_tokens()
                .or(active)
                .or(self.compacted_context_window_used_tokens)
            {
                usage.insert("contextWindowUsedTokens", JsValue::Number(used));
            }
            Some(JsValue::Object(usage))
        })();
        self.compacted_context_window_used_tokens = None;
        self.completed_result_turns += 1;
        result
    }

    /// `buildCompactionUsageEvent(postTokens)`.
    pub fn build_compaction_usage_event(&mut self, post_tokens: Option<f64>) -> JsValue {
        self.stream_request_input_tokens = None;
        self.stream_request_output_tokens = None;
        self.compacted_context_window_used_tokens = post_tokens;
        let mut usage = JsObject::new();
        if let Some(max) = self.context_window_max_tokens {
            usage.insert("contextWindowMaxTokens", JsValue::Number(max));
        }
        if let Some(post) = post_tokens {
            usage.insert("contextWindowUsedTokens", JsValue::Number(post));
        }
        usage_event(usage)
    }
}
