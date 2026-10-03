// Electron 44.2.0 host for renderer parity captures. It opens one frameless
// 1280x800 content-size window with no application menu, then
// waits for renderer-platform-cdp-capture.cjs (which sets viewport and scale
// factor 1 through Playwright page emulation, as the Chrome baseline does) to drive it over the DevTools
// port. It exits on its own after the bound so an orphan cannot linger.
const { app, BrowserWindow, Menu } = require("electron");

const port = process.env.CDP_PORT;
if (!port || port === "6767") {
  process.stderr.write("CDP_PORT must be set and must not be 6767\n");
  process.exit(2);
}
app.commandLine.appendSwitch("remote-debugging-port", port);
app.commandLine.appendSwitch("remote-debugging-address", "127.0.0.1");
app.commandLine.appendSwitch("lang", "en-US");

app.whenReady().then(async () => {
  Menu.setApplicationMenu(null);
  const window = new BrowserWindow({
    width: 1280,
    height: 800,
    useContentSize: true,
    show: true,
    frame: false,
    backgroundColor: "#ffffff",
    webPreferences: { contextIsolation: true, nodeIntegration: false },
  });
  await window.loadURL("about:blank");
  setTimeout(() => app.quit(), Number(process.env.HOST_BOUND_MS ?? 900_000));
});
app.on("window-all-closed", () => app.quit());
