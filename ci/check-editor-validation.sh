#!/usr/bin/env bash

set -euo pipefail

repo_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
manifest="$repo_root/editor-validation/journeys-v1.json"

[[ -f "$manifest" ]] || {
    echo "missing editor journey manifest: $manifest" >&2
    exit 1
}

jq -e '
  .schemaVersion == 1
  and .server.command == "lspf-markdown"
  and .server.pathEnvironment == "LSPF_MARKDOWN_SERVER"
  and ([.editors[].id] | sort == ["neovim", "vscode", "zed"])
  and (all(.editors[]; .maintained == true))
  and (all(.editors[];
    ([.journey[].action] | sort)
      == ["definition", "diagnostics", "edit", "hover", "open", "restart", "shutdown"]
  ))
  and .automatedEvidence.kind == "machine"
  and (.automatedEvidence.assertions | length > 0)
  and .humanUxObservations.kind == "human"
  and (.humanUxObservations.status == "pending" or .humanUxObservations.status == "recorded")
  and ((.frameworkGaps.untracked // []) | length == 0)
  and all(.frameworkGaps.tracked[]?;
    (.issue | test("^https://github.com/meymchen/lspf/issues/[0-9]+$"))
  )
' "$manifest" >/dev/null

while IFS= read -r relative_path; do
    [[ -f "$repo_root/$relative_path" ]] || {
        echo "missing editor validation file: $relative_path" >&2
        exit 1
    }
done < <(jq -r '.editors[].configurationFiles[]' "$manifest")

grep -q 'LSPF_MARKDOWN_SERVER' \
    "$repo_root/tools/vscode-test-client/src/serverPath.ts"
grep -q "vim.lsp.config('lspf_markdown'" \
    "$repo_root/editor-validation/neovim/init.lua"
grep -q 'worktree.which("lspf-markdown")' \
    "$repo_root/editor-validation/zed-extension/src/lib.rs"
grep -q '\[language_servers.lspf-markdown\]' \
    "$repo_root/editor-validation/zed-extension/extension.toml"

echo "editor validation manifest verified"
