import init, { serve } from "./pkg/worker_channel.js";

self.onmessage = async ({ data: port }) => {
  self.onmessage = null;
  await init();
  await serve(port);
  self.close();
};
