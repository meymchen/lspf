import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';

test('Notebook debug entries launch the example and provide an openable notebook', () => {
    // npm runs the test suite from tools/vscode-test-client.
    const root = path.resolve('../..');
    const launch = JSON.parse(readFileSync(path.join(root, '.vscode/launch.json'), 'utf8'));
    const picker = launch.inputs.find((input: { id: string }) => input.id === 'exampleName');
    assert.ok(picker.options.includes('notebooks'), 'the example picker must include notebooks');
    const configuration = launch.configurations.find(
        (entry: { name: string }) => entry.name === 'Run Notebook example client',
    );
    assert.equal(configuration.env.LSPF_TEST_EXAMPLE, 'notebooks');
    const sampleArgument = configuration.args.find((arg: string) => arg.endsWith('.ipynb'));
    assert.ok(sampleArgument, 'the dedicated launch opens a sample notebook');
    const sample = JSON.parse(readFileSync(
        path.join(root, sampleArgument.replace('${workspaceFolder}/', '')), 'utf8',
    ));
    assert.equal(sample.nbformat, 4);
    assert.ok(sample.cells.filter((cell: { cell_type: string }) => cell.cell_type === 'code').length >= 2);
    const manifest = JSON.parse(readFileSync(path.join(root, 'tools/vscode-test-client/package.json'), 'utf8'));
    assert.ok(manifest.activationEvents.includes('onNotebook:jupyter-notebook'));
});
