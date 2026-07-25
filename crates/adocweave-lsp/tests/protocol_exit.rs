use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn frame(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).expect("serialize protocol message");
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend(body);
    framed
}

fn run_server(messages: &[Value]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_adocweave-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start language server");
    {
        let mut stdin = child.stdin.take().expect("server stdin");
        for message in messages {
            stdin.write_all(&frame(message)).expect("write message");
        }
    }
    child.wait_with_output().expect("wait for language server")
}

#[test]
fn exit_without_shutdown_uses_a_nonzero_process_status() {
    let output = run_server(&[json!({
        "jsonrpc":"2.0",
        "method":"exit",
        "params":null
    })]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exit received before shutdown"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shutdown_then_exit_uses_a_zero_process_status() {
    let output = run_server(&[
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{"processId":null,"rootUri":null,"capabilities":{}}
        }),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}),
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
