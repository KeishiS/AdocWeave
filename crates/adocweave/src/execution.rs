//! Runtime-independent contracts for scheduling and adopting core analysis.

use std::fmt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{Analysis, AnalysisOptions, CancellationCheck, Engine, ParseError, SourceId};

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
    pub options: AnalysisOptions,
}

impl AnalysisRequest {
    pub fn new(
        source_id: Option<SourceId>,
        version: i64,
        generation: u64,
        source: impl Into<Arc<str>>,
        options: AnalysisOptions,
    ) -> Self {
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
        AnalysisCacheKey::new(
            &self.source,
            self.revision.source_id.as_ref(),
            &self.options,
        )
    }

    pub fn analyze(
        &self,
        cancellation: &dyn CancellationCheck,
    ) -> Result<AnalysisResult, ParseError> {
        let cache_key = self.cache_key();
        let analysis = Engine::new(self.options.clone()).analyze_cancellable_with_source_id(
            self.revision.source_id.as_ref(),
            &self.source,
            cancellation,
        )?;
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
    pub fn new(source: &str, source_id: Option<&SourceId>, options: &AnalysisOptions) -> Self {
        let crate::core::SyntaxOptions {
            syntax_mode,
            limits,
        } = &options.syntax;
        let crate::limits::AnalysisLimits {
            max_input_bytes,
            max_line_bytes,
            max_list_depth,
            max_list_continuations,
            max_block_depth,
            max_inline_depth,
            max_formula_bytes,
            max_table_bytes,
            max_table_cells,
            max_table_columns,
            max_table_depth,
            max_catalog_entries,
            max_catalog_bytes,
            max_blocks,
            max_nodes,
            max_references,
            max_attributes,
            max_attribute_expansion_depth,
            max_attribute_expansion_bytes,
        } = *limits;
        let config = &options.diagnostics.lint;
        let crate::url::AuthoredUrlPolicy {
            allowed_schemes,
            allow_relative,
        } = &config.authored_url_policy;
        let mut hasher = Sha256::new();
        hash_bytes(&mut hasher, crate::VERSION.as_bytes());
        hash_bytes(&mut hasher, source.as_bytes());
        hash_optional_string(&mut hasher, source_id.map(SourceId::as_str));
        hash_u8(
            &mut hasher,
            match syntax_mode {
                crate::limits::SyntaxMode::Permissive => 0,
                crate::limits::SyntaxMode::Strict => 1,
            },
        );
        for value in [
            max_input_bytes,
            max_line_bytes,
            max_list_depth,
            max_list_continuations,
            max_block_depth,
            max_inline_depth,
            max_formula_bytes,
            max_table_bytes,
            max_table_cells,
            max_table_columns,
            max_table_depth,
            max_catalog_entries,
            max_catalog_bytes,
            max_blocks,
            max_nodes,
            max_references,
            max_attributes,
            max_attribute_expansion_depth,
            max_attribute_expansion_bytes,
        ] {
            hash_u64(&mut hasher, u64::from(value));
        }
        for value in [
            config.max_line_length,
            config.max_consecutive_blank_lines,
            config.max_diagnostics,
        ] {
            hash_u64(
                &mut hasher,
                u64::try_from(value).expect("diagnostic limit fits u64"),
            );
        }
        hash_u64(
            &mut hasher,
            u64::try_from(config.protected_attributes.len()).expect("attribute count fits u64"),
        );
        for (name, value) in &config.protected_attributes {
            hash_bytes(&mut hasher, name.as_bytes());
            hash_bytes(&mut hasher, value.as_bytes());
        }
        hash_u8(
            &mut hasher,
            severity_tag(config.protected_attribute_severity),
        );
        for (rule, settings) in config.configured_rules() {
            hash_bytes(&mut hasher, rule.as_str().as_bytes());
            hash_bool(&mut hasher, settings.enabled);
            hash_u8(&mut hasher, severity_tag(settings.severity));
        }
        hash_bool(&mut hasher, *allow_relative);
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

const fn severity_tag(severity: crate::output::diagnostics::Severity) -> u8 {
    match severity {
        crate::output::diagnostics::Severity::Error => 0,
        crate::output::diagnostics::Severity::Warning => 1,
        crate::output::diagnostics::Severity::Information => 2,
        crate::output::diagnostics::Severity::Hint => 3,
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

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{CancellationToken, NeverCancel};

    use super::*;

    fn request(source: &str) -> AnalysisRequest {
        AnalysisRequest::new(
            Some(SourceId::new("host:one")),
            1,
            1,
            Arc::<str>::from(source),
            AnalysisOptions::default(),
        )
    }

    #[test]
    fn cache_key_is_stable_and_covers_every_analysis_option() {
        let baseline = request("text").cache_key();
        assert_eq!(
            baseline.to_hex(),
            "fab7cd83948a612e5f942c1ec8f0fa9fea233e1c6d0bf292a5aa4736d86e9ed5"
        );
        assert_eq!(baseline, request("text").cache_key());
        assert_ne!(baseline, request("other").cache_key());

        let mut variants = Vec::new();
        let options = AnalysisOptions {
            syntax: crate::core::SyntaxOptions {
                syntax_mode: crate::limits::SyntaxMode::Strict,
                ..crate::core::SyntaxOptions::default()
            },
            ..AnalysisOptions::default()
        };
        variants.push(options);
        let mut options = AnalysisOptions::default();
        options.syntax.limits.max_nodes += 1;
        variants.push(options);
        let mut options = AnalysisOptions::default();
        options.diagnostics.lint.protected_attributes =
            BTreeMap::from([("host".to_owned(), "value".to_owned())]);
        variants.push(options);
        let mut options = AnalysisOptions::default();
        options.diagnostics.lint.authored_url_policy.allow_relative = false;
        variants.push(options);
        let mut options = AnalysisOptions::default();
        options
            .diagnostics
            .lint
            .authored_url_policy
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
}
