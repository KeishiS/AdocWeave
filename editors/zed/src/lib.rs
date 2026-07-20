use zed_extension_api as zed;

struct AdocWeaveExtension;

impl zed::Extension for AdocWeaveExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let command = worktree.which("adocweave-lsp").ok_or_else(|| {
            "adocweave-lsp was not found on PATH; build it with `cargo build -p adocweave-lsp` \
             and expose target/debug on PATH"
                .to_owned()
        })?;
        Ok(zed::Command {
            command,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(AdocWeaveExtension);
