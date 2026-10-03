// Module loader hooks for terminal-emulator-capture.mjs. They give the pinned
// packages/server/dist/server/terminal/terminal.js two substitutes for what it
// imports, and nothing else:
//   - "node-pty" becomes terminal-emulator-fake-pty.mjs, a PTY that has no
//     process behind it and lets the capture feed output bytes;
//   - "@xterm/headless", when terminal.js asks for it, becomes
//     terminal-emulator-xterm-recorder.mjs, the same package with its Terminal
//     remembered so the capture can read the raw title events.

let urls = null;

export function initialize(data) {
  urls = data;
}

export function resolve(specifier, context, nextResolve) {
  if (specifier === "node-pty") {
    return { url: urls.fakePty, shortCircuit: true };
  }
  if (
    specifier === "@xterm/headless" &&
    context.parentURL?.endsWith("/dist/server/terminal/terminal.js")
  ) {
    return { url: urls.recorder, shortCircuit: true };
  }
  return nextResolve(specifier, context);
}
