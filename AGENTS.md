# AGENTS.md

Store scratch work in `.scratch/` and ask before deleting anything there.

## Releases

Never hand-edit the version in `Cargo.toml` or write entries into the
changelog. release-plz owns both: it opens the release pull request that bumps
the version and generates `crates/lspf/CHANGELOG.md` from the commits.

Write the explanation into the commit body instead. Use a conventional commit
subject, mark a breaking change with `!` (`refactor!: …`), and the changelog
template in `release-plz.toml` renders that body under the entry.

A breaking change needs its findings recorded in
`ci/public-api-breaking-approvals.json`. Run `bash ci/check-public-api.sh` and
paste the `approval candidate:` line it prints; the approval records the
reviewed findings, not a version.

## Agent skills

### Issue tracker

GitHub Issues at `meymchen/lspf`, managed via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default canonical names (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
