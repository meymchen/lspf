import * as path from 'node:path';

/**
 * Resolve the `lspf-hello` language server binary inside the lspf repository.
 *
 * The binary is produced by `cargo build -p lspf-hello` and lands at
 * `target/debug/lspf-hello` — the workspace member, not the old
 * `target/debug/examples/hello` example path.
 *
 * @param repoRoot Absolute path to the lspf repository root.
 */
export function resolveServerBinary(repoRoot: string): string {
    return path.join(repoRoot, 'target', 'debug', 'lspf-hello');
}
