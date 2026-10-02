#!/usr/bin/env node
// Generates crates/spocky-contracts/src/creation_schema.rs: the pinned
// CreationSnapshotSchema as spocky_contracts::zod::output shapes, so a creation
// receipt read back from disk takes the same key order, defaults and
// transforms as `RecordSchema.parse` in `server/creation/index.ts`.
// Read-only against the runtime checkout.
//
// Usage (Node 22.20.0, the binary pinned by the slice harness):
//   ~/.nvm/versions/node/v22.20.0/bin/node scripts/phase3/creation-zod.mjs \
//     --runtime <paseo-original build of 5de45e2> [--check]
//
// The runtime needs the built packages/protocol/dist; its messages.js must
// have the SHA-256 that crates/spocky-contracts/src/zod_schemas.rs pins.
//
// The walk reads each schema's `_zod.def` and stops with an error on any
// kind, check, format, custom error, or transform the port does not model,
// so a schema change can never be silently approximated.
//
// --check regenerates in memory and exits 1 if the committed file differs.

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const MESSAGES_JS_SHA256 = "bd22155340099ad027b9daa670139c91ab9cde626662526e0563077956b6cbe1";
const NODE_VERSION = "v22.20.0";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const outputPath = join(repoRoot, "crates/spocky-contracts/src/creation_schema.rs");

// Transforms, by SHA-256 of Function.prototype.toString().
const TRANSFORMS = new Map([
  // WorkspaceDescriptorPayloadSchema.statusEnteredAt.
  ["97112aa49296e85ee401745906c6e51e3d2ee50b1f6ed775e636628b980730e6", "NullishToNull"],
  // ProjectCheckoutLiteNotGitPayloadSchema.
  ["1645318032674e23bc208d5779b42b1f93f85d2ccf238dad9662771bfad7a1c3", 'SetNull("worktreeRoot")'],
  // ProjectCheckoutLiteGit{NonPaseo,Paseo}PayloadSchema.
  ["77bfd49a5db6a4584ea3e4b26cd66799da65a821c6cbe5d4804344dbca10907b", 'SetOrFallback("worktreeRoot", "cwd")'],
  // WorkspaceDescriptorPayloadSchema.
  [
    "baa5afc8ab921ffa3a06df6af2491d40ad6dea77438ab31f4e1730990f9d7202",
    'SetOrFallback("workspaceDirectory", "projectRootPath")',
  ],
]);

function fail(message) {
  process.stderr.write(`creation-zod: ${message}\n`);
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
  if (!args.runtime) fail("--runtime <paseo-original build> is required");
  return args;
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

function json(value, where) {
  const text = JSON.stringify(value);
  if (text === undefined) fail(`${where}: value ${String(value)} is not JSON`);
  return rustString(text, where);
}

function constantName(exportName) {
  return exportName
    .replace(/Schema$/, "")
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z])([A-Z][a-z])/g, "$1_$2")
    .toUpperCase();
}

function isLeaf(schema) {
  return ["string", "boolean", "null", "unknown"].includes(schema._zod.def.type) && !(schema._zod.def.checks ?? []).length;
}

class Generator {
  constructor(messages) {
    this.exportNames = new Map();
    for (const [name, value] of Object.entries(messages)) {
      if (value?._zod && !this.exportNames.has(value)) this.exportNames.set(value, name);
    }
    this.references = new Map();
    this.shared = new Map();
    this.statics = [];
  }

  discriminatedOptions(def, where) {
    if (def.unionFallback) fail(`${where}: unionFallback is not modeled`);
    const options = [];
    for (const option of def.options) {
      const values = option._zod.propValues?.[def.discriminator];
      if (!values || values.size === 0) fail(`${where}: option without discriminator values`);
      for (const value of values) {
        if (typeof value !== "string") fail(`${where}: discriminator value ${String(value)} is not a string`);
        options.push({ value, schema: option, shared: values.size > 1 });
      }
    }
    return options;
  }

  children(schema, where) {
    const def = schema._zod.def;
    switch (def.type) {
      case "object":
        return [...Object.keys(def.shape).map((key) => def.shape[key]), ...(def.catchall ? [def.catchall] : [])];
      case "array":
        return [def.element];
      case "record":
        return [def.valueType];
      case "union":
        if (def.discriminator) return this.discriminatedOptions(def, where).map((option) => option.schema);
        return def.options;
      case "optional":
      case "nullable":
      case "default":
      case "catch":
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
      this.statics.push(`static ${name}: LazyLock<Shape> = LazyLock::new(|| {\n    ${body}\n});`);
    }
    return `Shape::Lazy(|| &${name})`;
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
    if (checks.length > 0 && def.type !== "number") fail(`${where}: checks on ${def.type} are not modeled`);
    for (const check of checks) {
      if (check._zod.def.error || check._zod.def.abort) fail(`${where}: check options are not modeled`);
    }
    switch (def.type) {
      case "string":
        if (def.format) fail(`${where}: string formats are not modeled`);
        return "Shape::String";
      case "number": {
        const out = [];
        if (def.format) fail(`${where}: number format schemas are not modeled`);
        for (const check of checks) {
          const c = check._zod.def;
          if (c.check === "number_format" && c.format === "safeint") out.push("NumberCheck::Int");
          else if (c.check === "greater_than") out.push(`NumberCheck::${c.inclusive ? "Gte" : "Gt"}(${rustNumber(c.value, where)})`);
          else fail(`${where}: number check ${c.check} is not modeled`);
        }
        return `Shape::Number(${list(out)})`;
      }
      case "boolean":
        return "Shape::Boolean";
      case "null":
        return "Shape::Null";
      case "unknown":
        return "Shape::Unknown";
      case "literal":
        return `Shape::Literal(${list(def.values.map((value) => literal(value, where)))})`;
      case "enum": {
        const values = Object.values(def.entries);
        if (values.some((value) => typeof value !== "string")) fail(`${where}: non-string enum values are not modeled`);
        return `Shape::Enum(&[${values.map((value) => rustString(value, where)).join(", ")}])`;
      }
      case "array":
        return `Shape::Array(${boxed(def.element, "[]")})`;
      case "object": {
        const catchallType = def.catchall?._zod.def.type;
        let catchall;
        if (catchallType === undefined) catchall = "Catchall::Strip";
        else if (catchallType === "never") catchall = "Catchall::Never";
        else catchall = `Catchall::Keep(${boxed(def.catchall, "{*}")})`;
        const fields = Object.keys(def.shape).map((key) => {
          return `(${rustString(key, where)}, ${this.emit(def.shape[key], `${where}.${key}`, depth + 1)})`;
        });
        return `Shape::Object(\n${pad}${list(fields).replaceAll("\n", `\n    `)},\n${pad}${catchall},\n${end})`;
      }
      case "record": {
        const key = def.keyType._zod.def;
        if (key.type !== "string" || key.format || (key.checks ?? []).length > 0 || key.error) {
          fail(`${where}: record keys other than z.string() are not modeled`);
        }
        return `Shape::Record(${boxed(def.valueType, "{}")})`;
      }
      case "union": {
        if (def.inclusive === false && !def.discriminator) fail(`${where}: exclusive unions are not modeled`);
        if (!def.discriminator) {
          return `Shape::Union(${list(def.options.map((option, index) => this.emit(option, `${where}|${index}`, depth + 1)))})`;
        }
        const options = this.discriminatedOptions(def, where).map(({ value, schema: option }) => {
          return `(${rustString(value, where)}, ${this.emit(option, `${where}|${value}`, depth + 1)})`;
        });
        return `Shape::Discriminated(\n${pad}${rustString(def.discriminator, where)},\n${pad}${list(options).replaceAll("\n", `\n    `)},\n${end})`;
      }
      case "optional":
        if (schema._zod.traits.has("$ZodExactOptional")) fail(`${where}: exactOptional is not modeled`);
        return `Shape::Optional(${boxed(def.innerType, "?")})`;
      case "nullable":
        return `Shape::Nullable(${boxed(def.innerType, "")})`;
      case "default":
        return `Shape::Default(${json(def.defaultValue, where)}, ${boxed(def.innerType, "")})`;
      case "catch":
        return `Shape::Catch(${json(def.catchValue({ value: undefined, issues: [] }), where)}, ${boxed(def.innerType, "")})`;
      case "pipe":
        if (schema._zod.traits.has("$ZodCodec")) fail(`${where}: codecs are not modeled`);
        return `Shape::Pipe(${boxed(def.in, ">in")}, ${boxed(def.out, ">out")})`;
      case "transform": {
        const source = def.transform.toString();
        const transform = TRANSFORMS.get(sha256(source));
        if (!transform) fail(`${where}: transform ${sha256(source)} is not modeled:\n${source}`);
        return `Shape::Transform(Transform::${transform})`;
      }
      case "lazy":
        return this.reference(schema._zod.innerType, `${where}~`);
      default:
        return fail(`${where}: ${def.type} is not modeled`);
    }
  }
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
  const messagesPath = join(runtime, "packages/protocol/dist/messages.js");
  const digest = sha256(readFileSync(messagesPath));
  if (digest !== MESSAGES_JS_SHA256) fail(`${messagesPath} has SHA-256 ${digest}, not ${MESSAGES_JS_SHA256}`);
  const messages = await import(pathToFileURL(messagesPath).href);
  const root = messages.CreationSnapshotSchema;
  const generator = new Generator(messages);
  generator.count(root, "snapshot");
  const body = generator.inline(root, "snapshot", 1);

  const text = `// Generated by scripts/phase3/creation-zod.mjs; do not edit.
//! \`CreationSnapshotSchema\` of pinned Paseo \`5de45e2\` as
//! [\`crate::zod::output\`] shapes, generated from
//! \`packages/protocol/dist/messages.js\` (SHA-256 \`${digest}\`).

use std::sync::LazyLock;

use crate::js_value::JsValue;
use crate::zod::output::{Catchall, NumberCheck, Shape, Transform};

/// SHA-256 of the \`messages.js\` this file was generated from.
pub const MESSAGES_JS_SHA256: &str = "${digest}";

/// \`CreationSnapshotSchema\`.
pub static CREATION_SNAPSHOT: LazyLock<Shape> = LazyLock::new(|| {
    ${body}
});
${generator.statics.map((item) => `\n${item}\n`).join("")}`;

  if (args.check) {
    if (readFileSync(outputPath, "utf8") !== text) {
      process.stderr.write("creation-zod: creation_schema.rs is stale; rerun without --check\n");
      process.exit(1);
    }
    process.stdout.write(`creation-zod: ${outputPath} is current\n`);
    return;
  }
  writeFileSync(outputPath, text);
  process.stdout.write(`creation-zod: wrote ${outputPath}\n`);
}

await main();
