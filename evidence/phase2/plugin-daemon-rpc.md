# Plugin daemon RPC parity

Status: baseline inventory frozen before implementation; Rust wire model and
differential proof complete.

## Provenance

- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`
- Rust base: `42a2519316d4e08ec827061c8efe4ffc54f24a8d`
- Protocol source: `packages/protocol/src/messages.ts`
- Protocol tests: `packages/protocol/src/messages.plugins.test.ts`
- Daemon handlers: `packages/server/src/server/session.ts`
- Plugin service: `packages/server/src/server/plugins/index.ts`
- Client callers: `packages/client/src/daemon-client.ts`
- Namespacing contract: `docs/rpc-namespacing.md`
- Plugin contract: `docs/plugins.md`

## Baseline daemon RPC inventory

Requests keep `requestId` and inputs at top level. Successful responses keep
`requestId` and results under `payload`. Optional request fields are omitted,
not serialized as `null`. Optional response fields are also omitted. Explicit
`null` appears only where a nested contract declares it.

| Request | Request fields after `type`, `requestId` | Response payload after `requestId` |
|---|---|---|
| `plugin.catalog.get.request` | none | `plugins: [{ id, clientBundle, requirements? }]` |
| `plugin.list.request` | none | `plugins: PluginListItem[]` |
| `plugin.logs.get.request` | `pluginId` | `pluginId`, `entries: PluginLogEntry[]` |
| `plugin.directory.install.request` | `path`, `id?` | `plugin: PluginListItem` |
| `plugin.directory.inspect.request` | `path` | `id` |
| `plugin.source.install.request` | `source`, `id?`, `ref?`, `pluginPath?` | `plugin: PluginListItem` |
| `plugin.source.status.request` | `pluginId?` | `plugins: PluginSourceStatusItem[]` |
| `plugin.source.update.preview.request` | `pluginId?`, `target?` | `plugins: PluginUpdatePreview[]` |
| `plugin.source.update.apply.request` | `proposals` | `plugins: PluginUpdateResult[]` |
| `plugin.source.update.request` | `pluginId?` | `plugins: PluginSourceUpdateItem[]` |
| `plugin.reload.request` | `pluginId` | `plugin: PluginListItem` |
| `plugin.enable.request` | `pluginId` | `plugin: PluginListItem` |
| `plugin.disable.request` | `pluginId` | `plugin: PluginListItem` |
| `plugin.remove.request` | `pluginId` | no other fields, strict object |
| `plugin.rpc.invoke.request` | `pluginId`, `method`, `input` | `output` |

Every request has the same dotted prefix response ending in `.response`.
`pluginPath` on source install is a v0.7 compatibility input retained until
2027-09-01. Immediate `plugin.source.update.request` is a v0.8 compatibility
RPC whose handler rejects with update-client guidance.

### Nested shapes

- `PluginListItem`: required `id`, `path`, `enabled`, `status`; optional
  `description`, closed legacy `source: directory | git`, `npm`, `installation`,
  `remote`, `ref`, `commit`, and `error`.
- `PluginInstallation`: required discriminated `identity`; optional
  `currentRevision`. Identity is directory `{ kind, path }`, Git
  `{ kind, remote, pluginPath }`, or npm `{ kind, packageName, pluginPath }`.
- `PluginLogEntry`: required nonnegative integer `sequence`, ISO datetime
  `timestamp`, `stream: stdout | stderr`, and `message`.
- Status rows require `id`, closed `source: directory | git`, and `path`; npm,
  installation, remote, ref, current/latest commit, commits behind, and update
  availability are optional.
- Update targets are Git `{ kind, commit }` or npm
  `{ kind, version, resolved, integrity }`. Preview outcomes are `update`,
  `current`, `installed-newer`, `local`, or `error`. Apply outcomes are
  `updated` or `error`.
- Plugin IDs match `^[a-z][a-z0-9-]*$`. RPC methods are nonempty on the wire;
  SDK definitions further require `^[a-z][a-z0-9._-]*$` after trimming.

## Settings, attachment, and native UI routing

These surfaces add no separate daemon message family.

- Settings use `plugin.rpc.invoke` with methods
  `settings.<id>.read`, `settings.<id>.write`, and
  `settings.<id>.reset`. Read returns `ready` with `revision` and `values`, or
  `invalid` with `revision` and `error`. Write/reset return `saved` with
  `revision` and `values`, `conflict` with `error`, or `invalid` with `error`.
- Attachment sources are definitions inside `clientBundle`. Search calls their
  declared method through `plugin.rpc.invoke` with `{ query }` and returns
  `{ items }`. Each item requires `id`, `identifier`, `title`, `url`, `text`,
  and `resourceType`; `subtitle` is optional.
- Native surfaces, settings screens, panels, and other client contributions are
  executable content inside `plugin.catalog.get.response.plugins[].clientBundle`.
  Discovery and opening do not issue another plugin daemon RPC.

## Notifications and failures

- Catalog changes emit
  `{ type: "status", payload: { status: "plugin_catalog_changed", pluginId } }`.
- Settings saves, resets, and migrations emit
  `{ type: "status", payload: { status: "plugin_settings_changed", pluginId, settingsId } }`.
- Missing plugin runtime returns empty arrays for list and catalog. Every other
  plugin operation fails with `Plugin service is unavailable`.
- Handler failures emit correlated
  `{ type: "rpc_error", payload: { requestId, requestType, error: "Request failed: <message>", code: "handler_error" } }`,
  then an uncorrelated `activity_log` error carrying generated ID and timestamp.
- Authorization failures emit only correlated `rpc_error`, with
  `error: "Session is not authorized for <request type>"` and
  `code: "access_denied"`.
- Preview and apply convert failures into per-plugin `outcome: "error"` rows.
  Other service failures use the correlated handler error path.

## Differential result

`scripts/phase2/plugin-daemon-rpc-capture.sh` runs pinned TypeScript protocol
parsing, runs the Rust target, and compares raw JSON strings without
normalization. Forty-two byte-identical cases pass:

1. All 15 dotted request types.
2. All 15 matching response types.
3. Handler and authorization `rpc_error` responses.
4. Catalog and settings status notifications.
5. Settings read, write, and reset through generic plugin RPC.
6. Attachment search output with explicit nested `null` preservation.
7. Native client-bundle transport through the catalog.
8. Missing optional fields remain omitted.
9. Explicit `null` for optional request and response fields is rejected by both
   implementations.

The Rust model uses typed source identities, installation rows, list and log
rows, update selections, proposals, preview and apply outcomes, notifications,
and correlated errors. Generic plugin RPC input and output remain arbitrary JSON
so settings, attachment sources, and author-defined methods preserve their
nested missing and null values.

## Preserved baseline defects

- `plugin.source.update.request` remains accepted by the protocol but always
  fails with `Update the client to review plugin updates before applying them.`
- A missing plugin runtime returns empty list and catalog responses, while every
  other plugin operation returns a handler error.
- Handler failures emit both correlated `rpc_error` and an uncorrelated
  generated `activity_log` error.

## Verification

- `scripts/phase2/plugin-daemon-rpc-capture.sh`: 42 matched, 0 mismatched;
  pinned Vitest 1 passed, Rust capture 1 passed.
- `cargo test -p spocky-plugin-pilot --test settings_lifecycle --test hook_usage_runtime --test plugin_lifecycle --test protocol_manifest --test process_protocol --test runtime_acquisition --test plugin_daemon_rpc`: 31 passed, 0 failed.
- `cargo fmt --check`: passed.
- `cargo clippy -p spocky-plugin-pilot --all-targets -- -D warnings`: passed.

Raw differential log SHA-256:
`fef3051575dfda93900c6b99af0fb894c2fabc017d32a99ea17600fa8d713b53`.

Targeted verification log SHA-256:
`bae75cbaf77de4b2de2a4676185d76d6fb7cb81e9f21d3e9abb807fc54461be5`.

## Remaining gaps

- Native UI rendering and interaction remain platform qualification work. This
  task proves the daemon transports the exact client bundle carrying surfaces.
- The generated `activity_log` side effect is inventoried but not compared in
  raw form because its ID and timestamp are documented normalization fields.
