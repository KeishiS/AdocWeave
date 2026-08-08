//! Shell completion script rendering derived from the declarative CLI model.

use crate::arguments::CompletionShell;
use crate::commands::model::CompletionTree;

pub(crate) fn render_completion_script(shell: CompletionShell, tree: &CompletionTree) -> String {
    let shell_words = |values: &[&str]| values.join(" ");
    let powershell_words = |values: &[&str]| {
        values
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let completion_words = |command| {
        let mut words = Vec::new();
        for option in crate::commands::model::options_for_command(command) {
            for value in option.names.iter().chain(option.candidates()) {
                if !words.contains(value) {
                    words.push(*value);
                }
            }
        }
        words
    };
    let mut contract = format!("# adocweave-command-tree root={}\n", tree.roots.join(","));
    for group in &tree.nested {
        contract.push_str(&format!(
            "# adocweave-command-tree parent={} children={}\n",
            group.parent.join("/"),
            group.children.join(",")
        ));
    }
    for (command, path) in &tree.commands {
        for option in crate::commands::model::options_for_command(*command) {
            contract.push_str(&format!(
                "# adocweave-option command={} names={} metavar={} values={}\n",
                path.join("/"),
                option.names.join(","),
                option.metavar().unwrap_or("-"),
                option.candidates().join(","),
            ));
        }
    }
    let rendered = match shell {
        CompletionShell::Bash => {
            let nested_declarations = tree
                .nested
                .iter()
                .enumerate()
                .map(|(index, group)| {
                    format!(
                        "  local nested_{index}=\"{}\"",
                        shell_words(&group.children)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let option_declarations = tree
                .commands
                .iter()
                .enumerate()
                .filter_map(|(index, (command, _))| {
                    let options = completion_words(*command);
                    (!options.is_empty()).then(|| {
                        format!(
                            "  local command_options_{index}=\"{}\"",
                            shell_words(&options)
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
            let nested_branches = tree
                .nested
                .iter()
                .enumerate()
                .map(|(index, group)| {
                    let conditions = group
                        .parent
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("${{COMP_WORDS[{position_plus_one}]}} == {token}", position_plus_one = position + 1)
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    format!(
                        "  elif [[ ${{COMP_CWORD}} -eq {word_index} && {conditions} ]]; then\n    COMPREPLY=( $(compgen -W \"${{nested_{index}}}\" -- \"${{COMP_WORDS[COMP_CWORD]}}\") )",
                        word_index = group.parent.len() + 1,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let option_branches = tree
                .commands
                .iter()
                .enumerate()
                .filter_map(|(index, (command, path))| {
                    let options = completion_words(*command);
                    if options.is_empty() {
                        return None;
                    }
                    let conditions = path
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!(
                                "${{COMP_WORDS[{position_plus_one}]}} == {token}",
                                position_plus_one = position + 1
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    Some(format!(
                        "  elif [[ ${{COMP_CWORD}} -gt {path_len} && {conditions} ]]; then\n    COMPREPLY=( $(compgen -W \"${{command_options_{index}}}\" -f -- \"${{COMP_WORDS[COMP_CWORD]}}\") )",
                        path_len = path.len(),
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"_adocweave() {
  local commands="@ROOTS@"
@NESTED_DECLARATIONS@
@OPTION_DECLARATIONS@
  if [[ ${COMP_CWORD} -eq 1 ]]; then
    COMPREPLY=( $(compgen -W "${commands}" -- "${COMP_WORDS[COMP_CWORD]}") )
@NESTED_BRANCHES@
@OPTION_BRANCHES@
  else
    COMPREPLY=( $(compgen -f -- "${COMP_WORDS[COMP_CWORD]}") )
  fi
}
complete -F _adocweave adocweave
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED_DECLARATIONS@", &nested_declarations)
            .replace("@OPTION_DECLARATIONS@", &option_declarations)
            .replace("@NESTED_BRANCHES@", &nested_branches)
            .replace("@OPTION_BRANCHES@", &option_branches)
        }
        CompletionShell::Zsh => {
            let nested_branches = tree
                .nested
                .iter()
                .map(|group| {
                    let conditions = group
                        .parent
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("$words[{}] == {token}", position + 2)
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    format!(
                        "  elif [[ $CURRENT -eq {current} && {conditions} ]]; then\n    _values 'commands below {parent}' {children}",
                        current = group.parent.len() + 2,
                        parent = group.parent.join(" "),
                        children = shell_words(&group.children),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let option_branches = tree
                .commands
                .iter()
                .filter_map(|(command, path)| {
                    let options = completion_words(*command);
                    if options.is_empty() {
                        return None;
                    }
                    let conditions = path
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("$words[{}] == {token}", position + 2)
                        })
                        .collect::<Vec<_>>()
                        .join(" && ");
                    Some(format!(
                        "  elif [[ $CURRENT -gt {current} && {conditions} ]]; then\n    _values 'arguments for {parent}' {options}",
                        current = path.len() + 1,
                        parent = path.join(" "),
                        options = shell_words(&options),
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"#compdef adocweave
_adocweave() {
  if (( CURRENT == 2 )); then
    _values 'commands' @ROOTS@
@NESTED_BRANCHES@
@OPTION_BRANCHES@
  else
    _files
  fi
}
compdef _adocweave adocweave
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED_BRANCHES@", &nested_branches)
            .replace("@OPTION_BRANCHES@", &option_branches)
        }
        CompletionShell::Fish => {
            let nested = tree
                .nested
                .iter()
                .map(|group| {
                    format!(
                        "complete -c adocweave -f -n '__adocweave_at_path {}' -a '{}'",
                        group.parent.join(" "),
                        shell_words(&group.children),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let options = tree
                .commands
                .iter()
                .flat_map(|(command, path)| {
                    crate::commands::model::options_for_command(*command).map(move |option| {
                        let names = option
                            .names
                            .iter()
                            .map(|name| {
                                if let Some(long) = name.strip_prefix("--") {
                                    format!("-l {long}")
                                } else {
                                    format!("-s {}", &name[1..])
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let value = option.metavar().map_or_else(String::new, |_| {
                            let candidates = option.candidates();
                            if candidates.is_empty() {
                                " -r".to_owned()
                            } else {
                                format!(" -r -a '{}'", shell_words(candidates))
                            }
                        });
                        format!(
                            "complete -c adocweave -f -n '__adocweave_uses_command {}' {names}{value}",
                            path.join(" "),
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"function __adocweave_at_path
  set -l expected $argv
  set -l words (commandline -opc)
  test (count $words) -eq (math (count $expected) + 1); or return 1
  for index in (seq (count $expected))
    test $words[(math $index + 1)] = $expected[$index]; or return 1
  end
end
function __adocweave_uses_command
  set -l expected $argv
  set -l words (commandline -opc)
  test (count $words) -ge (math (count $expected) + 1); or return 1
  for index in (seq (count $expected))
    test $words[(math $index + 1)] = $expected[$index]; or return 1
  end
end
complete -c adocweave -f -n '__fish_use_subcommand' -a '@ROOTS@'
@NESTED@
@OPTIONS@
"#
            .replace("@ROOTS@", &shell_words(&tree.roots))
            .replace("@NESTED@", &nested)
            .replace("@OPTIONS@", &options)
        }
        CompletionShell::PowerShell => {
            let mut groups = tree.nested.iter().collect::<Vec<_>>();
            groups.sort_by_key(|group| std::cmp::Reverse(group.parent.len()));
            let nested = groups
                .into_iter()
                .map(|group| {
                    let conditions = group
                        .parent
                        .iter()
                        .enumerate()
                        .map(|(position, token)| {
                            format!("$words[{}] -eq '{token}'", position + 1)
                        })
                        .collect::<Vec<_>>()
                        .join(" -and ");
                    format!(
                        "  }} elseif ({conditions} -and ($words.Count -eq {parent_count} -or ($words.Count -eq {child_count} -and $wordToComplete -ne ''))) {{\n    @({children})",
                        parent_count = group.parent.len() + 1,
                        child_count = group.parent.len() + 2,
                        children = powershell_words(&group.children),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let option_branches = tree
                .commands
                .iter()
                .filter_map(|(command, path)| {
                    let options = completion_words(*command);
                    if options.is_empty() {
                        return None;
                    }
                    let conditions = path
                        .iter()
                        .enumerate()
                        .map(|(position, token)| format!("$words[{}] -eq '{token}'", position + 1))
                        .collect::<Vec<_>>()
                        .join(" -and ");
                    Some(format!(
                        "  }} elseif ({conditions}) {{\n    @({options})",
                        options = powershell_words(&options),
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            r#"Register-ArgumentCompleter -Native -CommandName adocweave -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)
  $words = @($commandAst.CommandElements | ForEach-Object { $_.Value })
  $candidates = if ($false) {
    @()
@NESTED@
@OPTION_BRANCHES@
  } elseif ($words.Count -le 2) {
    @(@ROOTS@)
  } else {
    @()
  }
  $candidates |
    Where-Object { $_ -like "$wordToComplete*" } |
    ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }
}
"#
            .replace("@ROOTS@", &powershell_words(&tree.roots))
            .replace("@NESTED@", &nested)
            .replace("@OPTION_BRANCHES@", &option_branches)
        }
    };
    format!("{contract}{rendered}")
}
