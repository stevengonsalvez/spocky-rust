#!/bin/sh
# Prints the errno table of this host as the Rust array process.rs::errno_name
# holds, from util.getSystemErrorMap() of the pinned node. Usage:
#   provider-claude-errno-table.sh /path/to/node
# The errno_differential test fails when the table in process.rs differs.
NODE=${1:?usage: provider-claude-errno-table.sh /path/to/node}
exec "$NODE" -e '
const rows = [...require("util").getSystemErrorMap()]
  .filter(([number]) => number >= -1000)
  .map(([number, [name]]) => [-number, name])
  .sort((a, b) => a[0] - b[0]);
console.log(`const TABLE: [(i32, &str); ${rows.length}] = [`);
for (const [number, name] of rows) console.log(`    (${number}, "${name}"),`);
console.log("];");
'
