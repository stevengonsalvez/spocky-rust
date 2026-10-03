#!/usr/bin/env node
// Generates crates/spocky-contracts/src/url_tables.rs: the data tables of
// ada 2.9.2, the WHATWG URL parser inside node v22.20.0. They are read from
// ada's own sources: the IDNA mapping table (Unicode 15.0), NFC
// decomposition/composition and combining-class tables, punycode-free Bidi
// classes, joining types, and the percent-encode sets. The Rust port in
// spocky_contracts::url::idna runs ada's algorithms over them.
//
// Usage (Node 22.20.0, the binary pinned by the slice harness):
//   ~/.nvm/versions/node/v22.20.0/bin/node scripts/phase3/contracts-url-tables.mjs \
//     --ada <ada-2.9.2 source tree> [--check]
//
// The source tree is the v2.9.2 tag of github.com/ada-url/ada (node 22.20.0
// reports ada 2.9.2); src/ada_idna.cpp and include/ada/character_sets-inl.h
// must have the SHA-256 pinned below. --check regenerates in memory and exits 1
// if the committed file differs.

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const NODE_VERSION = "v22.20.0";
const ADA_IDNA_SHA256 = "1b9af936cdb63fb295c61e9444fee89e0b565205f8002fb40425fcaae5d544b7";
const ADA_SETS_SHA256 = "2ba8b03b9a711595989c1953f816900c9bfe74331687895bba2c21b1b6c3ea28";
const here = dirname(fileURLToPath(import.meta.url));
const outputPath = join(resolve(here, "../.."), "crates/spocky-contracts/src/url_tables.rs");

function fail(message) {
  process.stderr.write(`contracts-url-tables: ${message}\n`);
  process.exit(2);
}

const args = process.argv.slice(2);
let adaDir = null;
let check = false;
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === "--ada") adaDir = args[++index];
  else if (args[index] === "--check") check = true;
  else fail(`unknown argument ${args[index]}`);
}
if (!adaDir) fail("--ada <ada-2.9.2 source tree> is required");
if (process.version !== NODE_VERSION) fail(`expected node ${NODE_VERSION}, running ${process.version}`);

function read(path, digest) {
  const bytes = readFileSync(resolve(adaDir, path));
  const actual = createHash("sha256").update(bytes).digest("hex");
  if (actual !== digest) fail(`${path} has SHA-256 ${actual}, not ${digest}`);
  return bytes.toString("utf8");
}
const idna = read("src/ada_idna.cpp", ADA_IDNA_SHA256);
const sets = read("include/ada/character_sets-inl.h", ADA_SETS_SHA256);

// The text between `name[...] = {` and its closing `};`.
function body(source, name) {
  const match = new RegExp(`\\b${name}\\s*(\\[[^\\]]*\\]\\s*)+=\\s*\\{`).exec(source);
  if (!match) fail(`table ${name} not found`);
  let depth = 1;
  let index = match.index + match[0].length;
  const start = index;
  while (depth > 0) {
    if (source[index] === "{") depth += 1;
    else if (source[index] === "}") depth -= 1;
    index += 1;
    if (index > source.length) fail(`table ${name} is not closed`);
  }
  return source.slice(start, index - 1);
}
// \`pad\`: C++ zero-fills the rows an initializer leaves out.
function numbers(source, name, expected, pad = false) {
  const values = body(source, name)
    .replace(/\/\/[^\n]*/g, "")
    .match(/0x[0-9a-fA-F]+|\d+/g)
    .map((token) => Number(token));
  if (pad && expected !== undefined && values.length < expected) {
    while (values.length < expected) values.push(0);
  }
  if (expected !== undefined && values.length !== expected) {
    fail(`table ${name} has ${values.length} values, expected ${expected}`);
  }
  return values;
}

const DIRECTIONS = ["NONE", "BN", "CS", "ES", "ON", "EN", "L", "R", "NSM", "AL", "AN", "ET", "WS", "RLO", "LRO", "PDF", "RLE", "RLI", "FSI", "PDI", "LRI", "B", "S", "LRE"];
const directions = [...body(idna, "dir_table").matchAll(/\{\s*(0x[0-9a-f]+)\s*,\s*(0x[0-9a-f]+)\s*,\s*direction::(\w+)\s*\}/g)].flatMap(
  ([, from, to, name]) => {
    const code = DIRECTIONS.indexOf(name);
    if (code < 0) fail(`unknown direction ${name}`);
    return [Number(from), Number(to), code];
  },
);

// Each percent-encode set is 32 bytes, each an `|` of bit masks.
function bitmap(name) {
  const text = body(sets, name).replace(/\/\/[^\n]*/g, "");
  const items = text.split(",").map((item) => item.trim()).filter(Boolean);
  if (items.length !== 32) fail(`set ${name} has ${items.length} bytes`);
  return items.map((item) => {
    if (!/^[0-9a-fx|\s]+$/i.test(item)) fail(`set ${name} has an unexpected byte ${item}`);
    return item.split("|").reduce((byte, part) => byte | Number(part.trim()), 0);
  });
}

const tables = [
  ["MAPPINGS", "u32", numbers(idna, "mappings", 5164)],
  ["RANGES", "u32", numbers(idna, "table", 16000)],
  ["DECOMPOSITION_INDEX", "u8", numbers(idna, "decomposition_index", 4352)],
  ["DECOMPOSITION_BLOCK", "u16", numbers(idna, "decomposition_block", 67 * 257)],
  ["DECOMPOSITION_DATA", "u32", numbers(idna, "decomposition_data", 9102)],
  ["COMBINING_CLASS_INDEX", "u8", numbers(idna, "canonical_combining_class_index", 4352)],
  ["COMBINING_CLASS_BLOCK", "u8", numbers(idna, "canonical_combining_class_block", 67 * 256, true)],
  ["COMPOSITION_INDEX", "u8", numbers(idna, "composition_index", 4352)],
  ["COMPOSITION_BLOCK", "u16", numbers(idna, "composition_block", 67 * 257, true)],
  ["COMPOSITION_DATA", "u32", numbers(idna, "composition_data", 1883)],
  ["DIRECTIONS", "u32", directions],
  ["COMBINING_MARKS", "u32", numbers(idna, "combining")],
  ["VIRAMA", "u32", numbers(idna, "virama")],
  ["JOINING_R", "u32", numbers(idna, "R")],
  ["JOINING_L", "u32", numbers(idna, "L")],
  ["JOINING_D", "u32", numbers(idna, "D")],
  ["C0_CONTROL_SET", "u8", bitmap("C0_CONTROL_PERCENT_ENCODE")],
  ["SPECIAL_QUERY_SET", "u8", bitmap("SPECIAL_QUERY_PERCENT_ENCODE")],
  ["QUERY_SET", "u8", bitmap("QUERY_PERCENT_ENCODE")],
  ["FRAGMENT_SET", "u8", bitmap("FRAGMENT_PERCENT_ENCODE")],
  ["USERINFO_SET", "u8", bitmap("USERINFO_PERCENT_ENCODE")],
  ["PATH_SET", "u8", bitmap("PATH_PERCENT_ENCODE")],
];

const docs = {
  MAPPINGS: "Mapped replacement code points, indexed by `RANGES` descriptors.",
  RANGES: "`(first code point, descriptor)` pairs, flattened. Descriptor byte 0: 0 ignored, 1 valid, 2 disallowed, 3 mapped; mapped: bits 8..24 index into `MAPPINGS`, bits 24.. count.",
  DECOMPOSITION_INDEX: "Block index of the NFD decomposition table, by code point >> 8.",
  DECOMPOSITION_BLOCK: "67 blocks of 257 offsets (flattened); entry `>> 2` indexes `DECOMPOSITION_DATA`, bit 0 marks a compatibility decomposition.",
  DECOMPOSITION_DATA: "Decomposition code points.",
  COMBINING_CLASS_INDEX: "Block index of the canonical combining classes, by code point >> 8.",
  COMBINING_CLASS_BLOCK: "67 blocks of 256 canonical combining classes (flattened).",
  COMPOSITION_INDEX: "Block index of the NFC composition table, by code point >> 8.",
  COMPOSITION_BLOCK: "67 blocks of 257 offsets (flattened) into `COMPOSITION_DATA`.",
  COMPOSITION_DATA: "`(second code point, composite)` pairs, flattened.",
  DIRECTIONS: "`(first, last, Bidi class)` triples, flattened; class numbers follow ada's `direction` enum.",
  COMBINING_MARKS: "Code points a label must not start with.",
  VIRAMA: "Virama code points for the `ContextJ` rules.",
  JOINING_R: "Right-joining code points.",
  JOINING_L: "Left-joining code points.",
  JOINING_D: "Dual-joining code points.",
  C0_CONTROL_SET: "C0 control percent-encode set as a 256-bit map.",
  SPECIAL_QUERY_SET: "Special-query percent-encode set as a 256-bit map.",
  QUERY_SET: "Query percent-encode set as a 256-bit map.",
  FRAGMENT_SET: "Fragment percent-encode set as a 256-bit map.",
  USERINFO_SET: "Userinfo percent-encode set as a 256-bit map.",
  PATH_SET: "Path percent-encode set as a 256-bit map.",
};

let text = `// Generated by scripts/phase3/contracts-url-tables.mjs; do not edit.
//! Data tables of ada 2.9.2 (node v22.20.0): IDNA mapping (Unicode 15.0), NFC
//! normalization, Bidi and joining classes, and the URL percent-encode sets.
//! Read from ada's \`src/ada_idna.cpp\` (SHA-256 \`${ADA_IDNA_SHA256}\`) and
//! \`include/ada/character_sets-inl.h\` (SHA-256 \`${ADA_SETS_SHA256}\`).

// Data copied from ada's sources keeps its literals as ada writes them.
#![allow(clippy::unreadable_literal)]

`;
for (const [name, type, values] of tables) {
  text += `/// ${docs[name]}\npub static ${name}: [${type}; ${values.length}] = [\n`;
  const per = type === "u8" ? 24 : 12;
  for (let index = 0; index < values.length; index += per) {
    text += `    ${values.slice(index, index + per).join(", ")},\n`;
  }
  text += "];\n\n";
}
text = text.trimEnd() + "\n";

if (check) {
  if (readFileSync(outputPath, "utf8") !== text) {
    process.stderr.write("contracts-url-tables: url_tables.rs is stale; rerun without --check\n");
    process.exit(1);
  }
  process.stdout.write(`contracts-url-tables: ${outputPath} is current\n`);
} else {
  writeFileSync(outputPath, text);
  process.stdout.write(`contracts-url-tables: wrote ${outputPath} (${text.length} bytes)\n`);
}
