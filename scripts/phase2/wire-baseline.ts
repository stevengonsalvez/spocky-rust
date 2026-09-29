import { writeFileSync } from "node:fs";

import {
  FileTransferOpcode,
  TerminalStreamOpcode,
  decodeBinaryFrame,
  decodeFileTransferFrame,
  decodeTerminalResizePayload,
  decodeTerminalStreamFrame,
  encodeFileTransferFrame,
  encodeTerminalResizePayload,
  encodeTerminalStreamFrame,
} from "./protocol/src/binary-frames/index.ts";

const outputPath = process.argv[2];
if (!outputPath) throw new Error("output path is required");
const baseline = process.env.PASEO_CAPTURE_BASELINE;
if (!baseline) throw new Error("PASEO_CAPTURE_BASELINE is required");

const terminal = encodeTerminalStreamFrame({
  opcode: TerminalStreamOpcode.Output,
  slot: 7,
  payload: "hello",
});
const resize = encodeTerminalResizePayload({ rows: 24, cols: 80, intent: "claim" });
const begin = encodeFileTransferFrame({
  opcode: FileTransferOpcode.FileBegin,
  requestId: "req-1",
  metadata: {
    mime: "image/png",
    size: 6,
    encoding: "binary",
    modifiedAt: "2026-05-02T00:00:00.000Z",
  },
});
const chunk = encodeFileTransferFrame({
  opcode: FileTransferOpcode.FileChunk,
  requestId: "req-1",
  payload: new Uint8Array([0, 1, 2, 253, 254, 255]),
});
const end = encodeFileTransferFrame({
  opcode: FileTransferOpcode.FileEnd,
  requestId: "req-1",
});

const result = {
  baseline,
  terminal: Array.from(terminal),
  terminalDecoded: decodeTerminalStreamFrame(terminal),
  resize: Array.from(resize),
  resizeDecoded: decodeTerminalResizePayload(resize),
  fileBegin: Array.from(begin),
  fileChunk: Array.from(chunk),
  fileEnd: Array.from(end),
  fileBeginDecoded: decodeFileTransferFrame(begin),
  demuxKind: decodeBinaryFrame(chunk)?.kind ?? null,
  malformed: {
    truncatedTerminal: decodeTerminalStreamFrame(
      new Uint8Array([TerminalStreamOpcode.Output]),
    ),
    unknownOpcode: decodeBinaryFrame(new Uint8Array([0xff, 0])),
    emptyRequestId: decodeFileTransferFrame(
      new Uint8Array([FileTransferOpcode.FileEnd, 0]),
    ),
  },
};

writeFileSync(outputPath, `${JSON.stringify(result, null, 2)}\n`);
