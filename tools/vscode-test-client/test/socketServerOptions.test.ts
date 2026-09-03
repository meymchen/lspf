import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';

import { createSocketSession, type SocketHost } from '../src/socketServerOptions.ts';
import { socketTransport } from '../src/serverTransport.ts';

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
