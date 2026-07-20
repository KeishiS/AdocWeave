#![no_main]

use adocweave::{CheckOutput, Operation, process, process_check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    for operation in [
        Operation::Convert,
        Operation::Check,
        Operation::Format,
        Operation::Symbols,
    ] {
        let _ = process(operation, input);
    }
    let _ = process_check(input, CheckOutput::Human);
    let _ = process_check(input, CheckOutput::Json);
});
