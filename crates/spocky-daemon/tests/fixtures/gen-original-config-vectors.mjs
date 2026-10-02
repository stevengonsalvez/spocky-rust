// Regenerates original-config-vectors.json: what the pinned original's
// `loadPersistedConfig` (Paseo 5de45e2, built by scripts/phase3/build-original.sh)
// returns or throws for config.json texts, run under Node 22.20.0 against a
// disposable PASEO_HOME. Usage:
//   node gen-original-config-vectors.mjs <original-build-root> > original-config-vectors.json
// The disposable config path in an error message is written as <config>.
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const { provenance } = createRequire(import.meta.url)("./generator-support.cjs");

const root = process.argv[2];
const module = await import(
  pathToFileURL(path.join(root, "packages/server/dist/server/server/persisted-config.js")).href
);
const bcrypt = "$2a$12$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234";
const cases = [
  ["empty_object", "{}"],
  ["daemon_fields", JSON.stringify({ daemon: { listen: "127.0.0.1:9", hostnames: [".a.com"], cors: { allowedOrigins: ["x"] }, auth: { password: bcrypt } } })],
  ["hostnames_true", '{"daemon":{"hostnames":true}}'],
  ["allowed_hosts_old_name", '{"daemon":{"allowedHosts":["h.example"]}}'],
  ["hostnames_win_over_allowed_hosts", '{"daemon":{"allowedHosts":true,"hostnames":["h"]}}'],
  ["unknown_top_level_key", '{"bogus":1}'],
  ["unknown_daemon_key", '{"daemon":{"bogus":true}}'],
  ["unknown_features_key", '{"features":{"bogus":1}}'],
  ["hostnames_false", '{"daemon":{"hostnames":false}}'],
  ["hostnames_mixed_invalid", '{"daemon":{"hostnames":[".a.com",5]}}'],
  ["allowed_hosts_mixed_invalid", '{"daemon":{"allowedHosts":["ok",5]}}'],
  ["listen_number", '{"daemon":{"listen":5}}'],
  ["cors_wrong_type", '{"daemon":{"cors":{"allowedOrigins":[1]}}}'],
  ["password_not_bcrypt", '{"daemon":{"auth":{"password":"plain"}}}'],
  ["password_null", '{"daemon":{"auth":{"password":null}}}'],
  ["several_issues", '{"daemon":{"listen":5,"bogus":1},"bogus2":true}'],
  ["invalid_json", "{not json"],
  ["empty_file", ""],
  ["truncated_json", '{"daemon":'],
  ["bom_prefixed", "﻿" + '{"daemon":{"listen":"127.0.0.1:9"}}'],
  ["root_array", "[]"],
  ["root_null", "null"],
  ["root_number", "5"],
  ["daemon_string", '{"daemon":"x"}'],
  ["agents_providers_number", '{"agents":{"providers":5}}'],
  ["relay_disabled", '{"daemon":{"relay":{"enabled":false}}}'],
];
const out = [];
for (const [name, text] of cases) {
  const home = mkdtempSync(path.join(tmpdir(), "spocky-config-vectors-"));
  const file = path.join(home, "config.json");
  writeFileSync(file, text, { mode: 0o600 });
  let result;
  try {
    const config = module.loadPersistedConfig(home);
    const daemon = config.daemon ?? {};
    result = {
      ok: {
        listen: daemon.listen ?? null,
        hostnames: daemon.hostnames ?? null,
        corsAllowedOrigins: daemon.cors?.allowedOrigins ?? null,
        password: daemon.auth?.password ?? null,
      },
    };
  } catch (error) {
    result = { error: String(error?.message ?? error).split(file).join("<config>") };
  }
  rmSync(home, { recursive: true, force: true });
  out.push({ name, text, ...result });
}
process.stdout.write(JSON.stringify({ node: process.version, original: path.basename(root), provenance: provenance(root, ["packages/server/dist/server/server/persisted-config.js"]), cases: out }, null, 1));
