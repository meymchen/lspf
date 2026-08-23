export function serverEnvironment(
    environment: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv {
    return {
        ...environment,
        RUST_LOG: environment.RUST_LOG ?? 'lspf=trace',
        LSPF_LOG_FORMAT: environment.LSPF_LOG_FORMAT ?? 'json',
    };
}
