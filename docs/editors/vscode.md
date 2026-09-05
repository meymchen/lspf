# VS Code quick start

Run the Markdown server with the language-client extension bundled in this
repository. You need VS Code, Rust 1.98+, and Node.js 24.

## Launch the bundled client

```bash
git clone https://github.com/meymchen/lspf.git
cd lspf
code .
```

In **Run and Debug**, select **Debug LSP client (Extension Host)** and press
**F5**. Its pre-launch task installs the locked client dependencies if needed,
compiles the extension, and builds `lspf-markdown`.

In the new **Extension Development Host** window, open
`editor-validation/fixture/readme.md` from this checkout. The bundled extension
starts the stdio server for Markdown documents.

## Try the language features

1. Open the Problems panel. The link to `missing.md` has a diagnostic from
   `lspf-markdown`.
2. Hover over the path `guide.md` inside the parentheses. The hover shows the
   resolved URI and `Validation guide` heading.
3. Put the cursor in that path and run **Go to Definition** from the context
   menu. VS Code opens the target heading.
4. Return to the fixture and replace `missing.md` with `guide.md`. The diagnostic
   clears after the edit.

## Use an installed server

The default launch builds and uses `target/debug/lspf-markdown`. To exercise
an installed copy, run:

```bash
cargo install --path crates/lspf-markdown --locked
export LSPF_MARKDOWN_SERVER="$HOME/.cargo/bin/lspf-markdown"
code .
```

Adjust the absolute path if Cargo uses another install directory. Close any
existing VS Code process before starting it from this shell so the Extension
Host receives the environment variable. Then use the same F5 launch.

For your own language server, start with the
[lspf VS Code extension template](https://github.com/meymchen/lspf-vscode-extension-template)
and set its server command and language selector for your language. The
[VS Code language server extension guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide)
explains how the client extension connects the editor to the server.

## If nothing happens

Check that you opened the file in the **Extension Development Host**, and that
its language mode is Markdown. Open **View → Output** and select the
`lspf-markdown` channel for traffic and startup errors. Build failures appear
in the pre-launch task's terminal.

Return to the [README](../../README.md), or see the
[test-client guide](../../tools/vscode-test-client/README.md) for debugging and
socket transport examples.
