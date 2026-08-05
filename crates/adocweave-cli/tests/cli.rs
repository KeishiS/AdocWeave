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

/// Reads the address `preview` bound, from the line it prints on startup.
///
/// Choosing a port by binding one, closing it and handing the number to
/// `preview` leaves a gap in which any other process can take that port, and
/// the test then fails for a reason that has nothing to do with what it checks.
/// Passing `--port 0` closes the gap: the operating system assigns the port to
/// `preview` itself, which never releases it, and reports which one it got.
#[cfg(unix)]
fn preview_address(stderr: &mut std::process::ChildStderr) -> std::net::SocketAddr {
    use std::io::{BufRead, BufReader};

    let mut line = String::new();
    BufReader::new(stderr)
        .read_line(&mut line)
        .expect("preview startup line");
    let address = line
        .trim()
        .strip_prefix("AdocWeave preview: http://")
        .and_then(|rest| rest.strip_suffix('/'))
        .unwrap_or_else(|| panic!("unexpected preview startup line: {line:?}"));
    address.parse().expect("preview address")
}

#[cfg(unix)]
fn try_preview_get(address: std::net::SocketAddr, path: &str) -> Option<String> {
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::Duration;

    for _ in 0..200 {
        if let Ok(mut stream) = TcpStream::connect(address)
            && write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\n\r\n").is_ok()
        {
            let mut response = String::new();
            if stream.read_to_string(&mut response).is_ok() {
                return Some(response);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[cfg(unix)]
fn preview_get(address: std::net::SocketAddr, path: &str) -> String {
    try_preview_get(address, path).expect("preview response timed out")
}

#[cfg(unix)]
fn stop_preview(child: &mut std::process::Child) {
    use std::os::unix::process::ExitStatusExt;

    // SAFETY: the child PID is live and SIGTERM has no pointer arguments.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = child.wait().expect("preview exit");
    assert!(status.success(), "{:?}", status.signal());
}

#[test]
fn configured_resource_limit_rejects_root_before_processing() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join(".adocweave.toml"),
        "schema-version = 1\n[resources]\nmax-files = 1\nmax-total-bytes = 4\nmax-resource-bytes = 4\n",
    )
    .expect("configuration");
    std::fs::write(root.path().join("document.adoc"), "12345").expect("document");

    let output = adocweave()
        .current_dir(root.path())
        .args(["check", "document.adoc"])
        .output()
        .expect("command");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("single-resource byte limit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn workspace_scan_excludes_do_not_filter_explicit_cli_inputs() {
    let root = tempfile::tempdir().expect("root");
    let excluded = root.path().join("generated");
    std::fs::create_dir(&excluded).expect("excluded directory");
    std::fs::write(
        root.path().join(".adocweave.toml"),
        "schema-version = 1\n[workspace.scan]\nexclude = [\"generated\"]\n",
    )
    .expect("configuration");
    std::fs::write(excluded.join("document.adoc"), "= Title\n\ntext\n").expect("document");

    let output = adocweave()
        .current_dir(root.path())
        .args(["check", "generated/document.adoc"])
        .output()
        .expect("command");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn each_kind_of_failure_reports_its_own_exit_status() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("clean.adoc"), "= Title\n\ntext\n").expect("clean document");
    std::fs::write(root.path().join("broken.adoc"), "= Title\n\n<<missing>>\n")
        .expect("document with a diagnostic");
    std::fs::write(
        root.path().join(".adocweave.toml"),
        "schema-version = 1\n[resources]\nmax-files = 1\nmax-total-bytes = 4\nmax-resource-bytes = 4\n",
    )
    .expect("configuration");
    let run = |arguments: &[&str]| {
        adocweave()
            .current_dir(root.path())
            .args(arguments)
            .output()
            .expect("command")
            .status
            .code()
    };

    // 0: the work finished and nothing reached the failure threshold.
    assert_eq!(run(&["check", "--no-config", "clean.adoc"]), Some(0));
    // 1: diagnostics reached the threshold.
    assert_eq!(
        run(&[
            "check",
            "--no-config",
            "--fail-on",
            "warning",
            "broken.adoc"
        ]),
        Some(1)
    );
    // 2: the command cannot be acted on as written.
    assert_eq!(run(&["not-a-command"]), Some(2));
    assert_eq!(
        run(&["check", "--no-config", "--fail-on", "bogus", "clean.adoc"]),
        Some(2)
    );
    // 3: a file could not be read.
    assert_eq!(run(&["check", "--no-config", "absent.adoc"]), Some(3));
    // 4: a configured limit stopped the work.
    assert_eq!(run(&["check", "clean.adoc"]), Some(4));
}

#[test]
fn only_a_misused_command_is_pointed_at_the_help_text() {
    let root = tempfile::tempdir().expect("root");
    let stderr = |arguments: &[&str]| {
        String::from_utf8_lossy(
            &adocweave()
                .current_dir(root.path())
                .args(arguments)
                .output()
                .expect("command")
                .stderr,
        )
        .into_owned()
    };

    assert!(stderr(&["not-a-command"]).contains("--help"));
    // A missing file is not a misuse of the command, so the help text would not
    // tell the caller anything about it.
    assert!(!stderr(&["check", "--no-config", "absent.adoc"]).contains("--help"));
}

#[test]
fn configured_resource_limit_bounds_standard_input_while_reading() {
    let root = tempfile::tempdir().expect("root");
    let config = root.path().join(".adocweave.toml");
    std::fs::write(
        &config,
        "schema-version = 1\n[resources]\nmax-files = 1\nmax-total-bytes = 4\nmax-resource-bytes = 4\n",
    )
    .expect("configuration");
    let mut child = adocweave()
        .current_dir(root.path())
        .args(["check", "--config", ".adocweave.toml", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("command");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"12345")
        .expect("input");
    let output = child.wait_with_output().expect("output");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("single-resource byte limit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explicit_primary_parent_does_not_expand_include_root_authority() {
    let root = tempfile::tempdir().expect("root");
    let includes = root.path().join("includes");
    std::fs::create_dir(&includes).expect("include root");
    std::fs::write(
        root.path().join(".adocweave.toml"),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\"includes\"]\nmax-files = 2\nmax-total-bytes = 128\nmax-resource-bytes = 128\n",
    )
    .expect("configuration");
    std::fs::write(
        root.path().join("document.adoc"),
        "include::includes/part.adoc[]\n",
    )
    .expect("primary");
    std::fs::write(includes.join("part.adoc"), "included\n").expect("include");
    std::fs::write(root.path().join("outside.adoc"), "outside\n").expect("outside");

    let accepted = adocweave()
        .current_dir(root.path())
        .args(["check", "document.adoc"])
        .output()
        .expect("authorized include");
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    std::fs::write(
        root.path().join("document.adoc"),
        "include::outside.adoc[]\n",
    )
    .expect("outside include request");
    let rejected = adocweave()
        .current_dir(root.path())
        .args(["check", "document.adoc"])
        .output()
        .expect("unauthorized include");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("outside"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn analysis_resource_count_includes_root_and_includes() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join(".adocweave.toml"),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\nmax-files = 1\nmax-total-bytes = 64\nmax-resource-bytes = 64\n",
    )
    .expect("configuration");
    std::fs::write(
        root.path().join("root.adoc"),
        include_bytes!("../../../fixtures/resource-limits/root-with-include.adoc"),
    )
    .expect("root document");
    std::fs::write(
        root.path().join("part.adoc"),
        include_bytes!("../../../fixtures/resource-limits/part.adoc"),
    )
    .expect("included document");

    let output = adocweave()
        .current_dir(root.path())
        .args(["check", "root.adoc"])
        .output()
        .expect("command");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("file limit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn multi_project_scan_applies_file_limits_per_resolved_scope() {
    const FILES_PER_PROJECT: usize = 6_000;

    let root = tempfile::tempdir().expect("root");
    for name in ["project-a", "project-b"] {
        let project = root.path().join(name);
        std::fs::create_dir(&project).expect("project directory");
        std::fs::write(
            project.join(".adocweave.toml"),
            format!(
                "schema-version = 1\n[resources]\nmax-files = {FILES_PER_PROJECT}\nmax-total-bytes = 1048576\nmax-resource-bytes = 1024\n"
            ),
        )
        .expect("configuration");
        for index in 0..FILES_PER_PROJECT {
            std::fs::write(project.join(format!("{index:04}.adoc")), "").expect("document");
        }
    }

    let accepted = adocweave()
        .current_dir(root.path())
        .args(["format", "--check", "project-a", "project-b"])
        .output()
        .expect("two-project command");
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    std::fs::write(root.path().join("project-a/overflow.adoc"), "").expect("overflow document");
    let rejected = adocweave()
        .current_dir(root.path())
        .args(["format", "--check", "project-a", "project-b"])
        .output()
        .expect("over-limit command");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("filesystem resource count limit exceeded: 6000"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn multi_path_byte_budget_is_shared_only_within_the_resolved_project() {
    let root = tempfile::tempdir().expect("root");
    let same = root.path().join("same");
    std::fs::create_dir(&same).expect("same project");
    std::fs::write(
        same.join(".adocweave.toml"),
        "schema-version = 1\n[resources]\nmax-files = 4\nmax-total-bytes = 3\nmax-resource-bytes = 3\n",
    )
    .expect("same config");
    std::fs::write(same.join("a.adoc"), "aa").expect("first");
    std::fs::write(same.join("b.adoc"), "bb").expect("second");

    let rejected = adocweave()
        .current_dir(root.path())
        .args(["format", "--check", "same/a.adoc", "same/b.adoc"])
        .output()
        .expect("same project");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("total byte limit"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    for name in ["one", "two"] {
        let project = root.path().join(name);
        std::fs::create_dir(&project).expect("project");
        std::fs::write(
            project.join(".adocweave.toml"),
            "schema-version = 1\n[resources]\nmax-files = 2\nmax-total-bytes = 3\nmax-resource-bytes = 3\n",
        )
        .expect("config");
        std::fs::write(project.join("document.adoc"), "xx").expect("document");
    }
    let accepted = adocweave()
        .current_dir(root.path())
        .args([
            "format",
            "--check",
            "one/document.adoc",
            "two/document.adoc",
        ])
        .output()
        .expect("separate projects");
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

#[test]
fn multi_path_include_budget_is_shared_only_within_the_resolved_project() {
    const FIRST_SOURCE: &str = "include::first-part.adoc[]\n";
    const SECOND_SOURCE: &str = "include::second-part.adoc[]\n";
    const PART: &str = "part\n";

    let root = tempfile::tempdir().expect("root");
    let same_files = root.path().join("same-files");
    std::fs::create_dir(&same_files).expect("file-limit project");
    std::fs::write(
        same_files.join(".adocweave.toml"),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\nmax-files = 3\nmax-total-bytes = 1024\nmax-resource-bytes = 1024\n",
    )
    .expect("file-limit config");
    std::fs::write(same_files.join("a.adoc"), FIRST_SOURCE).expect("first primary");
    std::fs::write(same_files.join("b.adoc"), SECOND_SOURCE).expect("second primary");
    std::fs::write(same_files.join("first-part.adoc"), PART).expect("first include");
    std::fs::write(same_files.join("second-part.adoc"), PART).expect("second include");

    let file_rejected = adocweave()
        .current_dir(root.path())
        .args([
            "format",
            "--check",
            "same-files/a.adoc",
            "same-files/b.adoc",
        ])
        .output()
        .expect("shared file budget");
    assert!(!file_rejected.status.success());
    assert!(
        String::from_utf8_lossy(&file_rejected.stderr).contains("file limit exceeded: 3"),
        "{}",
        String::from_utf8_lossy(&file_rejected.stderr)
    );

    let same_bytes = root.path().join("same-bytes");
    std::fs::create_dir(&same_bytes).expect("byte-limit project");
    let byte_limit = FIRST_SOURCE.len() + PART.len() + SECOND_SOURCE.len();
    std::fs::write(
        same_bytes.join(".adocweave.toml"),
        format!(
            "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\nmax-files = 4\nmax-total-bytes = {byte_limit}\nmax-resource-bytes = {byte_limit}\n"
        ),
    )
    .expect("byte-limit config");
    std::fs::write(same_bytes.join("a.adoc"), FIRST_SOURCE).expect("first primary");
    std::fs::write(same_bytes.join("b.adoc"), SECOND_SOURCE).expect("second primary");
    std::fs::write(same_bytes.join("first-part.adoc"), PART).expect("first include");
    std::fs::write(same_bytes.join("second-part.adoc"), PART).expect("second include");

    let byte_rejected = adocweave()
        .current_dir(root.path())
        .args([
            "format",
            "--check",
            "same-bytes/a.adoc",
            "same-bytes/b.adoc",
        ])
        .output()
        .expect("shared byte budget");
    assert!(!byte_rejected.status.success());
    assert!(
        String::from_utf8_lossy(&byte_rejected.stderr).contains("byte limit exceeded"),
        "{}",
        String::from_utf8_lossy(&byte_rejected.stderr)
    );

    const PROJECT_SOURCE: &str = "include::part.adoc[]\n";
    for name in ["one", "two"] {
        let project = root.path().join(name);
        std::fs::create_dir(&project).expect("independent project");
        let project_limit = PROJECT_SOURCE.len() + PART.len();
        std::fs::write(
            project.join(".adocweave.toml"),
            format!(
                "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\nmax-files = 2\nmax-total-bytes = {project_limit}\nmax-resource-bytes = {project_limit}\n"
            ),
        )
        .expect("independent config");
        std::fs::write(project.join("root.adoc"), PROJECT_SOURCE).expect("primary");
        std::fs::write(project.join("part.adoc"), PART).expect("include");
    }
    let accepted = adocweave()
        .current_dir(root.path())
        .args(["format", "--check", "one/root.adoc", "two/root.adoc"])
        .output()
        .expect("independent project budgets");
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

/// `--port 0` lets the operating system choose, and preview says which it got.
///
/// Without this a caller has to pick a free port by binding one and closing it,
/// which leaves a gap where another process can take that port. The tests here
/// rely on the reported address, so it is checked directly rather than only
/// through the tests that consume it.
#[cfg(unix)]
#[test]
fn preview_accepts_port_zero_and_reports_the_port_it_bound() {
    let directory = tempfile::tempdir().expect("tempdir");
    let document = directory.path().join("document.adoc");
    std::fs::write(&document, "= Chosen port\n").expect("document");
    let mut child = adocweave()
        .args([
            "preview",
            "--port",
            "0",
            document.to_str().expect("utf-8 path"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("preview");

    let address = preview_address(child.stderr.as_mut().expect("preview stderr"));
    assert_ne!(address.port(), 0, "the reported port must be the bound one");
    assert!(address.ip().is_loopback());
    assert!(preview_get(address, "/document").contains("Chosen port"));

    // SAFETY: the process was spawned above and has not been reaped.
    unsafe {
        libc::kill(
            i32::try_from(child.id()).expect("preview process id"),
            libc::SIGTERM,
        );
    }
    child.wait().expect("reap preview");
}

#[cfg(unix)]
#[test]
fn preview_sigterm_exits_cleanly_and_releases_the_listener() {
    use std::net::TcpListener;

    let directory = tempfile::tempdir().expect("tempdir");
    let document = directory.path().join("document.adoc");
    std::fs::write(&document, "= SIGTERM readiness marker\n").expect("document");
    let mut child = adocweave()
        .args([
            "preview",
            "--port",
            "0",
            document.to_str().expect("utf-8 path"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("preview");
    let address = preview_address(child.stderr.as_mut().expect("preview stderr"));
    let Some(response) = try_preview_get(address, "/document") else {
        let status = child.try_wait().expect("preview status");
        if status.is_none() {
            child.kill().expect("stop unready preview");
            child.wait().expect("reap unready preview");
        }
        panic!("preview did not become ready before SIGTERM; process status: {status:?}");
    };
    if !response.starts_with("HTTP/1.1 200 OK\r\n")
        || !response.contains("SIGTERM readiness marker")
    {
        child.kill().expect("stop unexpected preview");
        child.wait().expect("reap unexpected preview");
        panic!("preview returned an unexpected readiness response: {response}");
    }
    stop_preview(&mut child);
    TcpListener::bind(address).expect("listener released");
}

#[cfg(unix)]
#[test]
fn preview_never_serves_an_include_through_an_outside_root_symlink() {
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    let include_dir = root.path().join("parts");
    std::fs::create_dir(&include_dir).expect("parts");
    std::fs::write(
        root.path().join("document.adoc"),
        "include::parts/part.adoc[]\n",
    )
    .expect("document");
    std::fs::write(include_dir.join("part.adoc"), "SAFE_ONE\n").expect("safe include");
    std::fs::write(outside.path().join("part.adoc"), "EXTERNAL_SECRET_BODY\n").expect("secret");
    let mut child = adocweave()
        .current_dir(root.path())
        .args([
            "preview",
            "--include",
            "--debounce-ms",
            "10",
            "--port",
            "0",
            "document.adoc",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("preview");
    let address = preview_address(child.stderr.as_mut().expect("preview stderr"));

    assert!(preview_get(address, "/document").contains("SAFE_ONE"));
    std::fs::rename(&include_dir, root.path().join("parts-safe")).expect("move safe directory");
    symlink(outside.path(), &include_dir).expect("outside symlink");
    for _ in 0..200 {
        let response = preview_get(address, "/document");
        assert!(!response.contains("EXTERNAL_SECRET_BODY"));
        if preview_get(address, "/events").contains("\"generation\":2") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_file(&include_dir).expect("remove symlink");
    std::fs::rename(root.path().join("parts-safe"), &include_dir).expect("restore safe directory");
    std::fs::write(include_dir.join("part.adoc"), "SAFE_TWO\n").expect("update safe include");
    for _ in 0..200 {
        let response = preview_get(address, "/document");
        assert!(!response.contains("EXTERNAL_SECRET_BODY"));
        if response.contains("SAFE_TWO") {
            stop_preview(&mut child);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stop_preview(&mut child);
    panic!("preview did not recover after restoring the safe directory");
}

#[cfg(unix)]
#[test]
fn preview_non_privileged_child_recovers_after_include_permission_returns() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::time::Duration;

    let running_as_root = unsafe { libc::geteuid() } == 0;
    let root = tempfile::tempdir().expect("root");
    let document = root.path().join("document.adoc");
    let dependency = root.path().join("part.adoc");
    std::fs::write(&document, "include::part.adoc[]\n").expect("document");
    std::fs::write(&dependency, "VISIBLE_ONE\n").expect("include");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))
        .expect("root mode");
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o644))
        .expect("document mode");
    std::fs::set_permissions(&dependency, std::fs::Permissions::from_mode(0o644))
        .expect("include mode");
    let mut command = adocweave();
    command
        .current_dir(root.path())
        .args([
            "preview",
            "--include",
            "--debounce-ms",
            "10",
            "--port",
            "0",
            "document.adoc",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if running_as_root {
        // SAFETY: this runs in the forked child before exec and only drops privileges.
        unsafe {
            command.pre_exec(|| {
                if libc::setgid(65534) != 0 || libc::setuid(65534) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().expect("non-privileged preview");
    let address = preview_address(child.stderr.as_mut().expect("preview stderr"));

    assert!(preview_get(address, "/document").contains("VISIBLE_ONE"));
    std::fs::set_permissions(&dependency, std::fs::Permissions::from_mode(0o000)).expect("deny");
    let mut denied_generation = None;
    for _ in 0..200 {
        let diagnostics = preview_get(address, "/diagnostics");
        if diagnostics.contains("permission") {
            let events = preview_get(address, "/events");
            denied_generation = events
                .split("\"generation\":")
                .nth(1)
                .and_then(|value| value.split('}').next())
                .and_then(|value| value.parse::<u64>().ok());
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let denied_generation = denied_generation.expect("permission diagnostic remains served");
    assert!(child.try_wait().expect("child state").is_none());

    std::fs::set_permissions(&dependency, std::fs::Permissions::from_mode(0o644)).expect("restore");
    std::fs::write(&dependency, "VISIBLE_TWO\n").expect("update restored include");
    for _ in 0..200 {
        let document = preview_get(address, "/document");
        let events = preview_get(address, "/events");
        let advanced = events
            .split("\"generation\":")
            .nth(1)
            .and_then(|value| value.split('}').next())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|generation| generation > denied_generation);
        if advanced && document.contains("VISIBLE_TWO") {
            stop_preview(&mut child);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stop_preview(&mut child);
    panic!("preview did not recover after read access returned");
}

#[cfg(unix)]
#[test]
fn preview_serves_and_recovers_from_an_initial_css_read_failure() {
    use std::time::Duration;

    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("document.adoc"), "= Preview\n").expect("document");
    let css = root.path().join("preview.css");
    let mut child = adocweave()
        .current_dir(root.path())
        .args([
            "preview",
            "--css",
            css.to_str().expect("css path"),
            "--debounce-ms",
            "10",
            "--port",
            "0",
            "document.adoc",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("preview");
    let address = preview_address(child.stderr.as_mut().expect("preview stderr"));

    assert!(preview_get(address, "/document").contains("Preview error"));
    assert!(child.try_wait().expect("child state").is_none());
    std::fs::write(&css, "body { color: green; }\n").expect("create css");
    for _ in 0..200 {
        let response = preview_get(address, "/document");
        if response.contains("color: green") {
            stop_preview(&mut child);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stop_preview(&mut child);
    panic!("preview did not recover after the stylesheet became readable");
}

#[cfg(unix)]
#[test]
fn preview_serves_and_recovers_from_an_initial_include_read_failure() {
    use std::time::Duration;

    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("document.adoc"), "include::part.adoc[]\n").expect("document");
    let include = root.path().join("part.adoc");
    let mut child = adocweave()
        .current_dir(root.path())
        .args([
            "preview",
            "--include",
            "--debounce-ms",
            "10",
            "--port",
            "0",
            "document.adoc",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("preview");
    let address = preview_address(child.stderr.as_mut().expect("preview stderr"));

    assert!(preview_get(address, "/diagnostics").contains("missing"));
    assert!(child.try_wait().expect("child state").is_none());
    std::fs::write(&include, "RECOVERED_INCLUDE\n").expect("create include");
    for _ in 0..200 {
        if preview_get(address, "/document").contains("RECOVERED_INCLUDE") {
            stop_preview(&mut child);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stop_preview(&mut child);
    panic!("preview did not recover after the include became readable");
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
fn public_help_paths_match_the_command_model_snapshots() {
    let expected_root = include_bytes!("snapshots/help-root.txt");
    for arguments in [&["--help"][..], &["help"][..]] {
        let output = adocweave().args(arguments).output().expect("root help");
        assert!(output.status.success(), "{arguments:?}");
        assert_eq!(output.stdout, expected_root, "{arguments:?}");
        assert!(output.stderr.is_empty(), "{arguments:?}");
    }

    let mut expected_commands = include_str!("snapshots/help-commands.txt");
    for path in [
        &["convert"][..],
        &["preview"][..],
        &["check"][..],
        &["format"][..],
        &["symbols"][..],
        &["config", "show"][..],
    ] {
        let marker = format!("=== {} ===\n", path.join(" "));
        let section = expected_commands
            .strip_prefix(&marker)
            .unwrap_or_else(|| panic!("missing snapshot for {}", path.join(" ")));
        let next = section.find("=== ").unwrap_or(section.len());
        let expected = &section[..next];
        let output = adocweave()
            .args(path.iter().copied().chain(["--help"]))
            .output()
            .expect("command help");
        assert!(output.status.success(), "{path:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{path:?}"
        );
        assert!(output.stderr.is_empty(), "{path:?}");
        expected_commands = &section[next..];
    }
}

#[test]
fn completion_scripts_cover_every_supported_shell() {
    for (shell, marker) in [
        ("bash", "complete -F"),
        ("zsh", "compdef"),
        ("fish", "complete -c adocweave"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let output = adocweave()
            .args(["completion", shell])
            .output()
            .expect("completion");
        assert!(output.status.success(), "{shell}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(marker));
        assert!(stdout.contains("config"), "{shell}");
        assert!(stdout.contains("show"), "{shell}");
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
    assert_eq!(value["packageVersion"], adocweave::VERSION);
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
    let records = serde_json::from_slice::<Vec<serde_json::Value>>(&json.stdout)
        .expect("check JSON is an array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], "trailing-whitespace@8:9");
    assert_eq!(records[0]["sourceId"], "<stdin>");
    assert_eq!(records[0]["related"], serde_json::json!([]));
    assert_eq!(records[0]["fixes"][0]["applicability"], "always");
}

/// Every path emits the same keys, so a consumer that reads one record reads all
/// of them. A document that gains an include used to change the key set.
#[test]
fn check_json_records_share_one_key_set() {
    let expected = [
        "code", "fixes", "id", "message", "range", "related", "severity", "sourceId",
    ];
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("part.adoc"), "== Included\n\ntrailing \n").expect("part");
    std::fs::write(
        root.path().join("root.adoc"),
        "= Root\n\ntrailing \n\ninclude::part.adoc[]\n\nxref:missing.adoc[missing]\n",
    )
    .expect("root document");

    for arguments in [
        vec!["check", "--format", "json", "root.adoc"],
        vec!["check", "--format", "json", "--include", "root.adoc"],
    ] {
        let output = adocweave()
            .current_dir(root.path())
            .args(&arguments)
            .output()
            .expect("command");
        let records = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)
            .unwrap_or_else(|error| panic!("{arguments:?} produced invalid JSON: {error}"));
        assert!(!records.is_empty(), "{arguments:?} produced no diagnostics");
        for record in &records {
            let mut keys = record
                .as_object()
                .expect("each record is an object")
                .keys()
                .filter(|key| !["target", "line", "column"].contains(&key.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            assert_eq!(keys, expected, "{arguments:?} record {record}");
        }
    }
}

#[test]
fn check_uses_one_failure_threshold_for_every_output_format() {
    let source = b"trailing \n";
    for format in ["human", "json", "github", "sarif"] {
        let failed = run_with_stdin(
            &["check", "--format", format, "--fail-on", "warning", "-"],
            source,
        );
        assert!(!failed.status.success(), "{format} should fail on warnings");

        let passed = run_with_stdin(
            &["check", "--format", format, "--fail-on", "never", "-"],
            source,
        );
        assert!(passed.status.success(), "{format} should honor never");
    }
}

#[test]
fn invalid_project_config_fails_regardless_of_threshold_and_output_format() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-invalid-config-ci-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    std::fs::write(root.join(".adocweave.toml"), "schema-version = 99\n")
        .expect("write invalid config");
    std::fs::write(root.join("document.adoc"), "content\n").expect("write document");

    for format in ["human", "json", "github", "sarif"] {
        let output = adocweave()
            .current_dir(&root)
            .args([
                "check",
                "--format",
                format,
                "--fail-on",
                "never",
                "document.adoc",
            ])
            .output()
            .expect("configuration failure");
        assert!(
            !output.status.success(),
            "{format} must not suppress an input failure"
        );
        assert!(output.stdout.is_empty(), "{format}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("schema-version"),
            "{format}"
        );
    }

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn check_emits_sarif_with_stable_rule_and_source_location() {
    let output = run_with_stdin(
        &["check", "--format", "sarif", "--summary", "-"],
        b"trailing \n",
    );

    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid SARIF JSON");
    assert_eq!(value["version"], "2.1.0");
    assert_eq!(value["runs"][0]["tool"]["driver"]["name"], "AdocWeave");
    assert_eq!(
        value["runs"][0]["results"][0]["ruleId"],
        "trailing-whitespace"
    );
    assert_eq!(
        value["runs"][0]["results"][0]["partialFingerprints"]["adocweaveDiagnosticId"],
        "trailing-whitespace@8:9"
    );
    assert_eq!(
        value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "<stdin>"
    );
    assert_eq!(
        value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startColumn"],
        9
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "adocweave check: errors=0, warnings=1, information=0, hints=0\n"
    );
}

#[test]
fn multi_file_sarif_is_one_log_with_one_run() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-sarif-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    std::fs::write(root.join("a.adoc"), "a \n").expect("first");
    std::fs::write(root.join("b.adoc"), "b \n").expect("second");

    let output = adocweave()
        .args(["check", "--format", "sarif", "--fail-on", "never"])
        .arg(&root)
        .output()
        .expect("SARIF check");
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one SARIF document");
    assert_eq!(value["runs"].as_array().expect("runs").len(), 1);
    assert_eq!(
        value["runs"][0]["results"]
            .as_array()
            .expect("results")
            .len(),
        2
    );

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn check_emits_github_annotations_and_an_opt_in_stderr_summary() {
    let output = run_with_stdin(
        &["check", "--format", "github", "--summary", "-"],
        b"trailing \n",
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with(
        "::warning file=<stdin>,line=1,col=9,title=trailing-whitespace::trailing whitespace\n"
    ));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "adocweave check: errors=0, warnings=1, information=0, hints=0\n"
    );
}

#[test]
fn check_rejects_unknown_ci_contract_values() {
    for arguments in [
        ["check", "--format", "yaml", "-"],
        ["check", "--fail-on", "information", "-"],
    ] {
        let output = run_with_stdin(&arguments, b"");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn project_config_is_discovered_and_can_be_disabled_explicitly() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-config-cli-{unique}"));
    let docs = root.join("docs");
    std::fs::create_dir_all(&docs).expect("create project");
    std::fs::write(
        root.join(".adocweave.toml"),
        include_str!("../../../fixtures/config/shared-v1/.adocweave.toml"),
    )
    .expect("write config");
    std::fs::write(docs.join("manual.adoc"), "One\n\nTwo\n").expect("write document");

    let configured = adocweave()
        .current_dir(&root)
        .args(["format", "docs/manual.adoc"])
        .output()
        .expect("run configured formatter");
    assert!(configured.status.success());
    assert_eq!(configured.stdout, b"One\r\n\r\nTwo");

    let defaults = adocweave()
        .current_dir(&root)
        .args(["format", "--no-config", "docs/manual.adoc"])
        .output()
        .expect("run default formatter");
    assert!(defaults.status.success());
    assert_eq!(defaults.stdout, b"One\n\nTwo\n");

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn project_config_bounds_cli_diagnostics_before_json_projection() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-config-limit-cli-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    std::fs::write(
        root.join(".adocweave.toml"),
        "schema-version = 1\n[lint]\nmax-line-length = 4\nmax-diagnostics = 1\n",
    )
    .expect("write config");
    std::fs::write(root.join("manual.adoc"), "long \n*x\n").expect("write document");

    let output = adocweave()
        .current_dir(&root)
        .args(["check", "--json", "manual.adoc"])
        .output()
        .expect("run configured check");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    let diagnostics = diagnostics.as_array().expect("diagnostic array");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "trailing-whitespace");

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn commands_validate_only_the_configuration_paths_they_consume() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-config-scope-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    std::fs::write(
        root.join(".adocweave.toml"),
        "schema-version = 1\n[html]\ncomplete = true\nstylesheet-files = [\"missing.css\"]\n",
    )
    .expect("write config");
    std::fs::write(root.join("manual.adoc"), "text\n").expect("write document");

    let checked = adocweave()
        .current_dir(&root)
        .args(["check", "manual.adoc"])
        .output()
        .expect("check");
    assert!(checked.status.success());

    let converted = adocweave()
        .current_dir(&root)
        .args(["convert", "manual.adoc"])
        .output()
        .expect("convert");
    assert!(!converted.status.success());
    assert!(String::from_utf8_lossy(&converted.stderr).contains("missing.css"));

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn configured_include_accepts_explicit_roots_without_a_redundant_flag() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-config-include-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    std::fs::write(
        root.join(".adocweave.toml"),
        "schema-version = 1\n[resources]\ninclude = true\n",
    )
    .expect("write config");
    std::fs::write(root.join("manual.adoc"), "include::part.adoc[]\n").expect("root");
    std::fs::write(root.join("part.adoc"), "included\n").expect("part");

    let output = adocweave()
        .current_dir(&root)
        .args(["convert", "--allow-root", ".", "manual.adoc"])
        .output()
        .expect("configured include");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("included"));

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn config_show_reports_source_and_redacts_attribute_values() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-config-show-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    let config = root.join("project.toml");
    std::fs::write(
        &config,
        "schema-version = 1\n[analysis.attributes.token]\nvalue = \"do-not-print\"\n",
    )
    .expect("write config");

    let output = adocweave()
        .current_dir(&root)
        .args(["config", "show", "--config", "project.toml"])
        .output()
        .expect("show config");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 JSON");
    assert!(!stdout.contains("do-not-print"));
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("config JSON");
    assert_eq!(
        value["source"],
        config.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(value["analysis"]["attributes"]["token"]["state"], "set");

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn format_write_recurses_deterministically_and_preserves_file_mode() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-format-write-{unique}"));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).expect("create project");
    let first = root.join("a.adoc");
    let second = nested.join("b.adoc");
    std::fs::write(&first, "a  \n").expect("first");
    std::fs::write(&second, "b  \r\nline").expect("second");
    std::fs::write(root.join("ignored.txt"), "ignored  \n").expect("ignored");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o640))
            .expect("permissions");
    }

    let output = adocweave()
        .args(["format", "--write", "--summary"])
        .arg(&root)
        .output()
        .expect("format directory");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "b\r\nline");
    assert_eq!(
        std::fs::read_to_string(root.join("ignored.txt")).unwrap(),
        "ignored  \n"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "adocweave format: files=2, changed=2\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn format_diff_and_dry_run_do_not_modify_inputs() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-format-diff-{unique}"));
    std::fs::create_dir(&root).expect("project");
    let document = root.join("manual.adoc");
    std::fs::write(&document, "text  \n").expect("document");

    let diff = adocweave()
        .args(["format", "--diff"])
        .arg(&document)
        .output()
        .expect("diff");
    assert!(diff.status.success());
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(stdout.contains("--- a/"));
    assert!(stdout.contains("-text  "));
    assert!(stdout.contains("+text"));
    assert_eq!(std::fs::read_to_string(&document).unwrap(), "text  \n");

    let dry_run = adocweave()
        .args(["format", "--write", "--dry-run"])
        .arg(&document)
        .output()
        .expect("dry run");
    assert!(dry_run.status.success());
    assert_eq!(std::fs::read_to_string(&document).unwrap(), "text  \n");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn check_fix_applies_only_always_safe_non_conflicting_edits() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-check-fix-{unique}"));
    std::fs::create_dir(&root).expect("project");
    let first = root.join("a.adoc");
    let second = root.join("b.adoc");
    std::fs::write(&first, "first  \n").expect("first");
    std::fs::write(&second, "second  \n").expect("second");

    let dry_run = adocweave()
        .args(["check", "--fix", "--dry-run", "--summary"])
        .args([&first, &second])
        .output()
        .expect("dry-run fix");
    assert!(dry_run.status.success());
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "first  \n");
    assert_eq!(
        String::from_utf8_lossy(&dry_run.stderr),
        "adocweave check: errors=0, warnings=0, information=0, hints=0, changed=2\n"
    );

    let output = adocweave()
        .args(["check", "--fix", "--summary"])
        .args([&first, &second])
        .output()
        .expect("fix");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "first\n");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "second\n");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "adocweave check: errors=0, warnings=0, information=0, hints=0, changed=2\n"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn check_glob_deduplicates_files_and_emits_source_ids() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-check-glob-{unique}"));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).expect("project");
    let first = root.join("a.adoc");
    let second = nested.join("b.adoc");
    std::fs::write(&first, "first  \n").expect("first");
    std::fs::write(&second, "second  \n").expect("second");

    let output = adocweave()
        .current_dir(&root)
        .args(["check", "--format", "json", "--glob", "**/*.adoc", "a.adoc"])
        .output()
        .expect("glob");
    assert!(output.status.success());
    let diagnostics: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("diagnostic JSON");
    assert_eq!(diagnostics.len(), 2);
    let source_ids = diagnostics
        .iter()
        .map(|diagnostic| diagnostic["sourceId"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(source_ids.len(), 2);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn color_never_is_plain_and_color_always_is_explicit() {
    let plain = run_with_stdin(&["check", "--color", "never", "-"], b"text  \n");
    let colored = run_with_stdin(&["check", "--color", "always", "-"], b"text  \n");
    assert!(!plain.stdout.contains(&0x1b));
    assert!(colored.stdout.contains(&0x1b));
}

#[cfg(unix)]
#[test]
fn explicit_symlink_inputs_are_rejected_without_modifying_the_target() {
    use std::os::unix::fs::symlink;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-format-symlink-{unique}"));
    std::fs::create_dir(&root).expect("project");
    let target = root.join("target.adoc");
    let link = root.join("link.adoc");
    std::fs::write(&target, "target  \n").expect("target");
    symlink(&target, &link).expect("symlink");

    let output = adocweave()
        .args(["format", "--write"])
        .arg(&link)
        .output()
        .expect("format");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("symbolic links"));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "target  \n");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn check_reports_link_and_xref_usage_consistently() {
    let source = b"link:guide.adoc[Guide]\nxref:data.json[Data]\n";
    let human = run_with_stdin(&["check", "-"], source);
    let json = run_with_stdin(&["check", "--json", "-"], source);

    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("1:1: warning[asciidoc-file-link]"));
    assert!(human.contains("2:1: warning[non-asciidoc-xref]"));

    assert!(json.status.success());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("diagnostic JSON");
    let diagnostics = diagnostics.as_array().expect("diagnostics");
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0]["code"], "asciidoc-file-link");
    assert_eq!(diagnostics[0]["severity"], "warning");
    assert_eq!(diagnostics[0]["range"]["start"], 0);
    assert_eq!(diagnostics[0]["range"]["end"], 4);
    assert_eq!(diagnostics[1]["code"], "non-asciidoc-xref");
    assert_eq!(diagnostics[1]["severity"], "warning");
    assert_eq!(diagnostics[1]["range"]["start"], 23);
    assert_eq!(diagnostics[1]["range"]["end"], 27);
}

#[test]
fn check_enables_macro_boundary_as_an_opt_in_rule() {
    let source = "本文xref:guide.adoc[Guide]\n".as_bytes();
    let default = run_with_stdin(&["check", "--json", "-"], source);
    let human = run_with_stdin(
        &[
            "check",
            "--enable-rule",
            "macro-boundary",
            "--enable-rule",
            "macro-boundary",
            "-",
        ],
        source,
    );
    let json = run_with_stdin(
        &["check", "--json", "--enable-rule", "macro-boundary", "-"],
        source,
    );

    assert!(default.status.success());
    assert!(!String::from_utf8_lossy(&default.stdout).contains("macro-boundary"));
    assert!(human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stdout)
            .contains("1:7: warning[macro-boundary]: xref inline macro")
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("diagnostic JSON");
    assert_eq!(diagnostics.as_array().expect("diagnostics").len(), 1);
    assert_eq!(diagnostics[0]["code"], "macro-boundary");
    assert_eq!(diagnostics[0]["severity"], "warning");
    assert_eq!(diagnostics[0]["range"]["start"], 6);
    assert_eq!(diagnostics[0]["range"]["end"], 10);
}

#[test]
fn check_rejects_unknown_default_and_catalog_rule_enabling() {
    for arguments in [
        vec!["check", "--enable-rule", "unknown-rule", "-"],
        vec!["check", "--enable-rule", "trailing-whitespace", "-"],
        vec![
            "check",
            "--list-rules",
            "--json",
            "--enable-rule",
            "macro-boundary",
        ],
    ] {
        let output = run_with_stdin(&arguments, b"");
        assert!(!output.status.success(), "{arguments:?}");
        assert!(output.stdout.is_empty());
    }

    let output = adocweave()
        .args(["check", "--list-rules", "--json"])
        .output()
        .expect("catalog");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("catalog JSON");
    let rule = value["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .find(|rule| rule["code"] == "macro-boundary")
        .expect("macro-boundary rule");
    assert_eq!(rule["enabledByDefault"], false);
    assert_eq!(rule["userConfigurable"], true);
    assert_eq!(rule["fixable"], true);
}

#[test]
fn check_lists_the_typed_rule_catalog_without_reading_input() {
    let output = adocweave()
        .args(["check", "--list-rules", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("the adocweave binary should run");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("catalog JSON");
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["packageVersion"], adocweave::VERSION);
    let rules = value["rules"].as_array().expect("rules");
    assert!(!rules.is_empty());
    assert!(
        rules
            .windows(2)
            .all(|pair| pair[0]["code"].as_str() < pair[1]["code"].as_str())
    );
    assert!(rules.iter().all(|rule| {
        rule.get("defaultSeverity").is_some()
            && rule.get("enabledByDefault").is_some()
            && rule.get("description").is_some()
            && rule.get("fixable").is_some()
    }));
}

#[test]
fn list_rules_rejects_missing_json_and_document_options() {
    for arguments in [
        vec!["check", "--list-rules"],
        vec!["check", "--list-rules", "--json", "document.adoc"],
        vec!["check", "--list-rules", "--json", "--include"],
    ] {
        let output = adocweave()
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .expect("the adocweave binary should run");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

/// `--list-rules` works with the canonical option, not only its compatibility alias.
#[test]
fn list_rules_accepts_the_canonical_format_option() {
    let canonical = adocweave()
        .args(["check", "--list-rules", "--format", "json"])
        .stdin(Stdio::null())
        .output()
        .expect("the adocweave binary should run");
    let alias = adocweave()
        .args(["check", "--list-rules", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("the adocweave binary should run");

    assert!(canonical.status.success());
    assert_eq!(canonical.stdout, alias.stdout);

    // The message names the option a caller should reach for.
    let rejected = adocweave()
        .args(["check", "--list-rules"])
        .stdin(Stdio::null())
        .output()
        .expect("the adocweave binary should run");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("--format json"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn check_accepts_relative_targets_without_activating_them_in_html() {
    let source = b"link:../release-manifest.json[release manifest]\n\
                   xref:../guide.adoc[guide]\n";
    let checked = run_with_stdin(&["check", "--json", "-"], source);
    let converted = run_with_stdin(&["convert", "-"], source);

    assert!(checked.status.success());
    assert_eq!(checked.stdout, b"[]");
    assert!(checked.stderr.is_empty());
    assert!(converted.status.success());
    assert_eq!(converted.stdout, b"<p>release manifest guide</p>\n");
    assert!(converted.stderr.is_empty());
}

#[test]
fn multi_file_format_preflights_configured_includes_before_writing() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-format-include-{unique}"));
    std::fs::create_dir(&root).expect("create project");
    std::fs::write(
        root.join(".adocweave.toml"),
        "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\n",
    )
    .expect("write config");
    let first = root.join("a.adoc");
    let second = root.join("b.adoc");
    std::fs::write(&first, "a  \n").expect("first");
    std::fs::write(&second, "include::missing.adoc[]\nb  \n").expect("second");

    let output = adocweave()
        .current_dir(&root)
        .args(["format", "--write", "."])
        .output()
        .expect("format directory");
    assert!(!output.status.success());
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "a  \n");
    assert_eq!(
        std::fs::read_to_string(&second).unwrap(),
        "include::missing.adoc[]\nb  \n"
    );

    std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn local_target_check_is_explicit_and_fails_for_missing_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = b"xref:missing-v011-target.adoc[missing]\n\
include::missing-v011-include.adoc[]\n\
include::missing-v011-optional.adoc[optional]\n\
ifdef::never[]\n\
include::missing-v011-inactive.adoc[]\n\
endif::[]\n";

    let default = run_with_stdin(&["check", "--json", "-"], source);
    let checked = run_with_stdin(
        &[
            "check",
            "--json",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "-",
        ],
        source,
    );

    assert!(default.status.success());
    assert!(!String::from_utf8_lossy(&default.stdout).contains("local-target-"));
    assert!(!checked.status.success());
    assert!(checked.stderr.is_empty());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&checked.stdout).expect("local target JSON");
    let local = diagnostics
        .as_array()
        .expect("array")
        .iter()
        .filter(|value| value["code"] == "local-target-missing")
        .collect::<Vec<_>>();
    assert_eq!(local.len(), 2);
    assert_eq!(local[0]["target"], "missing-v011-target.adoc");
    assert_eq!(local[0]["line"], 1);
    assert_eq!(local[0]["column"], 6);
    assert_eq!(local[1]["target"], "missing-v011-include.adoc");
}

#[cfg(unix)]
fn run_permission_fixture(root: &std::path::Path, json: bool) -> Output {
    fn command(root: &std::path::Path, json: bool) -> Command {
        let mut command = adocweave();
        command
            .current_dir(root)
            .args(["check", "--local-targets", "--project-root", "."]);
        if json {
            command.arg("--json");
        }
        command.arg("root.adoc");
        command
    }

    let output = command(root, json).output().expect("permission fixture");
    if !output.status.success() {
        return output;
    }

    use std::os::unix::process::CommandExt;
    command(root, json)
        .uid(65_534)
        .gid(65_534)
        .output()
        .expect("permission fixture as an unprivileged user")
}

#[cfg(unix)]
#[test]
fn local_target_permission_failure_has_stable_cli_contract() {
    use std::os::unix::fs::PermissionsExt;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-local-permission-{unique}"));
    let blocked = root.join("blocked");
    std::fs::create_dir_all(&blocked).expect("fixture directory");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
        .expect("project root permissions");
    std::fs::write(
        root.join("root.adoc"),
        include_str!("../../../fixtures/local-target/permission-limit/permission/root.adoc"),
    )
    .expect("root fixture");
    std::fs::write(
        blocked.join("target.adoc"),
        include_str!(
            "../../../fixtures/local-target/permission-limit/permission/blocked/target.adoc"
        ),
    )
    .expect("target fixture");
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000))
        .expect("blocked directory permissions");

    let human = run_permission_fixture(&root, false);
    assert!(!human.status.success());
    assert!(human.stderr.is_empty());
    assert_eq!(
        human.stdout,
        b"root.adoc:1:6: error[local-target-permission-denied]: local target cannot be read (target: blocked/target.adoc)\n"
    );

    let json = run_permission_fixture(&root, true);
    assert!(!json.status.success());
    assert!(json.stderr.is_empty());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("permission JSON");
    assert_eq!(diagnostics.as_array().expect("array").len(), 1);
    assert_eq!(
        diagnostics[0]["id"],
        "local-target-permission-denied@root.adoc:5:24"
    );
    assert_eq!(diagnostics[0]["code"], "local-target-permission-denied");
    assert_eq!(diagnostics[0]["sourceId"], "root.adoc");
    assert_eq!(
        diagnostics[0]["range"],
        serde_json::json!({ "start": 5, "end": 24 })
    );
    assert_eq!(diagnostics[0]["target"], "blocked/target.adoc");

    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755))
        .expect("restore directory permissions");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn local_target_inspection_limit_has_stable_cli_contract() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/local-target/permission-limit/limit");
    let run = |json| {
        let mut command = adocweave();
        command.current_dir(&root).args([
            "check",
            "--config",
            ".adocweave.toml",
            "--local-targets",
            "--project-root",
            ".",
        ]);
        if json {
            command.arg("--json");
        }
        command
            .arg("root.adoc")
            .output()
            .expect("inspection limit fixture")
    };

    let human = run(false);
    assert!(!human.status.success());
    assert!(human.stderr.is_empty());
    assert_eq!(
        human.stdout,
        b"root.adoc:2:6: error[local-target-limit-exceeded]: local target inspection limit exceeded (target: second.adoc)\n"
    );

    let json = run(true);
    assert!(!json.status.success());
    assert!(json.stderr.is_empty());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("inspection limit JSON");
    assert_eq!(diagnostics.as_array().expect("array").len(), 1);
    assert_eq!(
        diagnostics[0]["id"],
        "local-target-limit-exceeded@root.adoc:28:39"
    );
    assert_eq!(diagnostics[0]["code"], "local-target-limit-exceeded");
    assert_eq!(diagnostics[0]["sourceId"], "root.adoc");
    assert_eq!(
        diagnostics[0]["range"],
        serde_json::json!({ "start": 28, "end": 39 })
    );
    assert_eq!(diagnostics[0]["target"], "second.adoc");
}

#[test]
fn local_target_check_accepts_every_supported_fixture_kind() {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/local-target/all-kinds"
    );
    let document = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/local-target/all-kinds/root.adoc"
    );
    let output = adocweave()
        .args([
            "check",
            "--local-targets",
            "--project-root",
            root,
            "--json",
            document,
        ])
        .output()
        .expect("all local target kinds");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(output.stdout, b"[]");
}

#[test]
fn local_target_check_classifies_paths_and_ignores_external_targets() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-local-paths-{unique}"));
    std::fs::create_dir_all(root.join("directory")).expect("directory");
    let document = root.join("root.adoc");
    std::fs::write(
        &document,
        "xref:directory[dir]\nxref:../outside.adoc[out]\nlink:bad%0Aname[bad]\nlink:http//example.com[incomplete]\nlink:https://example.com[web]\n",
    )
    .expect("source");

    let output = adocweave()
        .args([
            "check",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "--json",
            document.to_str().expect("UTF-8 document"),
        ])
        .output()
        .expect("local target check");
    assert!(!output.status.success());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    let codes = diagnostics
        .as_array()
        .expect("array")
        .iter()
        .map(|value| value["code"].as_str().expect("code"))
        .filter(|code| code.starts_with("local-target-"))
        .collect::<Vec<_>>();
    assert_eq!(
        codes,
        vec![
            "local-target-not-file",
            "local-target-outside-root",
            "local-target-unverifiable",
            "local-target-unverifiable"
        ]
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn local_target_human_output_escapes_control_characters() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = run_with_stdin(
        &[
            "check",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "-",
        ],
        b"link:bad\x1bname[target]\n",
    );

    assert!(!output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    assert!(String::from_utf8_lossy(&output.stdout).contains(r"bad\u{1b}name"));
}

#[test]
fn local_target_columns_use_utf8_bytes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = run_with_stdin(
        &[
            "check",
            "--json",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "-",
        ],
        "あ xref:missing-unicode.adoc[target]\n".as_bytes(),
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    let local = diagnostics
        .as_array()
        .expect("array")
        .iter()
        .find(|value| value["code"] == "local-target-missing")
        .expect("local diagnostic");
    assert_eq!(local["column"], 10);
}

#[test]
fn local_target_file_base_uses_the_invocation_directory_for_bare_paths() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-local-base-{unique}"));
    let docs = root.join("docs");
    let include_base = root.join("include-base");
    std::fs::create_dir_all(&docs).expect("docs");
    std::fs::create_dir_all(&include_base).expect("include base");
    std::fs::write(docs.join("root.adoc"), "xref:target.adoc[target]\n").expect("source");
    std::fs::write(docs.join("target.adoc"), "= Target\n").expect("target");

    let output = adocweave()
        .current_dir(&docs)
        .args([
            "check",
            "--include",
            "--base-dir",
            "../include-base",
            "--local-targets",
            "--project-root",
            "..",
            "root.adoc",
        ])
        .output()
        .expect("local target check");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn local_target_check_rejects_symlink_escape_and_keeps_duplicate_positions() {
    use std::os::unix::fs::symlink;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-local-root-{unique}"));
    let outside = std::env::temp_dir().join(format!("adocweave-local-outside-{unique}"));
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("target.adoc"), "outside\n").expect("outside target");
    symlink(&outside, root.join("escape")).expect("symlink");
    let document = root.join("root.adoc");
    std::fs::write(
        &document,
        "xref:escape/target.adoc[first]\nxref:escape/target.adoc[second]\n",
    )
    .expect("source");

    let output = adocweave()
        .args([
            "check",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "--json",
            document.to_str().expect("UTF-8 document"),
        ])
        .output()
        .expect("local target check");
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    assert!(!output.status.success());
    assert_eq!(diagnostics.as_array().expect("array").len(), 2);
    assert_eq!(diagnostics[0]["code"], "local-target-outside-root");
    assert_eq!(diagnostics[0]["line"], 1);
    assert_eq!(diagnostics[1]["line"], 2);

    std::fs::remove_dir_all(root).expect("root cleanup");
    std::fs::remove_dir_all(outside).expect("outside cleanup");
}

#[test]
fn check_reports_invalid_explicit_ordered_numbers_without_losing_the_list() {
    let source = b"4294967296. overflow\n0. zero\n";
    let output = run_with_stdin(&["check", "--json", "-"], source);
    let diagnostics: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");

    assert!(output.status.success());
    assert_eq!(diagnostics.as_array().expect("array").len(), 2);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("explicit ordered-list number must be a positive 32-bit integer")
    );
}

#[test]
fn format_check_is_non_mutating_and_reports_needed_changes() {
    let formatted = run_with_stdin(&["format", "--check", "-"], b"clean\n");
    let formatted_crlf = run_with_stdin(&["format", "--check", "-"], b"clean\r\n");
    let unformatted = run_with_stdin(&["format", "--check", "-"], b"dirty  \n");

    assert!(formatted.status.success());
    assert!(formatted.stdout.is_empty());
    assert!(formatted_crlf.status.success());
    assert!(formatted_crlf.stdout.is_empty());
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
fn bibliography_consumer_fixture_is_shared_by_cli() {
    let source = adocweave::output::conformance::fixture_source("bibliography-consumer-coverage")
        .expect("shared inline conformance fixture");

    let checked = run_with_stdin(&["check", "--json", "-"], source.as_bytes());
    let symbols = run_with_stdin(&["symbols", "-"], source.as_bytes());

    assert!(checked.status.success());
    assert!(checked.stderr.is_empty());
    assert!(symbols.status.success());
    assert!(String::from_utf8_lossy(&symbols.stdout).contains("References"));
}

#[test]
fn table_presentation_fixture_is_shared_by_cli() {
    let source = include_bytes!("../../../fixtures/conformance/table-presentation.adoc");
    let expected_html = include_bytes!("../../../fixtures/conformance/table-presentation.html");

    let converted = run_with_stdin(&["convert", "-"], source);
    let checked = run_with_stdin(&["check", "--json", "-"], source);

    assert!(converted.status.success());
    assert_eq!(converted.stdout, expected_html);
    assert!(converted.stderr.is_empty());
    assert!(checked.status.success());
    assert_eq!(
        String::from_utf8_lossy(&checked.stdout)
            .matches("\"code\":\"invalid-table\"")
            .count(),
        2
    );
}

#[test]
fn source_block_shorthand_default_and_listing_are_consistent_on_crlf_input() {
    let source = b"= Source\xf0\x9f\x98\x80\r\n:source-language: rust\r\n\r\n[,python]\r\n----\r\nprint(\"\xf0\x9f\x98\x80\")\r\n----\r\n\r\n----\r\nfn main() {}\r\n----\r\n\r\n[listing]\r\n----\r\nplain\r\n----\r\n";
    let expected = b"<h1 class=\"document-title\" id=\"_source\">Source\xf0\x9f\x98\x80</h1>\n<pre><code class=\"language-python\">print(&#34;\xf0\x9f\x98\x80&#34;)\r\n</code></pre>\n<pre><code class=\"language-rust\">fn main() {}\r\n</code></pre>\n<pre>plain\r\n</pre>\n";

    let output = run_with_stdin(&["convert", "-"], source);

    assert!(output.status.success());
    assert_eq!(output.stdout, expected);
    assert!(output.stderr.is_empty());
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
fn include_loader_follows_runtime_attributes_and_skips_inactive_candidates() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-runtime-include-{unique}"));
    std::fs::create_dir_all(&root).expect("directory");
    std::fs::write(
        root.join("root.adoc"),
        "\
:part: first
:literal: retained \\
include::not-a-request.adoc[]
include::{part}.adoc[]
include::{next}.adoc[]
ifdef::missing[]
include::inactive.adoc[]
endif::[]
",
    )
    .expect("root source");
    std::fs::write(root.join("first.adoc"), ":next: second\nfirst\n").expect("first include");
    std::fs::write(root.join("second.adoc"), "second\n").expect("second include");

    let output = adocweave()
        .args([
            "convert",
            "--include",
            root.join("root.adoc").to_str().expect("UTF-8 path"),
        ])
        .output()
        .expect("conversion");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = String::from_utf8(output.stdout).expect("UTF-8 HTML");
    assert!(html.contains("first"));
    assert!(html.contains("second"));
    assert!(!html.contains("inactive"));

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn local_include_with_literal_url_suffix_uses_the_shared_fixture() {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/includes/literal-suffix-root.adoc"
    );
    let project_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = adocweave()
        .args([
            "check",
            "--include",
            "--local-targets",
            "--project-root",
            project_root,
            "--json",
            root,
        ])
        .output()
        .expect("literal-suffix include check");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(output.stdout, b"[]");
    assert!(output.stderr.is_empty());
}

#[test]
fn local_target_check_uses_the_explicit_include_base_fixture() {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/includes/separate-base/root.adoc"
    );
    let base = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/includes/separate-base/resources"
    );
    let project_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = adocweave()
        .args([
            "check",
            "--include",
            "--base-dir",
            base,
            "--local-targets",
            "--project-root",
            project_root,
            "--json",
            root,
        ])
        .output()
        .expect("separate include base check");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(output.stdout, b"[]");
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
    assert!(String::from_utf8_lossy(&traversal.stderr).contains("unsafe include target"));

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
fn stdin_and_include_share_the_project_retained_and_total_byte_boundary() {
    let root = tempfile::tempdir().expect("root");
    let source = b"include::part.adoc[]\n";
    let include = b"part";
    std::fs::write(root.path().join("part.adoc"), include).expect("include");
    let total = source.len() + include.len();
    let config = root.path().join(".adocweave.toml");
    let run = |max_files: usize, limit: usize| {
        std::fs::write(
            &config,
            format!(
                "schema-version = 1\n[resources]\ninclude = true\nroots = [\".\"]\nmax-files = {max_files}\nmax-total-bytes = {limit}\nmax-resource-bytes = {limit}\n"
            ),
        )
        .expect("configuration");
        let mut child = adocweave()
            .current_dir(root.path())
            .args(["check", "--base-dir", ".", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("command");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(source)
            .expect("input");
        child.wait_with_output().expect("output")
    };

    let accepted = run(2, total);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let rejected = run(2, total - 1);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("byte limit"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let rejected = run(1, total);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("file limit"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
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
    std::fs::write(&part, "bad \n本文xref:guide.adoc[Guide]\n").expect("part source");

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
    let opt_in = adocweave()
        .args([
            "check",
            "--include",
            "--enable-rule",
            "macro-boundary",
            "--json",
            document.to_str().expect("UTF-8 path"),
        ])
        .output()
        .expect("opt-in JSON check");
    assert!(human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stdout)
            .contains("include:part.adoc:1:4: warning[trailing-whitespace]")
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON diagnostics");
    assert_eq!(value[0]["sourceId"], "include:part.adoc");
    assert!(
        !value
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "macro-boundary")
    );
    let opt_in: serde_json::Value =
        serde_json::from_slice(&opt_in.stdout).expect("opt-in JSON diagnostics");
    let boundary = opt_in
        .as_array()
        .expect("diagnostics")
        .iter()
        .find(|diagnostic| diagnostic["code"] == "macro-boundary")
        .expect("macro-boundary diagnostic");
    assert_eq!(boundary["sourceId"], "include:part.adoc");
    assert_eq!(boundary["range"]["start"], 11);
    assert_eq!(boundary["range"]["end"], 15);

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn local_target_check_shares_include_resolution_and_honors_optional() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-local-include-{unique}"));
    let docs = root.join("docs");
    std::fs::create_dir_all(&docs).expect("directory");
    let document = docs.join("root.adoc");
    std::fs::write(
        &document,
        "include::../part.adoc[]\ninclude::missing-part.adoc[]\ninclude::optional.adoc[optional]\n",
    )
    .expect("root source");
    std::fs::write(
        root.join("part.adoc"),
        "xref:missing.adoc#section[missing]\n",
    )
    .expect("part source");

    let output = adocweave()
        .args([
            "check",
            "--include",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "--json",
            document.to_str().expect("UTF-8 document"),
        ])
        .output()
        .expect("local include check");

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    assert_eq!(
        diagnostics
            .as_array()
            .expect("array")
            .iter()
            .filter(|value| value["code"] == "local-target-missing")
            .count(),
        2
    );
    assert!(diagnostics.as_array().expect("array").iter().any(|value| {
        value["sourceId"] == "include:part.adoc"
            && value["target"] == "missing.adoc"
            && value["range"]["end"].as_u64().expect("end")
                - value["range"]["start"].as_u64().expect("start")
                == "missing.adoc".len() as u64
    }));

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn local_target_diagnostic_ids_include_the_fixture_source() {
    let document = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/local-target/diagnostic-id/root.adoc"
    );
    let project_root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/local-target/diagnostic-id"
    );
    let output = adocweave()
        .args([
            "check",
            "--include",
            "--local-targets",
            "--project-root",
            project_root,
            "--json",
            document,
        ])
        .output()
        .expect("diagnostic identity check");

    assert!(!output.status.success());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    let missing = diagnostics
        .as_array()
        .expect("array")
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "local-target-missing")
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 2);
    assert_ne!(missing[0]["id"], missing[1]["id"]);
    assert_ne!(missing[0]["sourceId"], missing[1]["sourceId"]);
}

#[test]
fn local_target_check_reports_include_read_failures() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-local-read-{unique}"));
    std::fs::create_dir_all(&root).expect("directory");
    let document = root.join("root.adoc");
    std::fs::write(
        &document,
        "include::invalid.adoc[]\ninclude::http//bad.adoc[]\n",
    )
    .expect("root source");
    std::fs::write(root.join("invalid.adoc"), [0xff]).expect("invalid UTF-8");
    std::fs::create_dir_all(root.join("http")).expect("incomplete target directory");
    std::fs::write(
        root.join("http/bad.adoc"),
        "xref:nested-missing.adoc[nested]\n",
    )
    .expect("incomplete target file");

    let output = adocweave()
        .args([
            "check",
            "--include",
            "--local-targets",
            "--project-root",
            root.to_str().expect("UTF-8 root"),
            "--json",
            document.to_str().expect("UTF-8 document"),
        ])
        .output()
        .expect("local include check");
    assert!(!output.status.success());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    assert!(diagnostics.as_array().expect("array").iter().any(|value| {
        value["code"] == "local-target-unverifiable" && value["target"] == "invalid.adoc"
    }));
    assert!(
        diagnostics
            .as_array()
            .expect("array")
            .iter()
            .any(|value| value["target"] == "http//bad.adoc")
    );
    assert!(
        !diagnostics
            .as_array()
            .expect("array")
            .iter()
            .any(|value| value["target"] == "nested-missing.adoc")
    );

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

#[cfg(unix)]
#[test]
fn include_diagnostics_use_logical_ids_for_canonical_file_aliases() {
    use std::os::unix::fs::symlink;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("adocweave-cli-alias-{unique}"));
    std::fs::create_dir_all(&root).expect("directory");
    let document = root.join("root.adoc");
    std::fs::write(&document, "include::part.adoc[]\ninclude::alias.adoc[]\n")
        .expect("root source");
    std::fs::write(root.join("part.adoc"), "bad \n").expect("part source");
    symlink("part.adoc", root.join("alias.adoc")).expect("inside alias");

    let output = adocweave()
        .args([
            "check",
            "--include",
            "--json",
            document.to_str().expect("UTF-8 document"),
        ])
        .output()
        .expect("alias check");
    assert!(output.status.success());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics");
    let source_ids = diagnostics
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "trailing-whitespace")
        .map(|diagnostic| diagnostic["sourceId"].as_str().expect("source ID"))
        .collect::<Vec<_>>();
    assert_eq!(source_ids, ["include:part.adoc", "include:alias.adoc"]);
    assert!(
        source_ids
            .iter()
            .all(|source_id| !source_id.contains(root.to_str().expect("UTF-8 root")))
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn complete_conversion_embeds_validated_stylesheets_in_order() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let css = std::env::temp_dir().join(format!("adocweave-cli-css-{unique}.css"));
    std::fs::write(&css, "p { color: red; }\n").expect("css source");

    let output = run_with_stdin(
        &[
            "convert",
            "--complete",
            "--css",
            css.to_str().expect("UTF-8 path"),
            "--css-url",
            "https://example.com/theme.css",
            "-",
        ],
        b"paragraph\n",
    );

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            "<!doctype html>\n",
            "<html lang=\"\">\n",
            "<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<title>AdocWeave document</title>\n",
            "<style>\n",
            "p { color: red; }\n",
            "</style>\n",
            "<link rel=\"stylesheet\" href=\"https://example.com/theme.css\">\n",
            "</head>\n",
            "<body>\n",
            "<p>paragraph</p>\n",
            "</body>\n",
            "</html>\n"
        )
    );

    std::fs::remove_file(css).expect("cleanup css");
}

#[test]
fn stylesheet_options_fail_closed() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let evil = std::env::temp_dir().join(format!("adocweave-cli-evil-{unique}.css"));
    std::fs::write(&evil, "p {}</style><script>alert(1)</script>").expect("css source");

    let breakout = run_with_stdin(
        &[
            "convert",
            "--complete",
            "--css",
            evil.to_str().expect("UTF-8 path"),
            "-",
        ],
        b"paragraph\n",
    );
    assert!(!breakout.status.success());
    assert!(breakout.stdout.is_empty());
    assert!(String::from_utf8_lossy(&breakout.stderr).contains("forbidden sequence"));

    let unsafe_url = run_with_stdin(
        &[
            "convert",
            "--complete",
            "--css-url",
            "javascript:alert(1)",
            "-",
        ],
        b"paragraph\n",
    );
    assert!(!unsafe_url.status.success());
    assert!(unsafe_url.stdout.is_empty());
    assert!(String::from_utf8_lossy(&unsafe_url.stderr).contains("not allowed by the URL policy"));

    let fragment_css = run_with_stdin(
        &["convert", "--css-url", "https://example.com/theme.css", "-"],
        b"paragraph\n",
    );
    assert!(!fragment_css.status.success());
    assert!(String::from_utf8_lossy(&fragment_css.stderr).contains("require --complete"));

    std::fs::remove_file(evil).expect("cleanup css");
}
