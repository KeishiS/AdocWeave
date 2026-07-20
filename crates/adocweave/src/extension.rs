//! Data-driven semantic extensions applied after the standard syntax parse.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocatorConstraint {
    Opaque,
    CanonicalUuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceSchemeExtension {
    pub scheme: String,
    pub locator_constraint: LocatorConstraint,
    pub diagnostic_code: String,
    pub diagnostic_message: String,
}

impl ReferenceSchemeExtension {
    pub fn new(
        scheme: impl Into<String>,
        locator_constraint: LocatorConstraint,
        diagnostic_code: impl Into<String>,
        diagnostic_message: impl Into<String>,
    ) -> Self {
        Self {
            scheme: scheme.into().to_ascii_lowercase(),
            locator_constraint,
            diagnostic_code: diagnostic_code.into(),
            diagnostic_message: diagnostic_message.into(),
        }
    }

    pub fn accepts(&self, locator: &str) -> bool {
        match self.locator_constraint {
            LocatorConstraint::Opaque => !locator.is_empty(),
            LocatorConstraint::CanonicalUuid => is_canonical_uuid(locator),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtensionConfig {
    reference_schemes: BTreeMap<String, ReferenceSchemeExtension>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionRegistrationError {
    InvalidScheme,
    InvalidDiagnosticCode,
    DuplicateScheme(String),
}

impl ExtensionConfig {
    pub fn register_reference_scheme(
        &mut self,
        extension: ReferenceSchemeExtension,
    ) -> Result<(), ExtensionRegistrationError> {
        if !valid_scheme(&extension.scheme) {
            return Err(ExtensionRegistrationError::InvalidScheme);
        }
        if extension.diagnostic_code.is_empty()
            || !extension.diagnostic_code.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
            })
        {
            return Err(ExtensionRegistrationError::InvalidDiagnosticCode);
        }
        if self.reference_schemes.contains_key(&extension.scheme) {
            return Err(ExtensionRegistrationError::DuplicateScheme(
                extension.scheme,
            ));
        }
        self.reference_schemes
            .insert(extension.scheme.clone(), extension);
        Ok(())
    }

    pub fn reference_scheme(&self, scheme: &str) -> Option<&ReferenceSchemeExtension> {
        self.reference_schemes.get(&scheme.to_ascii_lowercase())
    }
}

fn valid_scheme(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit() && index > 0
                || matches!(byte, b'+' | b'-' | b'.') && index > 0
        })
        && value.as_bytes()[0].is_ascii_lowercase()
}

pub fn is_canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{
        ExtensionConfig, ExtensionRegistrationError, LocatorConstraint, ReferenceSchemeExtension,
    };

    #[test]
    fn note_reference_is_an_optional_scheme_extension() {
        let mut extensions = ExtensionConfig::default();
        extensions
            .register_reference_scheme(ReferenceSchemeExtension::new(
                "note",
                LocatorConstraint::CanonicalUuid,
                "invalid-note-uuid",
                "note reference requires a canonical lowercase UUID",
            ))
            .expect("register");

        let note = extensions.reference_scheme("NOTE").expect("registered");
        assert!(note.accepts("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!note.accepts("123"));

        assert!(matches!(
            extensions.register_reference_scheme(ReferenceSchemeExtension::new(
                "note",
                LocatorConstraint::Opaque,
                "duplicate-note",
                "duplicate",
            )),
            Err(ExtensionRegistrationError::DuplicateScheme(scheme)) if scheme == "note"
        ));
    }
}
