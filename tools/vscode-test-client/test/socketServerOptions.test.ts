import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';

import * as net from 'node:net';

import { WebSocketServer } from 'ws';

import {
    createSocketSession,
    defaultSocketHost,
    type SocketHost,
} from '../src/socketServerOptions.ts';
import { socketTransport, type SocketTransport } from '../src/serverTransport.ts';

/**
 * Bind a port the way a transport example does, and hand back the address to
 * dial plus a close function. Port 0 lets the OS pick, so a test never fights
 * the ports the real examples bind.
 */
async function boundPort(): Promise<{ port: number; close: () => Promise<void> }> {
    const server = net.createServer();
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const { port } = server.address() as net.AddressInfo;
    return {
        port,
        close: () => new Promise<void>((resolve) => server.close(() => resolve())),
    };
}

function dialable(name: 'tcp' | 'websocket', port: number): SocketTransport {
    return { ...socketTransport(name), port };
}

/** A child process stand-in with the two streams the session subscribes to. */
function fakeServer(): any {
    const server: any = new EventEmitter();
    server.stdout = new EventEmitter();
    server.stderr = new EventEmitter();
    server.exitCode = null;
    server.killed = false;
    server.kill = () => {
        server.killed = true;
        server.exitCode = 0;
        return true;
    };
    return server;
}

function hostWith(overrides: Partial<SocketHost>, server = fakeServer()): SocketHost {
    return {
        spawnServer: () => server,
        connectTcp: async () => ({}) as any,
        connectWebSocket: async () => ({}) as any,
        delay: async () => {},
        now: () => 0,
        ...overrides,
    };
}

test('TCP hands the socket to the client as both reader and writer', async () => {
    const socket = { tcp: true } as any;
    const session = createSocketSession(
        '/repo/target/debug/examples/native_tcp',
        socketTransport('tcp'),
        {},
        () => {},
        hostWith({ connectTcp: async () => socket }),
    );

    const transports = await session.serverOptions();

    // `Content-Length` framing is what a StreamInfo makes the client apply, and
    // it is exactly what lspf's TCP adapter speaks.
    assert.deepEqual(transports, { reader: socket, writer: socket });
});

test('WebSocket supplies message transports rather than a byte stream', async () => {
    const socket = new EventEmitter() as any;
    socket.send = () => {};
    const session = createSocketSession(
        '/repo/target/debug/examples/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );

    const transports = (await session.serverOptions()) as any;

    // A StreamInfo would add `Content-Length`, which the WebSocket adapter does
    // not speak: it carries one envelope per message.
    assert.equal(typeof transports.reader.listen, 'function');
    assert.equal(typeof transports.writer.write, 'function');
});

test('the WebSocket reader decodes one envelope per message', async () => {
    const socket = new EventEmitter() as any;
    socket.send = () => {};
    socket.off = socket.removeListener.bind(socket);
    const session = createSocketSession(
        '/x/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );
    const transports = (await session.serverOptions()) as any;

    const seen: unknown[] = [];
    transports.reader.listen((message: unknown) => seen.push(message));
    socket.emit('message', Buffer.from('{"jsonrpc":"2.0","id":1,"result":null}'), false);

    assert.deepEqual(seen, [{ jsonrpc: '2.0', id: 1, result: null }]);
});

// `ws` types a message as a Buffer, an ArrayBuffer, or the fragments of a
// message that arrived split. Each one has to decode to the same envelope.
test('the WebSocket reader decodes every shape a message arrives in', async () => {
    const socket = new EventEmitter() as any;
    socket.send = () => {};
    socket.off = socket.removeListener.bind(socket);
    const session = createSocketSession(
        '/x/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );
    const transports = (await session.serverOptions()) as any;

    const seen: unknown[] = [];
    transports.reader.listen((message: unknown) => seen.push(message));
    const envelope = '{"jsonrpc":"2.0","id":1,"result":null}';
    const bytes = Buffer.from(envelope);
    socket.emit('message', [bytes.subarray(0, 10), bytes.subarray(10)], false);
    socket.emit(
        'message',
        bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
        true,
    );

    assert.deepEqual(seen, [
        { jsonrpc: '2.0', id: 1, result: null },
        { jsonrpc: '2.0', id: 1, result: null },
    ]);
});

test('the WebSocket writer sends bare JSON with no framing header', async () => {
    const sent: string[] = [];
    const socket = new EventEmitter() as any;
    socket.send = (text: string) => sent.push(text);
    const session = createSocketSession(
        '/x/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );
    const transports = (await session.serverOptions()) as any;

    await transports.writer.write({ jsonrpc: '2.0', method: 'exit' });

    assert.deepEqual(sent, ['{"jsonrpc":"2.0","method":"exit"}']);
});

test('retries the connection while the server is still binding its port', async () => {
    let attempts = 0;
    const socket = {} as any;
    let clock = 0;
    const session = createSocketSession(
        '/x/native_tcp',
        socketTransport('tcp'),
        {},
        () => {},
        hostWith({
            connectTcp: async () => {
                attempts++;
                if (attempts < 3) throw new Error('ECONNREFUSED');
                return socket;
            },
            delay: async () => {
                clock += 100;
            },
            now: () => clock,
        }),
    );

    const transports = (await session.serverOptions()) as any;

    assert.equal(attempts, 3);
    assert.equal(transports.reader, socket);
});

test('gives up once the connect deadline passes', async () => {
    let clock = 0;
    const session = createSocketSession(
        '/x/native_tcp',
        socketTransport('tcp'),
        {},
        () => {},
        hostWith({
            connectTcp: async () => {
                throw new Error('ECONNREFUSED');
            },
            delay: async () => {
                clock += 10_000;
            },
            now: () => clock,
        }),
    );

    await assert.rejects(session.serverOptions(), /ECONNREFUSED/);
});

test('forwards the server output that would otherwise be invisible', async () => {
    const server = fakeServer();
    const lines: string[] = [];
    const session = createSocketSession(
        '/x/native_tcp',
        socketTransport('tcp'),
        {},
        (line) => lines.push(line),
        hostWith({}, server),
    );

    await session.serverOptions();
    server.stderr.emit('data', Buffer.from('tracing span\n'));

    assert.deepEqual(lines, ['tracing span\n']);
});

// The default host is what a real debug session uses; the tests above replace
// it, so these dial actual sockets to keep it from going unexercised.
test('the default host dials a listening TCP port and sets no delay', async () => {
    const listener = await boundPort();

    const socket = await defaultSocketHost.connectTcp(dialable('tcp', listener.port));

    assert.equal(socket.readyState, 'open');
    assert.equal(socket.remotePort, listener.port);
    socket.destroy();
    await listener.close();
});

test('the default host reports a TCP port nothing is listening on', async () => {
    const listener = await boundPort();
    await listener.close();

    await assert.rejects(
        defaultSocketHost.connectTcp(dialable('tcp', listener.port)),
        (error: NodeJS.ErrnoException) => error.code === 'ECONNREFUSED',
    );
});

test('the default host completes a WebSocket handshake', async () => {
    const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
    await new Promise((resolve) => server.once('listening', resolve));
    const { port } = server.address() as net.AddressInfo;

    const socket = await defaultSocketHost.connectWebSocket(dialable('websocket', port));

    assert.equal(socket.readyState, socket.OPEN);
    socket.close();
    await new Promise((resolve) => server.close(resolve));
});

test('the default host reports a WebSocket port nothing is listening on', async () => {
    const listener = await boundPort();
    await listener.close();

    await assert.rejects(defaultSocketHost.connectWebSocket(dialable('websocket', listener.port)));
});

test('the default host waits and reads the clock the retry loop needs', async () => {
    const before = defaultSocketHost.now();
    await defaultSocketHost.delay(1);

    assert.ok(defaultSocketHost.now() >= before);
});

test('the WebSocket reader reports a message that is not an envelope', async () => {
    const socket = new EventEmitter() as any;
    socket.send = () => {};
    socket.off = socket.removeListener.bind(socket);
    const session = createSocketSession(
        '/x/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );
    const transports = (await session.serverOptions()) as any;

    const errors: unknown[] = [];
    transports.reader.onError((error: unknown) => errors.push(error));
    transports.reader.listen(() => {});
    socket.emit('message', Buffer.from('not json'), false);

    assert.equal(errors.length, 1);
});

test('disposing the reader stops it listening to the socket', async () => {
    const socket = new EventEmitter() as any;
    socket.send = () => {};
    socket.off = socket.removeListener.bind(socket);
    const session = createSocketSession(
        '/x/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );
    const transports = (await session.serverOptions()) as any;

    const seen: unknown[] = [];
    transports.reader.listen((message: unknown) => seen.push(message)).dispose();
    socket.emit('message', Buffer.from('{"jsonrpc":"2.0","id":1,"result":null}'), false);

    assert.deepEqual(seen, []);
    assert.equal(socket.listenerCount('message'), 0);
});

// A failed send is the writer's only error path, and `vscode-jsonrpc` counts
// consecutive failures, so the count has to reach the error handler.
test('the WebSocket writer reports and rethrows a failed send', async () => {
    const socket = new EventEmitter() as any;
    socket.send = () => {
        throw new Error('socket is closing');
    };
    const session = createSocketSession(
        '/x/native_websocket',
        socketTransport('websocket'),
        {},
        () => {},
        hostWith({ connectWebSocket: async () => socket }),
    );
    const transports = (await session.serverOptions()) as any;

    const counts: number[] = [];
    transports.writer.onError(([, , count]: [Error, unknown, number]) => counts.push(count));

    await assert.rejects(transports.writer.write({ jsonrpc: '2.0', method: 'exit' }), /closing/);
    await assert.rejects(transports.writer.write({ jsonrpc: '2.0', method: 'exit' }), /closing/);
    assert.deepEqual(counts, [1, 2]);
    assert.doesNotThrow(() => transports.writer.end());
});

test('stops the server it started when the client is disposed', async () => {
    const server = fakeServer();
    const session = createSocketSession(
        '/x/native_tcp',
        socketTransport('tcp'),
        {},
        () => {},
        hostWith({}, server),
    );

    await session.serverOptions();
    session.dispose();

    assert.equal(server.killed, true);
});
