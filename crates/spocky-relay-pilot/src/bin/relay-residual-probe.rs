use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(30);
const MAXIMUM_MESSAGE_PAYLOAD_BYTES: usize = 32 * 1024 * 1024 - 14;
const VALID_KEY: &str = "CQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const INVALID_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn main() {
    let address = std::env::args()
        .nth(1)
        .expect("usage: relay-residual-probe HOST:PORT")
        .parse::<SocketAddr>()
        .expect("probe address must be HOST:PORT");

    handshake_case(
        address,
        "escaped-valid",
        &format!(
            r#"{{"t\u0079pe":"e2ee_\u0068ello","k\u0065y":"{}"}}"#,
            escape_first_ascii(VALID_KEY)
        ),
        false,
    );
    handshake_case(
        address,
        "escaped-type-invalid",
        &format!(r#"{{"type":"h\u0065llo","key":"{INVALID_KEY}"}}"#),
        true,
    );
    handshake_case(
        address,
        "escaped-field-invalid",
        &format!(r#"{{"t\u0079pe":"hello","k\u0065y":"{INVALID_KEY}"}}"#),
        true,
    );
    handshake_case(
        address,
        "escaped-value-invalid",
        r#"{"type":"hello","key":"\u0041AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#,
        true,
    );

    fragment_case(
        address,
        "fragment-below-limit",
        MAXIMUM_MESSAGE_PAYLOAD_BYTES - 1,
        false,
    );
    fragment_case(
        address,
        "fragment-at-limit",
        MAXIMUM_MESSAGE_PAYLOAD_BYTES,
        false,
    );
    fragment_case(
        address,
        "fragment-above-limit",
        MAXIMUM_MESSAGE_PAYLOAD_BYTES + 1,
        true,
    );
}

fn handshake_case(address: SocketAddr, name: &str, payload: &str, rejected: bool) {
    let mut destination = connect(address, &format!("{name}-route"), "server");
    let mut source = connect(address, &format!("{name}-route"), "client");
    send_masked_frame(&mut source, 0x1, true, payload.as_bytes());
    if rejected {
        print_close(name, &read_frame(&mut source));
    } else {
        let frame = read_frame(&mut destination);
        assert_eq!(frame.opcode, 0x1, "{name}: expected forwarded text");
        assert_eq!(frame.payload, payload.as_bytes(), "{name}: changed payload");
        println!("{name}\tforward\t{payload}");
    }
}

fn escape_first_ascii(value: &str) -> String {
    let first = value.as_bytes()[0];
    format!(r"\u{first:04x}{}", &value[1..])
}

fn fragment_case(address: SocketAddr, name: &str, length: usize, rejected: bool) {
    let mut destination = connect(address, &format!("{name}-route"), "server");
    let mut source = connect(address, &format!("{name}-route"), "client");
    let first_length = length / 2;
    let second_length = length - first_length;
    send_masked_frame(&mut source, 0x2, false, &vec![0x3c; first_length]);
    send_masked_frame(&mut source, 0x0, true, &vec![0x3c; second_length]);
    if rejected {
        print_close(name, &read_frame(&mut source));
    } else {
        let frame = read_frame(&mut destination);
        assert_eq!(frame.opcode, 0x2, "{name}: expected forwarded binary");
        assert_eq!(frame.payload.len(), length, "{name}: changed length");
        assert!(
            frame.payload.iter().all(|byte| *byte == 0x3c),
            "{name}: changed payload"
        );
        println!("{name}\tforward\t{length}");
    }
}

fn print_close(name: &str, frame: &Frame) {
    assert_eq!(frame.opcode, 0x8, "{name}: expected close frame");
    assert!(frame.payload.len() >= 2, "{name}: close code missing");
    let code = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
    let reason = std::str::from_utf8(&frame.payload[2..]).expect("close reason must be UTF-8");
    if reason.is_empty() {
        println!("{name}\tclose\t{code}");
    } else {
        println!("{name}\tclose\t{code}\t{reason}");
    }
}

fn connect(address: SocketAddr, session: &str, role: &str) -> TcpStream {
    let mut stream = TcpStream::connect(address).expect("connect relay");
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    stream.set_write_timeout(Some(DEADLINE)).unwrap();
    write!(
        stream,
        concat!(
            "GET /ws?serverId={}&role={}&v=1 HTTP/1.1\r\n",
            "Host: {}\r\n",
            "Upgrade: websocket\r\n",
            "Connection: Upgrade\r\n",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
            "Sec-WebSocket-Version: 13\r\n\r\n"
        ),
        session, role, address
    )
    .unwrap();

    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("read upgrade response");
        response.push(byte[0]);
    }
    assert!(
        response.starts_with(b"HTTP/1.1 101 "),
        "upgrade failed: {}",
        String::from_utf8_lossy(&response)
    );
    stream
}

fn send_masked_frame(stream: &mut TcpStream, opcode: u8, finished: bool, payload: &[u8]) {
    let first = if finished { 0x80 } else { 0x00 } | opcode;
    stream.write_all(&[first]).unwrap();
    match payload.len() {
        length @ 0..=125 => stream
            .write_all(&[0x80 | u8::try_from(length).unwrap()])
            .unwrap(),
        length @ 126..=65_535 => {
            stream.write_all(&[0x80 | 0x7e]).unwrap();
            stream
                .write_all(&u16::try_from(length).unwrap().to_be_bytes())
                .unwrap();
        }
        length => {
            stream.write_all(&[0x80 | 127]).unwrap();
            stream
                .write_all(&u64::try_from(length).unwrap().to_be_bytes())
                .unwrap();
        }
    }
    stream.write_all(&[0_u8; 4]).unwrap();
    stream.write_all(payload).unwrap();
}

struct Frame {
    opcode: u8,
    payload: Vec<u8>,
}

fn read_frame(stream: &mut TcpStream) -> Frame {
    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).expect("read frame header");
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let mut length = u64::from(header[1] & 0x7f);
    if length == 126 {
        let mut extended = [0_u8; 2];
        stream.read_exact(&mut extended).unwrap();
        length = u64::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0_u8; 8];
        stream.read_exact(&mut extended).unwrap();
        length = u64::from_be_bytes(extended);
    }
    let mut mask = [0_u8; 4];
    if masked {
        stream.read_exact(&mut mask).unwrap();
    }
    let mut payload = vec![0_u8; usize::try_from(length).expect("frame length fits usize")];
    stream.read_exact(&mut payload).expect("read frame payload");
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    Frame { opcode, payload }
}
