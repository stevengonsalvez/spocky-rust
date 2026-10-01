//! Serde adapters for zod field presence.
//!
//! zod separates three states that plain serde `Option` merges:
//!
//! | zod | missing | `null` | value | Rust |
//! |---|---|---|---|---|
//! | `.optional()` | omitted | rejected | kept | `Option<T>` + [`optional`] |
//! | `.nullable()` | rejected | kept | kept | `Option<T>` + [`nullable`] |
//! | `.nullable().optional()` | omitted | kept | kept | `Option<Nullable<T>>` + [`optional`] |
//!
//! Each adapter is used with `#[serde(with = ...)]`. The optional forms also
//! need `default` and `skip_serializing_if = "Option::is_none"`, so a missing
//! key stays missing on output, exactly as `JSON.stringify` drops `undefined`.

/// `.optional()`: a missing key is `None`; an explicit `null` is an error.
pub mod optional {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// # Errors
    ///
    /// Propagates the inner serializer error.
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    /// # Errors
    ///
    /// Rejects `null` unless `T` itself accepts it.
    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        T::deserialize(deserializer).map(Some)
    }
}

/// `.nullable()`: the key is required; `null` maps to `None`.
pub mod nullable {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// # Errors
    ///
    /// Propagates the inner serializer error.
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }

    /// # Errors
    ///
    /// Rejects values that are neither `null` nor a valid `T`.
    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<T>::deserialize(deserializer)
    }
}

/// A `.nullable()` value that is either `null` or `T`.
///
/// `.nullable().optional()` is `Option<Nullable<T>>` with [`optional`]:
/// missing is `None`, `null` is `Some(Nullable::Null)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nullable<T> {
    Null,
    Value(T),
}

impl<T> Nullable<T> {
    #[must_use]
    pub fn as_option(&self) -> Option<&T> {
        match self {
            Self::Null => None,
            Self::Value(value) => Some(value),
        }
    }
}

impl<T> From<Option<T>> for Nullable<T> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Self::Value)
    }
}

impl<T: serde::Serialize> serde::Serialize for Nullable<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_option().serialize(serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Nullable<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Probe {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "super::optional"
        )]
        optional: Option<String>,
        #[serde(with = "super::nullable")]
        nullable: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "super::optional"
        )]
        both: Option<super::Nullable<String>>,
    }

    fn roundtrip(input: &str) -> Result<String, serde_json::Error> {
        serde_json::from_str::<Probe>(input).and_then(|probe| serde_json::to_string(&probe))
    }

    #[test]
    fn presence_states_survive_roundtrip() {
        for text in [
            r#"{"nullable":null}"#,
            r#"{"optional":"a","nullable":"b","both":null}"#,
            r#"{"nullable":null,"both":"c"}"#,
        ] {
            assert_eq!(roundtrip(text).unwrap(), text);
        }
    }

    #[test]
    fn rejects_states_zod_rejects() {
        assert!(roundtrip(r"{}").is_err(), "nullable is required");
        assert!(roundtrip(r#"{"optional":null,"nullable":null}"#).is_err());
    }
}
