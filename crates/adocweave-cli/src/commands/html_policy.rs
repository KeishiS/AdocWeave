use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use adocweave::output::html::{
    HtmlDocumentMode, HtmlOutput, RenderPolicy, StylesheetPolicy, StylesheetSource, render,
};
use adocweave::semantic::Document;

/// A stylesheet argument in command-line order. Files are embedded and URLs
/// are linked; both apply only to complete document output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StylesheetArgument {
    File(PathBuf),
    Url(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StylesheetFileOrigin {
    ProjectConfiguration,
    CommandLine,
}

#[derive(Clone)]
struct ExplicitStylesheetAuthority {
    authored: PathBuf,
    candidate: PathBuf,
    policy: adocweave_host::LocalTargetPolicy,
}

/// Retained authorities for stylesheet files selected on the command line.
///
/// Construction validates the shared stylesheet count before opening any
/// directory, and subsequent reads stay in the namespace selected here.
#[derive(Clone)]
pub(crate) struct ExplicitStylesheetAuthorities {
    entries: Vec<ExplicitStylesheetAuthority>,
}

impl ExplicitStylesheetAuthorities {
    pub(crate) fn new(
        project: &adocweave_config::HtmlSettings,
        stylesheets: &[StylesheetArgument],
    ) -> Result<Self, Error> {
        validate_stylesheet_count(project, stylesheets)?;
        let entries = stylesheets
            .iter()
            .filter_map(|argument| match argument {
                StylesheetArgument::File(path) => Some(path),
                StylesheetArgument::Url(_) => None,
            })
            .map(|authored| {
                let parent = authored
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                let file_name = authored.file_name().ok_or_else(|| {
                    Error::Usage(format!(
                        "stylesheet path has no file name: {}",
                        authored.display()
                    ))
                })?;
                let policy = adocweave_host::LocalTargetPolicy::new(parent).map_err(|source| {
                    Error::Read {
                        source_name: authored.display().to_string(),
                        source: io::Error::other(source),
                    }
                })?;
                Ok(ExplicitStylesheetAuthority {
                    authored: authored.clone(),
                    candidate: policy.root().join(file_name),
                    policy,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self { entries })
    }

    pub(crate) fn candidates(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().map(|entry| entry.candidate.as_path())
    }

    pub(crate) fn read_authored(&self, authored: &Path) -> io::Result<(PathBuf, Vec<u8>)> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.authored == authored)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "explicit stylesheet authority is missing: {}",
                    authored.display()
                ))
            })?;
        Self::read_entry(entry)
    }

    pub(crate) fn read_candidate(&self, candidate: &Path) -> io::Result<(PathBuf, Vec<u8>)> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.candidate == candidate)
            .ok_or_else(|| io::Error::other("explicit stylesheet authority is missing"))?;
        Self::read_entry(entry)
    }

    fn read_entry(entry: &ExplicitStylesheetAuthority) -> io::Result<(PathBuf, Vec<u8>)> {
        let mut session = crate::configuration_stylesheet_session(entry.policy.clone());
        session
            .read_candidate_bytes_no_symlinks(&entry.candidate)
            .map(adocweave_host::LoadedLocalBytes::into_parts)
            .map_err(io::Error::other)
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    Cancelled,
    InvalidUtf8 {
        valid_up_to: usize,
    },
    Read {
        source_name: String,
        source: io::Error,
    },
    Stylesheet(String),
    Usage(String),
}

/// Builds the render policy from project settings and command-line
/// stylesheet arguments.
///
/// The caller supplies file loading and cancellation checks so normal
/// conversion and live preview share policy construction without coupling
/// this module to a particular host or dependency tracker.
pub(crate) fn build(
    project: &adocweave_config::HtmlSettings,
    complete: bool,
    stylesheets: &[StylesheetArgument],
    mut read: impl FnMut(StylesheetFileOrigin, &Path) -> io::Result<Vec<u8>>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<RenderPolicy, Error> {
    let limits = StylesheetPolicy::default();
    validate_stylesheet_count(project, stylesheets)?;
    let mut sources = Vec::new();
    for path in &project.stylesheet_files {
        sources.push(StylesheetSource::Inline(read_stylesheet(
            StylesheetFileOrigin::ProjectConfiguration,
            path,
            limits.max_inline_bytes,
            &mut read,
            &mut is_cancelled,
        )?));
    }
    sources.extend(
        project
            .stylesheet_urls
            .iter()
            .cloned()
            .map(StylesheetSource::External),
    );
    for stylesheet in stylesheets {
        ensure_active(&mut is_cancelled)?;
        match stylesheet {
            StylesheetArgument::File(path) => {
                sources.push(StylesheetSource::Inline(read_stylesheet(
                    StylesheetFileOrigin::CommandLine,
                    path,
                    limits.max_inline_bytes,
                    &mut read,
                    &mut is_cancelled,
                )?));
            }
            StylesheetArgument::Url(url) => {
                sources.push(StylesheetSource::External(url.clone()));
            }
        }
    }
    let document_mode = if complete {
        HtmlDocumentMode::Complete
    } else {
        project.policy.document_mode
    };
    if document_mode != HtmlDocumentMode::Complete && !sources.is_empty() {
        return Err(Error::Usage(
            "--css and --css-url require --complete".to_owned(),
        ));
    }
    Ok(RenderPolicy {
        document_mode,
        stylesheets: StylesheetPolicy { sources, ..limits },
        ..RenderPolicy::default()
    })
}

/// Rejects an input that would exceed the renderer's stylesheet count before
/// any file is opened or read.
pub(crate) fn validate_stylesheet_count(
    project: &adocweave_config::HtmlSettings,
    stylesheets: &[StylesheetArgument],
) -> Result<(), Error> {
    let limits = StylesheetPolicy::default();
    let count = project
        .stylesheet_files
        .len()
        .saturating_add(project.stylesheet_urls.len())
        .saturating_add(stylesheets.len());
    if count > usize::try_from(limits.max_sources).unwrap_or(usize::MAX) {
        return Err(Error::Stylesheet(format!(
            "stylesheet count exceeds the limit of {}",
            limits.max_sources
        )));
    }
    Ok(())
}

pub(crate) fn render_checked(
    document: &Document,
    policy: &RenderPolicy,
) -> Result<HtmlOutput, Error> {
    let output = render(document, policy);
    if let Some(diagnostic) = output
        .diagnostics
        .iter()
        .find(|diagnostic| is_stylesheet_error(diagnostic.code.as_str()))
    {
        return Err(Error::Stylesheet(diagnostic.message.clone()));
    }
    Ok(output)
}

pub(crate) fn external_origins(policy: &RenderPolicy) -> BTreeSet<String> {
    policy
        .stylesheets
        .sources
        .iter()
        .filter_map(|source| match source {
            StylesheetSource::External(value) => url::Url::parse(value).ok(),
            StylesheetSource::Inline(_) => None,
        })
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.origin().ascii_serialization())
        .collect()
}

fn is_stylesheet_error(code: &str) -> bool {
    matches!(
        code,
        "invalid-stylesheet-url"
            | "invalid-stylesheet-content"
            | "stylesheet-limit-exceeded"
            | "stylesheet-not-applicable"
    )
}

fn read_stylesheet(
    origin: StylesheetFileOrigin,
    path: &Path,
    max_inline_bytes: u32,
    read: &mut impl FnMut(StylesheetFileOrigin, &Path) -> io::Result<Vec<u8>>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<String, Error> {
    ensure_active(is_cancelled)?;
    let bytes = read(origin, path).map_err(|source| Error::Read {
        source_name: path.display().to_string(),
        source,
    })?;
    ensure_active(is_cancelled)?;
    if bytes.len() > usize::try_from(max_inline_bytes).expect("u32 fits usize on supported targets")
    {
        return Err(Error::Stylesheet(format!(
            "stylesheet {} exceeds the limit of {} bytes",
            path.display(),
            max_inline_bytes
        )));
    }
    String::from_utf8(bytes).map_err(|error| Error::InvalidUtf8 {
        valid_up_to: error.utf8_error().valid_up_to(),
    })
}

fn ensure_active(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    if is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use adocweave::{AnalysisOptions, Engine};

    use super::*;

    fn complete_settings() -> adocweave_config::HtmlSettings {
        adocweave_config::HtmlSettings {
            policy: RenderPolicy {
                document_mode: HtmlDocumentMode::Complete,
                ..RenderPolicy::default()
            },
            ..adocweave_config::HtmlSettings::default()
        }
    }

    #[test]
    fn build_preserves_project_then_command_stylesheet_order() {
        let project = adocweave_config::HtmlSettings {
            stylesheet_files: vec![PathBuf::from("project.css")],
            stylesheet_urls: vec!["https://example.com/project.css".to_owned()],
            ..complete_settings()
        };
        let command = [
            StylesheetArgument::File(PathBuf::from("command.css")),
            StylesheetArgument::Url("https://example.com/command.css".to_owned()),
        ];

        let policy = build(
            &project,
            false,
            &command,
            |_, path| Ok(format!("/* {} */", path.display()).into_bytes()),
            || false,
        )
        .expect("render policy");

        assert_eq!(
            policy.stylesheets.sources,
            [
                StylesheetSource::Inline("/* project.css */".to_owned()),
                StylesheetSource::External("https://example.com/project.css".to_owned()),
                StylesheetSource::Inline("/* command.css */".to_owned()),
                StylesheetSource::External("https://example.com/command.css".to_owned()),
            ]
        );
        assert_eq!(
            external_origins(&policy),
            BTreeSet::from(["https://example.com".to_owned()])
        );
    }

    #[test]
    fn build_stops_before_reading_after_cancellation() {
        let project = adocweave_config::HtmlSettings {
            stylesheet_files: vec![PathBuf::from("unread.css")],
            ..complete_settings()
        };
        let mut read = false;

        let error = build(
            &project,
            false,
            &[],
            |_, _| {
                read = true;
                Ok(Vec::new())
            },
            || true,
        )
        .expect_err("cancelled policy");

        assert!(matches!(error, Error::Cancelled));
        assert!(!read);
    }

    #[test]
    fn build_checks_cancellation_after_each_loaded_snapshot() {
        let project = adocweave_config::HtmlSettings {
            stylesheet_files: vec![PathBuf::from("loaded.css")],
            ..complete_settings()
        };
        let cancelled = Cell::new(false);
        let reads = Cell::new(0);

        let error = build(
            &project,
            false,
            &[],
            |_, _| {
                reads.set(reads.get() + 1);
                cancelled.set(true);
                Ok(b"body {}".to_vec())
            },
            || cancelled.get(),
        )
        .expect_err("cancelled after loading");

        assert!(matches!(error, Error::Cancelled));
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn build_rejects_excess_stylesheets_before_reading_files() {
        let limit =
            usize::try_from(StylesheetPolicy::default().max_sources).expect("u32 fits usize");
        let project = adocweave_config::HtmlSettings {
            stylesheet_files: vec![PathBuf::from("project.css")],
            stylesheet_urls: vec!["https://example.com/project.css".to_owned()],
            ..complete_settings()
        };
        let command = (0..limit - 1)
            .map(|index| StylesheetArgument::File(PathBuf::from(format!("{index}.css"))))
            .collect::<Vec<_>>();
        let mut read = false;

        let error = build(
            &project,
            false,
            &command,
            |_, _| {
                read = true;
                Ok(Vec::new())
            },
            || false,
        )
        .expect_err("stylesheet count limit");

        assert!(matches!(
            error,
            Error::Stylesheet(message) if message == "stylesheet count exceeds the limit of 16"
        ));
        assert!(
            !read,
            "authority readers must not run after count rejection"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_stylesheet_authority_keeps_the_selected_directory() {
        let parent = tempfile::tempdir().expect("temporary parent");
        let root = parent.path().join("styles");
        std::fs::create_dir(&root).expect("stylesheet directory");
        let stylesheet = root.join("theme.css");
        std::fs::write(&stylesheet, "trusted").expect("trusted stylesheet");
        let arguments = [StylesheetArgument::File(stylesheet.clone())];
        let authorities = ExplicitStylesheetAuthorities::new(
            &adocweave_config::HtmlSettings::default(),
            &arguments,
        )
        .expect("stylesheet authorities");
        let displaced = parent.path().join("retained-styles");

        std::fs::rename(&root, &displaced).expect("displace stylesheet directory");
        std::fs::create_dir(&root).expect("replacement stylesheet directory");
        std::fs::write(&stylesheet, "outside").expect("replacement stylesheet");

        let (_, bytes) = authorities
            .read_authored(&stylesheet)
            .expect("retained stylesheet read");
        assert_eq!(bytes, b"trusted");
        std::fs::remove_dir_all(&root).expect("remove replacement directory");
        std::fs::rename(displaced, &root).expect("restore stylesheet directory");
    }

    #[test]
    fn build_rejects_read_utf8_size_and_fragment_failures() {
        let project = adocweave_config::HtmlSettings {
            stylesheet_files: vec![PathBuf::from("style.css")],
            ..complete_settings()
        };
        let read_error = build(
            &project,
            false,
            &[],
            |_, _| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
            || false,
        )
        .expect_err("read error");
        assert!(matches!(
            read_error,
            Error::Read { source_name, .. } if source_name == "style.css"
        ));

        let utf8_error =
            build(&project, false, &[], |_, _| Ok(vec![0xff]), || false).expect_err("UTF-8 error");
        assert!(matches!(utf8_error, Error::InvalidUtf8 { valid_up_to: 0 }));

        let oversize = usize::try_from(StylesheetPolicy::default().max_inline_bytes)
            .expect("u32 fits usize")
            + 1;
        let size_error = build(
            &project,
            false,
            &[],
            |_, _| Ok(vec![b'x'; oversize]),
            || false,
        )
        .expect_err("size error");
        assert!(matches!(size_error, Error::Stylesheet(message) if message.contains("exceeds")));

        let fragment = adocweave_config::HtmlSettings::default();
        let usage_error = build(
            &fragment,
            false,
            &[StylesheetArgument::Url(
                "https://example.com/style.css".to_owned(),
            )],
            |_, _| unreachable!("URL does not read a file"),
            || false,
        )
        .expect_err("fragment stylesheet");
        assert!(matches!(usage_error, Error::Usage(_)));
    }

    #[test]
    fn checked_render_promotes_the_first_stylesheet_diagnostic() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("body\n")
            .expect("analysis");
        let policy = RenderPolicy {
            document_mode: HtmlDocumentMode::Complete,
            stylesheets: StylesheetPolicy {
                sources: vec![StylesheetSource::External("javascript:alert(1)".to_owned())],
                ..StylesheetPolicy::default()
            },
            ..RenderPolicy::default()
        };

        let error =
            render_checked(analysis.document(), &policy).expect_err("invalid stylesheet URL");

        assert!(matches!(error, Error::Stylesheet(message) if message.contains("stylesheet URL")));
    }
}
