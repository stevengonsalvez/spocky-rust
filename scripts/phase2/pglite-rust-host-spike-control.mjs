// Like-for-like control for the PGlite Rust host spike. Compares two fresh
// Node-host data directories with each other and with the Rust-host
// snapshot: byte ranges that differ in global/pg_control and the first WAL
// segment, and the control-file fields decoded by the original host through
// pg_control_system(), pg_control_checkpoint() and pg_control_init() on
// copies of each directory.
import { cpSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [packageRoot, nodeData, node2Data, rustSnapshot, workDirectory] = process.argv.slice(2);
if (!workDirectory) {
  throw new Error(
    "usage: pglite-rust-host-spike-control.mjs <package> <node data> <node2 data> <rust snapshot> <work dir>",
  );
}

const files = ["global/pg_control", "pg_wal/000000010000000000000001"];
const directories = { node: nodeData, node2: node2Data, rust: rustSnapshot };

function differingOffsets(left, right) {
  const offsets = [];
  const length = Math.max(left.length, right.length);
  for (let index = 0; index < length; index += 1) {
    if (left[index] !== right[index]) offsets.push(index);
  }
  return offsets;
}

function ranges(offsets) {
  const output = [];
  for (const offset of offsets) {
    const last = output[output.length - 1];
    if (last && last[1] === offset - 1) last[1] = offset;
    else output.push([offset, offset]);
  }
  return output.map(([start, end]) => (start === end ? `${start}` : `${start}-${end}`));
}

const bytes = {};
for (const [label, directory] of Object.entries(directories)) {
  bytes[label] = Object.fromEntries(files.map((file) => [file, readFileSync(join(directory, file))]));
}

const byteDiff = {};
for (const file of files) {
  const control = differingOffsets(bytes.node[file], bytes.node2[file]);
  const candidate = differingOffsets(bytes.node[file], bytes.rust[file]);
  const controlSet = new Set(control);
  const outside = candidate.filter((offset) => !controlSet.has(offset));
  byteDiff[file] = {
    length: { node: bytes.node[file].length, node2: bytes.node2[file].length, rust: bytes.rust[file].length },
    nodeVsNode: { count: control.length, ranges: ranges(control) },
    nodeVsRust: { count: candidate.length, ranges: ranges(candidate) },
    nodeVsRustOutsideControl: { count: outside.length, ranges: ranges(outside) },
  };
}


// WAL classification. Walks the records of the first segment and names the
// field each byte belongs to, so differing bytes can be compared by field.
const PAGE = 8192;
function walClasses(buffer) {
  const classes = new Array(buffer.length).fill("unused");
  const mainIndex = new Array(buffer.length).fill(null);
  const mainOffsets = new Map();
  const logical = [];
  for (let page = 0; page < buffer.length; page += PAGE) {
    const info = buffer.readUInt16LE(page + 2);
    const longHeader = (info & 0x0002) !== 0;
    const header = longHeader ? 40 : 24;
    for (let offset = page; offset < page + header; offset += 1) {
      const within = offset - page;
      classes[offset] =
        longHeader && within >= 24 && within < 32 ? "page.sysid" : within < 20 ? "page.header" : "page.padding";
    }
    for (let offset = page + header; offset < page + PAGE; offset += 1) logical.push(offset);
  }
  let position = 0;
  const read = (count) => {
    const physical = logical.slice(position, position + count);
    position += count;
    return physical;
  };
  const u32 = (physical) =>
    physical.reduce((value, offset, index) => value + buffer[offset] * 2 ** (8 * index), 0);
  while (position + 24 <= logical.length) {
    position = Math.ceil(position / 8) * 8;
    const headerBytes = logical.slice(position, position + 24);
    const total = u32(headerBytes.slice(0, 4));
    if (total < 24 || position + total > logical.length) break;
    const rmid = buffer[headerBytes[17]];
    const xlInfo = buffer[headerBytes[16]];
    const record = read(total);
    record.slice(0, 20).forEach((offset) => (classes[offset] = "record.header"));
    record.slice(20, 24).forEach((offset) => (classes[offset] = "record.crc"));
    let cursor = 24;
    const blocks = [];
    let mainLength = 0;
    while (cursor < total) {
      const id = buffer[record[cursor]];
      if (id === 255) {
        mainLength = buffer[record[cursor + 1]];
        record.slice(cursor, cursor + 2).forEach((offset) => (classes[offset] = "record.blockheader"));
        cursor += 2;
        break;
      }
      if (id === 254) {
        mainLength = u32(record.slice(cursor + 1, cursor + 5));
        record.slice(cursor, cursor + 5).forEach((offset) => (classes[offset] = "record.blockheader"));
        cursor += 5;
        break;
      }
      if (id === 253 || id === 252) {
        const size = id === 253 ? 3 : 5;
        record.slice(cursor, cursor + size).forEach((offset) => (classes[offset] = "record.blockheader"));
        cursor += size;
        continue;
      }
      const start = cursor;
      const forkFlags = buffer[record[cursor + 1]];
      const dataLength = buffer[record[cursor + 2]] + buffer[record[cursor + 3]] * 256;
      cursor += 4;
      let imageLength = 0;
      if (forkFlags & 0x10) {
        imageLength = buffer[record[cursor]] + buffer[record[cursor + 1]] * 256;
        const bimgInfo = buffer[record[cursor + 4]];
        cursor += 5;
        if (bimgInfo & 0x01 && bimgInfo & 0x0e) cursor += 2;
      }
      if (!(forkFlags & 0x80)) cursor += 12;
      cursor += 4;
      record.slice(start, cursor).forEach((offset) => (classes[offset] = "record.blockheader"));
      blocks.push({ imageLength, dataLength });
    }
    for (const block of blocks) {
      record.slice(cursor, cursor + block.imageLength).forEach((offset) => (classes[offset] = "record.image"));
      cursor += block.imageLength;
      record.slice(cursor, cursor + block.dataLength).forEach((offset) => (classes[offset] = "record.blockdata"));
      cursor += block.dataLength;
    }
    const main = record.slice(cursor, cursor + mainLength);
    mainOffsets.set(record[0], main);
    const kind = `rm${rmid}.info${(xlInfo & 0xf0).toString(16)}`;
    main.forEach((offset, index) => {
      mainIndex[offset] = { record: record[0], index, length: mainLength };
      if (rmid === 1 && index < 8) classes[offset] = "xact.time";
      else if (rmid === 0 && ((xlInfo & 0xf0) === 0x00 || (xlInfo & 0xf0) === 0x10) && index >= 64 && index < 72)
        classes[offset] = "checkpoint.time";
      else classes[offset] = `main.${kind}`;
    });
  }
  return { classes, mainIndex, mainOffsets };
}

function classify(left, right, classes) {
  const counts = {};
  for (const offset of differingOffsets(left, right)) {
    counts[classes[offset]] = (counts[classes[offset]] ?? 0) + 1;
  }
  return counts;
}

const walFile = "pg_wal/000000010000000000000001";
const walLayout = walClasses(bytes.node[walFile]);
const walClassesNode = walLayout.classes;
const walFields = {
  nodeVsNode: classify(bytes.node[walFile], bytes.node2[walFile], walClassesNode),
  nodeVsRust: classify(bytes.node[walFile], bytes.rust[walFile], walClassesNode),
};
walFields.nodeVsRustClassesOutsideControl = Object.keys(walFields.nodeVsRust).filter(
  (name) => !(name in walFields.nodeVsNode),
);
// Every differing byte of a class outside the control, with its record, its
// index in the record's main data and the byte in each directory, so the
// struct field it belongs to can be named.
const outsideClasses = new Set(walFields.nodeVsRustClassesOutsideControl);
walFields.outsideControlBytes = differingOffsets(bytes.node[walFile], bytes.rust[walFile])
  .filter((offset) => outsideClasses.has(walClassesNode[offset]))
  .map((offset) => ({
    offset,
    class: walClassesNode[offset],
    ...walLayout.mainIndex[offset],
    node: bytes.node[walFile][offset],
    node2: bytes.node2[walFile][offset],
    rust: bytes.rust[walFile][offset],
  }));
const hexAt = (buffer, offsets) => offsets.map((offset) => buffer[offset].toString(16).padStart(2, "0")).join("");
walFields.outsideControlRecords = [...new Set(walFields.outsideControlBytes.map((entry) => entry.record))].map(
  (record) => {
    const offsets = walLayout.mainOffsets.get(record);
    return {
      record,
      class: walClassesNode[offsets[0]],
      node: hexAt(bytes.node[walFile], offsets),
      node2: hexAt(bytes.node2[walFile], offsets),
      rust: hexAt(bytes.rust[walFile], offsets),
    };
  },
);

const { PGlite } = await import(pathToFileURL(join(packageRoot, "dist/index.js")).href);
const decoded = {};
for (const [label, directory] of Object.entries(directories)) {
  const copy = join(workDirectory, `decode-${label}`);
  cpSync(directory, copy, { recursive: true });
  const client = new PGlite(copy);
  await client.waitReady;
  const row = async (sql) => (await client.query(sql)).rows[0];
  decoded[label] = {
    system: await row("select * from pg_control_system()"),
    checkpoint: await row("select * from pg_control_checkpoint()"),
    init: await row("select * from pg_control_init()"),
  };
  await client.close();
}

const flatten = (value) =>
  Object.fromEntries(
    Object.entries(value).flatMap(([group, fields]) =>
      Object.entries(fields).map(([name, field]) => [
        `${group}.${name}`,
        field instanceof Date ? field.toISOString() : String(field),
      ]),
    ),
  );
const flat = Object.fromEntries(Object.entries(decoded).map(([label, value]) => [label, flatten(value)]));
const fieldDiff = (left, right) =>
  Object.keys(flat[left]).filter((name) => flat[left][name] !== flat[right][name]);
const nodeVsNode = fieldDiff("node", "node2");
const nodeVsRust = fieldDiff("node", "rust");

// pg_control layout check. Reads the fields at their PostgreSQL 18
// ControlFileData offsets and checks them against the values the original
// host decoded, and checks the CRC-32C stored after the struct, so differing
// bytes can be named by field.
const CRC_OFFSET = 292;
const crcTable = Array.from({ length: 256 }, (_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) value = value & 1 ? (value >>> 1) ^ 0x82f63b78 : value >>> 1;
  return value >>> 0;
});
const crc32c = (buffer) => {
  let crc = 0xffffffff;
  for (const byte of buffer) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
};
const controlFields = [
  ["system_identifier", 0, 8],
  ["time", 24, 8],
  ["checkPointCopy.time", 104, 8],
  ["mock_authentication_nonce", 257, 32],
  ["crc", CRC_OFFSET, 4],
];
const controlField = (offset) =>
  controlFields.find(([, start, size]) => offset >= start && offset < start + size)?.[0] ?? "other";
const pgControl = {};
for (const label of Object.keys(directories)) {
  const buffer = bytes[label]["global/pg_control"];
  pgControl[label] = {
    systemIdentifierMatchesDecoded:
      buffer.readBigUInt64LE(0).toString() === flat[label]["system.system_identifier"],
    checkpointTimeMatchesDecoded:
      Number(buffer.readBigInt64LE(104)) * 1000 === Date.parse(flat[label]["checkpoint.checkpoint_time"]),
    crcMatches: crc32c(buffer.subarray(0, CRC_OFFSET)) === buffer.readUInt32LE(CRC_OFFSET),
  };
}
const fieldsOf = (left, right) => {
  const counts = {};
  for (const offset of differingOffsets(bytes[left]["global/pg_control"], bytes[right]["global/pg_control"])) {
    const name = controlField(offset);
    counts[name] = (counts[name] ?? 0) + 1;
  }
  return counts;
};
pgControl.differingFields = { nodeVsNode: fieldsOf("node", "node2"), nodeVsRust: fieldsOf("node", "rust") };

process.stdout.write(
  `${JSON.stringify(
    {
      byteDiff,
      walFields,
      pgControl,
      decodedFields: flat,
      fieldDiff: {
        nodeVsNode,
        nodeVsRust,
        nodeVsRustOutsideControl: nodeVsRust.filter((name) => !nodeVsNode.includes(name)),
      },
      note: "system.pg_control_last_modified is rewritten when a copy opens; other fields come from the file",
    },
    null,
    2,
  )}\n`,
);
