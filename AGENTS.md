# AGENTS.md

Store scratch work in `.scratch/` and ask before deleting anything there.

## Releases

Never hand-edit the version in `Cargo.toml` or write entries into the
changelog. release-plz owns both: it opens the release pull request that bumps
the version and generates `crates/lspf/CHANGELOG.md` from the commits.

`ci/check-changelog.sh` holds that generated file to two properties: it carries
an entry for the version in `Cargo.toml`, and no prose in it repeats the
unreleased heading, which release-plz would otherwise split the file on.

Use a Conventional Commit subject. Leave the body empty when the subject is
self-explanatory; otherwise use at most 500 characters to explain why, with at
most 1000 explanatory characters across a pull request. Keep implementation
details, test evidence, and development history in the pull request body. Final
Git trailers do not count toward these limits.

Mark a breaking change with `!` (`refactor!: …`) and give it a concise body
that states the impact and migration. The changelog template in
`release-plz.toml` renders that body under the entry.

A breaking change needs its findings recorded in
`ci/policy/public-api-breaking-approvals.json`. Run `bash ci/check-public-api.sh` and
paste the `approval candidate:` line it prints; the approval records the
reviewed findings, not a version.

## CI scripts

`ci/` holds the scripts the workflows run: `check-*` verifies, `run-*` produces
evidence, `prepare-*` assembles artifacts. `ci/policy/` holds the checked-in
expectations those checks compare against.

Tests live under `ci/tests/` and carry no `test-` prefix, so a test's path names
its subject:

- `ci/tests/unit/NAME.sh` tests `ci/NAME.sh`
- `ci/tests/workflow/NAME.sh` asserts the structure of a workflow YAML
- `ci/tests/system/NAME.sh` drives a real bench or toolchain

`ci/test-coverage-*.sh` are libraries about test coverage, not tests.

Every test resolves the repo root with
`cd "$(dirname "${BASH_SOURCE[0]}")/../../.."` and refers to other files by a
path from there, so a moved test needs that depth updated.

## Agent skills

### Issue tracker

GitHub Issues at `meymchen/lspf`, managed via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default canonical names (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
