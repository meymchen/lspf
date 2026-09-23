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
    const { ensureDependencies } = await import('../scripts/ensureDependencies.mts');
    const invocations: { command: string; args: string[]; shell: boolean }[] = [];
    for (const platform of ['win32', 'linux'] as const) {
        ensureDependencies(
            () => {
                throw new Error('missing');
            },
            (command, args, options) => {
                invocations.push({ command, args, shell: options.shell });
                return { status: 0 };
            },
            platform,
        );
    }

    assert.deepEqual(invocations, [
        { command: 'npm ci', args: [], shell: true },
        { command: 'npm', args: ['ci'], shell: false },
    ]);
});

test('reports a failed install', async () => {
    const { ensureDependencies } = await import('../scripts/ensureDependencies.mts');
    assert.throws(
        () =>
            ensureDependencies(
                () => {
                    throw new Error('missing');
                },
                () => ({ status: 1 }),
                'linux',
            ),
        /npm ci exited with status 1/,
    );
});
