use std::io::Write;
use std::process::{Command, Output, Stdio};

fn adocweave() -> Command {
    Command::new(env!("CARGO_BIN_EXE_adocweave"))
}

fn run_with_stdin(arguments: &[&str], input: &[u8]) -> Output {
    let mut child = adocweave()
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the adocweave binary should start");

    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(input)
        .expect("test input should be writable");

    child
        .wait_with_output()
        .expect("the adocweave binary should exit")
}

#[test]
fn every_subcommand_displays_help() {
    for command in ["convert", "check", "format"] {
        let output = adocweave()
            .args([command, "--help"])
            .output()
            .expect("the adocweave binary should run");

        assert!(output.status.success(), "{command} --help should succeed");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "{command} --help should display usage"
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn cli_reports_release_name_and_version() {
    let output = adocweave()
        .arg("--version")
        .output()
        .expect("the adocweave binary should run");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"adocweave 0.1.0\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn convert_reads_a_file() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/plain/basic.adoc"
    );
    let output = adocweave()
        .args(["convert", fixture])
        .output()
        .expect("the adocweave binary should run");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"<h1 class=\"document-title\" id=\"_adocweave\">AdocWeave</h1>\n<p>Small steps produce reliable software.</p>\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn format_reads_standard_input() {
    let source = b"= Document\n\nParagraph\n";
    let output = run_with_stdin(&["format", "-"], source);

    assert!(output.status.success());
    assert_eq!(output.stdout, source);
    assert!(output.stderr.is_empty());
}

#[test]
fn invalid_utf8_is_a_user_facing_error() {
    let output = run_with_stdin(&["convert", "-"], &[b'a', 0xff]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("input is not valid UTF-8"));
    assert!(stderr.contains("offset 1"));
}

#[test]
fn missing_file_is_a_user_facing_error() {
    let missing = "fixtures/plain/does-not-exist.adoc";
    let output = adocweave()
        .args(["check", missing])
        .output()
        .expect("the adocweave binary should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("could not read"));
    assert!(stderr.contains(missing));
}

#[test]
fn check_supports_human_and_json_diagnostics() {
    let source = b"trailing \n";
    let human = run_with_stdin(&["check", "-"], source);
    let json = run_with_stdin(&["check", "--json", "-"], source);

    assert!(human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stdout)
            .contains("1:9: warning[trailing-whitespace]: trailing whitespace")
    );
    assert!(json.status.success());
    assert!(
        String::from_utf8_lossy(&json.stdout).starts_with("[{\"id\":\"trailing-whitespace@8:9\"")
    );
}

#[test]
fn format_check_is_non_mutating_and_reports_needed_changes() {
    let formatted = run_with_stdin(&["format", "--check", "-"], b"clean\n");
    let unformatted = run_with_stdin(&["format", "--check", "-"], b"dirty  \n");

    assert!(formatted.status.success());
    assert!(formatted.stdout.is_empty());
    assert!(!unformatted.status.success());
    assert!(unformatted.stdout.is_empty());
    assert!(String::from_utf8_lossy(&unformatted.stderr).contains("not formatted"));
}

#[test]
fn symbols_command_emits_heading_hierarchy_as_json() {
    let output = run_with_stdin(&["symbols", "-"], b"= Title\n\n== Section\n=== Child\n");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("\"name\":\"Title\""));
    assert!(stdout.contains("\"name\":\"Section\""));
    assert!(stdout.contains("\"name\":\"Child\""));
}

#[test]
fn release_fixture_works_across_convert_check_and_format() {
    let source = include_bytes!("../../../fixtures/release/core.adoc");
    let expected_html = include_bytes!("../../../fixtures/release/core.html");

    let converted = run_with_stdin(&["convert", "-"], source);
    let checked = run_with_stdin(&["check", "--json", "-"], source);
    let formatted = run_with_stdin(&["format", "-"], source);

    assert!(converted.status.success());
    assert_eq!(converted.stdout, expected_html);
    assert!(converted.stderr.is_empty());
    assert!(checked.status.success());
    assert_eq!(checked.stdout, b"[]");
    assert!(checked.stderr.is_empty());
    assert!(formatted.status.success());
    assert_eq!(formatted.stdout, source);
    assert!(formatted.stderr.is_empty());
}
