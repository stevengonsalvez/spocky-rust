#!/usr/bin/env node

import { readFile } from "node:fs/promises";

const [snapshotPath, journalPath] = process.argv.slice(2);
if (!snapshotPath || !journalPath) {
  throw new Error("usage: hub-embedded-schema-generate.mjs SNAPSHOT JOURNAL");
}

const snapshot = JSON.parse(await readFile(snapshotPath, "utf8"));
const journal = JSON.parse(await readFile(journalPath, "utf8"));

const quote = (identifier) => `"${identifier.replaceAll('"', '""')}"`;
const unqualify = (expression, table) => expression.replaceAll(`${quote(table)}.`, "");

function sqliteType(column) {
  if (column.type === "boolean" || column.type === "integer" || column.type === "bigint") {
    return "INTEGER";
  }
  if (column.type === "jsonb" || column.type === "text[]") return "TEXT";
  if (column.type === "timestamp with time zone") return "TEXT";
  return "TEXT";
}

function sqliteDefault(value) {
  if (value === undefined || value === "gen_random_uuid()") return "";
  if (value === "now()") return " DEFAULT CURRENT_TIMESTAMP";
  if (value === true || value === "true") return " DEFAULT 1";
  if (value === false || value === "false") return " DEFAULT 0";
  if (typeof value === "number") return ` DEFAULT ${value}`;
  return ` DEFAULT ${String(value).replace(/::[a-z][a-z0-9_\[\]]*/giu, "")}`;
}

function sqliteCheck(check, table) {
  if (check.name === "organization_api_keys_scopes_check") {
    return "json_valid(scopes) AND json_array_length(scopes) > 0";
  }
  return unqualify(check.value, table);
}

const tables = Object.values(snapshot.tables).sort((left, right) =>
  left.name.localeCompare(right.name),
);
const statements = [];
const constraintNames = [];

for (const table of tables) {
  const definitions = [];
  for (const column of Object.values(table.columns)) {
    const primaryKey = column.primaryKey ? " PRIMARY KEY" : "";
    const notNull = column.notNull ? " NOT NULL" : "";
    definitions.push(
      `  ${quote(column.name)} ${sqliteType(column)}${primaryKey}${notNull}${sqliteDefault(column.default)}`,
    );
  }
  for (const constraint of Object.values(table.compositePrimaryKeys ?? {})) {
    definitions.push(
      `  CONSTRAINT ${quote(constraint.name)} PRIMARY KEY (${constraint.columns.map(quote).join(", ")})`,
    );
  }
  for (const constraint of Object.values(table.uniqueConstraints ?? {})) {
    constraintNames.push(constraint.name);
    definitions.push(
      `  CONSTRAINT ${quote(constraint.name)} UNIQUE (${constraint.columns.map(quote).join(", ")})`,
    );
  }
  for (const constraint of Object.values(table.foreignKeys ?? {})) {
    constraintNames.push(constraint.name);
    const onDelete = constraint.onDelete === "no action" ? "" : ` ON DELETE ${constraint.onDelete.toUpperCase()}`;
    const onUpdate = constraint.onUpdate === "no action" ? "" : ` ON UPDATE ${constraint.onUpdate.toUpperCase()}`;
    definitions.push(
      `  CONSTRAINT ${quote(constraint.name)} FOREIGN KEY (${constraint.columnsFrom.map(quote).join(", ")}) REFERENCES ${quote(constraint.tableTo)} (${constraint.columnsTo.map(quote).join(", ")})${onDelete}${onUpdate}`,
    );
  }
  for (const constraint of Object.values(table.checkConstraints ?? {})) {
    constraintNames.push(constraint.name);
    definitions.push(
      `  CONSTRAINT ${quote(constraint.name)} CHECK (${sqliteCheck(constraint, table.name)})`,
    );
  }
  statements.push(
    `CREATE TABLE IF NOT EXISTS ${quote(table.name)} (\n${definitions.join(",\n")}\n);`,
  );
}

for (const table of tables) {
  for (const index of Object.values(table.indexes ?? {})) {
    constraintNames.push(index.name);
    const columns = index.columns.map((column) => {
      const expression = column.isExpression
        ? unqualify(column.expression, table.name)
        : quote(column.expression);
      return `${expression}${column.asc ? "" : " DESC"}`;
    });
    const where = index.where ? ` WHERE ${unqualify(index.where, table.name)}` : "";
    statements.push(
      `CREATE ${index.isUnique ? "UNIQUE " : ""}INDEX IF NOT EXISTS ${quote(index.name)} ON ${quote(table.name)} (${columns.join(", ")})${where};`,
    );
  }
}

constraintNames.sort();
const rustStrings = (values) => values.map((value) => `    ${JSON.stringify(value)},`).join("\n");
const journalRows = journal.entries
  .map((entry) => `    (${entry.idx}, ${JSON.stringify(entry.tag)}, ${entry.when}),`)
  .join("\n");

process.stdout.write(`// Generated from pinned Hub Drizzle snapshot 0048 and journal.\n`);
process.stdout.write(`// Regenerate with scripts/phase2/hub-embedded-schema-generate.mjs.\n\n`);
process.stdout.write(`#![allow(clippy::unreadable_literal)]\n\n`);
process.stdout.write(`pub const BASELINE_SCHEMA_SQL: &str = r#"\n${statements.join("\n")}\n"#;\n\n`);
process.stdout.write(`pub const BASELINE_TABLES: &[&str] = &[\n${rustStrings(tables.map((table) => table.name))}\n];\n\n`);
process.stdout.write(`pub const BASELINE_CONSTRAINT_NAMES: &[&str] = &[\n${rustStrings(constraintNames)}\n];\n\n`);
process.stdout.write(`pub const BASELINE_JOURNAL: &[(u32, &str, i64)] = &[\n${journalRows}\n];\n`);
