import { type ChildProcess, spawn } from 'node:child_process';
import * as net from 'node:net';

// The package root, not its `/node` subpath: that subpath has no file
// extension and no `exports` map, so Node's ESM resolver cannot load it. The
// root's `main` resolves, and the abstract reader and writer live in the common
// API it re-exports.
import {
    AbstractMessageReader,
    AbstractMessageWriter,
    type DataCallback,
    type Disposable,
    type Message,
    type MessageReader,
    type MessageWriter,
} from 'vscode-jsonrpc';
import type { MessageTransports, StreamInfo } from 'vscode-languageclient/node';
import WebSocket from 'ws';

import type { SocketTransport } from './serverTransport.js';

/** How long to keep retrying the connection while the server binds its port. */
const CONNECT_TIMEOUT_MS = 30_000;
const RETRY_DELAY_MS = 100;

export interface SocketSession {
    /** Passed to `LanguageClient` as its `ServerOptions`. */
    readonly serverOptions: () => Promise<StreamInfo | MessageTransports>;
    /** Stops the server this session started. */
    dispose(): void;
}

export interface SocketHost {
    spawnServer(binary: string, env: NodeJS.ProcessEnv): ChildProcess;
    connectTcp(transport: SocketTransport): Promise<net.Socket>;
    connectWebSocket(transport: SocketTransport): Promise<WebSocket>;
    delay(ms: number): Promise<void>;
    now(): number;
}

export const defaultSocketHost: SocketHost = {
    spawnServer: (binary, env) =>
        spawn(binary, [], { env, stdio: ['ignore', 'pipe', 'pipe'] }),
    connectTcp: (transport) =>
        new Promise((resolve, reject) => {
            const socket = net.createConnection({
                host: transport.host,
                port: transport.port,
            });
            socket.once('connect', () => {
                socket.setNoDelay(true);
                socket.removeListener('error', reject);
                resolve(socket);
            });
            socket.once('error', (error) => {
                socket.destroy();
                reject(error);
            });
        }),
    connectWebSocket: (transport) =>
        new Promise((resolve, reject) => {
            const socket = new WebSocket(
                `ws://${transport.host}:${transport.port}`,
            );
            socket.once('open', () => {
                socket.removeListener('error', reject);
                resolve(socket);
            });
            socket.once('error', (error) => {
                socket.terminate();
                reject(error);
            });
        }),
    delay: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    now: () => Date.now(),
};

/**
 * Read one JSON-RPC envelope per WebSocket message.
 *
 * lspf's WebSocket adapter maps one envelope to one message and adds no
 * `Content-Length` header, so this reader must not reuse the stream reader that
 * `vscode-jsonrpc` applies to byte channels.
 */
class WebSocketMessageReader extends AbstractMessageReader implements MessageReader {
    private readonly socket: WebSocket;

    constructor(socket: WebSocket) {
        super();
        this.socket = socket;
    }

    listen(callback: DataCallback): Disposable {
        const onMessage = (data: WebSocket.RawData): void => {
            // The adapter accepts text and binary alike; both carry UTF-8 JSON,
            // so the frame's own opcode changes nothing here.
            const text = utf8(data);
            try {
                callback(JSON.parse(text) as Message);
            } catch (error) {
                this.fireError(error);
            }
        };
        const onClose = (): void => this.fireClose();
        const onError = (error: Error): void => this.fireError(error);

        this.socket.on('message', onMessage);
        this.socket.on('close', onClose);
        this.socket.on('error', onError);
        return {
            dispose: () => {
                this.socket.off('message', onMessage);
                this.socket.off('close', onClose);
                this.socket.off('error', onError);
            },
        };
    }
}

/**
 * Decode one WebSocket message as UTF-8.
 *
 * `RawData` is a `Buffer`, an `ArrayBuffer`, or the fragments of a message that
 * arrived split. Only the first decodes correctly through `toString()` alone:
 * the array would join its fragments with a comma and the `ArrayBuffer` would
 * stringify as its own type name, both of them producing JSON no parser
 * accepts.
 */
function utf8(data: WebSocket.RawData): string {
    if (Array.isArray(data)) {
        return Buffer.concat(data).toString('utf8');
    }
    if (Buffer.isBuffer(data)) {
        return data.toString('utf8');
    }
    return Buffer.from(data).toString('utf8');
}

/** Write one JSON-RPC envelope per WebSocket message, with no framing header. */
class WebSocketMessageWriter extends AbstractMessageWriter implements MessageWriter {
    private errorCount = 0;
    private readonly socket: WebSocket;

    constructor(socket: WebSocket) {
        super();
        this.socket = socket;
    }

    async write(message: Message): Promise<void> {
        try {
            this.socket.send(JSON.stringify(message));
            this.errorCount = 0;
        } catch (error) {
            this.errorCount++;
            this.fireError(error, message, this.errorCount);
            throw error;
        }
    }

    end(): void {
        // The engine closes on `exit`; nothing is buffered here to flush.
    }
}

/**
 * Start a transport example and connect the language client to its socket.
 *
 * The client cannot own the server's standard streams here, so it spawns the
 * process and then dials the port the process binds. Each transport example
 * binds once, accepts one client, and drops its listener, so a retry loop is
 * the only safe readiness check: a throwaway probe socket would consume the one
 * connection the server serves.
 */
export function createSocketSession(
    binary: string,
    transport: SocketTransport,
    env: NodeJS.ProcessEnv,
    onServerOutput: (line: string) => void,
    host: SocketHost = defaultSocketHost,
): SocketSession {
    let server: ChildProcess | undefined;

    const serverOptions = async (): Promise<StreamInfo | MessageTransports> => {
        server = host.spawnServer(binary, env);
        // The server's stderr carries its tracing spans. stdout is unused by a
        // socket adapter but is forwarded too, so nothing is silently dropped.
        server.stderr?.on('data', (chunk: Buffer) => onServerOutput(chunk.toString()));
        server.stdout?.on('data', (chunk: Buffer) => onServerOutput(chunk.toString()));

        const deadline = host.now() + CONNECT_TIMEOUT_MS;
        if (transport.name === 'tcp') {
            // TCP carries `Content-Length` framing, which is exactly what
            // `StreamInfo` makes the client apply.
            const socket = await connectWithRetry(
                () => host.connectTcp(transport),
                host,
                deadline,
            );
            return { reader: socket, writer: socket };
        }

        const socket = await connectWithRetry(
            () => host.connectWebSocket(transport),
            host,
            deadline,
        );
        return {
            reader: new WebSocketMessageReader(socket),
            writer: new WebSocketMessageWriter(socket),
        };
    };

    return {
        serverOptions,
        dispose: () => {
            if (server && server.exitCode === null) {
                server.kill();
            }
            server = undefined;
        },
    };
}

async function connectWithRetry<T>(
    connect: () => Promise<T>,
    host: Pick<SocketHost, 'delay' | 'now'>,
    deadline: number,
): Promise<T> {
    for (;;) {
        try {
            return await connect();
        } catch (error) {
            if (host.now() >= deadline) {
                throw error;
            }
            await host.delay(RETRY_DELAY_MS);
        }
    }
}
