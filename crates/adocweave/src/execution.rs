//! Runtime-independent contracts for scheduling and adopting core analysis.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{
    Analysis, CORE_API_VERSION, CancellationCheck, Engine, ParseError, ParseOptions, SourceId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentRevision {
    pub source_id: Option<SourceId>,
    pub version: i64,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisRequest {
    pub revision: DocumentRevision,
    pub source: Arc<str>,
    pub options: ParseOptions,
}

impl AnalysisRequest {
    pub fn new(
        source_id: Option<SourceId>,
        version: i64,
        generation: u64,
        source: impl Into<Arc<str>>,
        mut options: ParseOptions,
    ) -> Self {
        options.source_id = source_id.clone();
        Self {
            revision: DocumentRevision {
                source_id,
                version,
                generation,
            },
            source: source.into(),
            options,
        }
    }

    pub fn cache_key(&self) -> AnalysisCacheKey {
        AnalysisCacheKey::new(&self.source, &self.options)
    }

    pub fn analyze(
        &self,
        cancellation: &dyn CancellationCheck,
    ) -> Result<AnalysisResult, ParseError> {
        let cache_key = self.cache_key();
        let analysis =
            Engine::new(self.options.clone()).analyze_cancellable(&self.source, cancellation)?;
        Ok(AnalysisResult {
            revision: self.revision.clone(),
            cache_key,
            analysis,
        })
    }
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnalysisCacheKey([u8; 32]);

impl AnalysisCacheKey {
    pub fn new(source: &str, options: &ParseOptions) -> Self {
        let ParseOptions {
            source_id,
            profile,
            limits,
            protected_attributes,
            url_policy,
        } = options;
        let crate::limits::ProcessingLimits {
            max_input_bytes,
            max_output_bytes,
            max_line_bytes,
            max_list_depth,
            max_inline_depth,
            max_formula_bytes,
            max_blocks,
            max_nodes,
            max_references,
            max_attributes,
            max_diagnostics,
        } = *limits;
        let crate::url::UrlPolicy {
            allowed_schemes,
            allow_relative,
            allow_data_uris,
        } = url_policy;
        let mut hasher = Sha256::new();
        hash_u16(&mut hasher, CORE_API_VERSION);
        hash_bytes(&mut hasher, source.as_bytes());
        hash_optional_string(&mut hasher, source_id.as_ref().map(SourceId::as_str));
        hash_u16(&mut hasher, profile.version);
        hash_u8(
            &mut hasher,
            match profile.mode {
                crate::limits::SyntaxMode::Permissive => 0,
                crate::limits::SyntaxMode::Strict => 1,
            },
        );
        for value in [
            max_input_bytes,
            max_output_bytes,
            max_line_bytes,
            max_list_depth,
            max_inline_depth,
            max_formula_bytes,
            max_blocks,
            max_nodes,
            max_references,
            max_attributes,
            max_diagnostics,
        ] {
            hash_u64(&mut hasher, u64::from(value));
        }
        hash_u64(
            &mut hasher,
            u64::try_from(protected_attributes.len()).expect("attribute count fits u64"),
        );
        for (name, value) in protected_attributes {
            hash_bytes(&mut hasher, name.as_bytes());
            hash_bytes(&mut hasher, value.as_bytes());
        }
        hash_bool(&mut hasher, *allow_relative);
        hash_bool(&mut hasher, *allow_data_uris);
        hash_u64(
            &mut hasher,
            u64::try_from(allowed_schemes.len()).expect("scheme count fits u64"),
        );
        for scheme in allowed_schemes {
            hash_bytes(&mut hasher, scheme.as_bytes());
        }
        Self(hasher.finalize().into())
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in self.0 {
            output.push(HEX[usize::from(byte >> 4)] as char);
            output.push(HEX[usize::from(byte & 0x0f)] as char);
        }
        output
    }
}

impl fmt::Debug for AnalysisCacheKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AnalysisCacheKey")
            .field(&self.to_hex())
            .finish()
    }
}

#[derive(Debug)]
pub struct AnalysisResult {
    pub revision: DocumentRevision,
    pub cache_key: AnalysisCacheKey,
    pub analysis: Analysis,
}

impl AnalysisResult {
    pub fn is_current(
        &self,
        current: &DocumentRevision,
        cancellation: &dyn CancellationCheck,
    ) -> bool {
        !cancellation.is_cancelled() && self.revision == *current
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionResultClass {
    Success,
    Cancelled,
    InvalidInput,
    LimitExceeded,
    Failed,
    Panicked,
    Stale,
}

impl From<&ParseError> for ExecutionResultClass {
    fn from(error: &ParseError) -> Self {
        match error {
            ParseError::Cancelled => Self::Cancelled,
            ParseError::InvalidProfileVersion { .. } | ParseError::UnsupportedSyntax => {
                Self::InvalidInput
            }
            ParseError::LimitExceeded { .. } => Self::LimitExceeded,
            ParseError::Position(_) | ParseError::InternalInvariant => Self::Failed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionObservation {
    pub result: ExecutionResultClass,
    pub elapsed: Duration,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub diagnostic_count: usize,
}

impl ExecutionObservation {
    pub fn success(
        request: &AnalysisRequest,
        result: &AnalysisResult,
        elapsed: Duration,
        output_bytes: usize,
    ) -> Self {
        Self {
            result: ExecutionResultClass::Success,
            elapsed,
            input_bytes: request.source.len(),
            output_bytes,
            diagnostic_count: result.analysis.diagnostics().len(),
        }
    }

    pub const fn failure(
        input_bytes: usize,
        elapsed: Duration,
        result: ExecutionResultClass,
    ) -> Self {
        Self {
            result,
            elapsed,
            input_bytes,
            output_bytes: 0,
            diagnostic_count: 0,
        }
    }
}

fn hash_optional_string(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash_u8(hasher, 1);
            hash_bytes(hasher, value.as_bytes());
        }
        None => hash_u8(hasher, 0),
    }
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hash_u64(
        hasher,
        u64::try_from(value.len()).expect("byte length fits u64"),
    );
    hasher.update(value);
}

fn hash_bool(hasher: &mut Sha256, value: bool) {
    hash_u8(hasher, u8::from(value));
}

fn hash_u8(hasher: &mut Sha256, value: u8) {
    hasher.update([value]);
}

fn hash_u16(hasher: &mut Sha256, value: u16) {
    hasher.update(value.to_le_bytes());
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{CancellationToken, NeverCancel, SyntaxProfile};

    use super::*;

    fn request(source: &str) -> AnalysisRequest {
        AnalysisRequest::new(
            Some(SourceId::new("host:one")),
            1,
            1,
            Arc::<str>::from(source),
            ParseOptions::default(),
        )
    }

    #[test]
    fn cache_key_is_stable_and_covers_every_parse_option() {
        let baseline = request("text").cache_key();
        assert_eq!(
            baseline.to_hex(),
            "86bf76bdfc441926e28abedff4a2e0d5ff2718d4f9d06f637eb9aab4a5da92ad"
        );
        assert_eq!(baseline, request("text").cache_key());
        assert_ne!(baseline, request("other").cache_key());

        let mut variants = Vec::new();
        let mut options = ParseOptions::default();
        options.profile = SyntaxProfile {
            version: 1,
            ..options.profile
        };
        variants.push(options);
        let mut options = ParseOptions::default();
        options.limits.max_nodes += 1;
        variants.push(options);
        let options = ParseOptions {
            protected_attributes: BTreeMap::from([("host".to_owned(), "value".to_owned())]),
            ..ParseOptions::default()
        };
        variants.push(options);
        let mut options = ParseOptions::default();
        options.url_policy.allow_relative = true;
        variants.push(options);
        let mut options = ParseOptions::default();
        options
            .url_policy
            .allowed_schemes
            .insert("mailto".to_owned());
        variants.push(options);

        for options in variants {
            let candidate = AnalysisRequest::new(
                Some(SourceId::new("host:one")),
                1,
                1,
                Arc::<str>::from("text"),
                options,
            );
            assert_ne!(baseline, candidate.cache_key());
        }
        assert_eq!(baseline.to_hex().len(), 64);
    }

    #[test]
    fn cancellation_and_revision_gate_result_adoption() {
        let request = request("= Current");
        let result = request.analyze(&NeverCancel).expect("analysis");
        assert!(result.is_current(&request.revision, &NeverCancel));

        let stale = DocumentRevision {
            generation: request.revision.generation + 1,
            ..request.revision.clone()
        };
        assert!(!result.is_current(&stale, &NeverCancel));

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(!result.is_current(&request.revision, &cancelled));
    }

    #[test]
    fn observations_contain_sizes_and_counts_but_never_source_text() {
        let request = request("secret body ");
        let result = request.analyze(&NeverCancel).expect("analysis");
        let observation =
            ExecutionObservation::success(&request, &result, Duration::from_millis(2), 10);
        let debug = format!("{observation:?}");

        assert_eq!(observation.input_bytes, request.source.len());
        assert_eq!(observation.diagnostic_count, 1);
        assert!(!debug.contains("secret body"));
    }
}
