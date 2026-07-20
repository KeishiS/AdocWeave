#[tokio::main]
async fn main() {
    if let Err(error) = adocweave_lsp::run_stdio().await {
        eprintln!("adocweave-lsp: {error}");
        std::process::exit(1);
    }
}
