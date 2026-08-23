import assert from 'node:assert/strict';
import { test } from 'node:test';

test('reports only dependencies that cannot be resolved', async () => {
    const { missingDependencies } = await import('../scripts/ensureDependencies.mts');
    const missing = missingDependencies((specifier) => {
        if (specifier === 'vscode-languageclient/node') {
            throw new Error('missing');
        }
        return `/resolved/${specifier}`;
    });

    assert.deepEqual(missing, ['vscode-languageclient/node']);
});

test('does not run npm when every dependency is present', async () => {
    const { ensureDependencies } = await import('../scripts/ensureDependencies.mts');
    let commandRuns = 0;
    ensureDependencies(
        (specifier) => `/resolved/${specifier}`,
        () => {
            commandRuns += 1;
            return { status: 0 };
        },
        'linux',
    );

    assert.equal(commandRuns, 0);
});

test('installs locked dependencies when a runtime module is missing', async () => {
    const { ensureDependencies, npmExecutable } = await import(
        '../scripts/ensureDependencies.mts'
    );
    let invocation: { command: string; args: string[] } | undefined;
    ensureDependencies(
        () => {
            throw new Error('missing');
        },
        (command, args) => {
            invocation = { command, args };
            return { status: 0 };
        },
        'win32',
    );

    assert.deepEqual(invocation, { command: 'npm.cmd', args: ['ci'] });
    assert.equal(npmExecutable('linux'), 'npm');
});
