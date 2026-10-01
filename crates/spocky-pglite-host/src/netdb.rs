//! `getaddrinfo`, `getnameinfo` and the glue's fake `DNS` address map.

use std::collections::HashMap;
use std::net::Ipv6Addr;

use wasmtime::{Caller, Val};

use crate::runtime::{
    self, Runtime, c_string, read_i16, read_i32, string_to_utf8, to_int32, write_bytes, write_i16,
    write_i32, write_u32,
};

/// `DNS.address_map`.
#[derive(Debug, Default)]
pub struct Dns {
    next: u32,
    addresses: HashMap<String, String>,
    names: HashMap<String, String>,
}

impl Dns {
    fn lookup_name(&mut self, name: &str) -> String {
        if inet_pton4(name).is_some() || inet_pton6(name).is_some() {
            return name.to_owned();
        }
        if let Some(address) = self.addresses.get(name) {
            return address.clone();
        }
        if self.next == 0 {
            self.next = 1;
        }
        let id = self.next;
        self.next += 1;
        let address = format!("172.29.{}.{}", id & 255, id & 65280);
        self.names.insert(address.clone(), name.to_owned());
        self.addresses.insert(name.to_owned(), address.clone());
        address
    }

    fn lookup_address(&self, address: &str) -> Option<String> {
        self.names.get(address).cloned()
    }
}

fn htons(value: u16) -> u16 {
    value.swap_bytes()
}

fn htonl(value: u32) -> u32 {
    value.swap_bytes()
}

/// `inetPton4`: `Number()` of each dotted part, packed little-endian.
fn inet_pton4(text: &str) -> Option<u32> {
    let parts: Vec<&str> = text.split('.').collect();
    let mut values = [0_u32; 4];
    for (index, slot) in values.iter_mut().enumerate() {
        let part = parts.get(index)?;
        let number = js_number(part)?;
        *slot = to_int32(number).cast_unsigned();
    }
    Some(
        values[0]
            | values[1].wrapping_shl(8)
            | values[2].wrapping_shl(16)
            | values[3].wrapping_shl(24),
    )
}

/// JavaScript `Number(string)` for the forms these helpers see.
fn js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).ok().map(|value| {
            #[allow(clippy::cast_precision_loss)]
            let value = value as f64;
            value
        });
    }
    if trimmed
        .chars()
        .any(|character| character.is_ascii_alphabetic() && character != 'e' && character != 'E')
    {
        return match trimmed {
            "Infinity" | "+Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        };
    }
    trimmed.parse::<f64>().ok()
}

fn inet_pton6(text: &str) -> Option<[i32; 4]> {
    let address: Ipv6Addr = text.parse().ok()?;
    let segments = address.segments();
    let swapped: Vec<u32> = segments
        .iter()
        .map(|segment| u32::from(htons(*segment)))
        .collect();
    Some([
        (swapped[1].wrapping_shl(16) | swapped[0]).cast_signed(),
        (swapped[3].wrapping_shl(16) | swapped[2]).cast_signed(),
        (swapped[5].wrapping_shl(16) | swapped[4]).cast_signed(),
        (swapped[7].wrapping_shl(16) | swapped[6]).cast_signed(),
    ])
}

fn inet_ntop4(value: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        value & 255,
        (value >> 8) & 255,
        (value >> 16) & 255,
        (value >> 24) & 255
    )
}

/// `inetNtop6`.
fn inet_ntop6(words: [i32; 4]) -> String {
    let parts: [i32; 8] = [
        words[0] & 65535,
        words[0] >> 16,
        words[1] & 65535,
        words[1] >> 16,
        words[2] & 65535,
        words[2] >> 16,
        words[3] & 65535,
        words[3] >> 16,
    ];
    if parts[..5].iter().all(|part| *part == 0) {
        let v4 = inet_ntop4((parts[6] | (parts[7] << 16)).cast_unsigned());
        if parts[5] == -1 {
            return format!("::ffff:{v4}");
        }
        if parts[5] == 0 {
            let suffix = match v4.as_str() {
                "0.0.0.0" => String::new(),
                "0.0.0.1" => "1".to_owned(),
                _ => v4,
            };
            return format!("::{suffix}");
        }
    }
    let (mut longest, mut start, mut last_zero, mut run) = (0_i32, 0_i32, 0_i32, 0_i32);
    for (position, part) in parts.iter().enumerate() {
        let position = i32::try_from(position).unwrap_or(0);
        if *part == 0 {
            if position - last_zero > 1 {
                run = 0;
            }
            last_zero = position;
            run += 1;
        }
        if run > longest {
            longest = run;
            start = position - longest + 1;
        }
    }
    let mut output = String::new();
    for (position, part) in parts.iter().enumerate() {
        let position = i32::try_from(position).unwrap_or(0);
        if longest > 1 && *part == 0 && position >= start && position < start + longest {
            if position == start {
                output.push(':');
                if start == 0 {
                    output.push(':');
                }
            }
            continue;
        }
        let value = htons(u16::try_from(part & 65535).unwrap_or(0));
        output.push_str(&format!("{value:x}"));
        if position < 7 {
            output.push(':');
        }
    }
    output
}

enum Address {
    V4(u32),
    V6([i32; 4]),
}

/// `writeSockaddr`; returns 0 or an errno.
fn write_sockaddr(
    memory: &mut [u8],
    address: u32,
    family: i32,
    host: &str,
    port: u16,
    length: u32,
) -> i32 {
    match family {
        2 => {
            let value = inet_pton4(host).unwrap_or(0);
            write_bytes(memory, address, &[0; 16]);
            if length != 0 {
                write_i32(memory, length, 16);
            }
            write_i16(memory, address, 2);
            write_u32(memory, address + 4, value);
            write_i16(memory, address + 2, htons(port).cast_signed());
            0
        }
        10 => {
            let words = inet_pton6(host).unwrap_or([0; 4]);
            write_bytes(memory, address, &[0; 28]);
            if length != 0 {
                write_i32(memory, length, 28);
            }
            write_i32(memory, address, 10);
            for (position, word) in words.iter().enumerate() {
                write_i32(
                    memory,
                    address + 8 + u32::try_from(position * 4).unwrap_or(0),
                    *word,
                );
            }
            write_i16(memory, address + 2, htons(port).cast_signed());
            0
        }
        _ => 5,
    }
}

/// `getaddrinfo(node, service, hints, out)`.
pub fn getaddrinfo(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    params: &[Val],
) -> wasmtime::Result<i32> {
    let arg = |position: usize| {
        params
            .get(position)
            .and_then(Val::i32)
            .unwrap_or(0)
            .cast_unsigned()
    };
    let (node, service, hints, out) = (arg(0), arg(1), arg(2), arg(3));
    let memory = caller.data().modules[index].memory;
    let data = memory.data(&*caller);
    let (mut flags, mut family, mut socktype, mut protocol) = (0, 0, 0, 0);
    if hints != 0 {
        flags = read_i32(data, hints);
        family = read_i32(data, hints + 4);
        socktype = read_i32(data, hints + 8);
        protocol = read_i32(data, hints + 12);
    }
    if socktype != 0 && protocol == 0 {
        protocol = if socktype == 2 { 17 } else { 6 };
    }
    if socktype == 0 && protocol != 0 {
        socktype = if protocol == 17 { 2 } else { 1 };
    }
    if protocol == 0 {
        protocol = 6;
    }
    if socktype == 0 {
        socktype = 1;
    }
    if node == 0 && service == 0 {
        return Ok(-2);
    }
    if flags & -1088 != 0 || (hints != 0 && read_i32(data, hints) & 2 != 0 && node == 0) {
        return Ok(-1);
    }
    if flags & 32 != 0 {
        return Ok(-2);
    }
    if socktype != 0 && socktype != 1 && socktype != 2 {
        return Ok(-7);
    }
    if family != 0 && family != 2 && family != 10 {
        return Ok(-6);
    }
    let mut port = 0_i64;
    if service != 0 {
        let text = c_string(data, service, None);
        match parse_int_prefix(&text) {
            Some(value) => port = value,
            None => return Ok(if flags & 1024 != 0 { -2 } else { -8 }),
        }
    }
    let node_text = (node != 0).then(|| c_string(data, node, None));
    let (family, address, canonical) = match node_text {
        None => {
            let family = if family == 0 { 2 } else { family };
            let address = if flags & 1 != 0 {
                if family == 2 {
                    Address::V4(0)
                } else {
                    Address::V6([0; 4])
                }
            } else if family == 2 {
                Address::V4(htonl(2_130_706_433))
            } else {
                Address::V6([0, 0, 0, htonl(1).cast_signed()])
            };
            (family, address, None)
        }
        Some(text) => {
            if let Some(value) = inet_pton4(&text) {
                if family == 0 || family == 2 {
                    (2, Address::V4(value), Some(text))
                } else if family == 10 && flags & 8 != 0 {
                    (
                        10,
                        Address::V6([0, 0, htonl(65535).cast_signed(), value.cast_signed()]),
                        Some(text),
                    )
                } else {
                    return Ok(-2);
                }
            } else if let Some(words) = inet_pton6(&text) {
                if family == 0 || family == 10 {
                    (10, Address::V6(words), Some(text))
                } else {
                    return Ok(-2);
                }
            } else if flags & 4 != 0 {
                return Ok(-2);
            } else {
                let mapped = caller.data_mut().modules[index].dns.lookup_name(&text);
                let value = inet_pton4(&mapped).unwrap_or(0);
                if family == 0 {
                    (2, Address::V4(value), None)
                } else if family == 10 {
                    (
                        10,
                        Address::V6([0, 0, htonl(65535).cast_signed(), value.cast_signed()]),
                        None,
                    )
                } else {
                    (family, Address::V4(value), None)
                }
            }
        }
    };
    let host = match address {
        Address::V4(value) => inet_ntop4(value),
        Address::V6(words) => inet_ntop6(words),
    };
    let size = if family == 10 { 28 } else { 16 };
    let sockaddr = runtime::call_i32(caller, index, "malloc", &[Val::I32(size)])?.cast_unsigned();
    let info = runtime::call_i32(caller, index, "malloc", &[Val::I32(32)])?.cast_unsigned();
    let memory = caller.data().modules[index].memory;
    let data = memory.data_mut(&mut *caller);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let port16 = port as u16;
    write_sockaddr(data, sockaddr, family, &host, port16, 0);
    write_i32(data, info + 4, family);
    write_i32(data, info + 8, socktype);
    write_i32(data, info + 12, protocol);
    // HEAPU32[ai+24] = canonname, a JavaScript string or null: ToNumber.
    let canonical_value = canonical.as_deref().and_then(js_number).map_or(0, to_int32);
    write_u32(data, info + 24, canonical_value.cast_unsigned());
    write_u32(data, info + 20, sockaddr);
    write_i32(data, info + 16, size);
    write_i32(data, info + 28, 0);
    write_u32(data, out, info);
    Ok(0)
}

/// `parseInt(text, 10)`: leading decimal integer after whitespace.
fn parse_int_prefix(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let numeric: String = digits.chars().take_while(char::is_ascii_digit).collect();
    if numeric.is_empty() {
        return None;
    }
    numeric.parse::<i64>().ok().map(|value| sign * value)
}

/// `getnameinfo(sa, salen, node, nodelen, serv, servlen, flags)`.
pub fn getnameinfo(caller: &mut Caller<'_, Runtime>, index: usize, params: &[Val]) -> i32 {
    let arg = |position: usize| params.get(position).and_then(Val::i32).unwrap_or(0);
    let (address, length, node, node_length, service, service_length, flags) = (
        arg(0).cast_unsigned(),
        arg(1),
        arg(2).cast_unsigned(),
        arg(3),
        arg(4).cast_unsigned(),
        arg(5),
        arg(6),
    );
    let memory = caller.data().modules[index].memory;
    let data = memory.data(&*caller);
    let family = i32::from(read_i16(data, address));
    let port = htons(u16::from_le_bytes([
        crate::runtime::read_u8(data, address + 2),
        crate::runtime::read_u8(data, address + 3),
    ]));
    let host = match family {
        2 if length == 16 => inet_ntop4(read_i32(data, address + 4).cast_unsigned()),
        10 if length == 28 => inet_ntop6([
            read_i32(data, address + 8),
            read_i32(data, address + 12),
            read_i32(data, address + 16),
            read_i32(data, address + 20),
        ]),
        _ => return -6,
    };
    let mut overflow = false;
    let mut name = host;
    if node != 0 && node_length != 0 {
        let mapped = caller.data().modules[index].dns.lookup_address(&name);
        match mapped {
            Some(mapped) if flags & 1 == 0 => name = mapped,
            _ => {
                if flags & 8 != 0 {
                    return -2;
                }
            }
        }
        let data = memory.data_mut(&mut *caller);
        let written = string_to_utf8(data, &name, node, usize::try_from(node_length).unwrap_or(0));
        if written + 1 >= usize::try_from(node_length).unwrap_or(0) {
            overflow = true;
        }
    }
    if service != 0 && service_length != 0 {
        let data = memory.data_mut(&mut *caller);
        let text = port.to_string();
        let written = string_to_utf8(
            data,
            &text,
            service,
            usize::try_from(service_length).unwrap_or(0),
        );
        if written + 1 >= usize::try_from(service_length).unwrap_or(0) {
            overflow = true;
        }
    }
    if overflow { -12 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_dns_and_ipv6_text_match_the_glue() {
        let mut dns = Dns::default();
        assert_eq!(dns.lookup_name("localhost"), "172.29.1.0");
        assert_eq!(dns.lookup_name("localhost"), "172.29.1.0");
        assert_eq!(dns.lookup_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(inet_ntop4(htonl(2_130_706_433)), "127.0.0.1");
        let loopback = inet_pton6("::1").expect("ipv6");
        assert_eq!(inet_ntop6(loopback), "::1");
        let address = inet_pton6("fe80::1:2").expect("ipv6");
        assert_eq!(inet_ntop6(address), "fe80::1:2");
    }
}
