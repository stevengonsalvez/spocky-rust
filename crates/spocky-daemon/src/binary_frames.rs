//! Binary frames: `decodeBinaryFrame` of the protocol package
//! (`binary-frames/demux.ts`), with the terminal stream frames of
//! `terminal.ts` and the file-transfer frames of `file-transfer.ts`.
//!
//! The transport decodes every inbound message with this before it tries the
//! JSON path (`maybeHandleBinaryFrame`, `websocket-server.ts`), whether the
//! WebSocket frame was text or binary: what matters is the first byte.

use spocky_contracts::js_value::{JsValue, parse as parse_js};

/// `TerminalStreamOpcode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOpcode {
    Output,
    Input,
    Resize,
    Snapshot,
    Restore,
}

impl TerminalOpcode {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Output),
            0x02 => Some(Self::Input),
            0x03 => Some(Self::Resize),
            0x04 => Some(Self::Snapshot),
            0x05 => Some(Self::Restore),
            _ => None,
        }
    }
}

/// `TerminalStreamFrame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalStreamFrame {
    pub opcode: TerminalOpcode,
    pub slot: u8,
    pub payload: Vec<u8>,
}

/// `FileTransferFrame`.
#[derive(Debug, Clone, PartialEq)]
pub enum FileTransferFrame {
    /// `FileBegin`: the request id and the validated metadata.
    Begin {
        request_id: String,
        metadata: FileBeginMetadata,
    },
    /// `FileChunk`.
    Chunk {
        request_id: String,
        payload: Vec<u8>,
    },
    /// `FileEnd`: never carries a payload.
    End { request_id: String },
}

/// `FileBeginMetadataSchema`'s encoding enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEncoding {
    Utf8,
    Binary,
}

/// `FileBeginMetadataSchema`. Keys the schema does not know are dropped, as
/// zod does for a non-strict object.
#[derive(Debug, Clone, PartialEq)]
pub struct FileBeginMetadata {
    pub mime: String,
    /// A non-negative integer-valued JS number.
    pub size: f64,
    pub encoding: FileEncoding,
    pub modified_at: String,
    pub revision: Option<String>,
    pub file_name: Option<String>,
}

/// `BinaryFrame`.
#[derive(Debug, Clone, PartialEq)]
pub enum BinaryFrame {
    Terminal(TerminalStreamFrame),
    FileTransfer(FileTransferFrame),
}

const FILE_BEGIN: u8 = 0x10;
const FILE_CHUNK: u8 = 0x11;
const FILE_END: u8 = 0x12;

/// `decodeBinaryFrame`: `None` is the baseline's `null`, and the message then
/// goes to the JSON path.
#[must_use]
pub fn decode_binary_frame(bytes: &[u8]) -> Option<BinaryFrame> {
    match *bytes.first()? {
        first if TerminalOpcode::from_byte(first).is_some() => {
            decode_terminal_frame(bytes).map(BinaryFrame::Terminal)
        }
        FILE_BEGIN | FILE_CHUNK | FILE_END => {
            decode_file_transfer_frame(bytes).map(BinaryFrame::FileTransfer)
        }
        _ => None,
    }
}

/// `decodeTerminalStreamFrame`.
fn decode_terminal_frame(bytes: &[u8]) -> Option<TerminalStreamFrame> {
    let [opcode, slot, payload @ ..] = bytes else {
        return None;
    };
    Some(TerminalStreamFrame {
        opcode: TerminalOpcode::from_byte(*opcode)?,
        slot: *slot,
        payload: payload.to_vec(),
    })
}

/// `decodeFileTransferFrame`.
fn decode_file_transfer_frame(bytes: &[u8]) -> Option<FileTransferFrame> {
    let [opcode, request_id_length, rest @ ..] = bytes else {
        return None;
    };
    let request_id_length = usize::from(*request_id_length);
    if request_id_length == 0 || request_id_length > rest.len() {
        return None;
    }
    let (request_id, body) = rest.split_at(request_id_length);
    let request_id = decode_text(request_id);

    match *opcode {
        FILE_BEGIN => {
            let [high, low, metadata @ ..] = body else {
                return None;
            };
            if usize::from(u16::from_be_bytes([*high, *low])) != metadata.len() {
                return None;
            }
            let metadata = file_begin_metadata(&parse_js(&decode_text(metadata)).ok()?)?;
            Some(FileTransferFrame::Begin {
                request_id,
                metadata,
            })
        }
        FILE_CHUNK => Some(FileTransferFrame::Chunk {
            request_id,
            payload: body.to_vec(),
        }),
        _ => body
            .is_empty()
            .then_some(FileTransferFrame::End { request_id }),
    }
}

/// `new TextDecoder().decode(bytes)`: UTF-8 with replacement characters, and a
/// leading byte order mark removed.
fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// `z.number().int().nonnegative()` of zod 4: an integer-valued number no
/// larger than 2^53 - 1 (`Number.isSafeInteger`), negative zero included.
fn is_non_negative_safe_integer(number: f64) -> bool {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number)
}

/// `FileBeginMetadataSchema.safeParse(value)`.
fn file_begin_metadata(value: &JsValue) -> Option<FileBeginMetadata> {
    let JsValue::Object(object) = value else {
        return None;
    };
    let string = |key: &str| object.get(key).and_then(JsValue::as_str).map(str::to_owned);
    let optional_string = |key: &str| match object.get(key) {
        None | Some(JsValue::Undefined) => Some(None),
        Some(JsValue::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    };
    let mime = string("mime").filter(|mime| !mime.is_empty())?;
    let size = match object.get("size") {
        Some(JsValue::Number(size)) if is_non_negative_safe_integer(*size) => *size,
        _ => return None,
    };
    let encoding = match string("encoding")?.as_str() {
        "utf-8" => FileEncoding::Utf8,
        "binary" => FileEncoding::Binary,
        _ => return None,
    };
    Some(FileBeginMetadata {
        mime,
        size,
        encoding,
        modified_at: string("modifiedAt")?,
        revision: optional_string("revision")?,
        file_name: optional_string("fileName")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_frames_need_two_bytes_and_a_known_opcode() {
        let frame = decode_binary_frame(&[0x03, 7, b'x']);
        assert_eq!(
            frame,
            Some(BinaryFrame::Terminal(TerminalStreamFrame {
                opcode: TerminalOpcode::Resize,
                slot: 7,
                payload: vec![b'x'],
            }))
        );
        assert_eq!(decode_binary_frame(&[0x01]), None);
        assert_eq!(decode_binary_frame(&[0x06, 0]), None);
        assert_eq!(decode_binary_frame(&[]), None);
    }

    #[test]
    fn file_frames_follow_the_request_id_and_length_rules() {
        assert_eq!(decode_binary_frame(&[FILE_CHUNK, 0, 1]), None);
        assert_eq!(decode_binary_frame(&[FILE_CHUNK, 3, b'a', b'b']), None);
        assert_eq!(
            decode_binary_frame(&[FILE_END, 1, b'a']),
            Some(BinaryFrame::FileTransfer(FileTransferFrame::End {
                request_id: "a".into()
            }))
        );
        assert_eq!(decode_binary_frame(&[FILE_END, 1, b'a', 0]), None);
        assert_eq!(
            decode_binary_frame(&[0xEF, 0xBB, 0xBF]),
            None,
            "an unknown first byte is not a frame"
        );
    }

    #[test]
    fn a_request_id_loses_a_leading_byte_order_mark() {
        let frame = decode_binary_frame(&[FILE_CHUNK, 4, 0xEF, 0xBB, 0xBF, b'a']);
        assert_eq!(
            frame,
            Some(BinaryFrame::FileTransfer(FileTransferFrame::Chunk {
                request_id: "a".into(),
                payload: Vec::new(),
            }))
        );
    }

    #[test]
    fn begin_size_is_a_safe_integer() {
        for (size, decodes) in [
            ("9007199254740991", true),
            ("-0", true),
            ("1e2", true),
            ("9007199254740992", false),
            ("1e21", false),
        ] {
            let metadata =
                format!(r#"{{"mime":"a","size":{size},"encoding":"utf-8","modifiedAt":"x"}}"#);
            assert_eq!(
                decode_binary_frame(&begin(&metadata)).is_some(),
                decodes,
                "{size}"
            );
        }
    }

    #[test]
    fn begin_text_is_decoded_like_a_text_decoder() {
        let good = br#"{"mime":"a","size":1,"encoding":"utf-8","modifiedAt":"x"}"#;
        let frame = |metadata: &[u8]| {
            let mut bytes = vec![FILE_BEGIN, 1, b'r'];
            bytes.extend_from_slice(&u16::try_from(metadata.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(metadata);
            decode_binary_frame(&bytes)
        };
        let with_bom = [&[0xEF, 0xBB, 0xBF][..], good].concat();
        assert!(frame(&with_bom).is_some(), "a byte order mark is dropped");
        let in_string = [
            br#"{"mime":"te"#.as_slice(),
            &[0xFF],
            br#"xt","size":1,"encoding":"utf-8","modifiedAt":"x"}"#,
        ]
        .concat();
        assert!(
            frame(&in_string).is_some(),
            "an invalid byte in a string is replaced"
        );
        let outside = [good.as_slice(), &[0xFF]].concat();
        assert!(
            frame(&outside).is_none(),
            "an invalid byte outside a string is not JSON"
        );
    }

    fn begin(metadata: &str) -> Vec<u8> {
        let mut bytes = vec![FILE_BEGIN, 1, b'r'];
        bytes.extend_from_slice(&u16::try_from(metadata.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(metadata.as_bytes());
        bytes
    }

    #[test]
    fn begin_metadata_follows_the_schema() {
        let good = decode_binary_frame(&begin(
            r#"{"mime":"text/plain","size":1e2,"encoding":"binary","modifiedAt":"x","extra":1}"#,
        ));
        assert!(matches!(
            good,
            Some(BinaryFrame::FileTransfer(FileTransferFrame::Begin { metadata, .. }))
                if (metadata.size - 100.0).abs() < f64::EPSILON && metadata.encoding == FileEncoding::Binary
        ));
        for bad in [
            r"{}",
            r"[]",
            r#"{"mime":"","size":1,"encoding":"utf-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":-1,"encoding":"utf-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":9007199254740992,"encoding":"utf-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":1e21,"encoding":"utf-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":1e300,"encoding":"utf-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":1.5,"encoding":"utf-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":1,"encoding":"UTF-8","modifiedAt":"x"}"#,
            r#"{"mime":"a","size":1,"encoding":"utf-8","modifiedAt":"x","revision":null}"#,
            r#"{"mime":"a""#,
        ] {
            assert_eq!(decode_binary_frame(&begin(bad)), None, "{bad}");
        }
    }
}
