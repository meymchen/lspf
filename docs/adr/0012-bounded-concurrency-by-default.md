# Bounded concurrency by default

The default [[Default stack]] includes a concurrency layer capped at 64
in-flight requests. The cap is tunable
(`.layer(ConcurrencyLayer::new(N))`) and removable
(`.no_default_layers()`).

We rejected unbounded spawning — async-lsp's default — because heavy
editing fires hundreds of completion and semantic-token requests per
second, and even with quick cancellation a misbehaving or fast client
can pile up thousands of in-flight tasks and OOM the server. The "safer
default" trade has a real cost too: a slow handler will start queueing
work and clients will feel latency spikes once the cap is exceeded. We
accept that visibility (`tracing` spans on queuing) and a reasonable cap
catch this earlier than an OOM does.

64 is a round guess based on typical LSP traffic patterns; we expect to
revisit it once real benchmarks land.
