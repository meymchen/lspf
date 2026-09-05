# Zed quick start

Run `lspf-markdown` using the development extension in this repository.
You need Zed and Rust 1.98+ installed through rustup; Zed builds the extension
as WebAssembly when you install it.

## Install the server and extension

```bash
git clone https://github.com/meymchen/lspf.git
cd lspf
cargo install --path crates/lspf-markdown --locked
```

In Zed's Extensions view, choose **Install Dev Extension** and select
`editor-validation/zed-extension` from this checkout. Open the repository
as a workspace, then open `editor-validation/fixture/readme.md`.

The extension finds `lspf-markdown` on the worktree's shell `PATH`. If it is
not there, merge this into the workspace's `.zed/settings.json`, replacing the
path with your installed binary:

```json
{
  "lsp": {
    "lspf-markdown": {
      "binary": {
        "path": "/absolute/path/to/.cargo/bin/lspf-markdown"
      }
    }
  }
}
```

The extension registers the server for Markdown. The `binary.path` setting
selects its executable after that registration. See Zed's
[development extension instructions](https://zed.dev/docs/extensions/developing-extensions)
and [language server configuration](https://zed.dev/docs/configuring-languages).

## Try the language features

1. Inspect the diagnostic on `missing.md`; its source is `lspf-markdown`.
2. Hover over `guide.md` inside the parentheses to see the resolved URI and
   `Validation guide` heading.
3. Run **Go to Definition** on that path to open the target heading.
4. Return and change `missing.md` to `guide.md`; the diagnostic clears after
   the edit.

Use **language server: restart** from the command palette to restart the
server. Closing the workspace ends the connection.

## Connect your own server

Use the bundled extension as a small example of Zed's language-server
registration. Adapt its `extension.toml` language mapping and the executable
returned by `language_server_command` to your server. The
[Zed language extension guide](https://zed.dev/docs/extensions/languages)
describes the registration contract.

## If nothing happens

Confirm that the development extension installed successfully and that the
fixture is recognized as Markdown. Check the configured binary path, then
restart the language server. If you have disabled language servers for
Markdown in your settings, enable them again. Zed's LSP logs help distinguish
extension build failures from server startup errors.

Return to the [README](../../README.md), or follow the
[full editor validation journey](../../editor-validation/README.md).
