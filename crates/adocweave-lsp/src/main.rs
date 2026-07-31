use adocweave_host::ExitStatus;

#[tokio::main]
async fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {}
        ["-V" | "--version"] => {
            println!("adocweave-lsp {}", adocweave_lsp::VERSION);
            return;
        }
        ["--version", "--json"] => {
            println!(
                "{}",
                serde_json::json!({
                    "name": adocweave_lsp::SERVER_NAME,
                    "packageVersion": adocweave_lsp::VERSION,
                })
            );
            return;
        }
        ["-h" | "--help"] => {
            println!("Usage: adocweave-lsp [--version [--json]]");
            return;
        }
        _ => {
            eprintln!("adocweave-lsp: unsupported arguments");
            std::process::exit(i32::from(ExitStatus::Usage.code()));
        }
    }
    if let Err(error) = adocweave_lsp::run_stdio().await {
        // The Language Server Protocol fixes the status for `exit` without a
        // preceding `shutdown`, so that case keeps the number the specification
        // names. Anything else ended because the standard input and output
        // transport failed.
        let status = if matches!(error, async_lsp::Error::Protocol(_)) {
            ExitStatus::Diagnostics
        } else {
            ExitStatus::InputOutput
        };
        eprintln!("adocweave-lsp: {error}");
        std::process::exit(i32::from(status.code()));
    }
}
