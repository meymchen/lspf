import * as path from 'node:path';

export interface ServerProfile {
    id: string;
    name: string;
    language: string;
    outputChannel: string;
    commandOutput: string | undefined;
}

export function serverProfile(serverBinary: string): ServerProfile {
    const executable = path.basename(serverBinary).replace(/\.exe$/, '');
    if (executable === 'lspf-markdown') {
        return {
            id: 'lspf-markdown',
            name: 'lspf Markdown',
            language: 'markdown',
            outputChannel: 'lspf-markdown',
            commandOutput: undefined,
        };
    }

    return {
        id: 'lspf-hello',
        name: 'lspf hello',
        language: 'plaintext',
        outputChannel: 'lspf-hello',
        commandOutput: 'lspf-hello commands',
    };
}
