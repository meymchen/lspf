const { parentPort } = require("node:worker_threads");
const { serve } = require("./pkg/worker_channel.js");

parentPort.once("message", async (port) => {
  try {
    const code = await serve(port);
    parentPort.postMessage({ code });
  } catch (error) {
    parentPort.postMessage({ error: error?.stack ?? String(error) });
  } finally {
    parentPort.close();
  }
});
