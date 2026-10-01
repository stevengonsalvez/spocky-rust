//! Pattern-checked identifiers from the wire schemas.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

/// `^wks_[a-f0-9]{16}$`, the `WorkspaceCreateRequestSchema.workspaceId`
/// pattern. Daemon-generated ids are `wks_` plus 16 lowercase hex digits.
#[must_use]
pub fn is_workspace_id(value: &str) -> bool {
    value.strip_prefix("wks_").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The zod 4 `z.uuid()` pattern: an RFC 9562 version 1 to 8 UUID with the
/// RFC variant, or the nil or max UUID, in either case.
#[must_use]
pub fn is_zod_uuid(value: &str) -> bool {
    if value == "00000000-0000-0000-0000-000000000000"
        || value == "ffffffff-ffff-ffff-ffff-ffffffffffff"
    {
        return true;
    }
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let ok = match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => (b'1'..=b'8').contains(byte),
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b' | b'A' | b'B'),
            _ => is_hex(*byte),
        };
        if !ok {
            return false;
        }
    }
    true
}

macro_rules! checked_id {
    ($(#[$attr:meta])* $name:ident, $check:path, $expected:literal) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Returns `None` when `value` does not match the pattern.
            #[must_use]
            pub fn new(value: String) -> Option<Self> {
                $check(&value).then_some(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?)
                    .ok_or_else(|| de::Error::custom($expected))
            }
        }
    };
}

checked_id!(
    /// A workspace id matching `^wks_[a-f0-9]{16}$`.
    WorkspaceId,
    is_workspace_id,
    "expected a workspace id matching ^wks_[a-f0-9]{16}$"
);

checked_id!(
    /// A string accepted by zod 4 `z.uuid()`.
    ZodUuid,
    is_zod_uuid,
    "expected a UUID"
);

#[cfg(test)]
mod tests {
    use super::{is_workspace_id, is_zod_uuid};

    #[test]
    fn workspace_id_pattern() {
        assert!(is_workspace_id("wks_0123456789abcdef"));
        assert!(!is_workspace_id("wks_0123456789ABCDEF"));
        assert!(!is_workspace_id("wks_0123456789abcde"));
        assert!(!is_workspace_id("wks_0123456789abcdefa"));
        assert!(!is_workspace_id("wsk_0123456789abcdef"));
    }

    #[test]
    fn uuid_pattern_matches_zod() {
        assert!(is_zod_uuid("123e4567-e89b-42d3-a456-426614174000"));
        assert!(is_zod_uuid("123E4567-E89B-82D3-B456-426614174000"));
        assert!(is_zod_uuid("00000000-0000-0000-0000-000000000000"));
        assert!(is_zod_uuid("ffffffff-ffff-ffff-ffff-ffffffffffff"));
        assert!(!is_zod_uuid("FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF"));
        assert!(!is_zod_uuid("123e4567-e89b-02d3-a456-426614174000"));
        assert!(!is_zod_uuid("123e4567-e89b-92d3-a456-426614174000"));
        assert!(!is_zod_uuid("123e4567-e89b-42d3-c456-426614174000"));
        assert!(!is_zod_uuid("123e4567e89b42d3a456426614174000"));
    }
}
