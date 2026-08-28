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
    const serverBinary = resolveServerBinary(repoRoot);
    const profile = serverProfile(serverBinary);

    const serverOptions: ServerOptions = {
        command: serverBinary,
        transport: host.stdioTransport,
        options: {
            env: serverEnvironment(),
        },
    };

    const commandOutput = profile.commandOutput
        ? host.createOutputChannel(profile.commandOutput)
        : undefined;
    if (commandOutput) {
        context.subscriptions.push(commandOutput);
    }
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: profile.language }],
        outputChannelName: profile.outputChannel,
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
