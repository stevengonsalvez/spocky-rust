// Drives the pinned ClaudeAgentClient with a scripted Query and prints one
// ordered log: every stream event the session emits, every call it makes on
// the query, and the result of every step. The Rust test drives
// ClaudeClient with the same scenarios and the same log format.
//
// argv: <dist> <scenario file>. Output lines:
//   EVENT <json>   a stream event
//   CALL <text>    a call on the scripted query
//   RESULT <json>  the outcome of a step (resolved value or ERROR <message>)
import { readFileSync } from "node:fs";

const [dist, scenarioFile] = process.argv.slice(1);
const scenario = JSON.parse(readFileSync(scenarioFile, "utf8"));
const { ClaudeAgentClient } = await import(`${dist}/server/agent/providers/claude/agent.js`);

const SETTLE_MS = 60;
const REACTION_GAP_MS = 5;
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
const log = [];
// RESULT lines are flushed, sorted, at the end of each `wait` step and of the
// scenario: the Rust session lives on its own thread, so the order of a reply
// against the session's own events, and of replies among themselves, is not a
// contract and is not compared. EVENT and CALL order is.
const results = [];
const flushResults = () => log.push(...results.splice(0).sort());
const normalize = (text) => text.replace(UUID, "<uuid>");
const put = (kind, value) =>
  (kind === "RESULT" ? results : log).push(`${kind} ${normalize(typeof value === "string" ? value : JSON.stringify(value))}`);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const quietLogger = () => {
  const logger = {};
  for (const level of ["trace", "debug", "info", "warn", "error", "fatal"]) logger[level] = () => {};
  logger.child = () => logger;
  return logger;
};

function createQueue() {
  const items = [];
  const resolvers = [];
  let ended = false;
  return {
    push(value) {
      if (ended) return;
      const resolve = resolvers.shift();
      if (resolve) resolve({ value, done: false });
      else items.push(value);
    },
    next() {
      if (items.length > 0) return Promise.resolve({ value: items.shift(), done: false });
      if (ended) return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve) => resolvers.push(resolve));
    },
    end() {
      ended = true;
      while (resolvers.length > 0) resolvers.shift()({ value: undefined, done: true });
    },
  };
}

function promptText(message) {
  const content = message?.message?.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content.map((block) => (typeof block?.text === "string" ? block.text : `<${block?.type}>`)).join("|");
}

const queries = [];
let promptCount = 0;

function makeQuery(input) {
  const frames = createQueue();
  const index = queries.length;
  const query = {
    frames,
    options: input.options,
    next: () => frames.next(),
    [Symbol.asyncIterator]() {
      return this;
    },
    return: async () => {
      put("CALL", `return#${index}`);
      frames.end();
      return { value: undefined, done: true };
    },
    close: () => {
      put("CALL", `close#${index}`);
      frames.end();
    },
    interrupt: async () => put("CALL", `interrupt#${index}`),
    setPermissionMode: async (mode) => put("CALL", `setPermissionMode#${index} ${mode}`),
    setModel: async (model) => put("CALL", `setModel#${index} ${model}`),
    applyFlagSettings: async (settings) => put("CALL", `applyFlagSettings#${index} ${JSON.stringify(settings)}`),
    supportedCommands: async () => scenario.commands ?? [],
    rewindFiles: async (id, options) => {
      put("CALL", `rewindFiles#${index} ${id} ${JSON.stringify(options)}`);
      const reply = scenario.rewindReplies?.[id];
      if (reply === "throw") throw new Error("rewind failed");
      return reply ?? { canRewind: true, filesChanged: ["a.ts"], insertions: 1, deletions: 2 };
    },
    cancelAsyncMessage: async (uuid) => {
      put("CALL", `cancelAsyncMessage#${index} ${uuid}`);
      return true;
    },
  };
  queries.push(query);
  put("CALL", `query#${index} ${JSON.stringify({
    resume: input.options.resume,
    model: input.options.model,
    permissionMode: input.options.permissionMode,
  })}`);
  (async () => {
    for await (const message of input.prompt) {
      put("CALL", `prompt#${index} ${promptText(message)}`);
      const reaction = scenario.onPrompt?.[promptCount];
      promptCount += 1;
      for (const frame of reaction ?? []) {
        await sleep(REACTION_GAP_MS);
        frames.push(frame);
      }
    }
  })();
  return query;
}

const client = new ClaudeAgentClient({
  logger: quietLogger(),
  queryFactory: makeQuery,
  resolveBinary: async () => "/usr/bin/claude-scripted",
});

let session = null;
const pendingCalls = [];
const stepResult = async (promise) => {
  try {
    put("RESULT", (await promise) ?? null);
  } catch (error) {
    put("RESULT", `ERROR ${error instanceof Error ? error.message : String(error)}`);
  }
};

for (const step of scenario.steps) {
  switch (step.op) {
    case "create":
      await stepResult(
        client.createSession(scenario.config).then((created) => {
          session = created;
          session.subscribe((event) => put("EVENT", event));
          return { id: session.id };
        }),
      );
      break;
    case "startTurn":
      await stepResult(session.startTurn(step.prompt, step.options));
      break;
    case "run":
      await stepResult(session.run(step.prompt, step.options));
      break;
    case "emit":
      queries.at(-1).frames.push(step.message);
      break;
    case "emitEnd":
      queries.at(-1).frames.end();
      break;
    case "wait":
      await sleep(step.ms);
      break;
    case "interrupt":
      await stepResult(session.interrupt());
      break;
    case "steer":
      await stepResult(session.steerActiveTurn(step.prompt, step.options));
      break;
    case "setMode":
      await stepResult(session.setMode(step.mode));
      break;
    case "setModel":
      await stepResult(session.setModel(step.model));
      break;
    case "setThinking":
      await stepResult(session.setThinkingOption(step.option));
      break;
    case "setFeature":
      await stepResult(session.setFeature(step.id, step.value));
      break;
    case "canUseTool": {
      const options = queries.at(-1).options;
      const controller = new AbortController();
      pendingCalls.push(controller);
      const call = options.canUseTool(step.toolName, step.input, {
        signal: controller.signal,
        suggestions: step.suggestions,
        toolUseID: step.toolUseID,
      });
      call.then(
        (value) => put("RESULT", `canUseTool ${JSON.stringify(value)}`),
        (error) => put("RESULT", `canUseTool ERROR ${error.message}`),
      );
      break;
    }
    case "abortCanUseTool":
      pendingCalls[step.index].abort();
      break;
    case "respondPermission": {
      const pending = session.getPendingPermissions();
      await stepResult(session.respondToPermission(pending[step.index].id, step.response));
      break;
    }
    case "pending":
      put("RESULT", session.getPendingPermissions());
      break;
    case "state":
      put("RESULT", {
        id: session.id,
        mode: await session.getCurrentMode(),
        modes: (await session.getAvailableModes()).map((mode) => mode.id),
        persistence: session.describePersistence(),
        runtime: await session.getRuntimeInfo(),
        features: session.features,
      });
      break;
    case "listCommands":
      await stepResult(session.listCommands());
      break;
    case "history": {
      const events = [];
      for await (const event of session.streamHistory()) events.push(event);
      put("RESULT", events);
      break;
    }
    case "revertFiles":
      await stepResult(session.revertFiles({ messageId: step.messageId }));
      break;
    case "revertConversation":
      await stepResult(session.revertConversation({ messageId: step.messageId }));
      break;
    case "close":
      await stepResult(session.close());
      break;
    default:
      throw new Error(`unknown op ${step.op}`);
  }
  await sleep(SETTLE_MS);
  if (step.op === "wait") flushResults();
}
flushResults();
process.stdout.write(log.join("\n") + "\n");
process.exit(0);
