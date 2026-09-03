// A dependency-free LSP client that exercises the native TCP and WebSocket
// transport examples end to end.
//
//   node tools/lsp-transport-probe/main.mjs tcp
//   node tools/lsp-transport-probe/main.mjs websocket
//   node tools/lsp-transport-probe/main.mjs both
//   node tools/lsp-transport-probe/main.mjs tcp --attach
//
// For each transport the probe builds its example with only that adapter's
// feature, starts the server, runs one full LSP session against the handlers in
// `crates/lspf/examples/shared/mod.rs`, and checks every response. It exits
// non-zero when a check fails.
//
// `--attach` skips the build and spawn and connects to a server that is already
// listening, so a debugger can own the server process instead.
//
// Node's global `WebSocket` needs Node 22 or newer; the repository pins 24.

import net from "node:net";
import { spawn } from "node:child_process";
import { once } from "node:events";
import process from "node:process";

// Both addresses are hard-coded by their example's `serve` call. Keep these in
// step with `native_tcp.rs` and `native_websocket.rs`.
const TRANSPORTS = {
  tcp: { example: "native_tcp", feature: "tcp", host: "127.0.0.1", port: 9257 },
  websocket: {
    example: "native_websocket",
    feature: "websocket",
    host: "127.0.0.1",
    port: 9258,
  },
};

const CONNECT_TIMEOUT_MS = 30_000;
const SESSION_TIMEOUT_MS = 20_000;

// --- transport-independent JSON-RPC session -------------------------------

/// One in-flight LSP session over an already connected channel.
///
/// `channel` supplies `send(text)`, `close()`, and calls `onMessage(text)` for
/// each complete envelope. Framing lives in the channel, not here, which is
/// exactly the split the lspf Transport trait draws.
class Session {
  #channel;
  #pending = new Map();
  #nextId = 1;

  constructor(channel) {
    this.#channel = channel;
    channel.onMessage = (text) => {
      const message = JSON.parse(text);
      log("<--", message);
      const resolve = this.#pending.get(message.id);
      if (resolve) {
        this.#pending.delete(message.id);
        resolve(message);
      }
    };
  }

  /// Send a request and resolve with the whole response envelope.
  request(method, params) {
    const id = this.#nextId++;
    const envelope = { jsonrpc: "2.0", id, method };
    if (params !== undefined) envelope.params = params;
    const settled = new Promise((resolve) => this.#pending.set(id, resolve));
    this.#write(envelope);
    return settled;
  }

  notify(method, params) {
    const envelope = { jsonrpc: "2.0", method };
    if (params !== undefined) envelope.params = params;
    this.#write(envelope);
  }

  close() {
    this.#channel.close();
  }

  #write(envelope) {
    log("-->", envelope);
    this.#channel.send(JSON.stringify(envelope));
  }
}

function log(direction, message) {
  console.log(`  ${direction} ${JSON.stringify(message)}`);
}

// --- channels --------------------------------------------------------------

/// TCP carries `Content-Length` framed envelopes, the same framing as stdio.
async function connectTcp({ host, port }) {
  const socket = net.createConnection({ host, port });
  try {
    await once(socket, "connect");
  } catch (error) {
    socket.destroy();
    throw error;
  }
  socket.setNoDelay(true);
  // The server closes first after `exit`; that is the expected end of the
  // session, not a failure to report.
  socket.on("error", () => {});

  const channel = { onMessage: () => {} };
  let buffer = Buffer.alloc(0);

  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      const headerEnd = buffer.indexOf("\r\n\r\n");
      if (headerEnd < 0) return;
      const header = buffer.subarray(0, headerEnd).toString("ascii");
      const match = /content-length:\s*(\d+)/i.exec(header);
      if (!match) throw new Error(`framing has no Content-Length: ${header}`);
      const start = headerEnd + 4;
      const end = start + Number(match[1]);
      if (buffer.length < end) return;
      const body = buffer.subarray(start, end).toString("utf8");
      buffer = buffer.subarray(end);
      channel.onMessage(body);
    }
  });

  channel.send = (text) => {
    const body = Buffer.from(text, "utf8");
    socket.write(`Content-Length: ${body.length}\r\n\r\n`);
    socket.write(body);
  };
  channel.close = () => socket.end();
  return channel;
}

/// WebSocket carries one JSON envelope per message and adds no header.
async function connectWebSocket({ host, port }) {
  const socket = new WebSocket(`ws://${host}:${port}`);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", () => reject(new Error("connect failed")), {
      once: true,
    });
  });

  const channel = { onMessage: () => {} };
  socket.addEventListener("message", (event) => channel.onMessage(event.data));
  // The server closes first after `exit`, which surfaces here as an error
  // event. That is the expected end of the session.
  socket.addEventListener("error", () => {});
  channel.send = (text) => socket.send(text);
  channel.close = () => socket.close();
  return channel;
}

// --- the shared journey ----------------------------------------------------

const URI = "file:///probe.txt";

/// Drive the handler set every transport example registers, asserting the
/// result of each request.
async function runJourney(session, name) {
  const checks = [];

  const initialize = await session.request("initialize", {
    processId: null,
    rootUri: null,
    capabilities: {},
  });
  const capabilities = initialize.result?.capabilities ?? {};
  checks.push(["initialize advertises hover", capabilities.hoverProvider === true]);
  checks.push([
    "initialize advertises completion",
    capabilities.completionProvider !== undefined,
  ]);

  session.notify("initialized", {});
  session.notify("textDocument/didOpen", {
    textDocument: { uri: URI, languageId: "plaintext", version: 1, text: "hello" },
  });

  const hover = await session.request("textDocument/hover", {
    textDocument: { uri: URI },
    position: { line: 0, character: 0 },
  });
  checks.push(["hover returns the shared label", hover.result?.contents?.value === "shared"]);

  const completion = await session.request("textDocument/completion", {
    textDocument: { uri: URI },
    position: { line: 0, character: 0 },
  });
  const items = Array.isArray(completion.result)
    ? completion.result
    : (completion.result?.items ?? []);
  checks.push(["completion returns the shared item", items[0]?.label === "shared"]);

  // The custom request takes an object, as every LSP method does.
  const ping = await session.request("shared/ping", { message: name });
  checks.push([`shared/ping replies shared:${name}`, ping.result?.reply === `shared:${name}`]);

  // Neither method takes parameters, and the two spellings a client may use
  // for that must behave alike: `shutdown` omits `params` entirely while `exit`
  // sends an explicit null. A server that rejects the null never exits, which
  // the exit-code check below catches.
  const shutdown = await session.request("shutdown");
  checks.push(["shutdown succeeds", shutdown.error === undefined]);

  session.notify("exit", null);
  return checks;
}

// --- server lifecycle ------------------------------------------------------

/// Build one example with only its adapter feature and return the binary Cargo
/// produced, so the probe never guesses a target directory layout.
async function buildExample({ example, feature }) {
  const args = [
    "build",
    "-p",
    "lspf",
    "--example",
    example,
    "--no-default-features",
    "--features",
    feature,
    "--message-format=json-render-diagnostics",
  ];
  const cargo = spawn("cargo", args, { stdio: ["ignore", "pipe", "inherit"] });

  let stdout = "";
  cargo.stdout.setEncoding("utf8");
  cargo.stdout.on("data", (chunk) => (stdout += chunk));
  const [code] = await once(cargo, "exit");
  if (code !== 0) throw new Error(`cargo build exited ${code}`);

  let executable;
  for (const line of stdout.split("\n")) {
    if (!line.trim()) continue;
    const record = JSON.parse(line);
    if (record.reason === "compiler-artifact" && record.executable) {
      executable = record.executable;
    }
  }
  if (!executable) throw new Error(`cargo produced no binary for ${example}`);
  return executable;
}

/// Retry the real connection until the server has bound its port.
///
/// Each transport example binds once, accepts exactly one client, and then
/// drops its listener, so the probe cannot use a throwaway socket to poll for
/// readiness: that socket would be the one connection the server serves.
async function connectWithRetry(transport, connect, deadline) {
  for (;;) {
    try {
      return await connect(transport);
    } catch (error) {
      if (Date.now() > deadline) throw error;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
}

/// Run one transport's session.
///
/// By default the probe owns the server: it builds the example and starts it.
/// With `attach` it drives a server someone else started, which is how a
/// debugger session works — serve the example from a task, attach to it, then
/// run the probe against that process.
async function probe(name, { attach }) {
  const transport = TRANSPORTS[name];
  let server;
  let exited;

  if (attach) {
    console.log(`\n=== ${name}: attaching to ${transport.host}:${transport.port} ===`);
  } else {
    console.log(`\n=== ${name}: building ===`);
    const executable = await buildExample(transport);
    console.log(`=== ${name}: serving on ${transport.host}:${transport.port} ===`);
    server = spawn(executable, [], { stdio: ["ignore", "inherit", "inherit"] });
    exited = once(server, "exit");
  }

  try {
    const connect = name === "tcp" ? connectTcp : connectWebSocket;
    const channel = await connectWithRetry(
      transport,
      connect,
      Date.now() + CONNECT_TIMEOUT_MS,
    );
    const session = new Session(channel);
    const checks = await withTimeout(
      runJourney(session, name),
      SESSION_TIMEOUT_MS,
      `${name} session`,
    );
    session.close();

    if (exited) {
      const [code] = await withTimeout(exited, SESSION_TIMEOUT_MS, `${name} server exit`);
      checks.push(["server exits cleanly after exit", code === 0]);
    }

    let failed = 0;
    for (const [what, ok] of checks) {
      console.log(`  ${ok ? "ok  " : "FAIL"} ${what}`);
      if (!ok) failed++;
    }
    return failed === 0;
  } finally {
    if (server && server.exitCode === null) server.kill();
  }
}

function withTimeout(promise, ms, what) {
  return Promise.race([
    promise,
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error(`${what} timed out after ${ms}ms`)), ms).unref(),
    ),
  ]);
}

// --- entry point -----------------------------------------------------------

const args = process.argv.slice(2);
const attach = args.includes("--attach");
const selected = args.find((arg) => !arg.startsWith("--")) ?? "both";
const names = selected === "both" ? Object.keys(TRANSPORTS) : [selected];
if (!names.every((name) => name in TRANSPORTS)) {
  console.error("usage: node tools/lsp-transport-probe/main.mjs <tcp|websocket|both> [--attach]");
  process.exit(2);
}

let allPassed = true;
for (const name of names) {
  allPassed = (await probe(name, { attach })) && allPassed;
}
console.log(allPassed ? "\nall transport probes passed" : "\ntransport probes FAILED");
process.exit(allPassed ? 0 : 1);
