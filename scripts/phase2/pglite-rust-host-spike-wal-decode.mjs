// Names the struct field of every WAL byte that differs between the Node
// host and the Rust host but never between two Node runs. Reads the
// outsideControlRecords that pglite-rust-host-spike-control.mjs wrote (main
// data of each affected record, as hex, from each directory) and lays out
// the PostgreSQL 18 records involved:
//   rm 1 (Transaction) commit with XLOG_XACT_HAS_INFO: xl_xact_commit,
//     xl_xact_xinfo and the optional parts of XactLogCommitRecord;
//   rm 8 (Standby) XLOG_INVALIDATIONS: xl_invalidations;
//   rm 8 (Standby) XLOG_RUNNING_XACTS: xl_running_xacts;
//   rm 10 (Heap) XLOG_HEAP_INPLACE: xl_heap_inplace.
// SharedInvalidationMessage is a 16-byte union. Message kinds other than
// smgr fill 8 or 12 bytes after a 1-byte id and 3 padding bytes, so the
// rest of the union is never written by inval.c.
import { readFileSync } from "node:fs";

const [controlFields] = process.argv.slice(2);
if (!controlFields) throw new Error("usage: pglite-rust-host-spike-wal-decode.mjs <control-fields.json>");
const { walFields } = JSON.parse(readFileSync(controlFields, "utf8"));

const kindOfRecord = new Map();
for (const entry of walFields.outsideControlBytes) {
  if (entry.class.startsWith("main.")) kindOfRecord.set(entry.record, entry.class.slice("main.".length));
}

function invalMessages(fields, buffer, start, count) {
  for (let message = 0; message < count; message += 1) {
    const base = start + message * 16;
    const id = buffer.readInt8(base);
    const kind =
      id >= 0
        ? "catcache"
        : { [-1]: "catalog", [-2]: "relcache", [-3]: "smgr", [-4]: "relmap", [-5]: "snapshot", [-6]: "relsync" }[id] ??
          "unknown";
    const layout =
      kind === "smgr"
        ? [["id", 1], ["backend", 3], ["rlocator", 12]]
        : kind === "relmap"
          ? [["id", 1], ["padding", 3], ["dbId", 4], ["unused", 8]]
          : [["id", 1], ["padding", 3], ["dbId", 4], [kind === "catcache" ? "hashValue" : "objectId", 4], ["unused", 4]];
    let offset = base;
    for (const [name, size] of layout) {
      fields.push([offset, size, `inval.${kind}.${name}`]);
      offset += size;
    }
  }
}

function layout(kind, buffer) {
  const fields = [];
  if (kind === "xact") {
    fields.push([0, 8, "xact_time"], [8, 4, "xinfo"]);
    const xinfo = buffer.readUInt32LE(8);
    let offset = 12;
    if (xinfo & 0x01) {
      fields.push([offset, 8, "dbinfo"]);
      offset += 8;
    }
    if (xinfo & 0x02) {
      const count = buffer.readInt32LE(offset);
      fields.push([offset, 4 + count * 4, "subxacts"]);
      offset += 4 + count * 4;
    }
    if (xinfo & 0x04) {
      const count = buffer.readInt32LE(offset);
      fields.push([offset, 4 + count * 12, "relfilelocators"]);
      offset += 4 + count * 12;
    }
    if (xinfo & 0x100) {
      const count = buffer.readInt32LE(offset);
      fields.push([offset, 4 + count * 16, "dropped_stats"]);
      offset += 4 + count * 16;
    }
    if (xinfo & 0x08) {
      const count = buffer.readInt32LE(offset);
      fields.push([offset, 4, "inval.nmsgs"]);
      invalMessages(fields, buffer, offset + 4, count);
      offset += 4 + count * 16;
    }
    if (offset < buffer.length) fields.push([offset, buffer.length - offset, "xact.rest"]);
  } else if (kind === "standby.invalidations") {
    fields.push([0, 4, "dbId"], [4, 4, "tsId"], [8, 1, "relcacheInitFileInval"], [9, 3, "padding"], [12, 4, "inval.nmsgs"]);
    invalMessages(fields, buffer, 16, buffer.readInt32LE(12));
  } else if (kind === "standby.running_xacts") {
    fields.push(
      [0, 4, "xcnt"],
      [4, 4, "subxcnt"],
      [8, 1, "subxid_overflow"],
      [9, 3, "padding"],
      [12, 4, "nextXid"],
      [16, 4, "oldestRunningXid"],
      [20, 4, "latestCompletedXid"],
      [24, buffer.length - 24, "xids"],
    );
  } else if (kind === "heap.inplace") {
    fields.push(
      [0, 2, "offnum"],
      [2, 2, "padding"],
      [4, 4, "dbId"],
      [8, 4, "tsId"],
      [12, 1, "relcacheInitFileInval"],
      [13, 3, "padding"],
      [16, 4, "inval.nmsgs"],
    );
    invalMessages(fields, buffer, 20, buffer.readInt32LE(16));
  }
  return fields;
}

const recordKind = (record, info) => {
  if (info === "xact.time" || info.startsWith("rm1.")) return "xact";
  if (info === "rm8.info20") return "standby.invalidations";
  if (info === "rm8.info10") return "standby.running_xacts";
  if (info === "rm10.info70") return "heap.inplace";
  return `unhandled ${record} ${info}`;
};

const counts = {};
const records = {};
// A layout is trusted only when its fields tile the main data exactly and
// every invalidation message has a known id.
const layoutProblems = [];
for (const entry of walFields.outsideControlRecords) {
  const info = kindOfRecord.get(entry.record) ?? entry.class;
  const kind = recordKind(entry.record, info);
  records[kind] = (records[kind] ?? 0) + 1;
  const node = Buffer.from(entry.node, "hex");
  const rust = Buffer.from(entry.rust, "hex");
  const fields = layout(kind, node);
  const end = fields.reduce((last, [start, size]) => (start === last ? start + size : Number.NaN), 0);
  if (end !== node.length || fields.some(([, , name]) => name.includes("unknown") || name === "xact.rest")) {
    layoutProblems.push({ record: entry.record, kind, end, length: node.length });
  }
  const fieldOf = (index) => fields.find(([start, size]) => index >= start && index < start + size)?.[2] ?? "unmapped";
  for (let index = 0; index < node.length; index += 1) {
    if (node[index] === rust[index]) continue;
    const name = `${kind}: ${fieldOf(index)}`;
    counts[name] = (counts[name] ?? 0) + 1;
  }
}
const written = Object.keys(counts).filter(
  (name) => !/: (xact_time|padding|inval\.[a-z]+\.(padding|unused))$/.test(name),
);
process.stdout.write(
  `${JSON.stringify(
    {
      records,
      layoutProblems,
      differingBytesByField: counts,
      fieldsOtherThanTimePaddingOrUnused: written,
    },
    null,
    2,
  )}\n`,
);
