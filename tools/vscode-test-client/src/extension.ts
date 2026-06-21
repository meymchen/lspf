import * as path from 'path';
import { ExtensionContext, workspace } from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from 'vscode-languageclient/node';

import { resolveServerBinary } from './serverPath.js';

let client: LanguageClient | undefined;

export function activate(context: ExtensionContext): void {
    // tools/vscode-test-client/out/extension.js  →  repo root is two levels up.
    const repoRoot = path.resolve(context.extensionPath, '..', '..');
    const serverBinary = resolveServerBinary(repoRoot);

    const serverOptions: ServerOptions = {
        command: serverBinary,
        transport: TransportKind.stdio,
        options: {
            env: { ...process.env, RUST_LOG: process.env.RUST_LOG ?? 'lspf=trace' },
        },
    };

    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: 'plaintext' }],
        outputChannelName: 'lspf-hello',
        synchronize: {
            fileEvents: workspace.createFileSystemWatcher('**/*'),
        },
    };

    client = new LanguageClient('lspf-hello', 'lspf hello', serverOptions, clientOptions);
    client.start();
}

export function deactivate(): Thenable<void> | undefined {
    return client?.stop();
}
