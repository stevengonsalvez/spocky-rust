// Scoped comparison for the unarchive differential, class
// pinned-reply-interleave: the one frame whose place the pinned daemon itself
// varies is the send_agent_message_response, and only relative to the turn's
// last two agent_update frames (running, idle).
//
// Pinned evidence (separate full runs of this differential): the reply came
// after 6 of the 8 updates (before the last two) in 5 runs, and after all 8 in
// 1 run; after exactly 7 was never seen. So the allowed places are N-2 and N
// updates, where N is the number of agent_update frames.
//
// Every frame is compared in exact arrival order, byte for byte: with the reply
// removed the two sequences must be identical, the reply frame must be
// identical, it must come before every later response (the same number of
// non-update frames precede it on both sides), and its place among the
// updates must be one of the allowed two on both sides.
import { readFileSync } from "node:fs";

const [, , originalPath, spockyPath] = process.argv;
const REPLY = "send_agent_message_response";

const load = (path) =>
  readFileSync(path, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((text) => {
      const frame = JSON.parse(text);
      const message = frame.type === "session" && frame.message ? frame.message : frame;
      return { text, type: message.type };
    });

const describe = (frames) => {
  const at = frames.findIndex((frame) => frame.type === REPLY);
  const updates = frames.filter((frame) => frame.type === "agent_update").length;
  return {
    at,
    updates,
    rest: frames.filter((_, index) => index !== at).map((frame) => frame.text),
    reply: at < 0 ? null : frames[at].text,
    updatesBefore: frames.slice(0, Math.max(at, 0)).filter((f) => f.type === "agent_update").length,
    othersBefore: frames.slice(0, Math.max(at, 0)).filter((f) => f.type !== "agent_update").length,
  };
};

const original = describe(load(originalPath));
const spocky = describe(load(spockyPath));
const fail = (message) => {
  console.log(`FAIL: ${message}`);
  process.exit(1);
};
if (original.at < 0 || spocky.at < 0) fail(`no ${REPLY} frame (pinned ${original.at}, spocky ${spocky.at})`);
if (original.rest.length !== spocky.rest.length || original.rest.some((t, i) => t !== spocky.rest[i])) {
  fail("frames other than the send reply differ in content or order");
}
if (original.reply !== spocky.reply) fail("the send reply frame differs");
const allowed = [original.updates - 2, original.updates];
const place = (side, d) => {
  if (!allowed.includes(d.updatesBefore)) {
    fail(`${side} sent the reply after ${d.updatesBefore} of ${d.updates} updates; allowed ${allowed.join(" or ")}`);
  }
};
place("pinned", original);
place("spocky", spocky);
if (original.othersBefore !== spocky.othersBefore) fail("the reply is not in the same place among the other responses");
console.log(
  `PASS: frame sequences match byte for byte in arrival order, with the send reply allowed after ${allowed.join(" or ")} of ${original.updates} updates (pinned ${original.updatesBefore}, spocky ${spocky.updatesBefore})`,
);
