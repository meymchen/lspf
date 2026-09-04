# Protocol fuzzing

The fuzz suite exercises the JSON-RPC and transport boundaries plus the
document and endpoint transformations named in issue #183. It requires a
nightly Rust toolchain and `cargo-fuzz`.

## Budgets and limits

The scheduled gate runs every target with the following fixed limits. Maximum
input length bounds allocation driven by one generated input; timeout is the
libFuzzer per-input hang threshold; budget is the total run time per target.

| Target | Maximum input | Timeout | Scheduled budget |
| --- | ---: | ---: | ---: |
| `envelope` | 65536 | 5 s | 300 s |
| `content-length` | 65536 | 5 s | 300 s |
| `uri-identity` | 4096 | 5 s | 300 s |
| `position-conversion` | 65536 | 5 s | 300 s |
| `incremental-edits` | 65536 | 5 s | 300 s |
| `notebook-cell-sync` | 4096 | 5 s | 300 s |
| `lifecycle-sequences` | 16384 | 10 s | 300 s |

Each `corpus/<target>/` directory contains at least one valid and one malformed
seed. The Content-Length seeds use visible `\r\n` escapes, which that target
expands to wire CRLF before decoding. Run a target locally with:

```console
cargo +nightly fuzz run envelope fuzz/corpus/envelope -- \
  -max_len=65536 -timeout=5 -max_total_time=300
```

## Reproducing failures

`ci/run-fuzz.sh` leaves libFuzzer's original artifact in
`fuzz/artifacts/<target>/` and asks libFuzzer to create an exact `.minimized`
sibling when the failure is reproducible. The runner continues through all seven
targets, then reports an aggregate failure. The scheduled workflow uploads the
artifact directory when the run fails. Reproduce a retained input with:

```console
cargo +nightly fuzz run envelope fuzz/artifacts/envelope/<artifact>
```

Minimize it again with:

```console
cargo +nightly fuzz tmin envelope fuzz/artifacts/envelope/<artifact> -- -timeout=5
```
