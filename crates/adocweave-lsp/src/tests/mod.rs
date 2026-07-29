//! Typed Language Server service and transport tests.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, path::PathBuf};

use adocweave::resolution::ReferenceKey;
use async_lsp::lsp_types as lsp;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::{HostReferenceIndex, HostReferenceRequest, PositionEncoding, run};
use crate::service::LanguageService;
use crate::state::{Adoption, AnalysisJob, WorkspaceProblem};

mod support;

use support::*;

mod capabilities;
mod conformance;
mod diagnostics;
mod editing;
mod feature_integration;
mod navigation;
mod protocol;
mod semantic_tokens;
mod state_workspace;
