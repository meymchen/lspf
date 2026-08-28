import * as path from 'node:path';

/**
 * Resolve the language server binary selected for this debug session.
 *
 * The binary is produced by `cargo build -p lspf-hello` and lands under
 * `target/debug/` as `lspf-hello` (`lspf-hello.exe` on Windows) — the workspace
 * member, not the old `target/debug/examples/hello` example path.
 *
 * @param repoRoot Absolute path to the lspf repository root.
 * @param platform Platform used to select the executable suffix.
 * @param exampleName Optional Cargo example selected through `LSPF_TEST_EXAMPLE`.
 * @param selectedServer Installed server command or path selected through
 * `LSPF_TEST_SERVER` or `LSPF_MARKDOWN_SERVER`.
 */
export function resolveServerBinary(
    repoRoot: string,
    platform: NodeJS.Platform = process.platform,
    exampleName: string | undefined = process.env.LSPF_TEST_EXAMPLE,
    selectedServer: string | undefined =
        process.env.LSPF_TEST_SERVER ?? process.env.LSPF_MARKDOWN_SERVER,
): string {
    if (selectedServer) {
        return selectedServer;
    }
    const suffix = platform === 'win32' ? '.exe' : '';
    if (exampleName) {
        if (!/^[a-z0-9][a-z0-9_-]*$/.test(exampleName)) {
            throw new Error(`invalid LSPF_TEST_EXAMPLE: ${exampleName}`);
        }
        return path.join(
            repoRoot,
            'target',
            'debug',
            'examples',
            `${exampleName}${suffix}`,
        );
    }

    return path.join(repoRoot, 'target', 'debug', `lspf-hello${suffix}`);
}
