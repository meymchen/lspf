import assert from 'node:assert/strict';
import { test } from 'node:test';

import { serverProfile } from '../src/serverProfile.ts';

test('the installed reference server activates for Markdown documents', () => {
    assert.deepEqual(serverProfile('lspf-markdown'), {
        id: 'lspf-markdown',
        name: 'lspf Markdown',
        language: 'markdown',
        outputChannel: 'lspf-markdown',
        commandOutput: undefined,
    });
});

test('the default development server retains its plaintext command journey', () => {
    assert.deepEqual(serverProfile('/repo/target/debug/lspf-hello'), {
        id: 'lspf-hello',
        name: 'lspf hello',
        language: 'plaintext',
        outputChannel: 'lspf-hello',
        commandOutput: 'lspf-hello commands',
    });
});
