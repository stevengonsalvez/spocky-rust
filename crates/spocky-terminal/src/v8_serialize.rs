//! V8 structured clone serialization of plain JavaScript data, as Node's
//! `child_process` writes it with `serialization: "advanced"`: the
//! `v8.Serializer` wire format version 15 (Node 22.20.0), preceded on the
//! channel by a 4-byte big-endian length. Pinned Paseo forks its terminal
//! worker that way (`worker-terminal-manager.ts` `forkTerminalWorker`).
//!
//! Covered value shapes are those a worker message holds: `undefined`,
//! `null`, booleans, numbers, strings, arrays, and plain objects. Unlike
//! JSON the format keeps keys whose value is `undefined`, `NaN`, `Infinity`
//! and `-0`. Objects write their own enumerable keys in JavaScript order,
//! index-like keys as numbers.
//!
//! Where V8 picks a representation by internal state, this encoder picks by
//! value: an integral number in the `int32` range that is not `-0` is a Smi
//! (`I`), any other number a double (`N`); a string with only code units up
//! to `0xFF` is one-byte (`"`), otherwise two-byte (`c`); arrays are dense
//! (`A`). Values created the usual way (literals, `JSON.parse`, counters)
//! take those forms in V8; a number or string that V8 holds in the other
//! representation serializes to different bytes yet decodes to the same
//! value, and the decoder reads both.

use spocky_contracts::js_value::{
    JsObject, JsValue, array_index, js_number, js_text_from_utf16, js_text_utf16,
};

const VERSION: u32 = 15;
const TAG_VERSION: u8 = 0xFF;
const TAG_PADDING: u8 = 0x00;
const TAG_HOLE: u8 = b'-';
const TAG_UNDEFINED: u8 = b'_';
const TAG_NULL: u8 = b'0';
const TAG_TRUE: u8 = b'T';
const TAG_FALSE: u8 = b'F';
const TAG_INT32: u8 = b'I';
const TAG_UINT32: u8 = b'U';
const TAG_DOUBLE: u8 = b'N';
const TAG_ONE_BYTE_STRING: u8 = b'"';
const TAG_TWO_BYTE_STRING: u8 = b'c';
const TAG_UTF8_STRING: u8 = b'S';
const TAG_OBJECT_REFERENCE: u8 = b'^';
const TAG_BEGIN_OBJECT: u8 = b'o';
const TAG_END_OBJECT: u8 = b'{';
const TAG_BEGIN_SPARSE_ARRAY: u8 = b'a';
const TAG_END_SPARSE_ARRAY: u8 = b'@';
const TAG_BEGIN_DENSE_ARRAY: u8 = b'A';
const TAG_END_DENSE_ARRAY: u8 = b'$';

/// Why a buffer is not a serialized value this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The data ended inside a value.
    Truncated,
    /// A tag outside the supported shapes.
    UnsupportedTag(u8),
    /// A missing or too-new header.
    BadHeader,
    /// A back reference to an object that is not finished (a cycle) or does
    /// not exist.
    BadReference,
    /// Bytes left after the value.
    TrailingBytes,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("serialized data is truncated"),
            Self::UnsupportedTag(tag) => write!(f, "unsupported serialization tag 0x{tag:02x}"),
            Self::BadHeader => f.write_str("bad serialization header"),
            Self::BadReference => f.write_str("bad object reference"),
            Self::TrailingBytes => f.write_str("trailing bytes after the value"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// `v8.serialize(value)` for plain data: the header and the value.
#[must_use]
pub fn serialize(value: &JsValue) -> Vec<u8> {
    let mut writer = Writer { out: Vec::new() };
    writer.out.push(TAG_VERSION);
    writer.varint(u64::from(VERSION));
    writer.value(value);
    writer.out
}

/// One IPC channel message: the 4-byte big-endian length, then the
/// serialized bytes.
///
/// # Panics
///
/// Never in practice: a message that overflows `u32` bytes is not a worker
/// message.
#[must_use]
pub fn encode_frame(value: &JsValue) -> Vec<u8> {
    let body = serialize(value);
    let length = u32::try_from(body.len()).expect("message under 4 GiB");
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    frame
}

/// `v8.deserialize(bytes)` for plain data.
///
/// # Errors
///
/// A [`DecodeError`] for data that is not a value of the supported shapes.
pub fn deserialize(bytes: &[u8]) -> Result<JsValue, DecodeError> {
    let mut reader = Reader {
        bytes,
        index: 0,
        objects: Vec::new(),
    };
    if reader.byte()? != TAG_VERSION {
        return Err(DecodeError::BadHeader);
    }
    let version = reader.varint()?;
    if !(13..=u64::from(VERSION)).contains(&version) {
        return Err(DecodeError::BadHeader);
    }
    let value = reader.value()?;
    if reader.index != bytes.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(value)
}

/// One channel message as read: its bytes (length prefix included) and the
/// decoded value.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub raw: Vec<u8>,
    pub value: Result<JsValue, DecodeError>,
}

/// Splits the channel byte stream into messages and decodes each.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    pending: Vec<u8>,
}

impl FrameDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes and returns every message that is now complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.pending.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while self.pending.len() >= 4 {
            let length = u32::from_be_bytes([
                self.pending[0],
                self.pending[1],
                self.pending[2],
                self.pending[3],
            ]);
            let total = 4 + usize::try_from(length).unwrap_or(usize::MAX);
            if self.pending.len() < total {
                break;
            }
            let raw: Vec<u8> = self.pending.drain(..total).collect();
            let value = deserialize(&raw[4..]);
            frames.push(Frame { raw, value });
        }
        frames
    }
}

struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn varint(&mut self, mut value: u64) {
        loop {
            let byte = u8::try_from(value & 0x7F).unwrap_or(0);
            value >>= 7;
            if value == 0 {
                self.out.push(byte);
                return;
            }
            self.out.push(byte | 0x80);
        }
    }

    fn varint_len(value: u64) -> usize {
        let mut length = 1;
        let mut rest = value >> 7;
        while rest != 0 {
            length += 1;
            rest >>= 7;
        }
        length
    }

    fn number(&mut self, value: f64) {
        #[allow(clippy::float_cmp, clippy::cast_possible_truncation)]
        let smi = (value == value.trunc()
            && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&value)
            && !(value == 0.0 && value.is_sign_negative()))
        .then_some(value as i32);
        if let Some(int) = smi {
            self.out.push(TAG_INT32);
            // ZigZag: `(n << 1) ^ (n >> 31)`.
            let zigzag = ((int << 1) ^ (int >> 31)).cast_unsigned();
            self.varint(u64::from(zigzag));
        } else {
            self.out.push(TAG_DOUBLE);
            self.out.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn string(&mut self, text: &str) {
        let units: Vec<u16> = js_text_utf16(text).collect();
        if units.iter().all(|unit| *unit <= 0xFF) {
            self.out.push(TAG_ONE_BYTE_STRING);
            self.varint(units.len() as u64);
            self.out
                .extend(units.iter().map(|unit| u8::try_from(*unit).unwrap_or(0)));
            return;
        }
        let byte_length = units.len() as u64 * 2;
        // The reader expects two-byte data to start on an even offset.
        if (self.out.len() + 1 + Self::varint_len(byte_length)) & 1 == 1 {
            self.out.push(TAG_PADDING);
        }
        self.out.push(TAG_TWO_BYTE_STRING);
        self.varint(byte_length);
        for unit in units {
            self.out.extend_from_slice(&unit.to_le_bytes());
        }
    }

    /// A property key: an index-like key is a number, any other a string.
    fn key(&mut self, key: &str) {
        match array_index(key) {
            Some(index) => self.number(f64::from(index)),
            None => self.string(key),
        }
    }

    fn value(&mut self, value: &JsValue) {
        match value {
            JsValue::Undefined => self.out.push(TAG_UNDEFINED),
            JsValue::Null => self.out.push(TAG_NULL),
            JsValue::Bool(true) => self.out.push(TAG_TRUE),
            JsValue::Bool(false) => self.out.push(TAG_FALSE),
            JsValue::Number(number) => self.number(*number),
            JsValue::String(text) => self.string(text),
            JsValue::Array(items) => {
                self.out.push(TAG_BEGIN_DENSE_ARRAY);
                self.varint(items.len() as u64);
                for item in items {
                    self.value(item);
                }
                self.out.push(TAG_END_DENSE_ARRAY);
                self.varint(0);
                self.varint(items.len() as u64);
            }
            JsValue::Object(object) => {
                self.out.push(TAG_BEGIN_OBJECT);
                let mut written = 0u64;
                for (key, value) in object.iter() {
                    self.key(key);
                    self.value(value);
                    written += 1;
                }
                self.out.push(TAG_END_OBJECT);
                self.varint(written);
            }
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    index: usize,
    /// Objects in the order their first tag was read; `None` while open.
    objects: Vec<Option<JsValue>>,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, DecodeError> {
        let byte = *self.bytes.get(self.index).ok_or(DecodeError::Truncated)?;
        self.index += 1;
        Ok(byte)
    }

    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut result = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            if shift < 64 {
                result |= u64::from(byte & 0x7F) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    fn raw(&mut self, length: usize) -> Result<&[u8], DecodeError> {
        let end = self
            .index
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(DecodeError::Truncated)?;
        let slice = &self.bytes[self.index..end];
        self.index = end;
        Ok(slice)
    }

    fn length(&mut self) -> Result<usize, DecodeError> {
        usize::try_from(self.varint()?).map_err(|_| DecodeError::Truncated)
    }

    /// The next tag, skipping padding.
    fn tag(&mut self) -> Result<u8, DecodeError> {
        loop {
            let tag = self.byte()?;
            if tag != TAG_PADDING {
                return Ok(tag);
            }
        }
    }

    fn value(&mut self) -> Result<JsValue, DecodeError> {
        let tag = self.tag()?;
        self.value_with_tag(tag)
    }

    fn value_with_tag(&mut self, tag: u8) -> Result<JsValue, DecodeError> {
        Ok(match tag {
            TAG_UNDEFINED | TAG_HOLE => JsValue::Undefined,
            TAG_NULL => JsValue::Null,
            TAG_TRUE => JsValue::Bool(true),
            TAG_FALSE => JsValue::Bool(false),
            TAG_INT32 => {
                let zigzag = u32::try_from(self.varint()? & 0xFFFF_FFFF).unwrap_or(0);
                let int = (zigzag >> 1).cast_signed() ^ -((zigzag & 1).cast_signed());
                JsValue::Number(f64::from(int))
            }
            TAG_UINT32 => JsValue::Number(f64::from(
                u32::try_from(self.varint()? & 0xFFFF_FFFF).unwrap_or(0),
            )),
            TAG_DOUBLE => {
                let bytes: [u8; 8] = self
                    .raw(8)?
                    .try_into()
                    .map_err(|_| DecodeError::Truncated)?;
                JsValue::Number(f64::from_le_bytes(bytes))
            }
            TAG_ONE_BYTE_STRING => {
                let length = self.length()?;
                let units: Vec<u16> = self.raw(length)?.iter().map(|b| u16::from(*b)).collect();
                JsValue::String(js_text_from_utf16(&units))
            }
            TAG_TWO_BYTE_STRING => {
                let length = self.length()?;
                let units: Vec<u16> = self
                    .raw(length)?
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                JsValue::String(js_text_from_utf16(&units))
            }
            TAG_UTF8_STRING => {
                let length = self.length()?;
                JsValue::String(String::from_utf8_lossy(self.raw(length)?).into_owned())
            }
            TAG_OBJECT_REFERENCE => {
                let id = self.length()?;
                self.objects
                    .get(id)
                    .and_then(Clone::clone)
                    .ok_or(DecodeError::BadReference)?
            }
            TAG_BEGIN_OBJECT => self.object()?,
            TAG_BEGIN_DENSE_ARRAY => self.dense_array()?,
            TAG_BEGIN_SPARSE_ARRAY => self.sparse_array()?,
            other => return Err(DecodeError::UnsupportedTag(other)),
        })
    }

    /// A key: a string, or a number written as the property name.
    fn key(&mut self, tag: u8) -> Result<String, DecodeError> {
        match &self.value_with_tag(tag)? {
            JsValue::String(text) => Ok(text.clone()),
            JsValue::Number(number) => Ok(js_number(*number)),
            _ => Err(DecodeError::UnsupportedTag(tag)),
        }
    }

    fn object(&mut self) -> Result<JsValue, DecodeError> {
        let slot = self.objects.len();
        self.objects.push(None);
        let mut object = JsObject::new();
        let mut read = 0u64;
        loop {
            let tag = self.tag()?;
            if tag == TAG_END_OBJECT {
                break;
            }
            let key = self.key(tag)?;
            let value = self.value()?;
            object.insert(key, value);
            read += 1;
        }
        if self.varint()? != read {
            return Err(DecodeError::Truncated);
        }
        let value = JsValue::Object(object);
        self.objects[slot] = Some(value.clone());
        Ok(value)
    }

    fn dense_array(&mut self) -> Result<JsValue, DecodeError> {
        let slot = self.objects.len();
        self.objects.push(None);
        let length = self.length()?;
        let mut items = Vec::with_capacity(length.min(self.bytes.len()));
        for _ in 0..length {
            items.push(self.value()?);
        }
        // Non-index properties follow the elements, then `$`, the property
        // count and the length.
        loop {
            let tag = self.tag()?;
            if tag == TAG_END_DENSE_ARRAY {
                break;
            }
            let key = self.key(tag)?;
            let value = self.value()?;
            if let Some(index) = array_index(&key).and_then(|i| usize::try_from(i).ok())
                && index < items.len()
            {
                items[index] = value;
            }
        }
        self.varint()?;
        self.varint()?;
        let value = JsValue::Array(items);
        self.objects[slot] = Some(value.clone());
        Ok(value)
    }

    fn sparse_array(&mut self) -> Result<JsValue, DecodeError> {
        let slot = self.objects.len();
        self.objects.push(None);
        let length = self.length()?;
        let mut items = vec![JsValue::Undefined; length.min(self.bytes.len() * 8 + 1)];
        loop {
            let tag = self.tag()?;
            if tag == TAG_END_SPARSE_ARRAY {
                break;
            }
            let key = self.key(tag)?;
            let value = self.value()?;
            if let Some(index) = array_index(&key).and_then(|i| usize::try_from(i).ok())
                && index < items.len()
            {
                items[index] = value;
            }
        }
        self.varint()?;
        self.varint()?;
        let value = JsValue::Array(items);
        self.objects[slot] = Some(value.clone());
        Ok(value)
    }
}
