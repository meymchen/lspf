import * as path from 'path';
import { ExtensionContext, window, workspace } from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from 'vscode-languageclient/node';

import { resolveServerBinary } from './serverPath.js';
import { serverEnvironment } from './serverEnvironment.js';
import { serverCommandArguments } from './serverCommands.js';
import { serverProfile } from './serverProfile.js';

let client: LanguageClient | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
    // tools/vscode-test-client/out/extension.js  →  repo root is two levels up.
    const repoRoot = path.resolve(context.extensionPath, '..', '..');
    const serverBinary = resolveServerBinary(repoRoot);
    const profile = serverProfile(serverBinary);

    const serverOptions: ServerOptions = {
        command: serverBinary,
        transport: TransportKind.stdio,
        options: {
            env: serverEnvironment(),
        },
    };

    const commandOutput = profile.commandOutput
        ? window.createOutputChannel(profile.commandOutput)
        : undefined;
    if (commandOutput) {
        context.subscriptions.push(commandOutput);
    }
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: profile.language }],
        outputChannelName: profile.outputChannel,
        synchronize: {
            fileEvents: workspace.createFileSystemWatcher('**/*'),
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
                            window.activeTextEditor?.document.uri.toString(),
                        ),
                    );
                    const rendered = JSON.stringify(result, null, 2) ?? String(result);
                    commandOutput.appendLine(`${command}\n${rendered}`);
                    commandOutput.show(true);
                    return result;
                } catch (error) {
                    const message = error instanceof Error ? error.message : String(error);
                    await window.showErrorMessage(`lspf hello: ${message}`);
                    return undefined;
                }
            },
        },
    };

    client = new LanguageClient(profile.id, profile.name, serverOptions, clientOptions);
    if (profile.id === 'lspf-hello') {
        context.subscriptions.push(
            client.onRequest('lspf-hello/ping', () => 'pong'),
        );
    }
    await client.start();
}

export function deactivate(): Thenable<void> | undefined {
    return client?.stop();
}
