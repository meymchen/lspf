import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';

import type { LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node';

import type { ExtensionHost } from '../src/extensionCore.ts';

type ActivateClient = typeof import('../src/extensionCore.ts').activateClient;

async function loadActivateClient(): Promise<ActivateClient> {
    const compiledModulePath: string = '../out/extensionCore.js';
    const module = await import(compiledModulePath);
    return module.activateClient as ActivateClient;
}

interface ClientCall {
    id: string;
    name: string;
    serverOptions: ServerOptions;
    clientOptions: LanguageClientOptions;
}

const clientCalls: ClientCall[] = [];
const channelNames: string[] = [];
let started = false;

const outputChannel = {
    appendLine() {},
    append() {},
    show() {},
    dispose() {},
};
const watcher = { dispose() {} };
const host: ExtensionHost = {
    stdioTransport: 'stdio' as never,
    createOutputChannel: (name) => {
        channelNames.push(name);
        return outputChannel as never;
    },
    createFileSystemWatcher: () => watcher as never,
    createLanguageClient(id, name, serverOptions, clientOptions) {
        clientCalls.push({ id, name, serverOptions, clientOptions });
        return {
            async start() {
                started = true;
            },
            async stop() {},
        };
    },
};

afterEach(() => {
    delete process.env.LSPF_TEST_SERVER;
    delete process.env.LSPF_MARKDOWN_SERVER;
    delete process.env.LSPF_TEST_TRANSPORT;
    delete process.env.LSPF_TEST_EXAMPLE;
    clientCalls.length = 0;
    channelNames.length = 0;
    started = false;
});

test('activates the installed Markdown server', async () => {
    process.env.LSPF_TEST_SERVER = 'lspf-markdown';
    const subscriptions: unknown[] = [];
    const activateClient = await loadActivateClient();

    await activateClient(
        { extensionPath: '/repo/tools/vscode-test-client', subscriptions } as never,
        host,
    );

    assert.equal(clientCalls.length, 1);
    assert.equal(clientCalls[0].id, 'lspf-markdown');
    assert.equal(clientCalls[0].name, 'lspf Markdown');
    assert.ok('command' in clientCalls[0].serverOptions);
    assert.equal(clientCalls[0].serverOptions.command, 'lspf-markdown');
    assert.deepEqual(clientCalls[0].clientOptions.documentSelector, [
        { scheme: 'file', language: 'markdown' },
    ]);
    assert.equal(clientCalls[0].clientOptions.outputChannelName, 'lspf-markdown');
    assert.equal(started, true);
    assert.deepEqual(subscriptions, []);
});

test('uses the Markdown reference server by default', async () => {
    const subscriptions: unknown[] = [];
    const activateClient = await loadActivateClient();

    await activateClient(
        { extensionPath: '/repo/tools/vscode-test-client', subscriptions } as never,
        host,
    );

    assert.equal(clientCalls.length, 1);
    assert.equal(clientCalls[0].id, 'lspf-markdown');
    assert.equal(clientCalls[0].name, 'lspf Markdown');
    assert.deepEqual(clientCalls[0].clientOptions.documentSelector, [
        { scheme: 'file', language: 'markdown' },
    ]);
    assert.equal(clientCalls[0].clientOptions.outputChannelName, 'lspf-markdown');
    assert.deepEqual(subscriptions, []);
});

test('the Notebook example selects notebook cells without a file scheme restriction', async () => {
    process.env.LSPF_TEST_EXAMPLE = 'notebooks';
    const activateClient = await loadActivateClient();
    await activateClient(
        { extensionPath: '/repo/tools/vscode-test-client', subscriptions: [] } as never,
        host,
    );

    assert.equal(clientCalls[0].id, 'lspf-notebooks');
    assert.deepEqual(clientCalls[0].clientOptions.documentSelector, [
        { notebook: 'jupyter-notebook' },
    ]);
    assert.equal(clientCalls[0].clientOptions.outputChannelName, 'lspf notebooks');
    assert.ok('command' in clientCalls[0].serverOptions);
    assert.match(clientCalls[0].serverOptions.command, /examples[/\\]notebooks(?:\.exe)?$/);
    assert.equal(started, true);
});

// Over a socket the client owns no server process, so it pipes no stderr into
// its output channel. The extension has to do that itself, and into the channel
// the client already writes to: two channels, or a channel the client does not
// use, is what an empty output channel looks like.
test('a socket transport gives the client the channel the server output goes to', async () => {
    process.env.LSPF_TEST_TRANSPORT = 'tcp';
    const subscriptions: unknown[] = [];
    const activateClient = await loadActivateClient();

    await activateClient(
        { extensionPath: '/repo/tools/vscode-test-client', subscriptions } as never,
        host,
    );

    assert.equal(clientCalls.length, 1);
    assert.equal(clientCalls[0].id, 'lspf-transport');
    // One channel, named as the profile names it, handed to the client.
    assert.deepEqual(channelNames, ['lspf native_tcp']);
    assert.equal(clientCalls[0].clientOptions.outputChannel, outputChannel);
    assert.equal(clientCalls[0].clientOptions.outputChannelName, undefined);
    // A socket transport supplies transports lazily rather than a command.
    assert.equal(typeof clientCalls[0].serverOptions, 'function');
    assert.deepEqual(clientCalls[0].clientOptions.documentSelector, [
        { scheme: 'file', language: 'plaintext' },
    ]);
    // The shared example advertises no command and no reverse request.
});
