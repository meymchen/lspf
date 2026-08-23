import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as path from 'node:path';

import { resolveServerBinary } from '../src/serverPath.ts';

test('resolves the lspf-hello workspace binary under target/debug on Unix', () => {
    const binary = resolveServerBinary('/repo', 'linux');
    assert.equal(binary, path.join('/repo', 'target', 'debug', 'lspf-hello'));
});

test('uses the executable suffix on Windows', () => {
    const binary = resolveServerBinary('C:\\repo', 'win32');
    assert.equal(path.basename(binary), 'lspf-hello.exe');
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
        () => resolveServerBinary('/repo', 'linux', '../lspf-hello'),
        /invalid LSPF_TEST_EXAMPLE/,
    );
});

test('no longer points at the legacy examples/hello path', () => {
    const binary = resolveServerBinary('/repo');
    assert.ok(
        !binary.includes(path.join('examples', 'hello')),
        `expected the workspace binary, got the old example path: ${binary}`,
    );
});
