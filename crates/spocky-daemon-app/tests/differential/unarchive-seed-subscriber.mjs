// Seed recorder for the unarchive differential, run once against the pinned
// daemon: create a codex agent that runs the G1 prompt to the end, then
// archive it. The home the daemon leaves is what both sides start from.
const [, , paseoRoot, host, project] = process.argv;
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const client = await connectToDaemon({ target: { kind: "endpoint", host } });
const created = await client.createWorkspace({ source: { kind: "directory", path: project } });
const agent = await client.createAgent({
  provider: "codex",
  cwd: project,
  workspaceId: created.workspace.id,
  modeId: "full-access",
  initialPrompt: "Reply with the single word READY.",
});
const finished = await client.waitForFinish(agent.id, 120000);
const archived = await client.archiveAgent(agent.id);
console.error(`status=${finished.status} archivedAt=${Boolean(archived.archivedAt)}`);
await client.close();
process.exit(0);
