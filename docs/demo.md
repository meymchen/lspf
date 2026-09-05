# The 30-second Markdown demo

The [README demo](assets/lspf-markdown-demo.gif) uses `lspf-markdown` over stdio
in Neovim. It shows a missing local target diagnostic, hover with the target's
URI and heading, and navigation to that heading.

## Replay it yourself

Follow the [Neovim quick start](editors/neovim.md) to install the server and
open `editor-validation/fixture/readme.md`. Build and install before recording;
compilation time is outside the 30-second sequence.

| Time | Action | Visible result |
| --- | --- | --- |
| 0–3 s | Start Neovim with the bundled LSP configuration. | The editor is ready. |
| 3–8 s | Open `editor-validation/fixture/readme.md`. | Markdown with a missing local link. |
| 8–15 s | Put the cursor inside `missing.md`; run `:lua vim.diagnostic.open_float()`. | `local link target does not exist: missing.md`. |
| 15–22 s | Put the cursor inside `guide.md`; run `:lua vim.lsp.buf.hover()`. | `Validation guide` and the resolved file URI. |
| 22–30 s | Run `:lua vim.lsp.buf.definition()`. | `guide.md` opens at its first heading. |

Press `Ctrl-O` to return. As a follow-up, change `missing.md` to `guide.md` and
watch the diagnostic disappear after the incremental edit. The same fixture
works in [VS Code](editors/vscode.md) and [Zed](editors/zed.md).

## Regenerate the GIF

The checked-in GIF is a scripted capture of five real Neovim UI states, held
for the durations above. Captions and pacing are added by the recorder;
diagnostic text, the hover window, and navigation come from Neovim connected
to the actual server. It is not a continuous recording or a latency benchmark.

On Linux, install Neovim 0.11+, Python 3.10+, Pillow, and MessagePack, then run
from the repository root:

```bash
cargo build -p lspf-markdown --locked
python3 editor-validation/demo/record.py --output .scratch/lspf-markdown-demo.gif
```

The default font is DejaVu Sans Mono at
`/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf`. Pass `--font` with a local
monospace TrueType font if yours is elsewhere. Pass `--server` to capture an
installed binary instead of `target/debug/lspf-markdown`.

The recorder asserts the diagnostic message, hover contents, and destination
before writing the GIF. It uses the existing validation fixture and config,
leaves the fixture unchanged, and writes temporary editor state and rendered
PNG frames under a timestamped `.scratch/demo-*` directory. Keep those frames
for inspection. After reviewing the result, use
`--output docs/assets/lspf-markdown-demo.gif` to refresh the README asset.

The initial capture used Neovim 0.12.5 and the workspace `lspf-markdown` 1.0.0
binary. The recording script is
[`editor-validation/demo/record.py`](../editor-validation/demo/record.py).
The separate packaged protocol check is:

```bash
cargo test -p lspf-markdown --test packaged_editor_journey --locked
```

This demo does not change the human observation records in
[`editor-validation`](../editor-validation/README.md).
