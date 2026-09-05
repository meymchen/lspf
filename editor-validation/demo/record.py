#!/usr/bin/env python3
"""Capture real Neovim UI states driven by the Markdown server into a 30s GIF."""

import argparse
import os
from pathlib import Path
import select
import subprocess
import time

import msgpack
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
COLS, ROWS = 96, 21
CELL_W, CELL_H = 11, 24


class Neovim:
    def __init__(self, scratch, server):
        env = dict(os.environ, LSPF_MARKDOWN_SERVER=str(server))
        for name in ('XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
            env[name] = str(scratch / name.lower())
        self.process = subprocess.Popen(
            ['nvim', '--embed', '--clean', '-u', 'editor-validation/neovim/init.lua'],
            cwd=ROOT, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=(scratch / 'neovim-stderr.log').open('wb'),
        )
        self.unpacker = msgpack.Unpacker(raw=False)
        self.next_id = 0
        self.responses = {}
        self.grid = [[(' ', 0) for _ in range(COLS)] for _ in range(ROWS)]
        self.highlights = {0: {}}
        self.foreground, self.background = 0xD8DEE9, 0x18202D
        self.cursor = (0, 0)
        self.call('nvim_ui_attach', [COLS, ROWS, {'rgb': True, 'ext_linegrid': True}])
        self.lua("""
            vim.o.termguicolors = true
            vim.o.number = true
            vim.o.swapfile = false
            vim.o.shadafile = 'NONE'
            vim.o.laststatus = 2
            vim.o.statusline = ' lspf-markdown  |  %f %m%=%l:%c '
            vim.o.shortmess = vim.o.shortmess .. 'I'
            vim.cmd('colorscheme habamax')
            vim.api.nvim_set_hl(0, 'NormalFloat', {fg = '#dce4ef', bg = '#263343'})
            vim.api.nvim_set_hl(0, 'FloatBorder', {fg = '#81c8be', bg = '#263343'})
            vim.api.nvim_set_hl(0, 'Comment', {fg = '#b7c4d8'})
            vim.api.nvim_set_hl(0, 'DiagnosticFloatingError', {fg = '#ff8796', bg = '#263343'})
            vim.diagnostic.config({ virtual_text = false, signs = true })
        """)

    def poll(self, timeout=0.1):
        if not select.select([self.process.stdout], [], [], timeout)[0]:
            return
        data = os.read(self.process.stdout.fileno(), 65536)
        if not data:
            raise RuntimeError('Neovim exited before capture completed')
        self.unpacker.feed(data)
        for message in self.unpacker:
            if message[0] == 1:
                self.responses[message[1]] = message[2:]
            elif message[0] == 2 and message[1] == 'redraw':
                for event in message[2]:
                    for args in event[1:]:
                        self.redraw(event[0], args)

    def redraw(self, event, args):
        if event == 'default_colors_set':
            self.foreground, self.background = args[:2]
        elif event == 'hl_attr_define':
            self.highlights[args[0]] = args[1]
        elif event == 'grid_resize':
            _, width, height = args
            self.grid = [[(' ', 0) for _ in range(width)] for _ in range(height)]
        elif event == 'grid_clear':
            self.grid = [[(' ', 0) for _ in row] for row in self.grid]
        elif event == 'grid_cursor_goto':
            self.cursor = tuple(args[1:3])
        elif event == 'grid_line':
            _, row, column, cells, *_ = args
            highlight = 0
            for cell in cells:
                char = cell[0]
                if len(cell) > 1:
                    highlight = cell[1]
                for _ in range(cell[2] if len(cell) > 2 else 1):
                    self.grid[row][column] = (char, highlight)
                    column += 1
        elif event == 'grid_scroll':
            _, top, bottom, left, right, rows, cols = args
            old = [row[:] for row in self.grid]
            for row in range(top, bottom):
                for col in range(left, right):
                    source_row, source_col = row + rows, col + cols
                    self.grid[row][col] = (
                        old[source_row][source_col]
                        if top <= source_row < bottom and left <= source_col < right
                        else (' ', 0)
                    )

    def call(self, method, params):
        self.next_id += 1
        request_id = self.next_id
        self.process.stdin.write(msgpack.packb([0, request_id, method, params]))
        self.process.stdin.flush()
        deadline = time.monotonic() + 15
        while request_id not in self.responses:
            if time.monotonic() > deadline:
                raise TimeoutError(method)
            self.poll()
        error, result = self.responses.pop(request_id)
        if error:
            raise RuntimeError(error)
        return result

    def lua(self, code):
        return self.call('nvim_exec_lua', [code, []])

    def settle(self):
        self.call('nvim_command', ['redraw!'])
        deadline = time.monotonic() + 0.4
        while time.monotonic() < deadline:
            self.poll(0.05)

    def frame(self, title, action, font):
        self.settle()
        width, height = COLS * CELL_W + 48, ROWS * CELL_H + 136
        image = Image.new('RGB', (width, height), '#101620')
        draw = ImageDraw.Draw(image)
        draw.text((24, 15), 'LSPF / language capabilities for IDEs and agents', font=font, fill='#81c8be')
        draw.text((24, 44), title, font=font, fill='#f4f5f7')
        for row, cells in enumerate(self.grid):
            for col, (char, highlight) in enumerate(cells):
                attrs = self.highlights.get(highlight, {})
                fg = attrs.get('foreground', self.foreground)
                bg = attrs.get('background', self.background)
                if attrs.get('reverse'):
                    fg, bg = bg, fg
                x, y = 24 + col * CELL_W, 82 + row * CELL_H
                draw.rectangle((x, y, x + CELL_W, y + CELL_H), fill=f'#{bg:06x}')
                draw.text((x, y), char, font=font, fill=f'#{fg:06x}')
                if attrs.get('underline') or attrs.get('undercurl'):
                    draw.line((x, y + CELL_H - 3, x + CELL_W, y + CELL_H - 3), fill=f'#{fg:06x}')
        row, col = self.cursor
        x, y = 24 + col * CELL_W, 82 + row * CELL_H
        draw.rectangle((x, y, x + CELL_W, y + CELL_H), outline='#81c8be')
        draw.text((24, height - 34), action, font=font, fill='#b7c4d8')
        return image

    def close(self):
        if self.process.poll() is None:
            try:
                self.lua('vim.lsp.stop_client(vim.lsp.get_clients()); return true')
                self.lua('return vim.wait(3000, function() return #vim.lsp.get_clients() == 0 end)')
            finally:
                self.process.stdin.write(msgpack.packb([2, 'nvim_command', ['qa!']]))
                self.process.stdin.flush()
                self.process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=ROOT / 'target/debug/lspf-markdown')
    parser.add_argument('--output', type=Path, default=ROOT / '.scratch/lspf-markdown-demo.gif')
    parser.add_argument('--font', type=Path, default=Path('/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf'))
    args = parser.parse_args()
    scratch = ROOT / '.scratch' / ('demo-' + time.strftime('%Y%m%d-%H%M%S'))
    scratch.mkdir(parents=True, exist_ok=False)
    font = ImageFont.truetype(str(args.font), 18)
    editor = Neovim(scratch, args.server.resolve())
    frames = []
    try:
        frames.append(editor.frame('00-03s  Start Neovim with the bundled LSP config',
                                   'Neovim + lspf-markdown / stdio / server built before capture', font))
        editor.call('nvim_command', ['edit editor-validation/fixture/readme.md'])
        assert editor.lua('return vim.wait(10000, function() return #vim.diagnostic.get(0) == 1 end)'), 'Missing diagnostic'
        diagnostic = editor.lua('return vim.diagnostic.get(0)[1].message')
        assert diagnostic == 'local link target does not exist: missing.md', diagnostic
        frames.append(editor.frame('03-08s  Open Markdown with a broken local link',
                                   'The framework syncs the document; the handler checks link targets.', font))
        editor.lua("vim.api.nvim_win_set_cursor(0, {3, 23})")
        editor.settle()
        editor.lua("vim.diagnostic.open_float({border = 'rounded'})")
        frames.append(editor.frame('08-15s  Inspect the diagnostic',
                                   ':lua vim.diagnostic.open_float()', font))
        editor.lua("""
            for _, win in ipairs(vim.api.nvim_list_wins()) do
                if vim.api.nvim_win_get_config(win).relative ~= '' then vim.api.nvim_win_close(win, true) end
            end
            vim.api.nvim_win_set_cursor(0, {5, 14})
        """)
        editor.settle()
        editor.lua("""
            vim.lsp.buf.hover({border = 'rounded'})
            assert(vim.wait(5000, function() return #vim.api.nvim_list_wins() > 1 end), 'No hover window')
        """)
        hover_text = editor.lua("""
            for _, win in ipairs(vim.api.nvim_list_wins()) do
                if vim.api.nvim_win_get_config(win).relative ~= '' then
                    return table.concat(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(win), 0, -1, false), '\\n')
                end
            end
        """)
        assert 'Validation guide' in hover_text and 'guide.md' in hover_text, hover_text
        frames.append(editor.frame('15-22s  Hover to inspect the resolved target',
                                   ':lua vim.lsp.buf.hover()', font))
        editor.lua("""
            for _, win in ipairs(vim.api.nvim_list_wins()) do
                if vim.api.nvim_win_get_config(win).relative ~= '' then vim.api.nvim_win_close(win, true) end
            end
            vim.lsp.buf.definition()
            assert(vim.wait(5000, function() return vim.fn.expand('%:t') == 'guide.md' end), 'Definition did not open guide.md')
            assert(vim.api.nvim_win_get_cursor(0)[1] == 1, 'Definition missed heading')
        """)
        frames.append(editor.frame('22-30s  Go to definition at the target heading',
                                   ':lua vim.lsp.buf.definition()   |   Ctrl-O to return', font))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        frames[0].save(args.output, save_all=True, append_images=frames[1:],
                       duration=[3000, 5000, 7000, 7000, 8000], loop=0, optimize=True)
        for index, frame in enumerate(frames):
            frame.save(scratch / f'frame-{index}.png')
        print(f'Captured diagnostic, hover, and definition: {args.output}')
        print(f'Raw rendered UI frames: {scratch}')
    finally:
        editor.close()


if __name__ == '__main__':
    main()
