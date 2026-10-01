//! `z.literal("...")` values as zero-sized types.

/// Declares a unit type that serializes as, and only accepts, one string.
macro_rules! string_literal {
    ($(#[$attr:meta])* $name:ident = $text:literal) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
        pub struct $name;

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str($text)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                if value == $text {
                    Ok(Self)
                } else {
                    Err(serde::de::Error::invalid_value(
                        serde::de::Unexpected::Str(&value),
                        &concat!("\"", $text, "\""),
                    ))
                }
            }
        }
    };
}

pub(crate) use string_literal;

/// `z.null()`: serializes as `null` and accepts only `null`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Null;

impl serde::Serialize for Null {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_unit()
    }
}

impl<'de> serde::Deserialize<'de> for Null {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <()>::deserialize(deserializer).map(|()| Self)
    }
}

#[cfg(test)]
mod tests {
    use super::Null;

    string_literal!(Probe = "a/b");

    #[test]
    fn literals_accept_only_their_text() {
        assert_eq!(serde_json::to_string(&Probe).unwrap(), r#""a/b""#);
        assert!(serde_json::from_str::<Probe>(r#""a/b""#).is_ok());
        assert!(serde_json::from_str::<Probe>(r#""a/c""#).is_err());
        assert_eq!(serde_json::to_string(&Null).unwrap(), "null");
        assert!(serde_json::from_str::<Null>("null").is_ok());
        assert!(serde_json::from_str::<Null>("0").is_err());
    }
}
