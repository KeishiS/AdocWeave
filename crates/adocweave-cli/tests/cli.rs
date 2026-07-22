use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("adocweave {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn cli_reports_machine_readable_release_contracts() {
    let output = adocweave()
        .args(["--version", "--json"])
        .output()
        .expect("the adocweave binary should run");

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("version JSON");
    assert_eq!(value["packageVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        value["contracts"]["coreProfile"],
        adocweave::CORE_PROFILE_VERSION
    );
    assert_eq!(value["contracts"]["coreApi"], adocweave::CORE_API_VERSION);
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

#[test]
fn core_profile_fixture_is_shared_by_cli_conversion_and_symbols() {
    let source = include_bytes!("../../../fixtures/conformance/full.adoc");
    let expected_html = include_bytes!("../../../fixtures/conformance/full.html");

    let converted = run_with_stdin(&["convert", "-"], source);
    let symbols = run_with_stdin(&["symbols", "-"], source);

    assert!(converted.status.success());
    assert_eq!(converted.stdout, expected_html);
    assert!(converted.stderr.is_empty());
    assert!(symbols.status.success());
    assert!(String::from_utf8_lossy(&symbols.stdout).contains("統合文書"));
}

#[test]
fn local_includes_require_an_explicit_option_and_are_deterministic() {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/includes/root.adoc"
    );
    let expected = include_bytes!("../../../fixtures/includes/root.html");

    let disabled = adocweave()
        .args(["convert", root])
        .output()
        .expect("disabled conversion");
    assert!(disabled.status.success());
    assert!(
        !disabled
            .stdout
            .windows("After.".len())
            .any(|value| value == b"After.")
    );

    let first = adocweave()
        .args(["convert", "--include", root])
        .output()
        .expect("included conversion");
    let second = adocweave()
        .args(["convert", "--include", root])
        .output()
        .expect("repeated conversion");
    let symbols = adocweave()
        .args(["symbols", "--include", root])
        .output()
        .expect("included symbols");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(first.stdout, expected);
    assert_eq!(second.stdout, first.stdout);
    assert!(String::from_utf8_lossy(&symbols.stdout).contains("Included section"));
}

#[test]
fn stdin_include_requires_a_base_and_rejects_traversal() {
    let missing_base = run_with_stdin(&["convert", "--include", "-"], b"text\n");
    assert!(!missing_base.status.success());
    assert!(String::from_utf8_lossy(&missing_base.stderr).contains("requires --base-dir"));

    let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/includes");
    let traversal = run_with_stdin(
        &["convert", "--include", "--base-dir", base, "-"],
        b"include::../plain/basic.adoc[]\n",
    );
    assert!(!traversal.status.success());
    assert!(String::from_utf8_lossy(&traversal.stderr).contains("unsafe local resource target"));

    let missing = run_with_stdin(
        &["format", "--include", "--base-dir", base, "-"],
        b"include::missing.adoc[]\n",
    );
    assert!(
        !missing.status.success(),
        "format validates the include tree"
    );
}

#[test]
fn include_check_projects_diagnostics_to_the_resource_file() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-cli-{unique}"));
    std::fs::create_dir_all(&root).expect("directory");
    let document = root.join("root.adoc");
    let part = root.join("part.adoc");
    std::fs::write(&document, "include::part.adoc[]\n").expect("root source");
    std::fs::write(&part, "bad \n").expect("part source");

    let human = adocweave()
        .args(["check", "--include", document.to_str().expect("UTF-8 path")])
        .output()
        .expect("human check");
    let json = adocweave()
        .args([
            "check",
            "--include",
            "--json",
            document.to_str().expect("UTF-8 path"),
        ])
        .output()
        .expect("JSON check");
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains(&format!(
        "{}:1:4: warning[trailing-whitespace]",
        part.display()
    )));
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON diagnostics");
    assert_eq!(value[0]["sourceId"], part.to_string_lossy().as_ref());

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn include_provider_rejects_a_symlink_escape() {
    use std::os::unix::fs::symlink;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-cli-root-{unique}"));
    let outside = std::env::temp_dir().join(format!("adocweave-cli-outside-{unique}.adoc"));
    std::fs::create_dir_all(&root).expect("directory");
    std::fs::write(&outside, "outside\n").expect("outside source");
    std::fs::write(root.join("root.adoc"), "include::escape.adoc[]\n").expect("root source");
    symlink(&outside, root.join("escape.adoc")).expect("symlink");

    let output = adocweave()
        .args([
            "convert",
            "--include",
            root.join("root.adoc").to_str().expect("UTF-8 path"),
        ])
        .output()
        .expect("conversion");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside configured roots"));

    std::fs::remove_dir_all(root).expect("cleanup root");
    std::fs::remove_file(outside).expect("cleanup outside");
}
