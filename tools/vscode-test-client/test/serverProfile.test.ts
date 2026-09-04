import assert from 'node:assert/strict';
import { test } from 'node:test';

import { serverProfile } from '../src/serverProfile.ts';

test('the installed reference server activates for Markdown documents', () => {
    assert.deepEqual(serverProfile('lspf-markdown'), {
        id: 'lspf-markdown',
        name: 'lspf Markdown',
        language: 'markdown',
        outputChannel: 'lspf-markdown',
    });
});

// The socket examples serve the shared handler set, which advertises no
// command and no reverse request, so their profile must enable neither.
test('each transport example gets its own channel and no command journey', () => {
    assert.deepEqual(serverProfile('/repo/target/debug/examples/native_tcp'), {
        id: 'lspf-transport',
        name: 'lspf TCP example',
        language: 'plaintext',
        outputChannel: 'lspf native_tcp',
    });
    assert.deepEqual(serverProfile('C:/repo/target/debug/examples/native_websocket.exe'), {
        id: 'lspf-transport',
        name: 'lspf WebSocket example',
        language: 'plaintext',
        outputChannel: 'lspf native_websocket',
    });
});

test('a stdio example gets a plaintext profile', () => {
    assert.deepEqual(serverProfile('/repo/target/debug/examples/hover'), {
        id: 'lspf-example',
        name: 'lspf hover example',
        language: 'plaintext',
        outputChannel: 'lspf hover',
    });
});
