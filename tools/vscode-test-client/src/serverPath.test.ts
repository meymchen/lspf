import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as path from 'node:path';

import { resolveServerBinary } from './serverPath.ts';

test('resolves the lspf-hello workspace binary under target/debug', () => {
    const binary = resolveServerBinary('/repo');
    assert.equal(binary, path.join('/repo', 'target', 'debug', 'lspf-hello'));
});

test('no longer points at the legacy examples/hello path', () => {
    const binary = resolveServerBinary('/repo');
    assert.ok(
        !binary.includes(path.join('examples', 'hello')),
        `expected the workspace binary, got the old example path: ${binary}`,
    );
});
