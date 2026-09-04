# Contributing to lspf

Issues and pull requests are welcome. Use the
[issue tracker](https://github.com/meymchen/lspf/issues) for bug reports and
feature proposals. For a larger change, open an issue before investing in an
implementation so its scope and public API can be discussed first.

## Set up the repository

Clone the repository and work from its root. `rust-toolchain.toml` installs the
Rust version and components used by CI. Documentation work also needs Node.js
24.

Install the pre-commit and commit-message hooks with
[prek](https://prek.j178.dev/):

```bash
prek install
```

The hooks run rustfmt and Clippy for staged Rust changes and validate commit
messages. Run them across all tracked files at any time:

```bash
prek run --all-files
```

## Read the project context

Read [`CONTEXT.md`](./CONTEXT.md) before changing the framework. It defines the
terms used in code, issues, and reviews. Check the relevant
[architecture decision records](./docs/adr/) when a change touches ownership,
lifecycle, concurrency, protocol behavior, or the public API.

An accepted ADR records a decision; it does not prove that the implementation
already exists. If a change reverses a decision, explain why in the pull
request and add or supersede an ADR.

The [frozen public interface](./docs/public-interface.md) records the 1.0
compatibility boundary. For a breaking change, run:

```bash
bash ci/check-public-api.sh
```

Copy the printed `approval candidate:` record into
`ci/policy/public-api-breaking-approvals.json` after reviewing the findings.

## Make a focused change

Keep protocol ownership in the framework and application policy in handlers or
the host. Add tests at the narrowest level that proves the behavior. Public
examples and guides should compile against the API they describe.

Developer-facing documentation belongs in `website/src/content/docs/`. English
is the canonical source, and `zh-cn` mirrors its routes and heading structure.
Repository maintenance material, ADRs, evidence descriptions, and performance
baselines stay under `docs/`.

Do not edit crate versions or `crates/lspf/CHANGELOG.md`. release-plz updates
both in a release pull request.

## Run checks

Run the native workspace test surface:

```bash
cargo test-native
```

For documentation examples, run:

```bash
cargo test --workspace --features stdio,tcp,websocket,testing --doc
```

The installed hooks cover formatting and Clippy. Markdown changes use the
repository's pinned linter:

```bash
npx --yes markdownlint-cli2@0.22.1
```

It can fix most mechanical Markdown issues:

```bash
npx --yes markdownlint-cli2@0.22.1 --fix
```

For website changes, install the locked dependencies and run both checks:

```bash
npm --prefix website ci
npm --prefix website run check
npm --prefix website run build
```

To generate local HTML coverage, install the pinned tool and use the Cargo
alias:

```bash
cargo install cargo-llvm-cov --version 0.9.0 --locked
cargo coverage
```

Open `target/coverage/html/index.html` after the command finishes. CI also
uploads a coverage artifact for pull requests and pushes to `main`.

## Debug a server

The checked-in VS Code configuration provides these useful entries:

- `Debug LSP client (Extension Host)` builds `lspf-hello` and opens an
  Extension Development Host.
- `Run LSP example client (select example)` starts one framework example
  behind the bundled test client.
- `Attach to running LSP server/example` attaches CodeLLDB to the process
  started by either client configuration.

Open a `.txt` file in the Extension Development Host to exercise the server.
For an example, attach to the matching process and set breakpoints in
`crates/lspf/examples/<name>.rs`.

The `.zed/tasks.json` file has build, quick-test, full-test, and example tasks.
`.zed/debug.json` can attach CodeLLDB to a server that was started by another
LSP client. These files support repository debugging; they do not register a
new Zed language server.

Set `RUST_LOG=lspf=trace` for framework events. The example programs default to
JSON on stderr; set `LSPF_LOG_FORMAT=text` for plain text. The
[operations guide](https://meymchen.github.io/lspf/en/guides/operations/#emit-useful-payload-free-telemetry)
lists the event fields and redaction rules.

## Commit and open a pull request

Use a Conventional Commit subject. Add a body only when the subject does not
explain why the change is needed. Mark a breaking change with `!`, for example
`refactor!: replace the transport split contract`, and state the impact and
migration in its body.

Keep implementation notes, test results, and development history in the pull
request description. Before requesting review, confirm that the relevant local
checks pass and that the pull request does not include generated build output.
