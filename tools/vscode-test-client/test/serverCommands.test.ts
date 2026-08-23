import assert from 'node:assert/strict';
import { test } from 'node:test';

import { serverCommandArguments } from '../src/serverCommands.ts';

test('adds the active document URI when a server command needs one', () => {
    assert.deepEqual(
        serverCommandArguments(
            'lspf-hello.readFile',
            [],
            'file:///tmp/active.txt',
        ),
        ['file:///tmp/active.txt'],
    );
    assert.deepEqual(
        serverCommandArguments(
            'lspf-hello.outgoingJourney',
            [],
            'file:///tmp/active.txt',
        ),
        ['file:///tmp/active.txt'],
    );
});

test('preserves explicit arguments and commands that need no document', () => {
    const explicit = ['file:///tmp/explicit.txt'];
    assert.equal(
        serverCommandArguments('lspf-hello.readFile', explicit),
        explicit,
    );

    const noArguments: unknown[] = [];
    assert.equal(
        serverCommandArguments('lspf-hello.workspaceRoots', noArguments),
        noArguments,
    );
});

test('requires an active document when no URI was supplied', () => {
    assert.throws(
        () => serverCommandArguments('lspf-hello.outgoingJourney', []),
        /requires an active text document/,
    );
});
