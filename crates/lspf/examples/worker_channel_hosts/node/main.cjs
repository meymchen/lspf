const path = require("node:path");
const { MessageChannel, Worker } = require("node:worker_threads");

function receive(target) {
  return new Promise((resolve, reject) => {
    target.once("message", resolve);
    target.once("messageerror", reject);
  });
}

async function request(port, message) {
  port.postMessage(JSON.stringify(message));
  const response = JSON.parse(await receive(port));
  if (response.id !== message.id || response.error) {
    throw new Error(`unexpected LSP response: ${JSON.stringify(response)}`);
  }
  return response.result;
}

async function main() {
  const worker = new Worker(path.join(__dirname, "worker.cjs"));
  const channel = new MessageChannel();
  const outcome = receive(worker);
  channel.port1.start();
  worker.postMessage(channel.port2, [channel.port2]);

  try {
    await request(channel.port1, {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: { capabilities: {}, processId: null, rootUri: null },
    });
    channel.port1.postMessage(JSON.stringify({
      jsonrpc: "2.0",
      method: "initialized",
      params: {},
    }));
    await request(channel.port1, {
      jsonrpc: "2.0",
      id: 2,
      method: "shutdown",
    });
    channel.port1.postMessage(JSON.stringify({
      jsonrpc: "2.0",
      method: "exit",
    }));

    const report = await outcome;
    if (report.error || report.code !== 0) {
      throw new Error(report.error ?? `unexpected exit code ${report.code}`);
    }
  } finally {
    channel.port1.close();
    await worker.terminate();
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
