import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';

type Configuration = {
    name: string;
    type: string;
    args?: string[];
    env?: Record<string, string>;
    preLaunchTask?: string;
    presentation?: { hidden?: boolean };
};

test('client + server debugging connects the client to the address the server listens on', () => {
    // npm runs the test suite from tools/vscode-test-client.
    const root = path.resolve('../..');
    const launch = JSON.parse(readFileSync(path.join(root, '.vscode/launch.json'), 'utf8'));
    const tasks = JSON.parse(readFileSync(path.join(root, '.vscode/tasks.json'), 'utf8'));
    const labels = new Set(tasks.tasks.map((task: { label: string }) => task.label));
    const byName = new Map<string, Configuration>(
        launch.configurations.map((entry: Configuration) => [entry.name, entry]),
    );

    for (const compound of ['Windows', 'LLDB'].map((flavor) =>
        launch.compounds.find(
            (entry: { name: string }) =>
                entry.name === `Debug lspf-markdown client + server (${flavor})`,
        ),
    )) {
        assert.ok(compound, 'both debugger flavors have a compound');
        assert.equal(compound.stopAll, true);
        // The compound builds everything before either half starts, so the two
        // halves never race each other for the same Cargo build.
        assert.ok(labels.has(compound.preLaunchTask), `${compound.name} prepares the build`);
        const [server, client] = compound.configurations.map((name: string) => byName.get(name));
        assert.ok(server && client, `${compound.name} names existing configurations`);

        const listen = server.args?.[server.args.indexOf('--listen') + 1];
        assert.ok(listen, `${server.name} starts the server in --listen mode`);
        assert.equal(client.type, 'extensionHost');
        assert.equal(client.env?.LSPF_TEST_CONNECT, listen);
        // Alone, the client half has no server to dial.
        assert.equal(client.presentation?.hidden, true);
        assert.notEqual(client.preLaunchTask, server.preLaunchTask);
        for (const task of [server.preLaunchTask, client.preLaunchTask]) {
            assert.ok(task && labels.has(task), `${task} is a defined task`);
        }
    }
});
