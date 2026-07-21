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
                    "contracts": {
                        "coreProfile": adocweave::CORE_PROFILE_VERSION,
                        "coreApi": adocweave::CORE_API_VERSION,
                    }
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
            std::process::exit(2);
        }
    }
    if let Err(error) = adocweave_lsp::run_stdio().await {
        eprintln!("adocweave-lsp: {error}");
        std::process::exit(1);
    }
}
