//! PostgreSQL wire messages as the pinned PGlite client builds and parses
//! them (`We` serializers and the `pe` parser in the glue).

/// A bind parameter after PGlite's serializers ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindValue {
    Null,
    Text(String),
    Binary(Vec<u8>),
}

fn frame(code: u8, body: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(body.len() + 5);
    message.push(code);
    let length = i32::try_from(body.len() + 4).unwrap_or(i32::MAX);
    message.extend_from_slice(&length.to_be_bytes());
    message.extend_from_slice(body);
    message
}

fn cstring(body: &mut Vec<u8>, text: &str) {
    body.extend_from_slice(text.as_bytes());
    body.push(0);
}

#[must_use]
pub fn query(sql: &str) -> Vec<u8> {
    let mut body = Vec::new();
    cstring(&mut body, sql);
    frame(b'Q', &body)
}

#[must_use]
pub fn parse(sql: &str, types: &[i32]) -> Vec<u8> {
    let mut body = Vec::new();
    cstring(&mut body, "");
    cstring(&mut body, sql);
    let count = i16::try_from(types.len()).unwrap_or(i16::MAX);
    body.extend_from_slice(&count.to_be_bytes());
    for oid in types {
        body.extend_from_slice(&oid.to_be_bytes());
    }
    frame(b'P', &body)
}

#[must_use]
pub fn bind(values: &[BindValue]) -> Vec<u8> {
    let mut body = Vec::new();
    cstring(&mut body, "");
    cstring(&mut body, "");
    let count = i16::try_from(values.len()).unwrap_or(i16::MAX);
    body.extend_from_slice(&count.to_be_bytes());
    let mut data = Vec::new();
    for value in values {
        match value {
            BindValue::Null => {
                body.extend_from_slice(&0_i16.to_be_bytes());
                data.extend_from_slice(&(-1_i32).to_be_bytes());
            }
            BindValue::Binary(bytes) => {
                body.extend_from_slice(&1_i16.to_be_bytes());
                let length = i32::try_from(bytes.len()).unwrap_or(i32::MAX);
                data.extend_from_slice(&length.to_be_bytes());
                data.extend_from_slice(bytes);
            }
            BindValue::Text(text) => {
                body.extend_from_slice(&0_i16.to_be_bytes());
                let length = i32::try_from(text.len()).unwrap_or(i32::MAX);
                data.extend_from_slice(&length.to_be_bytes());
                data.extend_from_slice(text.as_bytes());
            }
        }
    }
    body.extend_from_slice(&count.to_be_bytes());
    body.extend_from_slice(&data);
    body.extend_from_slice(&0_i16.to_be_bytes());
    frame(b'B', &body)
}

/// `describe({type})` for the unnamed statement (`S`) or portal (`P`).
#[must_use]
pub fn describe(kind: u8) -> Vec<u8> {
    frame(b'D', &[kind, 0])
}

#[must_use]
pub fn execute() -> Vec<u8> {
    vec![69, 0, 0, 0, 9, 0, 0, 0, 0, 0]
}

#[must_use]
pub fn sync() -> Vec<u8> {
    vec![b'S', 0, 0, 0, 4]
}

#[must_use]
pub fn end() -> Vec<u8> {
    vec![b'X', 0, 0, 0, 4]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub table_id: i32,
    pub column_id: i16,
    pub type_oid: i32,
    pub type_size: i16,
    pub type_modifier: i32,
    pub binary: bool,
}

/// `DatabaseError` and `NoticeMessage` fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorFields {
    pub message: Option<String>,
    pub severity: Option<String>,
    pub code: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<String>,
    pub internal_position: Option<String>,
    pub internal_query: Option<String>,
    pub r#where: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub data_type: Option<String>,
    pub constraint: Option<String>,
    pub file: Option<String>,
    pub line: Option<String>,
    pub routine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    ParseComplete,
    BindComplete,
    CloseComplete,
    NoData,
    PortalSuspended,
    CopyDone,
    ReplicationStart,
    EmptyQuery,
    DataRow(Vec<Option<String>>),
    CommandComplete(String),
    ReadyForQuery(u8),
    Notification {
        process_id: i32,
        channel: String,
        payload: String,
    },
    Authentication(i32),
    ParameterStatus(String, String),
    BackendKeyData(i32, i32),
    Error(ErrorFields),
    Notice(ErrorFields),
    RowDescription(Vec<Field>),
    ParameterDescription(Vec<i32>),
    CopyIn,
    CopyOut,
    CopyData(Vec<u8>),
    Invalid(u8),
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> &[u8] {
        let start = self.offset.min(self.bytes.len());
        let end = (self.offset + count).min(self.bytes.len());
        self.offset += count;
        &self.bytes[start..end]
    }
    fn int16(&mut self) -> i16 {
        let bytes = self.take(2);
        if bytes.len() < 2 {
            return 0;
        }
        i16::from_be_bytes([bytes[0], bytes[1]])
    }
    fn int32(&mut self) -> i32 {
        let bytes = self.take(4);
        if bytes.len() < 4 {
            return 0;
        }
        i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
    fn byte(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }
    fn string(&mut self, count: usize) -> String {
        String::from_utf8_lossy(self.take(count)).into_owned()
    }
    fn cstring(&mut self) -> String {
        let start = self.offset.min(self.bytes.len());
        let end = self.bytes[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(self.bytes.len(), |position| start + position);
        let text = String::from_utf8_lossy(&self.bytes[start..end]).into_owned();
        self.offset = end + 1;
        text
    }
}

/// Splits a complete output buffer into messages. A trailing partial message
/// stays unparsed, as the streaming parser keeps it for the next chunk.
#[must_use]
pub fn parse_messages(bytes: &[u8]) -> Vec<Backend> {
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset + 5 <= bytes.len() {
        let code = bytes[offset];
        let length = u32::from_be_bytes([
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
            bytes[offset + 4],
        ]) as usize;
        let total = 1 + length;
        if length == 0 || offset + total > bytes.len() {
            break;
        }
        let body = &bytes[offset + 5..offset + total];
        messages.push(parse_one(code, body, length));
        offset += total;
    }
    messages
}

fn parse_one(code: u8, body: &[u8], length: usize) -> Backend {
    let mut reader = Reader {
        bytes: body,
        offset: 0,
    };
    match code {
        b'2' => Backend::BindComplete,
        b'1' => Backend::ParseComplete,
        b'3' => Backend::CloseComplete,
        b'n' => Backend::NoData,
        b's' => Backend::PortalSuspended,
        b'c' => Backend::CopyDone,
        b'W' => Backend::ReplicationStart,
        b'I' => Backend::EmptyQuery,
        b'D' => {
            let count = reader.int16();
            let mut fields = Vec::new();
            for _ in 0..count.max(0) {
                let size = reader.int32();
                fields.push(if size == -1 {
                    None
                } else {
                    Some(reader.string(usize::try_from(size).unwrap_or(0)))
                });
            }
            Backend::DataRow(fields)
        }
        b'C' => Backend::CommandComplete(reader.cstring()),
        b'Z' => Backend::ReadyForQuery(reader.byte()),
        b'A' => {
            let process_id = reader.int32();
            let channel = reader.cstring();
            let payload = reader.cstring();
            Backend::Notification {
                process_id,
                channel,
                payload,
            }
        }
        b'R' => Backend::Authentication(reader.int32()),
        b'S' => {
            let name = reader.cstring();
            let value = reader.cstring();
            Backend::ParameterStatus(name, value)
        }
        b'K' => {
            let process = reader.int32();
            let secret = reader.int32();
            Backend::BackendKeyData(process, secret)
        }
        b'E' | b'N' => {
            let mut fields = ErrorFields::default();
            loop {
                let kind = reader.byte();
                if kind == 0 {
                    break;
                }
                let value = Some(reader.cstring());
                match kind {
                    b'M' => fields.message = value,
                    b'S' => fields.severity = value,
                    b'C' => fields.code = value,
                    b'D' => fields.detail = value,
                    b'H' => fields.hint = value,
                    b'P' => fields.position = value,
                    b'p' => fields.internal_position = value,
                    b'q' => fields.internal_query = value,
                    b'W' => fields.r#where = value,
                    b's' => fields.schema = value,
                    b't' => fields.table = value,
                    b'c' => fields.column = value,
                    b'd' => fields.data_type = value,
                    b'n' => fields.constraint = value,
                    b'F' => fields.file = value,
                    b'L' => fields.line = value,
                    b'R' => fields.routine = value,
                    _ => {}
                }
            }
            if code == b'E' {
                Backend::Error(fields)
            } else {
                Backend::Notice(fields)
            }
        }
        b'T' => {
            let count = reader.int16();
            let mut fields = Vec::new();
            for _ in 0..count.max(0) {
                let name = reader.cstring();
                let table_id = reader.int32();
                let column_id = reader.int16();
                let type_oid = reader.int32();
                let type_size = reader.int16();
                let type_modifier = reader.int32();
                let binary = reader.int16() != 0;
                fields.push(Field {
                    name,
                    table_id,
                    column_id,
                    type_oid,
                    type_size,
                    type_modifier,
                    binary,
                });
            }
            Backend::RowDescription(fields)
        }
        b't' => {
            let count = reader.int16();
            Backend::ParameterDescription((0..count.max(0)).map(|_| reader.int32()).collect())
        }
        b'G' => Backend::CopyIn,
        b'H' => Backend::CopyOut,
        b'd' => Backend::CopyData(body[..length.saturating_sub(4).min(body.len())].to_vec()),
        other => Backend::Invalid(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializers_match_the_pinned_client_bytes() {
        assert_eq!(query("select 1"), b"Q\0\0\0\x0dselect 1\0".to_vec());
        assert_eq!(describe(b'S'), b"D\0\0\0\x06S\0".to_vec());
        assert_eq!(sync(), b"S\0\0\0\x04".to_vec());
        let bound = bind(&[
            BindValue::Text("a".into()),
            BindValue::Null,
            BindValue::Binary(vec![7]),
        ]);
        assert_eq!(
            bound,
            b"B\0\0\0\x20\0\0\0\x03\0\0\0\0\0\x01\0\x03\0\0\0\x01a\xff\xff\xff\xff\0\0\0\x01\x07\0\0".to_vec()
        );
    }

    #[test]
    fn parser_keeps_error_fields_and_null_cells() {
        let mut bytes = b"D\0\0\0\x0f\0\x02\0\0\0\x01x\xff\xff\xff\xff".to_vec();
        bytes.extend_from_slice(b"E\0\0\0\x15SERROR\0C42P01\0M\0\0");
        let messages = parse_messages(&bytes);
        assert_eq!(messages[0], Backend::DataRow(vec![Some("x".into()), None]));
        let Backend::Error(fields) = &messages[1] else {
            panic!("expected error");
        };
        assert_eq!(fields.code.as_deref(), Some("42P01"));
        assert_eq!(fields.severity.as_deref(), Some("ERROR"));
    }
}
