//! `cow_qs:parse_qs/1` as `cowboy_req:parse_qs/1` calls it (default `max_keys` of 100).

use crate::limits::MAXIMUM_QUERY_KEYS;
use std::collections::BTreeMap;

/// A failed parse. Cowboy answers both variants with an empty `400`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryError {
    LimitReached,
    Malformed,
}

/// A decoded query value. A bare name carries no `=`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryValue {
    Flag,
    Bytes(Vec<u8>),
}

/// Parses an `application/x-www-form-urlencoded` query string.
///
/// # Errors
///
/// Fails on the 101st key, an empty name before `=`, or a `%` not followed by two
/// hexadecimal digits.
pub fn parse_qs(query: &[u8]) -> Result<Vec<(Vec<u8>, QueryValue)>, QueryError> {
    let mut pairs = Vec::new();
    let mut remaining_keys = MAXIMUM_QUERY_KEYS;
    let mut position = 0;
    loop {
        if remaining_keys == 0 {
            return Err(QueryError::LimitReached);
        }
        let name_length = query[position..]
            .iter()
            .take_while(|byte| !matches!(**byte, b'&' | b'='))
            .count();
        let name_end = position + name_length;
        match query.get(name_end) {
            None if name_length == 0 => return Ok(pairs),
            None => {
                pairs.push((url_decode(&query[position..name_end])?, QueryValue::Flag));
                return Ok(pairs);
            }
            Some(b'&') if name_length == 0 => position = name_end + 1,
            Some(b'&') => {
                pairs.push((url_decode(&query[position..name_end])?, QueryValue::Flag));
                remaining_keys -= 1;
                position = name_end + 1;
            }
            Some(_) if name_length == 0 => return Err(QueryError::Malformed),
            Some(_) => {
                let name = url_decode(&query[position..name_end])?;
                let value_start = name_end + 1;
                let value_length = query[value_start..]
                    .iter()
                    .take_while(|byte| **byte != b'&')
                    .count();
                let value_end = value_start + value_length;
                let value = url_decode(&query[value_start..value_end])?;
                pairs.push((name, QueryValue::Bytes(value)));
                if value_end == query.len() {
                    return Ok(pairs);
                }
                remaining_keys -= 1;
                position = value_end + 1;
            }
        }
    }
}

/// Collapses parsed pairs the way `PaseoRelay.Socket.connection/1` does: a bare
/// name becomes an empty value and a later duplicate replaces an earlier one.
#[must_use]
pub fn into_query_map(pairs: Vec<(Vec<u8>, QueryValue)>) -> BTreeMap<Vec<u8>, Vec<u8>> {
    pairs
        .into_iter()
        .map(|(name, value)| {
            (
                name,
                match value {
                    QueryValue::Flag => Vec::new(),
                    QueryValue::Bytes(bytes) => bytes,
                },
            )
        })
        .collect()
}

fn url_decode(encoded: &[u8]) -> Result<Vec<u8>, QueryError> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while let Some(byte) = encoded.get(index) {
        match byte {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = encoded.get(index + 1).copied().and_then(hex_value);
                let low = encoded.get(index + 2).copied().and_then(hex_value);
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(QueryError::Malformed);
                };
                decoded.push((high << 4) | low);
                index += 2;
            }
            other => decoded.push(*other),
        }
        index += 1;
    }
    Ok(decoded)
}

fn hex_value(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}
