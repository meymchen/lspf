import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as path from 'node:path';

import { resolveServerBinary } from '../src/serverPath.ts';

test('resolves the Markdown reference server under target/debug on Unix', () => {
    const binary = resolveServerBinary('/repo', 'linux');
    assert.equal(binary, path.join('/repo', 'target', 'debug', 'lspf-markdown'));
});

test('resolves the packaged Markdown reference server selected by the journey', () => {
    const binary = resolveServerBinary('/repo', 'linux', undefined, 'lspf-markdown');
    assert.equal(binary, 'lspf-markdown');
});

test('accepts an explicit installed Markdown server path', () => {
    const binary = resolveServerBinary(
        '/repo',
        'linux',
        undefined,
        '/opt/lspf/bin/lspf-markdown',
    );
    assert.equal(binary, '/opt/lspf/bin/lspf-markdown');
});

test('uses the executable suffix on Windows', () => {
    const binary = resolveServerBinary('C:\\repo', 'win32');
    assert.equal(path.basename(binary), 'lspf-markdown.exe');
});

test('resolves a selected Cargo example binary', () => {
    assert.equal(
        resolveServerBinary('/repo', 'linux', 'hover'),
        path.join('/repo', 'target', 'debug', 'examples', 'hover'),
    );
    assert.equal(
        path.basename(resolveServerBinary('C:\\repo', 'win32', 'code_actions')),
        'code_actions.exe',
    );
});

test('rejects example names that could escape the Cargo examples directory', () => {
    assert.throws(
        () => resolveServerBinary('/repo', 'linux', '../hover'),
        /invalid LSPF_TEST_EXAMPLE/,
    );
});
