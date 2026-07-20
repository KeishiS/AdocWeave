use std::io::{self, Read as _};

use serde_json::{Value, json};

fn main() {
    let mut request = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut request) {
        println!("{}", json!({ "ok": false, "error": error.to_string() }));
        return;
    }
    let result = match adocweave_wasm::process_json(&request) {
        Ok(response) => json!({
            "ok": true,
            "value": serde_json::from_str::<Value>(&response)
                .expect("WASM adapter emits valid response JSON")
        }),
        Err(error) => json!({
            "ok": false,
            "error": serde_json::from_str::<Value>(&error)
                .unwrap_or(Value::String(error))
        }),
    };
    println!("{result}");
}
