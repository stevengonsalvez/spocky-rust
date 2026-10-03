// The pinned @xterm/headless with its Terminal subclassed only to remember
// each instance in globalThis.__spockyTerminals. terminal.js constructs the
// terminal it parses with; the capture reads that instance's raw title events.

import { createRequire } from "node:module";

const serverRequire = createRequire(
  `${process.env.SPOCKY_CAPTURE_PASEO_ROOT}/packages/server/package.json`,
);
const real = serverRequire("@xterm/headless");

class RecordingTerminal extends real.Terminal {
  constructor(options) {
    super(options);
    globalThis.__spockyTerminals.push(this);
  }
}

export default { ...real, Terminal: RecordingTerminal };
