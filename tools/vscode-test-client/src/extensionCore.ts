import * as path from 'node:path';
import type { ExtensionContext, FileSystemWatcher, OutputChannel } from 'vscode';
import type {
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from 'vscode-languageclient/node';

import { serverCommandArguments } from './serverCommands.js';
import { serverEnvironment } from './serverEnvironment.js';
import { resolveServerBinary } from './serverPath.js';
import { serverProfile } from './serverProfile.js';
import { createSocketSession } from './socketServerOptions.js';
import {
    resolveTransport,
    resolveTransportBinary,
    socketTransport,
} from './serverTransport.js';

export interface ExtensionClient {
    onRequest(method: string, handler: () => unknown): { dispose(): unknown };
    start(): Promise<void>;
    stop(): Thenable<void>;
}

export interface ExtensionHost {
    readonly stdioTransport: TransportKind;
    createOutputChannel(name: string): OutputChannel;
    createFileSystemWatcher(glob: string): FileSystemWatcher;
    activeDocumentUri(): string | undefined;
    showErrorMessage(message: string): Thenable<unknown>;
    createLanguageClient(
        id: string,
        name: string,
        serverOptions: ServerOptions,
        clientOptions: LanguageClientOptions,
    ): ExtensionClient;
}

export async function activateClient(
    context: Pick<ExtensionContext, 'extensionPath' | 'subscriptions'>,
    host: ExtensionHost,
): Promise<ExtensionClient> {
    // tools/vscode-test-client/out/extensionCore.js  →  repo root is two levels up.
    const repoRoot = path.resolve(context.extensionPath, '..', '..');
    const transport = resolveTransport();
    const serverBinary =
        transport === 'stdio'
            ? resolveServerBinary(repoRoot)
            : resolveTransportBinary(repoRoot, socketTransport(transport));
    const profile = serverProfile(serverBinary);

    let serverOptions: ServerOptions;
    // Over stdio the client owns the server process and pipes its stderr into
    // the output channel itself. A socket transport gives the client no process
    // to read, so the extension owns that forwarding — into the same channel,
    // so both transports produce one channel with the same name and contents.
    let serverOutput: OutputChannel | undefined;
    if (transport === 'stdio') {
        serverOptions = {
            command: serverBinary,
            transport: host.stdioTransport,
            options: {
                env: serverEnvironment(),
            },
        };
    } else {
        serverOutput = host.createOutputChannel(profile.outputChannel);
        context.subscriptions.push(serverOutput);
        const channel = serverOutput;
        const session = createSocketSession(
            serverBinary,
            socketTransport(transport),
            serverEnvironment(),
            (line) => channel.append(line),
        );
        context.subscriptions.push(session);
        serverOptions = session.serverOptions;
    }

    const commandOutput = profile.commandOutput
        ? host.createOutputChannel(profile.commandOutput)
        : undefined;
    if (commandOutput) {
        context.subscriptions.push(commandOutput);
    }
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: profile.language }],
        // Hand the client the channel the server's own output already goes to,
        // rather than letting it create a second one under the same name.
        ...(serverOutput
            ? { outputChannel: serverOutput }
            : { outputChannelName: profile.outputChannel }),
        synchronize: {
            fileEvents: host.createFileSystemWatcher('**/*'),
        },
        middleware: {
            async executeCommand(command, args, next) {
                if (!commandOutput) {
                    return next(command, args);
                }
                try {
                    const result = await next(
                        command,
                        serverCommandArguments(
                            command,
                            args,
                            host.activeDocumentUri(),
                        ),
                    );
                    const rendered = JSON.stringify(result, null, 2) ?? String(result);
                    commandOutput.appendLine(`${command}\n${rendered}`);
                    commandOutput.show(true);
                    return result;
                } catch (error) {
                    const message = error instanceof Error ? error.message : String(error);
                    await host.showErrorMessage(`lspf hello: ${message}`);
                    return undefined;
                }
            },
        },
    };

    const client = host.createLanguageClient(
        profile.id,
        profile.name,
        serverOptions,
        clientOptions,
    );
    if (profile.id === 'lspf-hello') {
        context.subscriptions.push(
            client.onRequest('lspf-hello/ping', () => 'pong'),
        );
    }
    await client.start();
    return client;
}
