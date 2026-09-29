use std::{error::Error, fmt, io};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TerminalOpcode {
    Output = 0x01,
    Input = 0x02,
    Resize = 0x03,
    Snapshot = 0x04,
    Restore = 0x05,
}

impl TerminalOpcode {
    fn decode(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Output),
            0x02 => Some(Self::Input),
            0x03 => Some(Self::Resize),
            0x04 => Some(Self::Snapshot),
            0x05 => Some(Self::Restore),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalFrame {
    pub opcode: TerminalOpcode,
    pub slot: u8,
    pub payload: Vec<u8>,
}

#[must_use]
pub fn encode_terminal_frame(opcode: TerminalOpcode, slot: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + payload.len());
    bytes.extend_from_slice(&[opcode as u8, slot]);
    bytes.extend_from_slice(payload);
    bytes
}

#[must_use]
pub fn decode_terminal_frame(bytes: &[u8]) -> Option<TerminalFrame> {
    if bytes.len() < 2 {
        return None;
    }
    Some(TerminalFrame {
        opcode: TerminalOpcode::decode(bytes[0])?,
        slot: bytes[1],
        payload: bytes[2..].to_vec(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalResizeIntent {
    Claim,
    Update,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct TerminalResize {
    pub rows: f64,
    pub cols: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<TerminalResizeIntent>,
}

/// Encodes a resize payload as the baseline JSON shape.
///
/// # Errors
///
/// Returns a JSON serialization error when the payload cannot be represented.
pub fn encode_terminal_resize(input: &TerminalResize) -> Result<Vec<u8>, serde_json::Error> {
    encode_json(input)
}

#[must_use]
pub fn decode_terminal_resize(bytes: &[u8]) -> Option<TerminalResize> {
    let resize: TerminalResize = decode_json(bytes)?;
    (is_positive_integer(resize.rows) && is_positive_integer(resize.cols)).then_some(resize)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalCursorStyle {
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCell {
    pub char: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg_mode: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_mode: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverse: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
}

impl TerminalCell {
    #[must_use]
    pub fn new(char: impl Into<String>) -> Self {
        Self {
            char: char.into(),
            fg: None,
            bg: None,
            fg_mode: None,
            bg_mode: None,
            bold: None,
            italic: None,
            underline: None,
            dim: None,
            inverse: None,
            strikethrough: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct TerminalCursor {
    pub row: f64,
    pub col: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<TerminalCursorStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blink: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalState {
    pub rows: f64,
    pub cols: f64,
    pub grid: Vec<Vec<TerminalCell>>,
    pub scrollback: Vec<Vec<TerminalCell>>,
    pub cursor: TerminalCursor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid_wrapped: Option<Vec<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollback_wrapped: Option<Vec<bool>>,
}

/// Encodes a terminal snapshot as the baseline JSON shape.
///
/// # Errors
///
/// Returns a JSON serialization error when the snapshot cannot be represented.
pub fn encode_terminal_snapshot(input: &TerminalState) -> Result<Vec<u8>, serde_json::Error> {
    encode_json(input)
}

#[must_use]
pub fn decode_terminal_snapshot(bytes: &[u8]) -> Option<TerminalState> {
    decode_json(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FileTransferOpcode {
    FileBegin = 0x10,
    FileChunk = 0x11,
    FileEnd = 0x12,
}

impl FileTransferOpcode {
    fn decode(value: u8) -> Option<Self> {
        match value {
            0x10 => Some(Self::FileBegin),
            0x11 => Some(Self::FileChunk),
            0x12 => Some(Self::FileEnd),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum FileEncoding {
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "binary")]
    Binary,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBeginMetadata {
    pub mime: String,
    pub size: f64,
    pub encoding: FileEncoding,
    pub modified_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FileTransferFrame {
    Begin {
        request_id: String,
        metadata: FileBeginMetadata,
        payload: Vec<u8>,
    },
    Chunk {
        request_id: String,
        payload: Vec<u8>,
    },
    End {
        request_id: String,
        payload: Vec<u8>,
    },
}

#[derive(Clone, Copy)]
pub enum FileTransferFrameInput<'a> {
    Begin {
        request_id: &'a str,
        metadata: &'a FileBeginMetadata,
    },
    Chunk {
        request_id: &'a str,
        payload: &'a [u8],
    },
    End {
        request_id: &'a str,
    },
}

#[derive(Debug)]
pub enum WireEncodeError {
    RequestIdRequired,
    RequestIdTooLong,
    MetadataTooLong,
    Json(serde_json::Error),
}

impl fmt::Display for WireEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestIdRequired => formatter.write_str("File transfer requestId is required"),
            Self::RequestIdTooLong => formatter.write_str("File transfer requestId is too long"),
            Self::MetadataTooLong => formatter.write_str("FileBegin metadata is too long"),
            Self::Json(error) => error.fmt(formatter),
        }
    }
}

impl Error for WireEncodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for WireEncodeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Encodes a file-transfer frame using the baseline byte layout.
///
/// # Errors
///
/// Returns an error for empty or oversized request IDs, oversized metadata, or
/// metadata that cannot be serialized as JSON.
pub fn encode_file_transfer_frame(
    input: FileTransferFrameInput<'_>,
) -> Result<Vec<u8>, WireEncodeError> {
    let (opcode, request_id) = match input {
        FileTransferFrameInput::Begin { request_id, .. } => {
            (FileTransferOpcode::FileBegin, request_id)
        }
        FileTransferFrameInput::Chunk { request_id, .. } => {
            (FileTransferOpcode::FileChunk, request_id)
        }
        FileTransferFrameInput::End { request_id } => (FileTransferOpcode::FileEnd, request_id),
    };
    let request_id = encode_request_id(request_id)?;
    let request_id_length =
        u8::try_from(request_id.len()).map_err(|_| WireEncodeError::RequestIdTooLong)?;

    match input {
        FileTransferFrameInput::Begin { metadata, .. } => {
            let metadata = encode_json(metadata)?;
            let metadata_length =
                u16::try_from(metadata.len()).map_err(|_| WireEncodeError::MetadataTooLong)?;
            let mut bytes = Vec::with_capacity(4 + request_id.len() + metadata.len());
            bytes.extend_from_slice(&[opcode as u8, request_id_length]);
            bytes.extend_from_slice(request_id);
            bytes.extend_from_slice(&metadata_length.to_be_bytes());
            bytes.extend_from_slice(&metadata);
            Ok(bytes)
        }
        FileTransferFrameInput::Chunk { payload, .. } => {
            let mut bytes = Vec::with_capacity(2 + request_id.len() + payload.len());
            bytes.extend_from_slice(&[opcode as u8, request_id_length]);
            bytes.extend_from_slice(request_id);
            bytes.extend_from_slice(payload);
            Ok(bytes)
        }
        FileTransferFrameInput::End { .. } => {
            let mut bytes = Vec::with_capacity(2 + request_id.len());
            bytes.extend_from_slice(&[opcode as u8, request_id_length]);
            bytes.extend_from_slice(request_id);
            Ok(bytes)
        }
    }
}

#[must_use]
pub fn decode_file_transfer_frame(bytes: &[u8]) -> Option<FileTransferFrame> {
    if bytes.len() < 2 {
        return None;
    }
    let opcode = FileTransferOpcode::decode(bytes[0])?;
    let request_id_length = usize::from(bytes[1]);
    if request_id_length == 0 || request_id_length > bytes.len() - 2 {
        return None;
    }
    let request_id = String::from_utf8_lossy(&bytes[2..2 + request_id_length]).into_owned();
    let body = &bytes[2 + request_id_length..];

    match opcode {
        FileTransferOpcode::FileBegin => {
            if body.len() < 2 {
                return None;
            }
            let metadata_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
            if metadata_length != body.len() - 2 {
                return None;
            }
            let metadata: FileBeginMetadata = decode_json(&body[2..])?;
            if metadata.mime.is_empty()
                || !metadata.size.is_finite()
                || metadata.size < 0.0
                || metadata.size.fract() != 0.0
            {
                return None;
            }
            Some(FileTransferFrame::Begin {
                request_id,
                metadata,
                payload: Vec::new(),
            })
        }
        FileTransferOpcode::FileChunk => Some(FileTransferFrame::Chunk {
            request_id,
            payload: body.to_vec(),
        }),
        FileTransferOpcode::FileEnd if body.is_empty() => Some(FileTransferFrame::End {
            request_id,
            payload: Vec::new(),
        }),
        FileTransferOpcode::FileEnd => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BinaryFrame {
    Terminal(TerminalFrame),
    FileTransfer(FileTransferFrame),
}

#[must_use]
pub fn decode_binary_frame(bytes: &[u8]) -> Option<BinaryFrame> {
    match bytes.first().copied()? {
        0x01..=0x05 => decode_terminal_frame(bytes).map(BinaryFrame::Terminal),
        0x10..=0x12 => decode_file_transfer_frame(bytes).map(BinaryFrame::FileTransfer),
        _ => None,
    }
}

fn encode_request_id(request_id: &str) -> Result<&[u8], WireEncodeError> {
    let bytes = request_id.as_bytes();
    if bytes.is_empty() {
        return Err(WireEncodeError::RequestIdRequired);
    }
    if bytes.len() > usize::from(u8::MAX) {
        return Err(WireEncodeError::RequestIdTooLong);
    }
    Ok(bytes)
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    serde_json::from_str(&String::from_utf8_lossy(bytes)).ok()
}

fn is_positive_integer(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value.fract() == 0.0
}

fn encode_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, JsNumberFormatter);
    value.serialize(&mut serializer)?;
    Ok(bytes)
}

struct JsNumberFormatter;

impl serde_json::ser::Formatter for JsNumberFormatter {
    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let mut buffer = ryu_js::Buffer::new();
        writer.write_all(buffer.format(value).as_bytes())
    }
}
