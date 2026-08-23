import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const requiredModules = ['typescript/bin/tsc', 'vscode-languageclient/node'];

type ResolveModule = (specifier: string) => string;
type CommandResult = { error?: Error; status: number | null };
type RunCommand = (
    command: string,
    args: string[],
    options: { cwd: string; stdio: 'inherit' },
) => CommandResult;

export function missingDependencies(resolveModule: ResolveModule): string[] {
    return requiredModules.filter((specifier) => {
        try {
            resolveModule(specifier);
            return false;
        } catch {
            return true;
        }
    });
}

export function npmExecutable(platform: NodeJS.Platform): string {
    return platform === 'win32' ? 'npm.cmd' : 'npm';
}

export function ensureDependencies(
    resolveModule: ResolveModule = createRequire(import.meta.url).resolve,
    runCommand: RunCommand = spawnSync,
    platform: NodeJS.Platform = process.platform,
): void {
    const missing = missingDependencies(resolveModule);
    if (missing.length === 0) {
        return;
    }

    console.log(`Installing missing VS Code test-client dependencies: ${missing.join(', ')}`);
    const result = runCommand(npmExecutable(platform), ['ci'], {
        cwd: packageRoot,
        stdio: 'inherit',
    });
    if (result.error) {
        throw result.error;
    }
    if (result.status !== 0) {
        throw new Error(`npm ci exited with status ${result.status ?? 'unknown'}`);
    }
}

const invokedPath = process.argv[1] && path.resolve(process.argv[1]);
if (invokedPath === fileURLToPath(import.meta.url)) {
    ensureDependencies();
}
