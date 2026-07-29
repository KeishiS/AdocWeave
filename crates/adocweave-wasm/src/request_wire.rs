//! JSON request values at the public WASM boundary.
//!
//! `protocol/public-api.json` is the source of truth for these shapes and
//! defaults. Core semantic types are introduced only by `request_conversion`.

pub use crate::request_wire_generated::*;
