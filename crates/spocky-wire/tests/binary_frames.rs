use spocky_wire::{
    BinaryFrame, FileBeginMetadata, FileTransferFrame, FileTransferFrameInput, FileTransferOpcode,
    TerminalCell, TerminalCursor, TerminalFrame, TerminalOpcode, TerminalResize,
    TerminalResizeIntent, TerminalState, decode_binary_frame, decode_file_transfer_frame,
    decode_terminal_frame, decode_terminal_resize, decode_terminal_snapshot,
    encode_file_transfer_frame, encode_terminal_frame, encode_terminal_resize,
    encode_terminal_snapshot,
};

#[test]
fn terminal_frames_match_the_pinned_byte_layout() {
    let encoded = encode_terminal_frame(TerminalOpcode::Output, 7, b"hello");
    assert_eq!(encoded, b"\x01\x07hello");
    assert_eq!(
        decode_terminal_frame(&encoded),
        Some(TerminalFrame {
            opcode: TerminalOpcode::Output,
            slot: 7,
            payload: b"hello".to_vec(),
        })
    );
    assert_eq!(
        decode_binary_frame(&encoded),
        Some(BinaryFrame::Terminal(TerminalFrame {
            opcode: TerminalOpcode::Output,
            slot: 7,
            payload: b"hello".to_vec(),
        }))
    );
}

#[test]
fn terminal_decoders_reject_truncated_unknown_and_malformed_payloads() {
    assert_eq!(decode_terminal_frame(&[TerminalOpcode::Output as u8]), None);
    assert_eq!(decode_terminal_frame(&[0xff, 1, 2]), None);
    assert_eq!(decode_binary_frame(&[0xff, 0]), None);
    assert_eq!(decode_terminal_resize(b"{"), None);
    assert_eq!(decode_terminal_resize(br#"{"rows":"24","cols":80}"#), None);
    assert_eq!(decode_terminal_resize(br#"{"rows":0,"cols":80}"#), None);
    assert_eq!(decode_terminal_snapshot(b"{"), None);
}

#[test]
fn terminal_json_payloads_round_trip_and_strip_unknown_fields() {
    let resize = TerminalResize {
        rows: 24.0,
        cols: 80.0,
        intent: Some(TerminalResizeIntent::Claim),
    };
    assert_eq!(
        encode_terminal_resize(&resize).unwrap(),
        br#"{"rows":24,"cols":80,"intent":"claim"}"#
    );
    assert_eq!(
        decode_terminal_resize(br#"{"rows":24,"cols":80,"extra":true}"#),
        Some(TerminalResize {
            rows: 24.0,
            cols: 80.0,
            intent: None
        })
    );

    let state = TerminalState {
        rows: 1.0,
        cols: 2.0,
        grid: vec![vec![TerminalCell::new("A"), TerminalCell::new("B")]],
        scrollback: vec![],
        cursor: TerminalCursor {
            row: 0.0,
            col: 2.0,
            hidden: None,
            style: None,
            blink: None,
        },
        title: None,
        grid_wrapped: None,
        scrollback_wrapped: None,
    };
    let bytes = encode_terminal_snapshot(&state).unwrap();
    assert_eq!(
        bytes,
        br#"{"rows":1,"cols":2,"grid":[[{"char":"A"},{"char":"B"}]],"scrollback":[],"cursor":{"row":0,"col":2}}"#
    );
    assert_eq!(decode_terminal_snapshot(&bytes), Some(state));
    assert_eq!(
        decode_terminal_snapshot(br#"{"rows":1,"cols":1,"grid":[[{"char":"A","extra":true}]],"scrollback":[],"cursor":{"row":0,"col":1},"extra":true}"#).unwrap().grid[0][0],
        TerminalCell::new("A")
    );
}

#[test]
fn file_frames_match_the_pinned_byte_layout() {
    let metadata = FileBeginMetadata {
        mime: "image/png".into(),
        size: 6.0,
        encoding: spocky_wire::FileEncoding::Binary,
        modified_at: "2026-05-02T00:00:00.000Z".into(),
        revision: None,
        file_name: None,
    };
    let begin = encode_file_transfer_frame(FileTransferFrameInput::Begin {
        request_id: "req-1",
        metadata: &metadata,
    })
    .unwrap();
    assert_eq!(begin[0], FileTransferOpcode::FileBegin as u8);
    assert_eq!(begin[1], 5);
    assert_eq!(&begin[2..7], b"req-1");
    let metadata_len = u16::from_be_bytes([begin[7], begin[8]]) as usize;
    assert_eq!(metadata_len, begin.len() - 9);
    assert_eq!(
        &begin[9..],
        br#"{"mime":"image/png","size":6,"encoding":"binary","modifiedAt":"2026-05-02T00:00:00.000Z"}"#
    );
    assert_eq!(
        decode_file_transfer_frame(&begin),
        Some(FileTransferFrame::Begin {
            request_id: "req-1".into(),
            metadata,
            payload: vec![],
        })
    );

    let chunk = encode_file_transfer_frame(FileTransferFrameInput::Chunk {
        request_id: "req-1",
        payload: &[0, 1, 2, 253, 254, 255],
    })
    .unwrap();
    assert_eq!(
        chunk,
        [b"\x11\x05req-1".as_slice(), &[0, 1, 2, 253, 254, 255]].concat()
    );
    assert_eq!(
        decode_binary_frame(&chunk),
        Some(BinaryFrame::FileTransfer(FileTransferFrame::Chunk {
            request_id: "req-1".into(),
            payload: vec![0, 1, 2, 253, 254, 255],
        }))
    );

    let end = encode_file_transfer_frame(FileTransferFrameInput::End {
        request_id: "req-1",
    })
    .unwrap();
    assert_eq!(end, b"\x12\x05req-1");
}

#[test]
fn file_frame_limits_and_malformed_behavior_match_the_baseline() {
    let empty =
        encode_file_transfer_frame(FileTransferFrameInput::End { request_id: "" }).unwrap_err();
    assert_eq!(empty.to_string(), "File transfer requestId is required");

    let long_id = "x".repeat(256);
    let too_long = encode_file_transfer_frame(FileTransferFrameInput::End {
        request_id: &long_id,
    })
    .unwrap_err();
    assert_eq!(too_long.to_string(), "File transfer requestId is too long");

    let oversized_metadata = FileBeginMetadata {
        mime: "image/png".into(),
        size: 1.0,
        encoding: spocky_wire::FileEncoding::Binary,
        modified_at: "x".repeat(65_536),
        revision: None,
        file_name: None,
    };
    let too_long = encode_file_transfer_frame(FileTransferFrameInput::Begin {
        request_id: "req-1",
        metadata: &oversized_metadata,
    })
    .unwrap_err();
    assert_eq!(too_long.to_string(), "FileBegin metadata is too long");

    assert_eq!(
        decode_file_transfer_frame(&[FileTransferOpcode::FileEnd as u8, 0]),
        None
    );
    assert_eq!(
        decode_file_transfer_frame(&[FileTransferOpcode::FileEnd as u8, 6, 1]),
        None
    );
    assert_eq!(decode_file_transfer_frame(b"\x12\x05req-1\x01"), None);
    assert_eq!(decode_file_transfer_frame(b"\x10\x05req-1\x00\x02{}"), None);
    assert_eq!(decode_file_transfer_frame(b"\x10\x05req-1\x00\x01{"), None);
}
