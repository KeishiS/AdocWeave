#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CommandId {
    Convert,
    Preview,
    Check,
    Format,
    Symbols,
    ConfigShow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandSpec {
    pub(crate) id: CommandId,
    pub(crate) path: &'static [&'static str],
    pub(crate) summary: &'static str,
    pub(crate) help: &'static str,
}

pub(crate) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::Convert,
        path: &["convert"],
        summary: "Convert an AsciiDoc document",
        help: "Usage:\n  adocweave convert [OPTIONS] [FILE]\n\nExample:\n  adocweave convert --complete manual.adoc\n",
    },
    CommandSpec {
        id: CommandId::Preview,
        path: &["preview"],
        summary: "Serve a live, loopback-only document preview",
        help: "\
使用法:
  adocweave preview [OPTIONS] FILE

引数:
  FILE  プレビューするAsciiDocファイル（標準入力とシンボリックリンクは使用不可）

オプション:
  --bind ADDRESS  待ち受けるIPアドレス（既定値: 127.0.0.1）
  --port PORT  待ち受けるポート（既定値: 4000）
  --debounce-ms MILLISECONDS  連続した変更をまとめる待ち時間（既定値: 100）
  --allow-external  ループバック以外のIPアドレスでの待ち受けを許可
  --include  上限を設けてローカルincludeを展開
  --base-dir DIR  起点文書のincludeをDIRから解決
  --allow-root DIR  includeを許可する範囲（複数指定可）
  --css FILE  完全なHTML文書へCSSを埋め込み（複数指定可）
  --css-url URL  許可されたCSSのURLを追加（複数指定可）
  --config FILE  指定したプロジェクト設定を使用
  --no-config  プロジェクト設定の探索を無効化
  --color WHEN  端末表示の色をauto、always、neverから選択（既定値: auto）
  -h, --help  この説明を表示

安全性:
  ループバック以外のIPアドレスには--allow-externalが必要です。
  このサーバーは利用者認証とTLSによる通信の暗号化を提供しません。

例:
  adocweave preview --include manual.adoc
",
    },
    CommandSpec {
        id: CommandId::Check,
        path: &["check"],
        summary: "Check an AsciiDoc document",
        help: "Usage:\n  adocweave check [OPTIONS] [FILE...]\n\nExamples:\n  adocweave check --fail-on warning docs\n  adocweave check --format github --summary manual.adoc\n  adocweave check --format sarif docs > adocweave.sarif\n  adocweave check --fix docs\n",
    },
    CommandSpec {
        id: CommandId::Format,
        path: &["format"],
        summary: "Format an AsciiDoc document",
        help: "Usage:\n  adocweave format [OPTIONS] [FILE...]\n\nExamples:\n  adocweave format --check docs\n  adocweave format --diff manual.adoc\n  adocweave format --write docs\n",
    },
    CommandSpec {
        id: CommandId::Symbols,
        path: &["symbols"],
        summary: "Print document symbols as JSON",
        help: "Usage:\n  adocweave symbols [FILE]\n\nExample:\n  adocweave symbols manual.adoc\n",
    },
    CommandSpec {
        id: CommandId::ConfigShow,
        path: &["config", "show"],
        summary: "Print the resolved project configuration as JSON",
        help: "Usage:\n  adocweave config show [--config FILE | --no-config]\n\nExample:\n  adocweave config show\n",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LookupError<'a> {
    UnknownCommand(&'a str),
    MissingSubcommand(&'static str),
    UnknownSubcommand {
        parent: &'static str,
        value: &'a str,
    },
}

pub(crate) fn lookup(tokens: &[String]) -> Result<(CommandId, usize), LookupError<'_>> {
    let Some(first) = tokens.first() else {
        return Err(LookupError::UnknownCommand(""));
    };
    let candidates = COMMANDS
        .iter()
        .filter(|spec| spec.path.first() == Some(&first.as_str()))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(LookupError::UnknownCommand(first));
    }
    if let Some(spec) = candidates.iter().find(|spec| spec.path.len() == 1) {
        return Ok((spec.id, 1));
    }
    let parent = candidates[0].path[0];
    let Some(second) = tokens.get(1) else {
        return Err(LookupError::MissingSubcommand(parent));
    };
    candidates
        .iter()
        .find(|spec| spec.path.get(1) == Some(&second.as_str()))
        .map(|spec| (spec.id, 2))
        .ok_or(LookupError::UnknownSubcommand {
            parent,
            value: second,
        })
}

pub(crate) fn spec(id: CommandId) -> &'static CommandSpec {
    COMMANDS
        .iter()
        .find(|spec| spec.id == id)
        .expect("every CommandId has a CommandSpec")
}

pub(crate) fn root_help() -> String {
    let mut commands = String::new();
    for spec in COMMANDS {
        let path = spec.path.join(" ");
        if spec.path.len() == 1 {
            commands.push_str(&format!("  {path:<7}  {}\n", spec.summary));
        } else {
            commands.push_str(&format!("  {path}  {}\n", spec.summary));
        }
    }
    format!(
        "\
AdocWeave command-line interface

Usage:
  adocweave <COMMAND> [FILE]

Commands:
{commands}  completion SHELL  Print Bash, Zsh, Fish, or PowerShell completion
  help     Print this message

Arguments:
  [FILE]   Input file; omit it or use '-' to read standard input

Options:
  --format FORMAT  Emit check diagnostics as human, json, github, or sarif
  --json      Emit check diagnostics as JSON (deprecated alias)
  --fail-on LEVEL  Fail check on error, warning, or never (default: error)
  --summary   Emit check diagnostic counts to standard error
  --fix       Apply non-conflicting, always-safe check fixes
  --config FILE  Use an explicit project configuration
  --no-config    Disable project configuration discovery
  --list-rules  List available check rules; requires --json
  --enable-rule CODE  Enable an opt-in check rule; repeatable
  --check     Check formatting without writing formatted text
  --write     Atomically replace formatted input files
  --diff      Print unified formatting differences
  --dry-run   Report changes without writing them
  --glob PATTERN  Add files matching a glob pattern
  --color WHEN  Use auto, always, or never for terminal colors
  --include   Enable bounded local include processing
  --base-dir DIR    Resolve root document includes from DIR
  --allow-root DIR  Permit include resources below DIR; repeatable
  --local-targets     Check local file targets; check only
  --project-root DIR  Restrict local targets below DIR; requires --local-targets
  --complete  Convert to a complete HTML document instead of a fragment
  --css FILE      Embed CSS from FILE into the complete document; repeatable
  --css-url URL   Link an allowed stylesheet URL; repeatable
  --bind ADDRESS  Preview listen address (default: 127.0.0.1)
  --port PORT     Preview listen port (default: 4000)
  --debounce-ms MILLISECONDS  Preview rebuild debounce (default: 100)
  --allow-external  Permit an explicitly selected non-loopback address
  -V, --version  Print version
  -h, --help  Print help
"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn command_model_has_unique_ids_and_paths() {
        assert_eq!(COMMANDS.len(), 6);
        assert_eq!(
            COMMANDS.iter().map(|spec| spec.id).collect::<BTreeSet<_>>(),
            BTreeSet::from([
                CommandId::Convert,
                CommandId::Preview,
                CommandId::Check,
                CommandId::Format,
                CommandId::Symbols,
                CommandId::ConfigShow,
            ])
        );
        assert_eq!(
            COMMANDS
                .iter()
                .map(|spec| spec.path.join(" "))
                .collect::<BTreeSet<_>>()
                .len(),
            COMMANDS.len()
        );
        assert!(COMMANDS.iter().all(|spec| {
            !spec.path.is_empty()
                && spec.path.iter().all(|token| !token.is_empty())
                && !spec.summary.is_empty()
                && !spec.help.is_empty()
        }));
    }

    #[test]
    fn nested_command_paths_are_resolved_from_the_model() {
        let tokens = ["config", "show"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(lookup(&tokens), Ok((CommandId::ConfigShow, 2)));
        assert_eq!(
            lookup(&["config".to_owned()]),
            Err(LookupError::MissingSubcommand("config"))
        );
    }

    #[test]
    fn generated_help_matches_the_public_snapshots() {
        assert_eq!(
            root_help(),
            include_str!("../../tests/snapshots/help-root.txt")
        );
        let mut command_help = String::new();
        for command in COMMANDS {
            command_help.push_str(&format!("=== {} ===\n", command.path.join(" ")));
            command_help.push_str(command.help);
        }
        assert_eq!(
            command_help,
            include_str!("../../tests/snapshots/help-commands.txt")
        );
    }
}
