// A node-pty stand-in for terminal-emulator-capture.mjs. spawn() returns an
// object with the surface terminal.js uses (pid, onData, onExit, write,
// resize, kill). Nothing runs: the capture calls emitData() with the bytes a
// shell would have printed and emitExit() when it is done, and reads what
// terminal.js wrote to the PTY from `written`.

export function spawn(file, args, options) {
  const dataListeners = new Set();
  const exitListeners = new Set();
  const fake = {
    pid: 4242,
    cols: options.cols,
    rows: options.rows,
    process: file,
    written: [],
    onData(listener) {
      dataListeners.add(listener);
      return { dispose: () => dataListeners.delete(listener) };
    },
    onExit(listener) {
      exitListeners.add(listener);
      return { dispose: () => exitListeners.delete(listener) };
    },
    write(data) {
      fake.written.push(data);
    },
    resize(cols, rows) {
      fake.cols = cols;
      fake.rows = rows;
    },
    kill() {},
    emitData(text) {
      for (const listener of [...dataListeners]) {
        listener(text);
      }
    },
    emitExit(event) {
      for (const listener of [...exitListeners]) {
        listener(event);
      }
    },
  };
  globalThis.__spockyFakePtys.push(fake);
  return fake;
}
