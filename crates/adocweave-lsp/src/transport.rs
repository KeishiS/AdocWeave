//! AdocWeave Language Server standard-I/O transport.

use std::io::{self, BufRead, Write};

use serde_json::Value;

use crate::Server;

pub fn run_stdio() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run(stdin.lock(), stdout.lock())
}

pub fn run<R: BufRead, W: Write>(mut input: R, mut output: W) -> Result<(), String> {
    let mut server = Server::default();
    while let Some(message) = read_message(&mut input)? {
        let exit = message.get("method").and_then(Value::as_str) == Some("exit");
        if let Some(response) = server.handle(&message)? {
            write_message(&mut output, &response)?;
        }
        for notification in server.drain_outgoing() {
            write_message(&mut output, &notification)?;
        }
        if exit {
            return if server.should_exit() {
                Ok(())
            } else {
                Err("exit received before shutdown".to_owned())
            };
        }
    }
    Ok(())
}

fn read_message<R: BufRead>(input: &mut R) -> Result<Option<Value>, String> {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        if input
            .read_line(&mut header)
            .map_err(|error| error.to_string())?
            == 0
        {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    let length = content_length.ok_or_else(|| "Content-Length is missing".to_owned())?;
    let mut body = vec![0; length];
    input
        .read_exact(&mut body)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn write_message<W: Write>(output: &mut W, message: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|error| error.to_string())?;
    output.write_all(&body).map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}
