export const serverCommands = [
    { id: 'lspf-hello.workspaceRoots', requiresDocument: false },
    { id: 'lspf-hello.readFile', requiresDocument: true },
    { id: 'lspf-hello.outgoingJourney', requiresDocument: true },
    { id: 'lspf-hello.cancellableProgress', requiresDocument: false },
] as const;

export type ServerCommandId = (typeof serverCommands)[number]['id'];

export function serverCommandArguments(
    command: string,
    args: unknown[],
    activeDocumentUri?: string,
): unknown[] {
    const definition = serverCommands.find(({ id }) => id === command);
    if (!definition?.requiresDocument || args.length > 0) {
        return args;
    }
    if (!activeDocumentUri) {
        throw new Error(`${command} requires an active text document`);
    }

    return [activeDocumentUri];
}
