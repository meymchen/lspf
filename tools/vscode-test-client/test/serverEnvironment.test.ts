import assert from 'node:assert/strict';
import { test } from 'node:test';

import { serverEnvironment } from '../src/serverEnvironment.ts';

test('defaults the test server to JSON logs without replacing user overrides', () => {
    assert.deepEqual(serverEnvironment({ PATH: '/bin' }), {
        PATH: '/bin',
        RUST_LOG: 'lspf=trace',
        LSPF_LOG_FORMAT: 'json',
    });
    assert.deepEqual(
        serverEnvironment({
            RUST_LOG: 'lspf=info',
            LSPF_LOG_FORMAT: 'text',
        }),
        {
            RUST_LOG: 'lspf=info',
            LSPF_LOG_FORMAT: 'text',
        },
    );
});
