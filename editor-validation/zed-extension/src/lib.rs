use zed_extension_api::{self as zed, LanguageServerId, Result, settings::LspSettings};

struct LspfMarkdownExtension;

impl zed::Extension for LspfMarkdownExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let configured = LspSettings::for_worktree("lspf-markdown", worktree)
            .ok()
            .and_then(|settings| settings.binary);
        let command = configured
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .or_else(|| worktree.which("lspf-markdown"))
            .ok_or_else(|| {
                "lspf-markdown was not found; install it or set lsp.lspf-markdown.binary.path"
                    .to_owned()
            })?;
        let args = configured
            .and_then(|binary| binary.arguments)
            .unwrap_or_default();

        Ok(zed::Command {
            command,
            args,
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(LspfMarkdownExtension);
