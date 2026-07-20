use std::io::Write;
use std::process::{Command, Output, Stdio};

fn asciiloom() -> Command {
    Command::new(env!("CARGO_BIN_EXE_asciiloom"))
}

fn run_with_stdin(arguments: &[&str], input: &[u8]) -> Output {
    let mut child = asciiloom()
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the asciiloom binary should start");

    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(input)
        .expect("test input should be writable");

    child
        .wait_with_output()
        .expect("the asciiloom binary should exit")
}

#[test]
fn every_subcommand_displays_help() {
    for command in ["convert", "check", "format"] {
        let output = asciiloom()
            .args([command, "--help"])
            .output()
            .expect("the asciiloom binary should run");

        assert!(output.status.success(), "{command} --help should succeed");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "{command} --help should display usage"
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn convert_reads_a_file() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/plain/basic.adoc"
    );
    let output = asciiloom()
        .args(["convert", fixture])
        .output()
        .expect("the asciiloom binary should run");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        include_bytes!("../../../fixtures/plain/basic.adoc")
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
    let output = asciiloom()
        .args(["check", missing])
        .output()
        .expect("the asciiloom binary should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("could not read"));
    assert!(stderr.contains(missing));
}
