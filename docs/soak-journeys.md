# Bounded-memory soak journeys

The soak jobs drive real `Server` endpoints through the public in-memory
Transport. They run on pushes to `main` and keep their artifacts for 90 days.
The jobs are meant to catch growth and cleanup failures that short correctness
tests can miss. CI assigns each journey to its own matrix job so every hosted
runner finishes well inside its execution lifetime; the versioned workload
still gives every journey its full duration.

## Workload

[`soak/workloads-v1.json`](../soak/workloads-v1.json) is the versioned workload
definition. Each of these journeys runs for 60 seconds, for a total measured
duration of about seven minutes across the matrix:

- 32 concurrent requests whose handlers stay in flight for the full 60-second
  journey before responding normally;
- batches of 16 long-lived requests followed by `$/cancelRequest`;
- full-document edits against a tracked 1 MiB Document;
- work-done progress create, begin, report, and end cycles, eight at a time;
- bursts of 128 notifications while the peer accepts writes slowly;
- repeated Transport disconnects followed by new connections;
- repeated initialize, shutdown, and exit lifecycles.

The harness samples resident memory and live resource counts once per second.
Structured connection telemetry supplies the actual admitted-request,
Document, and outbound-queue counts. The harness also records live handler
tasks, progress handles, and connections. Traffic totals record operations and
payload bytes.

Every connection uses these limits:

| Resource | Limit |
| --- | ---: |
| Inbound requests | 64 |
| Outbound messages | 64 |
| Outbound bytes | 4 MiB |
| Documents | 4 |
| Document text | 4 MiB |
| Handler timeout | 120 seconds |
| Outbound request timeout | 30 seconds |

## Failure rules

[`soak/thresholds-v1.json`](../soak/thresholds-v1.json) allows at most 512 MiB
peak RSS and 32 MiB unexplained growth between the first and last sample of
any journey. Each journey must produce at least 30 samples. A 10-minute
watchdog stops each command if it hangs.

The run also fails when a journey crashes, returns an unexpected terminal
outcome, misses a required scenario, or finishes with a nonzero resource
count. A slow-peer journey must observe outbound overload. Edit journeys probe
the public Documents view after close. Progress journeys cancel each ended
token and fail if the connection registry still recognizes it. Request and
cancellation journeys verify that admitted requests and handler tasks return
to zero.

## Artifacts and local runs

Each matrix entry uploads a `bounded-memory-soak-<scenario>` artifact that
contains:

- the workload and threshold files used by the run;
- `time-series.jsonl` for incremental evidence, including partial samples if
  the process fails;
- `time-series.json`, `raw-results.json`, and the evaluated `results.json`;
- the readable `results.md` summary and exact `command.log`.

The harness rewrites `raw-results.json` after every completed journey. On a
crash or timeout, the runner combines those results with the last incremental
sample and records the interrupted journey's terminal outcome.

Run the same revision-locked job locally on Linux:

```bash
bash ci/run-soak-journeys.sh \
  "$(git rev-parse HEAD)" \
  target/bounded-memory-soak
```

The memory sampler reads `/proc/self/status`, so this job is Linux-only. Change
the workload and thresholds together, increment `workloadVersion`, and use new
versioned filenames when the workload contract changes.
