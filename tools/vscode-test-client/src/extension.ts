import { ExtensionContext, window, workspace } from 'vscode';
import {
    LanguageClient,
    TransportKind,
} from 'vscode-languageclient/node';

import { activateClient, type ExtensionClient } from './extensionCore.js';

let client: ExtensionClient | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
    client = await activateClient(context, {
        stdioTransport: TransportKind.stdio,
        createOutputChannel: (name) => window.createOutputChannel(name),
        createFileSystemWatcher: (glob) => workspace.createFileSystemWatcher(glob),
        createLanguageClient: (id, name, serverOptions, clientOptions) =>
            new LanguageClient(id, name, serverOptions, clientOptions),
    });
}

export function deactivate(): Thenable<void> | undefined {
    return client?.stop();
}
