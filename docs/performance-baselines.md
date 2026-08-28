# Performance baselines

The performance gate runs fixed, versioned workloads against an optimized
build of `lspf`. It is a coarse regression alarm, not a substitute for a
profiler. The budget leaves room for shared GitHub runners while still catching
large changes in latency, throughput, or memory use.

## Workloads

[`performance/workloads-v1.json`](../performance/workloads-v1.json) defines the
workload counts and limits. Workload version 1 measures:

- startup across 50 complete initialize, shutdown, and exit journeys;
- request p95 and p99 latency after a warmup, plus throughput over 10,000
  request-response operations;
- 100 incremental edits to a 1 MiB open Document, with each edit followed by a
  request that confirms the new Document version;
- 128 outbound notifications with an eight-message queue and a peer that takes
  5 ms to accept each message.

The slow-peer run reports accepted, overloaded, and delivered messages. At
least one overload is required, which proves that the configured queue limit
remains observable instead of turning into unbounded buffering.

## Results and environment

The runner writes `raw-results.json`, `results.json`, and `results.md`.
`results.json` includes latency, operations per second, peak RSS, slow-peer
limit behavior, every budget check, and the overall result. Peak RSS comes from
Linux `/proc/self/status`, so the CI gate runs on Ubuntu.

The result schema and environment metadata both have explicit version fields.
Environment metadata records the operating system, architecture, logical CPU
count, Rust compiler version, and Cargo profile. These fields make two reports
comparable without pretending that results from different machines are the
same baseline.

## Regression budget

[`performance/regression-budget-v1.json`](../performance/regression-budget-v1.json)
contains the limits. The `Reproducible performance baseline` CI job runs on
pushes to `main`, fails when a limit is crossed, and retains the JSON and
Markdown report for 90 days. Gate A depends on this job.

Run the same gate locally with a full 40-character revision:

```bash
bash ci/run-performance-baseline.sh \
  "$(git rev-parse HEAD)" \
  target/performance-baseline
```

Change a workload and its budget together. Increment `workloadVersion`, create
new versioned filenames, and update the runner defaults and this document in
the same change. A budget-only relaxation needs measured evidence in the pull
request because it changes what the release gate accepts.
