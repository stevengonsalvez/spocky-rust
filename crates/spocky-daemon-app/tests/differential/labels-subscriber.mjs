// Workspace label recorder: create a workspace, then drive every label RPC
// (list, owned subscription, assignment, edit, delete inspect, delete, and
// their rejections) and print the exact wire text of the server_info frame
// and of every label frame and rpc_error, in arrival order.
//
// The client's own message listeners see zod-parsed frames, whose key order
// and extra keys are not the wire's, so the raw JSON text is taken where the
// client's transport hands it to `handleJsonPayload`. A throwaway first
// connection exposes the client class so the hook is in place before the
// recorded connection's handshake.
//
// Usage: node labels-subscriber.mjs <paseo-root> <host:port> <project-dir>
const [, , paseoRoot, host, project] = process.argv;
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const target = { kind: "endpoint", host };
const probe = await connectToDaemon({ target });
const prototype = Object.getPrototypeOf(probe);
await probe.close();
const raw = [];
const handleJsonPayload = prototype.handleJsonPayload;
prototype.handleJsonPayload = function (payload, length) {
  if (this !== probe) raw.push(payload);
  return handleJsonPayload.call(this, payload, length);
};
const client = await connectToDaemon({ target });
const outcomes = [];
const attempt = async (name, call) => {
  try {
    const result = await call();
    outcomes.push(`${name}:ok`);
    return result;
  } catch (error) {
    outcomes.push(`${name}:error:${error?.code ?? ""}:${error?.message}`);
    return undefined;
  }
};
const settle = () => new Promise((resolve) => setTimeout(resolve, 300));

const created = await client.createWorkspace({ source: { kind: "directory", path: project } });
const workspaceId = created.workspace.id;

const first = await attempt("list-empty", () => client.listWorkspaceLabels());
const observed = client.observeWorkspaceLabels();
await attempt("observe", () => observed.ready);
await attempt("assign-new", () =>
  client.setWorkspaceLabel({ workspaceId, label: { name: "  Needs   review ", color: "sky" }, assigned: true }),
);
await settle();
await attempt("assign-existing-other-colour", () =>
  client.setWorkspaceLabel({ workspaceId, label: { name: "needs REVIEW", color: "red" }, assigned: true }),
);
await attempt("assign-second", () =>
  client.setWorkspaceLabel({ workspaceId, label: { name: "Blocked", color: "amber" }, assigned: true }),
);
await settle();
await attempt("unassign-unknown", () =>
  client.setWorkspaceLabel({ workspaceId, label: { name: "Never made", color: "teal" }, assigned: false }),
);
await attempt("assign-empty-name", () =>
  client.setWorkspaceLabel({ workspaceId, label: { name: "   ", color: "teal" }, assigned: true }),
);
await attempt("assign-missing-workspace", () =>
  client.setWorkspaceLabel({ workspaceId: "wks_0000000000000000", label: { name: "X", color: "teal" }, assigned: true }),
);
await attempt("update-rename-recolour", () =>
  client.updateWorkspaceLabel({ name: "blocked", newName: "Urgent", color: "pink" }),
);
await attempt("update-collision", () =>
  client.updateWorkspaceLabel({ name: "Urgent", newName: "needs review" }),
);
await attempt("update-missing", () => client.updateWorkspaceLabel({ name: "nothing", color: "blue" }));
await attempt("update-noop", () =>
  client.updateWorkspaceLabel({ name: "urgent", newName: "Urgent", color: "pink" }),
);
await attempt("update-empty-new-name", () => client.updateWorkspaceLabel({ name: "Urgent", newName: " " }));
await settle();
await attempt("inspect", () => client.inspectWorkspaceLabelDelete({ name: "urgent" }));
await attempt("inspect-missing", () => client.inspectWorkspaceLabelDelete({ name: "nothing" }));
await attempt("delete", () => client.deleteWorkspaceLabel({ name: "URGENT" }));
await attempt("delete-missing", () => client.deleteWorkspaceLabel({ name: "nothing" }));
await settle();
await attempt("list", () => client.listWorkspaceLabels());
const sync = first ? { generation: first.sync.generation, afterSeq: first.sync.headSeq } : undefined;
await attempt("list-catch-up", () => client.listWorkspaceLabels({ sync }));
await attempt("list-stale-cursor", () =>
  client.listWorkspaceLabels({ sync: { generation: "expired", afterSeq: 1 } }),
);
await settle();

const keep = new Set([
  "workspace.label.list.response",
  "workspace.label.update",
  "workspace.label.assignment.set.response",
  "workspace.label.update.response",
  "workspace.label.delete.response",
  "workspace.label.delete.inspect.response",
  "rpc_error",
]);
for (const text of raw) {
  const frame = JSON.parse(text);
  const message = frame.type === "session" && frame.message ? frame.message : frame;
  const isServerInfo = message.type === "status" && message.payload?.status === "server_info";
  if (keep.has(message.type) || isServerInfo) console.log(text);
}
console.error(JSON.stringify(outcomes));
await client.close();
process.exit(0);
