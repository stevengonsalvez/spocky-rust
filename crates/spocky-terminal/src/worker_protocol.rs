//! Terminal worker messages and their IPC framing, following pinned
//! `packages/server/src/terminal/terminal-worker-protocol.ts` and the senders
//! in `terminal-worker-process.ts` and `worker-terminal-manager.ts`.
//!
//! The worker runs as a Node child process with `serialization: "json"`
//! IPC: every message is `JSON.stringify(message)` followed by `\n`. Keys are
//! written in the order the baseline object literals create them; payloads
//! that the baseline passes through untouched (terminal state, activity,
//! request options, results) stay [`JsValue`].

use spocky_contracts::js_value::{self, JsObject, JsValue, JsonSyntaxError};

/// Parent to worker. The parent builds `{ ...input, requestId }`, so
/// `requestId` is the last key.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerRequest {
    CreateTerminal {
        options: JsValue,
    },
    RegisterCwdEnv {
        cwd: String,
        env: JsValue,
    },
    SetActivity {
        terminal_id: String,
        state: String,
    },
    ClearAttention {
        terminal_id: String,
    },
    KillTerminal {
        terminal_id: String,
    },
    KillTerminalAndWait {
        terminal_id: String,
        options: Option<JsValue>,
    },
    GetTerminalState {
        terminal_id: String,
        options: Option<JsValue>,
    },
    CaptureTerminal {
        terminal_id: String,
        start: Option<f64>,
        end: Option<f64>,
        strip_ansi: Option<bool>,
    },
    KillAll,
    Send {
        terminal_id: String,
        message: JsValue,
    },
}

impl WorkerRequest {
    /// The `type` tag.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CreateTerminal { .. } => "createTerminal",
            Self::RegisterCwdEnv { .. } => "registerCwdEnv",
            Self::SetActivity { .. } => "setActivity",
            Self::ClearAttention { .. } => "clearAttention",
            Self::KillTerminal { .. } => "killTerminal",
            Self::KillTerminalAndWait { .. } => "killTerminalAndWait",
            Self::GetTerminalState { .. } => "getTerminalState",
            Self::CaptureTerminal { .. } => "captureTerminal",
            Self::KillAll => "killAll",
            Self::Send { .. } => "send",
        }
    }

    /// The message the parent sends for `request_id`.
    #[must_use]
    pub fn to_value(&self, request_id: &str) -> JsValue {
        let mut object = JsObject::new();
        object.insert("type", string(self.kind()));
        match self {
            Self::CreateTerminal { options } => object.insert("options", options.clone()),
            Self::RegisterCwdEnv { cwd, env } => {
                object.insert("cwd", string(cwd));
                object.insert("env", env.clone());
            }
            Self::SetActivity { terminal_id, state } => {
                object.insert("terminalId", string(terminal_id));
                object.insert("state", string(state));
            }
            Self::ClearAttention { terminal_id } | Self::KillTerminal { terminal_id } => {
                object.insert("terminalId", string(terminal_id));
            }
            Self::KillTerminalAndWait {
                terminal_id,
                options,
            }
            | Self::GetTerminalState {
                terminal_id,
                options,
            } => {
                object.insert("terminalId", string(terminal_id));
                if let Some(options) = options {
                    object.insert("options", options.clone());
                }
            }
            Self::CaptureTerminal {
                terminal_id,
                start,
                end,
                strip_ansi,
            } => {
                object.insert("terminalId", string(terminal_id));
                if let Some(start) = start {
                    object.insert("start", JsValue::Number(*start));
                }
                if let Some(end) = end {
                    object.insert("end", JsValue::Number(*end));
                }
                if let Some(strip_ansi) = strip_ansi {
                    object.insert("stripAnsi", JsValue::Bool(*strip_ansi));
                }
            }
            Self::KillAll => {}
            Self::Send {
                terminal_id,
                message,
            } => {
                object.insert("terminalId", string(terminal_id));
                object.insert("message", message.clone());
            }
        }
        object.insert("requestId", string(request_id));
        JsValue::Object(object)
    }
}

/// Worker to parent.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerMessage {
    /// `{ type: "response", requestId, ok, result? | error }`. `Ok(None)` is a
    /// response without `result`; `Ok(Some(JsValue::Null))` carries `null`.
    Response {
        request_id: String,
        outcome: Result<Option<JsValue>, String>,
    },
    TerminalCreated {
        terminal: JsValue,
        state: JsValue,
    },
    TerminalMessage {
        terminal_id: String,
        message: JsValue,
    },
    TerminalExit {
        terminal_id: String,
        info: JsValue,
    },
    TerminalTitleChange {
        terminal_id: String,
        title: Option<String>,
    },
    TerminalCommandFinished {
        terminal_id: String,
        exit_code: Option<f64>,
    },
    TerminalActivityChange {
        terminal_id: String,
        activity: JsValue,
        previous: JsValue,
    },
}

/// Why a frame is not a worker message.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameError {
    Json(JsonSyntaxError),
    Shape(&'static str),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => error.fmt(f),
            Self::Shape(what) => write!(f, "malformed terminal worker message: {what}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl WorkerMessage {
    /// The message as the worker writes it.
    #[must_use]
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        match self {
            Self::Response {
                request_id,
                outcome,
            } => {
                object.insert("type", string("response"));
                object.insert("requestId", string(request_id));
                match outcome {
                    Ok(result) => {
                        object.insert("ok", JsValue::Bool(true));
                        if let Some(result) = result {
                            object.insert("result", result.clone());
                        }
                    }
                    Err(error) => {
                        object.insert("ok", JsValue::Bool(false));
                        object.insert("error", string(error));
                    }
                }
            }
            Self::TerminalCreated { terminal, state } => {
                object.insert("type", string("terminalCreated"));
                object.insert("terminal", terminal.clone());
                object.insert("state", state.clone());
            }
            Self::TerminalMessage {
                terminal_id,
                message,
            } => {
                object.insert("type", string("terminalMessage"));
                object.insert("terminalId", string(terminal_id));
                object.insert("message", message.clone());
            }
            Self::TerminalExit { terminal_id, info } => {
                object.insert("type", string("terminalExit"));
                object.insert("terminalId", string(terminal_id));
                object.insert("info", info.clone());
            }
            Self::TerminalTitleChange { terminal_id, title } => {
                object.insert("type", string("terminalTitleChange"));
                object.insert("terminalId", string(terminal_id));
                // `title: undefined` keeps its key but JSON drops it.
                object.insert("title", title.as_deref().map_or(JsValue::Undefined, string));
            }
            Self::TerminalCommandFinished {
                terminal_id,
                exit_code,
            } => {
                object.insert("type", string("terminalCommandFinished"));
                object.insert("terminalId", string(terminal_id));
                let mut info = JsObject::new();
                info.insert("exitCode", exit_code.map_or(JsValue::Null, JsValue::Number));
                object.insert("info", JsValue::Object(info));
            }
            Self::TerminalActivityChange {
                terminal_id,
                activity,
                previous,
            } => {
                object.insert("type", string("terminalActivityChange"));
                object.insert("terminalId", string(terminal_id));
                object.insert("activity", activity.clone());
                object.insert("previous", previous.clone());
            }
        }
        JsValue::Object(object)
    }

    /// Reads one parsed frame.
    ///
    /// # Errors
    ///
    /// [`FrameError::Shape`] when the value is not a worker message.
    pub fn from_value(value: &JsValue) -> Result<Self, FrameError> {
        let text = |key: &'static str| -> Result<String, FrameError> {
            value
                .get(key)
                .and_then(JsValue::as_str)
                .map(str::to_owned)
                .ok_or(FrameError::Shape(key))
        };
        let field = |key: &'static str| value.get(key).cloned().unwrap_or(JsValue::Undefined);
        let kind = text("type")?;
        Ok(match kind.as_str() {
            "response" => Self::Response {
                request_id: text("requestId")?,
                outcome: match value.get("ok") {
                    Some(JsValue::Bool(true)) => Ok(value.get("result").cloned()),
                    Some(JsValue::Bool(false)) => Err(text("error")?),
                    _ => return Err(FrameError::Shape("ok")),
                },
            },
            "terminalCreated" => Self::TerminalCreated {
                terminal: field("terminal"),
                state: field("state"),
            },
            "terminalMessage" => Self::TerminalMessage {
                terminal_id: text("terminalId")?,
                message: field("message"),
            },
            "terminalExit" => Self::TerminalExit {
                terminal_id: text("terminalId")?,
                info: field("info"),
            },
            "terminalTitleChange" => Self::TerminalTitleChange {
                terminal_id: text("terminalId")?,
                title: value
                    .get("title")
                    .and_then(JsValue::as_str)
                    .map(str::to_owned),
            },
            "terminalCommandFinished" => Self::TerminalCommandFinished {
                terminal_id: text("terminalId")?,
                exit_code: value
                    .get("info")
                    .and_then(|info| info.get("exitCode"))
                    .and_then(JsValue::as_f64),
            },
            "terminalActivityChange" => Self::TerminalActivityChange {
                terminal_id: text("terminalId")?,
                activity: field("activity"),
                previous: field("previous"),
            },
            _ => return Err(FrameError::Shape("type")),
        })
    }
}

fn string(text: &str) -> JsValue {
    JsValue::String(text.to_owned())
}

/// One IPC frame: `JSON.stringify(message) + "\n"`.
#[must_use]
pub fn encode_frame(message: &JsValue) -> String {
    let mut frame = js_value::stringify(message);
    frame.push('\n');
    frame
}

/// Parses one frame's text into a worker message.
///
/// # Errors
///
/// The `JSON.parse` error, or [`FrameError::Shape`].
pub fn parse_frame(text: &str) -> Result<WorkerMessage, FrameError> {
    WorkerMessage::from_value(&js_value::parse(text).map_err(FrameError::Json)?)
}

/// Splits the IPC byte stream into frame texts, without their newline.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    pending: Vec<u8>,
}

impl FrameDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes and returns every completed frame.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            frames.push(String::from_utf8_lossy(&line[..end]).into_owned());
        }
        frames
    }
}
