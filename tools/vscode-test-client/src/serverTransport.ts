import * as path from 'node:path';

/**
 * How this debug session reaches the language server.
 *
 * `stdio` spawns the server and owns its standard streams, which is what every
 * editor does by default. `tcp` and `websocket` instead connect to a server
 * listening on a socket, so the socket adapters get exercised by a real editor
 * client rather than only by a scripted one.
 */
export type TransportName = 'stdio' | 'tcp' | 'websocket';

export interface SocketTransport {
    readonly name: 'tcp' | 'websocket';
    /** Cargo example that serves the shared handlers over this adapter. */
    readonly example: string;
    readonly host: string;
    readonly port: number;
}

/**
 * The socket examples bind these addresses in their own `serve` call, so the
 * client cannot choose them. Keep them in step with `native_tcp.rs` and
 * `native_websocket.rs`.
 */
const SOCKET_TRANSPORTS: Record<'tcp' | 'websocket', SocketTransport> = {
    tcp: {
        name: 'tcp',
        example: 'native_tcp',
        host: '127.0.0.1',
        port: 9257,
    },
    websocket: {
        name: 'websocket',
        example: 'native_websocket',
        host: '127.0.0.1',
        port: 9258,
    },
};

/**
 * Read the transport selected for this session.
 *
 * @param selected Value of `LSPF_TEST_TRANSPORT`; anything unset falls back to
 * stdio so existing launch configurations keep their behaviour.
 */
export function resolveTransport(
    selected: string | undefined = process.env.LSPF_TEST_TRANSPORT,
): TransportName {
    if (selected === undefined || selected === '') {
        return 'stdio';
    }
    if (selected === 'stdio' || selected === 'tcp' || selected === 'websocket') {
        return selected;
    }
    throw new Error(
        `invalid LSPF_TEST_TRANSPORT: ${selected} (expected stdio, tcp, or websocket)`,
    );
}

/** Describe the socket a non-stdio transport connects to. */
export function socketTransport(name: 'tcp' | 'websocket'): SocketTransport {
    return SOCKET_TRANSPORTS[name];
}

/**
 * Resolve the transport example binary built by
 * `cargo build -p lspf --example <name> --no-default-features --features <feature>`.
 *
 * The socket transports ignore `LSPF_TEST_EXAMPLE` because only these two
 * examples serve over a socket; the stdio examples have no listener to dial.
 */
export function resolveTransportBinary(
    repoRoot: string,
    transport: SocketTransport,
    platform: NodeJS.Platform = process.platform,
): string {
    const suffix = platform === 'win32' ? '.exe' : '';
    return path.join(
        repoRoot,
        'target',
        'debug',
        'examples',
        `${transport.example}${suffix}`,
    );
}
