const channel = new MessageChannel();
const worker = new Worker(new URL("./worker.mjs", import.meta.url), {
  type: "module",
});

worker.postMessage(channel.port2, [channel.port2]);
channel.port1.start();

// Connect an LSP client to this port. Each message is one JSON-RPC envelope
// encoded as a string or Uint8Array; worker-channel adds no byte framing.
export const lspPort = channel.port1;
