#!/usr/bin/env node
// Generates crates/spocky-contracts/src/zod_schemas.rs: the pinned
// WSInboundMessageSchema as spocky_contracts::zod schemas, so the Rust zod
// port reports the same issues as `WSInboundMessageSchema.safeParse`.
// Read-only against the runtime checkout.
//
// Usage (Node 22.20.0, the binary pinned by the slice harness):
//   ~/.nvm/versions/node/v22.20.0/bin/node scripts/phase3/contracts-zod.mjs \
//     --runtime <paseo-runtime> [--check]
//
// The walk reads each schema's `_zod.def` and stops with an error on any
// kind, check, format, custom error, or transform the port does not model,
// so a schema change can never be silently approximated. Session request
// types outside MODELED become Schema::Unmodeled.
//
// --check regenerates in memory and exits 1 if the committed file differs.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const PASEO_COMMIT = "5de45e208690b0efc51c59a585ae9729325a9204";
const NODE_VERSION = "v22.20.0";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const outputPath = join(repoRoot, "crates/spocky-contracts/src/zod_schemas.rs");

// The SessionInbound variants in crates/spocky-contracts/src/session.rs.
const MODELED = new Set([
  "ping",
  "workspace.create.request",
  "fetch_workspaces_request",
  "create_agent_request",
  "agent.create.request",
  "creation.subscribe.request",
  "send_agent_message_request",
  "wait_for_finish_request",
  "fetch_agents_request",
  "fetch_agent_request",
  "fetch_agent_timeline_request",
  "agent.timeline.set_subscription.request",
  "session.events.set_subscription.request",
  "subscription.release.request",
  "agent_permission_response",
  "cancel_agent_request",
  "resume_agent_request",
  "refresh_agent_request",
]);

// string_format checks, by format and RegExp.prototype.toString(), with the
// Rust predicate that implements the pattern.
const FORMATS = new Map([
  [
    "uuid /^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}|00000000-0000-0000-0000-000000000000|ffffffff-ffff-ffff-ffff-ffffffffffff)$/",
    "crate::id::is_zod_uuid",
  ],
  ["regex /^wks_[a-f0-9]{16}$/", "crate::id::is_workspace_id"],
]);

// Transforms, by SHA-256 of Function.prototype.toString().
const TRANSFORMS = new Map([
  // BrowserAutomationHostCapabilitySchema.supportedCommands.
  ["5fb85ee598a38ae73af366a018e87911df38f2a0e1f6fbab521b4fcac34bc763", "BrowserHostCommands"],
  // The attachment mapping that keeps contextKind only for "chat_history".
  ["b1ed742b9fbdc0a05e10629b056a74bc0b3451c7b0c2658313c969275912a07e", "NoIssue"],
  // normalizeAgentAttachments: drops invalid items, adds no issue.
  ["af76e991b5610fc74be306c80c765de8a33d7b0c0552548e97a5359babe9c5ed", "NoIssue"],
]);

function fail(message) {
  process.stderr.write(`contracts-zod: ${message}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const args = { runtime: null, check: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--runtime") {
      args.runtime = argv[index + 1];
      index += 1;
    } else if (arg === "--check") {
      args.check = true;
    } else {
      fail(`unknown argument ${arg}`);
    }
  }
  if (!args.runtime) fail("--runtime <paseo-runtime checkout> is required");
  return args;
}

function git(runtime, ...args) {
  return execFileSync("git", ["-C", runtime, ...args], { encoding: "utf8" }).trim();
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function rustString(value, where) {
  if (typeof value !== "string" || !/^[\x20-\x7e]*$/.test(value) || value.includes('"#')) {
    fail(`${where}: cannot write ${JSON.stringify(value)} as a Rust literal`);
  }
  return /["\\]/.test(value) ? `r#"${value}"#` : `"${value}"`;
}

function rustNumber(value, where) {
  if (typeof value !== "number" || !Number.isFinite(value)) fail(`${where}: bound ${value} is not finite`);
  return Number.isInteger(value) ? `${value}.0` : String(value);
}

function constantName(exportName) {
  return exportName
    .replace(/Schema$/, "")
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z])([A-Z][a-z])/g, "$1_$2")
    .toUpperCase();
}

class Generator {
  constructor(messages) {
    this.session = messages.SessionInboundMessageSchema;
    this.exportNames = new Map();
    for (const [name, value] of Object.entries(messages)) {
      if (value?._zod && !this.exportNames.has(value)) this.exportNames.set(value, name);
    }
    this.references = new Map();
    this.shared = new Map();
    this.statics = [];
  }

  // Options of a discriminated union with the discriminator value that
  // selects each, in zod's map order.
  discriminatedOptions(def, where) {
    if (def.unionFallback) fail(`${where}: unionFallback is not modeled`);
    const options = [];
    for (const option of def.options) {
      const values = option._zod.propValues?.[def.discriminator];
      if (!values || values.size === 0) fail(`${where}: option without discriminator values`);
      for (const value of values) {
        if (typeof value !== "string") fail(`${where}: discriminator value ${String(value)} is not a string`);
        const modeled = def !== this.session._zod.def || MODELED.has(value);
        options.push({ value, schema: modeled ? option : null, shared: values.size > 1 });
      }
    }
    return options;
  }

  children(schema, where) {
    const def = schema._zod.def;
    switch (def.type) {
      case "object":
        return Object.keys(def.shape).map((key) => def.shape[key]);
      case "array":
        return [def.element];
      case "record":
        return [def.valueType];
      case "union":
        if (def.discriminator) {
          return this.discriminatedOptions(def, where)
            .filter((option) => option.schema)
            .map((option) => option.schema);
        }
        return def.options;
      case "optional":
      case "nullable":
      case "default":
        return [def.innerType];
      case "pipe":
        return [def.in, def.out];
      case "lazy":
        return [schema._zod.innerType];
      default:
        return [];
    }
  }

  // Counts references so a schema reached more than once, or through
  // z.lazy(), becomes one static.
  count(schema, where) {
    const seen = this.references.get(schema) ?? 0;
    this.references.set(schema, seen + 1);
    if (seen > 0) return;
    const def = schema._zod.def;
    if (def.type === "lazy") this.references.set(schema._zod.innerType, 2);
    if (def.type === "union" && def.discriminator) {
      for (const option of this.discriminatedOptions(def, where)) {
        if (option.shared) this.references.set(option.schema, 2);
      }
    }
    for (const child of this.children(schema, where)) this.count(child, where);
  }

  reference(schema, where) {
    let name = this.shared.get(schema);
    if (!name) {
      const exported = this.exportNames.get(schema);
      name = exported ? constantName(exported) : `SHARED_${this.shared.size}`;
      if ([...this.shared.values()].includes(name)) name = `${name}_${this.shared.size}`;
      this.shared.set(schema, name);
      const body = this.inline(schema, where, 1);
      this.statics.push(`static ${name}: LazyLock<Schema> = LazyLock::new(|| {\n    ${body}\n});`);
    }
    return `Schema::Lazy(|| &${name})`;
  }

  emit(schema, where, depth) {
    if ((this.references.get(schema) ?? 0) > 1 && !isLeaf(schema)) return this.reference(schema, where);
    return this.inline(schema, where, depth);
  }

  inline(schema, where, depth) {
    const def = schema._zod.def;
    const pad = "    ".repeat(depth + 1);
    const end = "    ".repeat(depth);
    const list = (items) => (items.length === 0 ? "vec![]" : `vec![\n${items.map((item) => `${pad}${item},\n`).join("")}${end}]`);
    const boxed = (child, label) => `Box::new(${this.emit(child, `${where}${label}`, depth)})`;
    if (def.error) fail(`${where}: custom error maps are not modeled`);
    if (def.coerce) fail(`${where}: coercion is not modeled`);
    const checks = def.checks ?? [];
    if (checks.length > 0 && def.type !== "string" && def.type !== "number") {
      fail(`${where}: checks on ${def.type} are not modeled`);
    }
    for (const check of checks) {
      if (check._zod.def.error || check._zod.def.abort) fail(`${where}: check options are not modeled`);
    }
    switch (def.type) {
      case "string": {
        const out = [];
        if (def.format) out.push(this.format(def.format, def.pattern, where));
        for (const check of checks) {
          const c = check._zod.def;
          if (c.check === "min_length") out.push(`StringCheck::Min(${c.minimum})`);
          else if (c.check === "max_length") out.push(`StringCheck::Max(${c.maximum})`);
          else if (c.check === "overwrite" && c.tx.toString() === "(input) => input.trim()") out.push("StringCheck::Trim");
          else if (c.check === "string_format") out.push(this.format(c.format, c.pattern, where));
          else fail(`${where}: string check ${c.check} is not modeled`);
        }
        return `Schema::String(${list(out)})`;
      }
      case "number": {
        const out = [];
        if (def.format) fail(`${where}: number format schemas are not modeled`);
        for (const check of checks) {
          const c = check._zod.def;
          if (c.check === "number_format" && c.format === "safeint") out.push("NumberCheck::Int");
          else if (c.check === "greater_than") out.push(`NumberCheck::${c.inclusive ? "Gte" : "Gt"}(${rustNumber(c.value, where)})`);
          else if (c.check === "less_than" && c.inclusive) out.push(`NumberCheck::Lte(${rustNumber(c.value, where)})`);
          else fail(`${where}: number check ${c.check} is not modeled`);
        }
        return `Schema::Number(${list(out)})`;
      }
      case "boolean":
        return "Schema::Boolean";
      case "null":
        return "Schema::Null";
      case "unknown":
        return "Schema::Unknown";
      case "literal":
        return `Schema::Literal(${list(def.values.map((value) => literal(value, where)))})`;
      case "enum": {
        const values = Object.values(def.entries);
        if (values.some((value) => typeof value !== "string")) fail(`${where}: non-string enum values are not modeled`);
        return `Schema::Enum(&[${values.map((value) => rustString(value, where)).join(", ")}])`;
      }
      case "array":
        return `Schema::Array(${boxed(def.element, "[]")})`;
      case "object": {
        const catchall = def.catchall?._zod.def.type;
        let unknownKeys;
        if (catchall === undefined || catchall === "unknown") unknownKeys = "UnknownKeys::Allow";
        else if (catchall === "never") unknownKeys = "UnknownKeys::Strict";
        else fail(`${where}: catchall ${catchall} is not modeled`);
        const fields = Object.keys(def.shape).map((key) => {
          return `(${rustString(key, where)}, ${this.emit(def.shape[key], `${where}.${key}`, depth + 1)})`;
        });
        return `Schema::Object(\n${pad}${list(fields).replaceAll("\n", `\n    `)},\n${pad}${unknownKeys},\n${end})`;
      }
      case "record": {
        const key = def.keyType._zod.def;
        if (key.type !== "string" || key.format || (key.checks ?? []).length > 0 || key.error) {
          fail(`${where}: record keys other than z.string() are not modeled`);
        }
        return `Schema::Record(${boxed(def.valueType, "{}")})`;
      }
      case "union": {
        if (def.inclusive === false && !def.discriminator) fail(`${where}: exclusive unions are not modeled`);
        if (!def.discriminator) {
          return `Schema::Union(${list(def.options.map((option, index) => this.emit(option, `${where}|${index}`, depth + 1)))})`;
        }
        const options = this.discriminatedOptions(def, where).map(({ value, schema: option }) => {
          const body = option ? this.emit(option, `${where}|${value}`, depth + 1) : "Schema::Unmodeled";
          return `(${rustString(value, where)}, ${body})`;
        });
        return `Schema::Discriminated(\n${pad}${rustString(def.discriminator, where)},\n${pad}${list(options).replaceAll("\n", `\n    `)},\n${end})`;
      }
      case "optional":
        if (schema._zod.traits.has("$ZodExactOptional")) fail(`${where}: exactOptional is not modeled`);
        return `Schema::Optional(${boxed(def.innerType, "?")})`;
      case "nullable":
        return `Schema::Nullable(${boxed(def.innerType, "")})`;
      case "default":
        return `Schema::Default(${boxed(def.innerType, "")})`;
      case "pipe":
        if (schema._zod.traits.has("$ZodCodec")) fail(`${where}: codecs are not modeled`);
        return `Schema::Pipe(${boxed(def.in, ">in")}, ${boxed(def.out, ">out")})`;
      case "transform": {
        const source = def.transform.toString();
        const transform = TRANSFORMS.get(sha256(source));
        if (!transform) fail(`${where}: transform ${sha256(source)} is not modeled:\n${source}`);
        return `Schema::Transform(Transform::${transform})`;
      }
      case "lazy":
        return this.reference(schema._zod.innerType, `${where}~`);
      default:
        return fail(`${where}: ${def.type} is not modeled`);
    }
  }

  format(format, pattern, where) {
    const source = pattern?.toString();
    const test = FORMATS.get(`${format} ${source}`);
    if (!test) fail(`${where}: string format ${format} ${source} is not modeled`);
    return `StringCheck::Format {\n    format: ${rustString(format, where)},\n    pattern: ${rustString(source, where)},\n    test: ${test},\n}`;
  }
}

function isLeaf(schema) {
  return ["boolean", "null", "unknown"].includes(schema._zod.def.type);
}

function literal(value, where) {
  if (typeof value === "string") return `JsValue::String(${rustString(value, where)}.to_owned())`;
  if (typeof value === "boolean") return `JsValue::Bool(${value})`;
  return fail(`${where}: literal ${String(value)} is not modeled`);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (process.version !== NODE_VERSION) fail(`expected node ${NODE_VERSION}, running ${process.version}`);
  const runtime = resolve(args.runtime);
  const head = git(runtime, "rev-parse", "HEAD");
  if (head !== PASEO_COMMIT) fail(`runtime HEAD ${head} is not ${PASEO_COMMIT}`);
  const dirty = git(runtime, "status", "--porcelain", "--untracked-files=no");
  if (dirty !== "") fail(`runtime has tracked modifications:\n${dirty}`);

  const messagesPath = join(runtime, "packages/protocol/dist/messages.js");
  const messages = await import(pathToFileURL(messagesPath).href);
  const root = messages.WSInboundMessageSchema;
  const generator = new Generator(messages);
  generator.count(root, "inbound");
  const body = generator.inline(root, "inbound", 1);
  const missing = [...MODELED].filter(
    (type) => !generator.discriminatedOptions(messages.SessionInboundMessageSchema._zod.def, "session").some((o) => o.value === type),
  );
  if (missing.length > 0) fail(`MODELED types missing from SessionInboundMessageSchema: ${missing.join(", ")}`);

  const digest = sha256(readFileSync(messagesPath));
  const text = `// Generated by scripts/phase3/contracts-zod.mjs; do not edit.
//! \`WSInboundMessageSchema\` of pinned Paseo \`5de45e2\` as [\`crate::zod\`]
//! schemas, generated from \`packages/protocol/dist/messages.js\`
//! (SHA-256 \`${digest}\`).
//!
//! Session request types without a Rust model are [\`Schema::Unmodeled\`].

use std::sync::LazyLock;

use crate::js_value::JsValue;
use crate::zod::{NumberCheck, Outcome, Schema, StringCheck, Transform, UnknownKeys, check};

/// SHA-256 of the \`messages.js\` this file was generated from.
pub const MESSAGES_JS_SHA256: &str = "${digest}";

/// Runs \`WSInboundMessageSchema.safeParse(value)\`.
#[must_use]
pub fn check_inbound(value: &JsValue) -> Outcome {
    check(&WS_INBOUND_MESSAGE, value)
}

static WS_INBOUND_MESSAGE: LazyLock<Schema> = LazyLock::new(|| {
    ${body}
});
${generator.statics.map((item) => `\n${item}\n`).join("")}`;

  if (args.check) {
    if (readFileSync(outputPath, "utf8") !== text) {
      process.stderr.write("contracts-zod: zod_schemas.rs is stale; rerun without --check\n");
      process.exit(1);
    }
    process.stdout.write(`contracts-zod: ${outputPath} is current\n`);
    return;
  }
  writeFileSync(outputPath, text);
  process.stdout.write(`contracts-zod: wrote ${outputPath}\n`);
}

await main();
