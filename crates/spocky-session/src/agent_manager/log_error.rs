//! The `err` binding of a log line, as pino's default `err` serializer
//! prints it.

use spocky_store::js_value::{JsObject, JsValue};

use crate::agent_sdk::AgentError;

/// The errors whose constructor assigns `this.name`, an own enumerable property
/// pino prints after `message`.
const ERRORS_WITH_OWN_NAME: [&str; 3] = [
    "RewindCapabilityError",
    "AgentRunCancellationError",
    "AgentManagerShuttingDownError",
];

/// `stdSerializers.err(error)` for an error the manager logs: `type` (the
/// constructor name, held in [`AgentError::name`]) and `message`, then the
/// error's own enumerable properties, which for the errors logged here is
/// the `name` that `RewindCapabilityError` assigns. The `stack` is left out:
/// its frames are node source locations, which no Rust error has, so the
/// node twin drops it before comparing.
pub(crate) fn err_binding(error: &AgentError) -> JsValue {
    err_binding_with(&error.name, &error.message, Vec::new())
}

/// [`err_binding`] for an error with extra enumerable properties, in the
/// order node's `for...in` lists them.
pub(crate) fn err_binding_with(
    type_name: &str,
    message: &str,
    extras: Vec<(&str, JsValue)>,
) -> JsValue {
    let mut err = JsObject::new();
    err.insert("type", JsValue::String(type_name.to_owned()));
    err.insert("message", JsValue::String(message.to_owned()));
    if ERRORS_WITH_OWN_NAME.contains(&type_name) {
        err.insert("name", JsValue::String(type_name.to_owned()));
    }
    for (key, value) in extras {
        err.insert(key, value);
    }
    JsValue::Object(err)
}

#[cfg(test)]
mod tests {
    use spocky_store::js_value::stringify;

    use super::err_binding;
    use crate::agent_sdk::AgentError;

    fn named(name: &str) -> AgentError {
        AgentError {
            name: name.to_owned(),
            message: "m".to_owned(),
        }
    }

    #[test]
    fn errors_that_assign_their_name_print_it() {
        for name in [
            "RewindCapabilityError",
            "AgentRunCancellationError",
            "AgentManagerShuttingDownError",
        ] {
            assert_eq!(
                stringify(&err_binding(&named(name))),
                format!(r#"{{"type":"{name}","message":"m","name":"{name}"}}"#)
            );
        }
    }

    #[test]
    fn other_errors_print_type_and_message_only() {
        assert_eq!(
            stringify(&err_binding(&named("TypeError"))),
            r#"{"type":"TypeError","message":"m"}"#
        );
    }
}
