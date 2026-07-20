fn main() {
    if let Err(error) = asciiloom_lsp::run_stdio() {
        eprintln!("asciiloom-lsp: {error}");
        std::process::exit(1);
    }
}
