import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const repository = path.resolve(import.meta.dirname, "../..");
const output = path.join(import.meta.dirname, "yomu.png");
const chrome =
  process.env.CHROME_BIN ??
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const profile = mkdtempSync(path.join(os.tmpdir(), "yomu-screenshot-"));

const theme = JSON.stringify({
  background: "#1e1e2e",
  foreground: "#cdd6f4",
  cursor: "#f5e0dc",
  black: "#45475a",
  red: "#f38ba8",
  green: "#a6e3a1",
  yellow: "#f9e2af",
  blue: "#89b4fa",
  magenta: "#f5c2e7",
  cyan: "#94e2d5",
  white: "#bac2de",
  brightBlack: "#585b70",
  brightRed: "#f38ba8",
  brightGreen: "#a6e3a1",
  brightYellow: "#f9e2af",
  brightBlue: "#89b4fa",
  brightMagenta: "#f5c2e7",
  brightCyan: "#94e2d5",
  brightWhite: "#a6adc8",
});

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
}

async function retry(operation, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await operation();
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  throw lastError ?? new Error("Operation timed out");
}

function connect(target) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(target);
    const pending = new Map();
    let nextId = 1;

    socket.addEventListener(
      "open",
      () => {
        resolve({
          close: () => socket.close(),
          send(method, params = {}) {
            return new Promise((commandResolve, commandReject) => {
              const id = nextId++;
              pending.set(id, {
                resolve: commandResolve,
                reject: commandReject,
              });
              socket.send(JSON.stringify({ id, method, params }));
            });
          },
        });
      },
      { once: true },
    );
    socket.addEventListener("error", reject, { once: true });
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const command = pending.get(message.id);
      if (!command) {
        return;
      }
      pending.delete(message.id);
      if (message.error) {
        command.reject(new Error(message.error.message));
      } else {
        command.resolve(message.result);
      }
    });
  });
}

function stop(child) {
  if (!child || child.exitCode !== null) {
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, 2_000);
    child.once("exit", () => {
      clearTimeout(timeout);
      resolve();
    });
    child.kill("SIGTERM");
  });
}

const [terminalPort, debuggingPort] = await Promise.all([
  reservePort(),
  reservePort(),
]);
const terminalUrl = `http://127.0.0.1:${terminalPort}`;

const terminal = spawn(
  "ttyd",
  [
    "--port",
    String(terminalPort),
    "--interface",
    "127.0.0.1",
    "-t",
    "rendererType=canvas",
    "-t",
    "enableSixel=true",
    "-t",
    "disableResizeOverlay=true",
    "-t",
    "disableReconnect=true",
    "-t",
    "fontSize=18",
    "-t",
    "fontFamily=Menlo",
    "-t",
    `theme=${theme}`,
    "--once",
    "--writable",
    "bash",
    "assets/screenshots/capture.sh",
  ],
  { cwd: repository, stdio: "ignore" },
);

let browser;
let protocol;
try {
  await retry(async () => {
    const response = await fetch(terminalUrl);
    if (!response.ok) {
      throw new Error(`ttyd returned HTTP ${response.status}`);
    }
  });

  browser = spawn(
    chrome,
    [
      "--headless=new",
      "--disable-background-networking",
      "--disable-component-update",
      "--no-first-run",
      `--user-data-dir=${profile}`,
      `--remote-debugging-port=${debuggingPort}`,
      "--window-size=1200,887",
      terminalUrl,
    ],
    { stdio: "ignore" },
  );

  const page = await retry(async () => {
    const pages = await fetch(
      `http://127.0.0.1:${debuggingPort}/json/list`,
    ).then((response) => response.json());
    const candidate = pages.find(
      (item) => item.type === "page" && item.url.startsWith(terminalUrl),
    );
    if (!candidate) {
      throw new Error("Chrome has not opened the ttyd page yet");
    }
    return candidate;
  });

  protocol = await connect(page.webSocketDebuggerUrl);
  await protocol.send("Page.enable");
  await new Promise((resolve) => setTimeout(resolve, 5_000));
  const { data } = await protocol.send("Page.captureScreenshot", {
    format: "png",
    fromSurface: true,
    captureBeyondViewport: false,
  });
  writeFileSync(output, Buffer.from(data, "base64"));
} finally {
  protocol?.close();
  await stop(browser);
  await stop(terminal);
  rmSync(profile, {
    recursive: true,
    force: true,
    maxRetries: 5,
    retryDelay: 100,
  });
}
