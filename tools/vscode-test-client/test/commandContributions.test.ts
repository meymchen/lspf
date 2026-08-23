import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { serverCommands } from '../src/serverCommands.ts';

const manifest = JSON.parse(readFileSync('package.json', 'utf8')) as {
    contributes?: { commands?: Array<{ command: string }> };
};

test('contributes every lspf-hello server command to the Command Palette', () => {
    const commands = manifest.contributes?.commands?.map(({ command }) => command) ?? [];
    assert.deepEqual(commands, [
        'lspf-hello.workspaceRoots',
        'lspf-hello.readFile',
        'lspf-hello.outgoingJourney',
        'lspf-hello.cancellableProgress',
    ]);
});

test('does not manually register commands owned by vscode-languageclient', () => {
    const extensionSource = readFileSync('src/extension.ts', 'utf8');
    assert.doesNotMatch(
        extensionSource,
        /commands\s*\.\s*registerCommand/,
        'advertised LSP commands are registered automatically by vscode-languageclient',
    );
});

test('palette metadata describes the server command ids', () => {
    const commands = manifest.contributes?.commands?.map(({ command }) => command) ?? [];
    assert.deepEqual(commands, serverCommands.map(({ id }) => id));
});
