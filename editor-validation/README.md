# Editor validation

This directory contains the three editor journeys tracked by issue #188. They
all launch one installed `lspf-markdown` binary. The checked-in JSON is the
versioned, machine-readable journey definition; it does not claim that a human
has performed the UI checks.

## Install the server once

From the repository root:

```bash
install_root="$PWD/target/editor-validation-install"
cargo install --path crates/lspf-markdown --root "$install_root" --locked --force
export LSPF_MARKDOWN_SERVER="$install_root/bin/lspf-markdown"
export PATH="$install_root/bin:$PATH"
```

Keep those environment variables in the shell that starts the editor. On
Windows, use `lspf-markdown.exe` for the explicit path.

The fixture starts with one broken link and one valid link. Open
[`fixture/readme.md`](fixture/readme.md), then follow the seven steps for the
editor in [`journeys-v1.json`](journeys-v1.json).

## VS Code

Install the test-client dependencies and compile it:

```bash
npm --prefix tools/vscode-test-client ci
npm --prefix tools/vscode-test-client run compile
code .
```

Choose `Debug LSP client (Extension Host)`. The extension reads
`LSPF_MARKDOWN_SERVER`, launches that binary over stdio, and attaches only to
Markdown files. Restarting the Extension Development Host creates a new server
process; closing it sends the normal language-client shutdown sequence. Close
any existing VS Code process before running `code .` if it was started outside
the prepared shell.

## Neovim 0.11 or later

The config uses Neovim's built-in `vim.lsp.config` API. No plugin is needed:

```bash
nvim --clean -u editor-validation/neovim/init.lua \
  editor-validation/fixture/readme.md
```

Use the ordinary hover and definition mappings or run
`lua vim.lsp.buf.hover()` and `lua vim.lsp.buf.definition()`. The custom
commands `LspfMarkdownRestart` and `LspfMarkdownStop` cover restart and graceful
shutdown.

## Zed

Open Zed's Extensions view, choose `Install Dev Extension`, and select
`editor-validation/zed-extension`. The extension finds `lspf-markdown` on the
worktree shell path. If Zed was not started from the prepared shell, set the
same installed path in the worktree settings:

```json
{
  "lsp": {
    "lspf-markdown": {
      "binary": {
        "path": "/absolute/path/to/editor-validation-install/bin/lspf-markdown"
      }
    }
  }
}
```

Open the fixture and use Zed's hover, go-to-definition, and `language server:
restart` actions. Closing the workspace is the shutdown check.

## Evidence boundaries

Run the automated checks with:

```bash
cargo test -p lspf-markdown --test packaged_editor_journey
bash ci/test-check-editor-validation.sh
```

The Rust test drives the packaged stdio executable from outside the process. It
checks the protocol behavior shared by all three editor integrations, including
a second process start. The shell test checks the three configuration paths,
the required journey steps, evidence separation, and issue links for framework
gaps.

Human UI observations belong in a copy of
[`human-observations-template.md`](human-observations-template.md). Do not turn
an automated assertion into a UX observation. If a run exposes a framework
gap, open an issue first and add its URL to `frameworkGaps.tracked` in the JSON
manifest.
