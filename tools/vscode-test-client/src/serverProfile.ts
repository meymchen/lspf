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
    if (executable === 'native_tcp' || executable === 'native_websocket') {
        // The socket examples serve `examples/shared/mod.rs`: hover, completion,
        // and a custom request, over plain text documents. They advertise no
        // commands and no reverse request, so this profile enables neither.
        return {
            id: 'lspf-transport',
            name: `lspf ${executable === 'native_tcp' ? 'TCP' : 'WebSocket'} example`,
            language: 'plaintext',
            outputChannel: `lspf ${executable}`,
            commandOutput: undefined,
        };
    }
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
