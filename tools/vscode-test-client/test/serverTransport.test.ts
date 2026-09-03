import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as path from 'node:path';

import {
    resolveTransport,
    resolveTransportBinary,
    socketTransport,
} from '../src/serverTransport.ts';

test('defaults to stdio so existing launch configurations keep working', () => {
    assert.equal(resolveTransport(undefined), 'stdio');
    assert.equal(resolveTransport(''), 'stdio');
});

test('accepts the three transports the client can drive', () => {
    assert.equal(resolveTransport('stdio'), 'stdio');
    assert.equal(resolveTransport('tcp'), 'tcp');
    assert.equal(resolveTransport('websocket'), 'websocket');
});

test('rejects a transport the client has no adapter for', () => {
    assert.throws(
        () => resolveTransport('worker-channel'),
        /invalid LSPF_TEST_TRANSPORT/,
    );
});

test('dials the addresses the transport examples bind', () => {
    assert.deepEqual(socketTransport('tcp'), {
        name: 'tcp',
        example: 'native_tcp',
        host: '127.0.0.1',
        port: 9257,
    });
    assert.deepEqual(socketTransport('websocket'), {
        name: 'websocket',
        example: 'native_websocket',
        host: '127.0.0.1',
        port: 9258,
    });
});

test('resolves the transport example binary Cargo builds', () => {
    assert.equal(
        resolveTransportBinary('/repo', socketTransport('tcp'), 'linux'),
        path.join('/repo', 'target', 'debug', 'examples', 'native_tcp'),
    );
    assert.equal(
        path.basename(
            resolveTransportBinary('C:\\repo', socketTransport('websocket'), 'win32'),
        ),
        'native_websocket.exe',
    );
});
