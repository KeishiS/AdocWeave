use zed_extension_api as zed;

struct AsciiLoomExtension;

impl zed::Extension for AsciiLoomExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let command = worktree.which("asciiloom-lsp").ok_or_else(|| {
            "asciiloom-lsp was not found on PATH; build it with `cargo build -p asciiloom-lsp` \
             and expose target/debug on PATH"
                .to_owned()
        })?;
        Ok(zed::Command {
            command,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> zed::Result<Option<serde_json::Value>> {
        Ok(Some(serde_json::json!({
            "syntaxMode": "permissive"
        })))
    }
}

zed::register_extension!(AsciiLoomExtension);
