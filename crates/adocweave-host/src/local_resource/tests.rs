//! Behaviour tests for the bounded local-resource boundary.

use super::*;
use crate::io_observation::FilesystemIoUsage;
use std::error::Error;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn unbounded_job() -> FilesystemJobCoordinator {
    FilesystemJobCoordinator::new(crate::FilesystemJobLimits::unbounded()).expect("filesystem job")
}

/// Gives up one claim the way a live owner does: through a short draft.
///
/// The outcome is asserted, so a claim that has gone stale fails the test
/// rather than quietly releasing nothing.
fn release_binding(session: &mut LocalFilesystemSession, binding: &FilesystemResourceBinding) {
    let mut draft = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        draft.release_binding(binding).expect("release binding"),
        FilesystemReleaseOutcome::Released
    );
    draft
        .prepare_commit(session)
        .expect("prepare release")
        .commit()
        .expect("commit release");
}

fn job_limits(read_bytes: u64, directory_entries: u64) -> crate::FilesystemJobLimits {
    crate::FilesystemJobLimits {
        max_read_operations: 16,
        max_read_bytes: read_bytes,
        max_read_probe_bytes: 1,
        max_directory_operations: 16,
        max_directory_entries: directory_entries,
        max_directory_probe_entries: 1,
        max_candidate_changes: 16,
        max_sessions: 4,
    }
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "adocweave-host-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        let mut directory = Self(path);
        // `std::env::temp_dir` does not return a resolved path on every
        // platform. macOS answers with `/var/...`, which is a symbolic link
        // to `/private/var`, and Windows can answer with a shortened
        // `RUNNER~1` component. Roots are stored in resolved form, so a test
        // holding the unresolved spelling would build candidate paths that
        // the policy reports as outside its own root.
        directory.0 = directory.0.canonicalize().expect("resolve the test root");
        directory
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn policy(root: &Path, max_resource_bytes: u64) -> LocalFilesystemPolicy {
    LocalFilesystemPolicy::new(
        [root.to_owned()],
        FilesystemReadLimits {
            max_files: 10,
            max_total_bytes: 100,
            max_resource_bytes,
        },
    )
    .expect("valid policy")
}

fn source_id() -> LogicalSourceId {
    LogicalSourceId::new("test-source").expect("source ID")
}

/// The draft shares the meter of the session it was cloned from, so reading
/// it after the draft is gone still reports the draft's work.
fn draft_meter(draft: &LocalFilesystemDraft) -> FilesystemIoMeter {
    draft.candidate().meter.clone()
}

fn cached_texts(session: &LocalFilesystemSession) -> usize {
    session
        .state
        .sessions
        .iter()
        .map(LocalTargetSession::cached_texts)
        .sum()
}

#[test]
fn filesystem_draft_is_isolated_until_commit_and_drop_discards_it() {
    let root = TestDir::new("filesystem-draft-isolation");
    let path = root.path().join("source.adoc");
    fs::write(&path, "a").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    session.read_utf8(source_id(), &path).expect("initial read");

    fs::write(&path, "bb").expect("replacement");
    let mut discarded = session.draft(&unbounded_job()).expect("discarded draft");
    discarded
        .reread_utf8(source_id(), &path)
        .expect("draft reread");
    assert_eq!(discarded.budget().bytes(), 2);
    assert_eq!(session.budget().bytes(), 1);
    drop(discarded);
    assert_eq!(session.budget().bytes(), 1);

    let mut committed = session.draft(&unbounded_job()).expect("committed draft");
    let loaded = committed
        .reread_utf8(source_id(), &path)
        .expect("replacement reread");
    let binding = loaded.binding().clone();
    committed
        .prepare_commit(&mut session)
        .expect("prepare commit draft")
        .commit()
        .expect("commit");
    assert_eq!(session.budget().bytes(), 2);

    let mut released = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        released.release_binding(&binding).expect("release binding"),
        FilesystemReleaseOutcome::Released
    );
    assert_eq!(session.budget().bytes(), 2);
    released
        .prepare_commit(&mut session)
        .expect("prepare commit release")
        .commit()
        .expect("commit");
    assert_eq!(session.budget(), ResourceBudget::default());
}

#[test]
fn draft_operations_reuse_one_candidate_state_clone() {
    let root = TestDir::new("draft-state-clone-count");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let session = policy(root.path(), 100).session().expect("session");
    let clone_count = Arc::clone(&session.state.clone_count);
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    assert_eq!(clone_count.load(Ordering::Relaxed), 1);

    let first = draft.read_utf8(source_id(), &path).expect("read");
    draft.reread_utf8(source_id(), &path).expect("reread");
    draft
        .discover_adoc_paths_with_control(|_, _| false, || false)
        .expect("discover");
    draft.scan_utf8(|_| Ok(source_id())).expect("scan");
    assert_eq!(
        draft
            .release_binding(first.binding())
            .expect("stale release"),
        FilesystemReleaseOutcome::Stale
    );

    assert_eq!(clone_count.load(Ordering::Relaxed), 1);
}

#[test]
fn draft_clone_unwind_releases_the_lease() {
    let root = TestDir::new("draft-clone-unwind");
    let session = policy(root.path(), 100).session().expect("session");
    FORCE_DRAFT_STATE_CLONE_PANIC.with(|forced| forced.set(true));

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = session.draft(&unbounded_job());
    }));
    FORCE_DRAFT_STATE_CLONE_PANIC.with(|forced| forced.set(false));

    assert!(unwind.is_err());
    drop(
        session
            .draft(&unbounded_job())
            .expect("unwind released draft lease"),
    );
}

#[test]
fn draft_resource_error_preserves_its_typed_source() {
    let root = TestDir::new("draft-resource-source");
    let session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");

    let error = draft
        .read_utf8(source_id(), root.path())
        .expect_err("directory is not a resource");

    assert_eq!(
        error,
        FilesystemDraftError::Resource(ResourceError::NotRegularFile(root.path().to_owned()))
    );
    assert_eq!(
        Error::source(&error).and_then(|source| source.downcast_ref::<ResourceError>()),
        Some(&ResourceError::NotRegularFile(root.path().to_owned()))
    );
    assert_eq!(
        FilesystemDraftError::DraftBusy.to_string(),
        "filesystem session already has an active draft"
    );
    assert!(Error::source(&FilesystemDraftError::DraftBusy).is_none());
}

#[test]
fn draft_release_rejects_a_foreign_binding_with_a_typed_error() {
    let root = TestDir::new("foreign-binding");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let mut first = policy(root.path(), 100).session().expect("first session");
    let loaded = first.read_utf8(source_id(), &path).expect("first read");
    let second = policy(root.path(), 100).session().expect("second session");
    let mut draft = second.draft(&unbounded_job()).expect("second draft");

    assert_eq!(
        draft.release_binding(loaded.binding()),
        Err(FilesystemDraftError::ForeignBinding)
    );
}

#[test]
fn filesystem_draft_is_exclusive_and_failed_operations_poison_commit() {
    let root = TestDir::new("filesystem-draft-exclusive");
    let mut session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("first draft");
    assert!(matches!(
        session.draft(&unbounded_job()),
        Err(FilesystemDraftError::DraftBusy)
    ));
    assert!(draft.read_utf8(source_id(), root.path()).is_err());
    assert!(matches!(
        draft.prepare_commit(&mut session),
        Err(FilesystemDraftError::PoisonedDraft)
    ));
    drop(
        session
            .draft(&unbounded_job())
            .expect("poisoned draft released its lease"),
    );
}

#[test]
fn not_found_removes_only_the_draft_binding_until_commit() {
    let root = TestDir::new("not-found-draft-transition");
    let path = root.path().join("source.adoc");
    fs::write(&path, "old text").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    session.read_utf8(source_id(), &path).expect("initial read");
    session
        .reread_utf8(source_id(), &path)
        .expect("cache initial text");
    assert_eq!(session.budget().files(), 1);
    assert_eq!(cached_texts(&session), 1);

    fs::remove_file(&path).expect("remove source");
    let mut discarded = session.draft(&unbounded_job()).expect("discarded draft");
    assert_eq!(
        discarded.reread_utf8_outcome(source_id(), &path),
        Ok(FilesystemReadOutcome::NotFound {
            source_id: source_id(),
            candidate_path: path.clone(),
        })
    );
    assert_eq!(discarded.budget().files(), 0);
    drop(discarded);
    assert_eq!(session.budget().files(), 1);
    assert_eq!(cached_texts(&session), 1);

    let mut committed = session.draft(&unbounded_job()).expect("committed draft");
    assert!(matches!(
        committed.reread_utf8_outcome(source_id(), &path),
        Ok(FilesystemReadOutcome::NotFound { .. })
    ));
    committed
        .prepare_commit(&mut session)
        .expect("prepare")
        .commit()
        .expect("commit");
    assert_eq!(session.budget().files(), 0);
    assert_eq!(cached_texts(&session), 0);

    fs::write(&path, "new text").expect("recreate source");
    let loaded = session
        .read_utf8(source_id(), &path)
        .expect("read recreated source");
    assert_eq!(loaded.source(), "new text");
    assert_eq!(session.budget().files(), 1);
}

#[test]
fn missing_outcomes_keep_distinct_path_inspections_bounded() {
    let root = TestDir::new("not-found-path-bound");
    let first = root.path().join("first.adoc");
    let second = root.path().join("second.adoc");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 1,
            max_total_bytes: 100,
            max_resource_bytes: 100,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");

    assert!(matches!(
        session.read_utf8_outcome(source_id(), &first),
        Ok(FilesystemReadOutcome::NotFound { .. })
    ));
    assert_eq!(
        session.read_utf8_outcome(source_id(), &second),
        Err(ResourceError::FileLimit { limit: 1 })
    );
    assert_eq!(session.state.meter.usage().read_operations, 2);
    assert_eq!(session.state.meter.usage().read_bytes, 0);
}

#[test]
fn legacy_missing_reads_preserve_the_current_binding_and_budget() {
    let root = TestDir::new("legacy-missing-state");
    let path = root.path().join("source.adoc");
    fs::write(&path, "old text").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let initial = session.read_utf8(source_id(), &path).expect("initial read");
    let current = session
        .reread_utf8(source_id(), &path)
        .expect("cached reread");
    assert_eq!(cached_texts(&session), 1);
    fs::remove_file(&path).expect("remove source");

    assert_eq!(
        session.reread_utf8(source_id(), &path),
        Err(ResourceError::Missing(path.clone()))
    );
    assert_eq!(
        session.read_utf8(source_id(), &path),
        Err(ResourceError::Missing(path.clone()))
    );
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 8));
    assert_eq!(cached_texts(&session), 1);
    let mut release = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        release
            .release_binding(initial.binding())
            .expect("stale binding"),
        FilesystemReleaseOutcome::Stale
    );
    assert_eq!(
        release
            .release_binding(current.binding())
            .expect("current binding"),
        FilesystemReleaseOutcome::Released
    );
    release
        .prepare_commit(&mut session)
        .expect("prepare release")
        .commit()
        .expect("commit");
    assert_eq!(session.budget(), ResourceBudget::default());
    assert_eq!(cached_texts(&session), 0);
}

#[test]
fn legacy_initial_and_authored_reads_preserve_state_on_missing() {
    let root = TestDir::new("legacy-read-missing-state");
    let absolute = root.path().join("absolute.adoc");
    let authored = root.path().join("authored.adoc");
    fs::write(&absolute, "one").expect("absolute source");
    fs::write(&authored, "two").expect("authored source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let absolute_binding = session
        .read_utf8(source_id(), &absolute)
        .expect("absolute read")
        .binding()
        .clone();
    let authored_binding = session
        .read_target_utf8(source_id(), root.path(), "authored.adoc")
        .expect("authored read")
        .binding()
        .clone();
    fs::remove_file(&absolute).expect("remove absolute source");
    fs::remove_file(&authored).expect("remove authored source");

    assert_eq!(
        session.read_utf8(source_id(), &absolute),
        Err(ResourceError::Missing(absolute))
    );
    assert_eq!(
        session.read_target_utf8(source_id(), root.path(), "authored.adoc"),
        Err(ResourceError::Missing(authored))
    );
    assert_eq!((session.budget().files(), session.budget().bytes()), (2, 6));

    let mut release = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        release
            .release_binding(&absolute_binding)
            .expect("absolute binding"),
        FilesystemReleaseOutcome::Released
    );
    assert_eq!(
        release
            .release_binding(&authored_binding)
            .expect("authored binding"),
        FilesystemReleaseOutcome::Released
    );
    release
        .prepare_commit(&mut session)
        .expect("prepare release")
        .commit()
        .expect("commit");
    assert_eq!(session.budget(), ResourceBudget::default());
}

#[cfg(target_os = "linux")]
#[test]
fn not_found_releases_cached_text_only_after_the_last_alias() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("not-found-alias-cache");
    let source = root.path().join("source.adoc");
    let alias = root.path().join("alias.adoc");
    fs::write(&source, "text").expect("source");
    symlink("source.adoc", &alias).expect("alias");
    let mut session = policy(root.path(), 100).session().expect("session");
    session
        .read_utf8(source_id(), &source)
        .expect("source read");
    session.read_utf8(source_id(), &alias).expect("alias read");
    session
        .reread_utf8(source_id(), &source)
        .expect("cache source");
    assert_eq!(cached_texts(&session), 1);

    fs::remove_file(&alias).expect("remove alias");
    assert!(matches!(
        session.reread_utf8_outcome(source_id(), &alias),
        Ok(FilesystemReadOutcome::NotFound { .. })
    ));
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));
    assert_eq!(cached_texts(&session), 1);

    fs::remove_file(&source).expect("remove source");
    assert!(matches!(
        session.reread_utf8_outcome(source_id(), &source),
        Ok(FilesystemReadOutcome::NotFound { .. })
    ));
    assert_eq!(session.budget(), ResourceBudget::default());
    assert_eq!(cached_texts(&session), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn not_found_releases_each_root_sessions_unaliased_cache() {
    use std::os::unix::fs::symlink;

    let outer = TestDir::new("not-found-cross-root-cache");
    let nested = outer.path().join("nested");
    fs::create_dir(&nested).expect("nested root");
    let source = nested.join("source.adoc");
    let alias = outer.path().join("alias.adoc");
    fs::write(&source, "text").expect("source");
    symlink("nested/source.adoc", &alias).expect("outer alias");
    let policy = LocalFilesystemPolicy::new(
        [outer.path().to_owned(), nested],
        FilesystemReadLimits::default(),
    )
    .expect("policy");
    let mut session = policy.session().expect("session");
    session.read_utf8(source_id(), &alias).expect("alias read");
    session
        .read_utf8(source_id(), &source)
        .expect("source read");
    session
        .reread_utf8(source_id(), &alias)
        .expect("cache alias session");
    session
        .reread_utf8(source_id(), &source)
        .expect("cache source session");
    assert_eq!(cached_texts(&session), 2);

    fs::remove_file(&alias).expect("remove alias");
    assert!(matches!(
        session.reread_utf8_outcome(source_id(), &alias),
        Ok(FilesystemReadOutcome::NotFound { .. })
    ));
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));
    assert_eq!(cached_texts(&session), 1);

    fs::remove_file(&source).expect("remove source");
    assert!(matches!(
        session.reread_utf8_outcome(source_id(), &source),
        Ok(FilesystemReadOutcome::NotFound { .. })
    ));
    assert_eq!(session.budget(), ResourceBudget::default());
    assert_eq!(cached_texts(&session), 0);
}

#[test]
fn live_session_reads_invalidate_an_active_draft() {
    let root = TestDir::new("live-read-invalidates-draft");
    let path = root.path().join("source.adoc");
    fs::write(&path, "a").expect("source");

    let mut read = policy(root.path(), 100).session().expect("session");
    let draft = read.draft(&unbounded_job()).expect("draft before read");
    read.read_utf8(source_id(), &path).expect("live read");
    assert!(matches!(
        read.draft(&unbounded_job()),
        Err(FilesystemDraftError::DraftBusy)
    ));
    assert!(matches!(
        draft.prepare_commit(&mut read),
        Err(FilesystemDraftError::InvalidDraft)
    ));
    drop(
        read.draft(&unbounded_job())
            .expect("invalid draft released its lease"),
    );

    let mut reread = policy(root.path(), 100).session().expect("session");
    reread.read_utf8(source_id(), &path).expect("initial read");
    let draft = reread.draft(&unbounded_job()).expect("draft before reread");
    fs::write(&path, "bb").expect("replacement");
    reread.reread_utf8(source_id(), &path).expect("live reread");
    assert!(matches!(
        draft.prepare_commit(&mut reread),
        Err(FilesystemDraftError::InvalidDraft)
    ));
    assert_eq!(reread.budget().bytes(), 2);
}

#[test]
fn draft_rejects_a_foreign_session_and_exhausted_revision() {
    let first_root = TestDir::new("draft-first-session");
    let second_root = TestDir::new("draft-second-session");
    let mut first = policy(first_root.path(), 100)
        .session()
        .expect("first session");
    let mut second = policy(second_root.path(), 100)
        .session()
        .expect("second session");

    let draft = first.draft(&unbounded_job()).expect("draft");
    assert!(matches!(
        draft.prepare_commit(&mut second),
        Err(FilesystemDraftError::InvalidDraft)
    ));
    drop(
        first
            .draft(&unbounded_job())
            .expect("foreign prepare released the lease"),
    );

    first.revision = u64::MAX;
    assert!(matches!(
        first.draft(&unbounded_job()),
        Err(FilesystemDraftError::SessionRevisionExhausted)
    ));
    assert_eq!(first.active_draft.load(Ordering::Acquire), 0);
}

#[test]
fn dropping_a_prepared_commit_preserves_live_state_and_releases_lease() {
    let root = TestDir::new("prepared-commit-drop");
    let path = root.path().join("source.adoc");
    fs::write(&path, "a").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    session.read_utf8(source_id(), &path).expect("initial read");
    fs::write(&path, "bb").expect("replacement");

    let mut draft = session.draft(&unbounded_job()).expect("draft");
    draft.reread_utf8(source_id(), &path).expect("draft read");
    let prepared = draft.prepare_commit(&mut session).expect("prepare");
    drop(prepared);

    assert_eq!(session.budget().bytes(), 1);
    drop(
        session
            .draft(&unbounded_job())
            .expect("prepared drop released lease"),
    );
}

#[test]
fn stale_binding_from_an_older_committed_generation_cannot_release_replacement() {
    let root = TestDir::new("filesystem-draft-stale-binding");
    let path = root.path().join("source.adoc");
    fs::write(&path, "a").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let mut first = session.draft(&unbounded_job()).expect("first draft");
    let first_binding = first
        .read_utf8(source_id(), &path)
        .expect("first read")
        .binding()
        .clone();
    first
        .prepare_commit(&mut session)
        .expect("prepare first commit")
        .commit()
        .expect("commit");

    fs::write(&path, "bb").expect("replacement");
    let mut second = session.draft(&unbounded_job()).expect("second draft");
    let second_binding = second
        .reread_utf8(source_id(), &path)
        .expect("second read")
        .binding()
        .clone();
    second
        .prepare_commit(&mut session)
        .expect("prepare second commit")
        .commit()
        .expect("commit");

    let mut release = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        release
            .release_binding(&first_binding)
            .expect("stale release"),
        FilesystemReleaseOutcome::Stale
    );
    assert_eq!(
        release
            .release_binding(&second_binding)
            .expect("current release"),
        FilesystemReleaseOutcome::Released
    );
    release
        .prepare_commit(&mut session)
        .expect("prepare release commit")
        .commit()
        .expect("commit");
    assert_eq!(session.budget(), ResourceBudget::default());
}

#[test]
fn binding_from_a_dropped_draft_cannot_release_a_later_commit() {
    let root = TestDir::new("dropped-draft-binding");
    let path = root.path().join("source.adoc");
    fs::write(&path, "a").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");

    let mut discarded = session.draft(&unbounded_job()).expect("discarded draft");
    let stale = discarded
        .read_utf8(source_id(), &path)
        .expect("discarded read")
        .binding()
        .clone();
    drop(discarded);

    let mut committed = session.draft(&unbounded_job()).expect("committed draft");
    committed
        .read_utf8(source_id(), &path)
        .expect("committed read");
    committed
        .prepare_commit(&mut session)
        .expect("prepare commit")
        .commit()
        .expect("commit");

    let mut release = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        release.release_binding(&stale).expect("stale release"),
        FilesystemReleaseOutcome::Stale
    );
}

#[test]
fn exhausted_binding_generation_rejects_read_before_io() {
    let root = TestDir::new("binding-generation-exhausted");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let session = policy(root.path(), 100).session().expect("session");
    session
        .next_binding_generation
        .store(u64::MAX, Ordering::Relaxed);
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let before_reads = draft.candidate().sessions[0].read_files();
    let before_inspections = draft.candidate().sessions[0].inspected_paths();

    assert_eq!(
        draft.read_utf8(source_id(), &path),
        Err(FilesystemDraftError::BindingGenerationExhausted)
    );
    assert_eq!(draft.candidate().sessions[0].read_files(), before_reads);
    assert_eq!(
        draft.candidate().sessions[0].inspected_paths(),
        before_inspections
    );
}

#[test]
fn a_failed_read_keeps_the_bytes_it_already_obtained() {
    let root = TestDir::new("meter-invalid-utf8");
    let path = root.path().join("source.adoc");
    fs::write(&path, [0xff, 0xfe, 0xfd]).expect("invalid UTF-8 source");
    let session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);

    let result = draft.read_utf8(source_id(), &path);

    assert_eq!(
        result,
        Err(FilesystemDraftError::Resource(ResourceError::InvalidUtf8 {
            path,
            source: "input is not valid UTF-8".to_owned(),
        }))
    );
    assert_eq!(
        meter.usage(),
        FilesystemIoUsage {
            read_operations: 1,
            read_bytes: 3,
            directory_read_operations: 0,
            directory_entries: 0,
        }
    );
}

#[test]
fn a_missing_resource_counts_an_attempt_and_keeps_the_draft_usable() {
    let root = TestDir::new("meter-missing-then-usable");
    let missing = root.path().join("missing.adoc");
    let existing = root.path().join("existing.adoc");
    fs::write(&existing, "text").expect("existing source");
    let session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);

    assert_eq!(
        draft.read_utf8_outcome(source_id(), &missing),
        Ok(FilesystemReadOutcome::NotFound {
            source_id: source_id(),
            candidate_path: missing,
        })
    );
    let after_failure = meter.usage();
    assert_eq!(after_failure.read_operations, 1);
    assert_eq!(after_failure.read_bytes, 0);

    assert!(matches!(
        draft.read_utf8_outcome(source_id(), &existing),
        Ok(FilesystemReadOutcome::Found(_))
    ));
    assert_eq!(meter.usage().read_operations, 2);
}

#[test]
fn a_poisoned_draft_starts_no_filesystem_work_at_all() {
    let root = TestDir::new("meter-poisoned-draft-entry-points");
    let existing = root.path().join("existing.adoc");
    fs::write(&existing, "text").expect("existing source");
    let session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);
    assert!(draft.read_utf8(source_id(), root.path()).is_err());
    let after_failure = meter.usage();

    assert_eq!(
        draft.scan_utf8(path_source_id),
        Err(FilesystemDraftError::PoisonedDraft)
    );
    assert_eq!(
        draft.reread_utf8(source_id(), &existing),
        Err(FilesystemDraftError::PoisonedDraft)
    );
    assert_eq!(
        draft.read_target_utf8(source_id(), root.path(), "existing.adoc"),
        Err(FilesystemDraftError::PoisonedDraft)
    );
    assert_eq!(
        draft
            .discover_adoc_paths_with_control(|_, _| false, || false)
            .err(),
        Some(FilesystemDraftError::PoisonedDraft)
    );
    assert_eq!(meter.usage(), after_failure);
}

#[test]
fn a_capacity_rejection_counts_an_attempt_without_bytes() {
    let root = TestDir::new("meter-capacity-rejection");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let session = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 0,
            max_total_bytes: 100,
            max_resource_bytes: 100,
        },
    )
    .expect("policy")
    .session()
    .expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);

    assert_eq!(
        draft.read_utf8(source_id(), &path),
        Err(FilesystemDraftError::Resource(ResourceError::FileLimit {
            limit: 0
        }))
    );
    assert_eq!(meter.usage().read_operations, 1);
    assert_eq!(meter.usage().read_bytes, 0);
}

#[test]
fn a_cached_read_counts_the_request_without_reading_bytes() {
    let root = TestDir::new("meter-cache-hit");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);
    draft
        .mutation_cursor()
        .read_utf8_with(source_id(), &path, true, || {})
        .expect("cache miss");
    let after_miss = meter.usage();

    draft
        .mutation_cursor()
        .read_utf8_with(source_id(), &path, true, || {})
        .expect("cache hit");

    assert_eq!(after_miss.read_operations, 1);
    assert_eq!(after_miss.read_bytes, 4);
    let hit = meter.usage().since(after_miss);
    assert_eq!(hit.read_operations, 1);
    assert_eq!(hit.read_bytes, 0);
}

#[test]
fn a_discarded_draft_leaves_its_work_counted_in_the_session() {
    let root = TestDir::new("meter-shared-with-session");
    let path = root.path().join("source.adoc");
    fs::write(&path, "abc").expect("source");
    let session = policy(root.path(), 100).session().expect("session");
    let session_meter = session.state.meter.clone();
    let mut draft = session.draft(&unbounded_job()).expect("draft");

    draft.read_utf8(source_id(), &path).expect("read");
    drop(draft);

    assert_eq!(session_meter.usage().read_operations, 1);
    assert_eq!(session_meter.usage().read_bytes, 3);
}

#[test]
fn legacy_read_maps_binding_exhaustion_without_starting_io() {
    let root = TestDir::new("legacy-binding-generation-exhausted");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    session
        .next_binding_generation
        .store(u64::MAX, Ordering::Relaxed);
    let before_reads = session.state.sessions[0].read_files();
    let before_inspections = session.state.sessions[0].inspected_paths();

    assert_eq!(
        session.read_utf8(source_id(), &path),
        Err(ResourceError::Unverifiable(
            "filesystem binding generation space is exhausted".to_owned()
        ))
    );
    assert_eq!(session.state.sessions[0].read_files(), before_reads);
    assert_eq!(
        session.state.sessions[0].inspected_paths(),
        before_inspections
    );
}

#[test]
fn a_discarded_reread_leaves_the_original_binding_current() {
    let root = TestDir::new("discarded-reread-binding");
    let path = root.path().join("source.adoc");
    fs::write(&path, "a").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let original = session.read_utf8(source_id(), &path).expect("initial read");
    fs::write(&path, "bb").expect("replacement");

    let mut discarded = session.draft(&unbounded_job()).expect("reread draft");
    discarded
        .reread_utf8(source_id(), &path)
        .expect("reread into the draft");
    drop(discarded);

    let mut release = session.draft(&unbounded_job()).expect("release draft");
    assert_eq!(
        release
            .release_binding(original.binding())
            .expect("original binding remains current"),
        FilesystemReleaseOutcome::Released
    );
    release
        .prepare_commit(&mut session)
        .expect("prepare release")
        .commit()
        .expect("commit");
    assert_eq!(session.budget(), ResourceBudget::default());
}

#[test]
fn loaded_source_value_equality_ignores_lifecycle_bindings() {
    let root = TestDir::new("loaded-source-value-equality");
    let path = root.path().join("source.adoc");
    fs::write(&path, "text").expect("source");
    let mut first = policy(root.path(), 100).session().expect("first session");
    let mut second = policy(root.path(), 100).session().expect("second session");

    let first_loaded = first.read_utf8(source_id(), &path).expect("first read");
    let second_loaded = second.read_utf8(source_id(), &path).expect("second read");

    assert_ne!(first_loaded.binding(), second_loaded.binding());
    assert_eq!(first_loaded, second_loaded);
    let expected_binding = first_loaded.binding().clone();
    let (logical_id, source, binding) = first_loaded.into_parts_with_binding();
    assert_eq!(logical_id, source_id());
    assert_eq!(source.as_ref(), "text");
    assert_eq!(binding, expected_binding);
}

fn path_source_id(path: &Path) -> Result<LogicalSourceId, ResourceError> {
    LogicalSourceId::new(format!(
        "logical:{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ResourceError::Unverifiable(
                "test path has no UTF-8 file name".to_owned()
            ))?
    ))
}

#[test]
fn scan_is_deterministic_and_keeps_paths_out_of_logical_ids() {
    let root = TestDir::new("scan");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("nested directory");
    fs::write(root.path().join("b.adoc"), "second\n").expect("second source");
    fs::write(nested.join("a.adoc"), "first\n").expect("first source");
    fs::write(root.path().join("ignored.txt"), "ignored\n").expect("ignored source");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned(), root.path().to_owned()],
        FilesystemReadLimits::default(),
    )
    .expect("policy");
    let mut session = policy.session().expect("session");

    let loaded = session.scan_utf8(path_source_id).expect("scan");

    assert_eq!(loaded.len(), 2);
    assert_eq!(
        loaded
            .iter()
            .map(|source| source.source_id().as_str())
            .collect::<Vec<_>>(),
        ["logical:b.adoc", "logical:a.adoc"]
    );
    assert_eq!(
        loaded
            .iter()
            .map(LoadedFilesystemSource::source)
            .collect::<Vec<_>>(),
        ["second\n", "first\n"]
    );
    assert!(loaded.iter().all(|source| {
        !source
            .source_id()
            .as_str()
            .contains(root.path().to_string_lossy().as_ref())
    }));
    assert_eq!(
        (session.budget().files(), session.budget().bytes()),
        (2, 13)
    );
}

#[test]
fn adding_roots_over_the_policy_limit_is_transactional() {
    let parent = TestDir::new("root-policy-limit");
    let initial = parent.path().join("root-000");
    fs::create_dir(&initial).expect("initial root");
    let mut policy = LocalFilesystemPolicy::new([initial], FilesystemReadLimits::default())
        .expect("initial policy");
    let anchor = policy.roots()[0].clone();
    let mut additions = Vec::new();
    for index in 1..MAX_FILESYSTEM_POLICY_ROOTS {
        let root = parent.path().join(format!("root-{index:03}"));
        fs::create_dir(&root).expect("additional root");
        additions.push(root);
    }
    policy
        .access_derived(
            &anchor,
            DerivedFilesystemRoots {
                confined: Vec::new(),
                independent: additions,
            },
            FilesystemReadLimits::default(),
        )
        .expect("fill policy root limit");
    let before = policy.roots().to_vec();
    let rejected = parent.path().join("root-over-limit");
    fs::create_dir(&rejected).expect("rejected root");

    assert_eq!(
        policy
            .access_derived(
                &anchor,
                DerivedFilesystemRoots {
                    confined: Vec::new(),
                    independent: vec![rejected.clone()],
                },
                FilesystemReadLimits::default(),
            )
            .expect_err("root limit"),
        ResourceError::RootLimit {
            limit: MAX_FILESYSTEM_POLICY_ROOTS,
        }
    );
    assert_eq!(policy.roots(), before);
    assert!(before.iter().all(|root| policy.root_policy(root).is_some()));
    let duplicate = policy
        .access_derived(
            &anchor,
            DerivedFilesystemRoots {
                confined: Vec::new(),
                independent: vec![before[0].clone()],
            },
            FilesystemReadLimits::default(),
        )
        .expect("duplicate root at the limit");
    assert_eq!(duplicate.roots(), [before[0].clone()]);
    assert_eq!(policy.roots(), before);
    drop(policy);

    let mut staged = LocalFilesystemPolicy::new(
        before[..MAX_FILESYSTEM_POLICY_ROOTS - 1].iter().cloned(),
        FilesystemReadLimits::default(),
    )
    .expect("policy below the limit");
    let staged_before = staged.roots().to_vec();
    let staged_anchor = staged_before[0].clone();
    assert_eq!(
        staged
            .access_derived(
                &staged_anchor,
                DerivedFilesystemRoots {
                    confined: Vec::new(),
                    independent: vec![
                        before[MAX_FILESYSTEM_POLICY_ROOTS - 1].clone(),
                        rejected.clone(),
                    ],
                },
                FilesystemReadLimits::default(),
            )
            .expect_err("staged roots exceed the limit"),
        ResourceError::RootLimit {
            limit: MAX_FILESYSTEM_POLICY_ROOTS,
        }
    );
    assert_eq!(staged.roots(), staged_before);
    assert!(
        staged
            .root_policy(&before[MAX_FILESYSTEM_POLICY_ROOTS - 1])
            .is_none()
    );
    drop(staged);

    assert_eq!(
        LocalFilesystemPolicy::new(
            before.into_iter().chain([rejected]),
            FilesystemReadLimits::default(),
        )
        .expect_err("constructor root limit"),
        ResourceError::RootLimit {
            limit: MAX_FILESYSTEM_POLICY_ROOTS,
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn policy_session_keeps_the_root_opened_at_policy_construction() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("policy-root-swap");
    let outside = TestDir::new("policy-root-swap-outside");
    let candidate = root.path().join("root.adoc");
    fs::write(&candidate, "inside").expect("inside source");
    fs::write(outside.path().join("root.adoc"), "outside").expect("outside source");
    let policy = policy(root.path(), 100);
    let displaced = root.path().with_extension("anchored");
    fs::rename(root.path(), &displaced).expect("displace trusted root");
    symlink(outside.path(), root.path()).expect("replace root path");

    let loaded = policy
        .session()
        .expect("session")
        .read_utf8(source_id(), &candidate)
        .expect("read from retained policy root");

    assert_eq!(loaded.source(), "inside");
    assert_ne!(loaded.source(), "outside");
    fs::remove_file(root.path()).expect("remove replacement symlink");
    fs::rename(displaced, root.path()).expect("restore trusted root");
}

#[test]
fn derived_session_cannot_expand_policy_limits() {
    let root = TestDir::new("derived-session-limits");
    let policy = policy(root.path(), 10);
    let root_path = policy.roots()[0].clone();

    for limits in [
        FilesystemReadLimits {
            max_files: 11,
            max_total_bytes: 100,
            max_resource_bytes: 10,
        },
        FilesystemReadLimits {
            max_files: 10,
            max_total_bytes: 101,
            max_resource_bytes: 10,
        },
        FilesystemReadLimits {
            max_files: 10,
            max_total_bytes: 100,
            max_resource_bytes: 11,
        },
    ] {
        assert!(matches!(
            policy.access_existing([root_path.clone()], limits),
            Err(ResourceError::Unverifiable(reason))
                if reason == "filesystem access limits exceed the authority limits"
        ));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn confined_root_derivation_keeps_the_anchor_namespace() {
    let directory = TestDir::new("derived-root-authority");
    let root = directory.path().join("workspace");
    let nested = root.join("docs");
    fs::create_dir_all(&nested).expect("trusted nested root");
    fs::write(nested.join("document.adoc"), "trusted").expect("trusted document");
    let mut policy = LocalFilesystemPolicy::new([root.clone()], FilesystemReadLimits::default())
        .expect("filesystem policy");
    let anchor = policy.roots()[0].clone();

    let moved = directory.path().join("moved-workspace");
    fs::rename(&root, &moved).expect("move trusted workspace");
    fs::create_dir_all(root.join("docs")).expect("replacement nested root");
    fs::write(root.join("docs/document.adoc"), "replacement").expect("replacement document");

    let access = policy
        .access_derived(
            &anchor,
            DerivedFilesystemRoots {
                confined: vec![nested.clone()],
                independent: Vec::new(),
            },
            FilesystemReadLimits::default(),
        )
        .expect("derive nested authority");
    let mut session = access.session().expect("derived session");
    let loaded = session
        .read_utf8(
            LogicalSourceId::new("document").expect("source id"),
            &nested.join("document.adoc"),
        )
        .expect("read through retained namespace");

    assert_eq!(loaded.source(), "trusted");
}

#[cfg(target_os = "linux")]
#[test]
fn scan_enumerates_the_retained_root_after_namespace_replacement() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("scan-root-swap");
    let outside = TestDir::new("scan-root-swap-outside");
    fs::write(root.path().join("inside.adoc"), "inside").expect("inside source");
    fs::write(outside.path().join("outside.adoc"), "outside").expect("outside source");
    let policy = policy(root.path(), 100);
    let displaced = root.path().with_extension("anchored");
    fs::rename(root.path(), &displaced).expect("displace trusted root");
    symlink(outside.path(), root.path()).expect("replace root path");

    let loaded = policy
        .session()
        .expect("session")
        .scan_utf8(path_source_id)
        .expect("scan retained root");

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].source(), "inside");
    assert_eq!(loaded[0].source_id().as_str(), "logical:inside.adoc");
    fs::remove_file(root.path()).expect("remove replacement symlink");
    fs::rename(displaced, root.path()).expect("restore trusted root");
}

#[cfg(unix)]
#[test]
fn scan_does_not_follow_symlinked_files_or_directories() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("scan-symlink-root");
    let outside = TestDir::new("scan-symlink-outside");
    let outside_file = outside.path().join("outside.adoc");
    fs::write(&outside_file, "outside\n").expect("outside source");
    symlink(&outside_file, root.path().join("file.adoc")).expect("file symlink");
    symlink(outside.path(), root.path().join("directory")).expect("directory symlink");
    fs::write(root.path().join("inside.adoc"), "inside\n").expect("inside source");
    let mut session = policy(root.path(), 100).session().expect("session");

    let loaded = session.scan_utf8(path_source_id).expect("scan");

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].source_id().as_str(), "logical:inside.adoc");
    assert_eq!(loaded[0].source(), "inside\n");
}

#[test]
fn scan_applies_candidate_and_shared_byte_budgets_in_the_host_session() {
    let root = TestDir::new("scan-budget");
    fs::write(root.path().join("a.adoc"), "1234").expect("first source");
    fs::write(root.path().join("b.adoc"), "5678").expect("second source");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 1,
            max_total_bytes: 8,
            max_resource_bytes: 4,
        },
    )
    .expect("policy");
    assert_eq!(
        policy.session().expect("session").scan_utf8(path_source_id),
        Err(ResourceError::FileLimit { limit: 1 })
    );

    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 2,
            max_total_bytes: 7,
            max_resource_bytes: 4,
        },
    )
    .expect("policy");
    assert_eq!(
        policy.session().expect("session").scan_utf8(path_source_id),
        Err(ResourceError::ByteLimit)
    );
}

#[test]
fn directory_pruning_happens_before_the_scan_entry_limit() {
    let root = TestDir::new("scan-pruning-limit");
    let excluded = root.path().join("excluded");
    fs::create_dir(&excluded).expect("excluded directory");
    for name in ["one", "two", "three"] {
        fs::write(excluded.join(name), "ignored").expect("excluded entry");
    }
    fs::write(root.path().join("kept.adoc"), "kept\n").expect("kept source");
    let session = policy(root.path(), 100).session().expect("session");

    assert_eq!(
        session.discover_adoc_paths_with_limit(2, |_, _| false, || false),
        Err(ResourceError::ScanEntryLimit { limit: 2 })
    );
    assert_eq!(
        session
            .discover_adoc_paths_with_limit(
                2,
                |scan_root, relative| {
                    assert_eq!(scan_root, root.path());
                    relative == Path::new("excluded")
                },
                || false,
            )
            .expect("pruned discovery"),
        [root.path().join("kept.adoc")]
    );
}

#[test]
fn pruned_directory_itself_still_counts_toward_the_scan_limit() {
    let root = TestDir::new("scan-pruned-directory-boundary");
    fs::create_dir(root.path().join("excluded")).expect("excluded directory");
    let session = policy(root.path(), 100).session().expect("session");

    assert_eq!(
        session.discover_adoc_paths_with_limit(0, |_, _| true, || false),
        Err(ResourceError::ScanEntryLimit { limit: 0 })
    );
    assert!(
        session
            .discover_adoc_paths_with_limit(
                1,
                |_, relative| relative == Path::new("excluded"),
                || false,
            )
            .expect("boundary discovery")
            .is_empty()
    );
}

#[test]
fn cancelled_discovery_never_returns_a_partial_candidate_set() {
    let root = TestDir::new("scan-cancelled");
    fs::write(root.path().join("first.adoc"), "first\n").expect("first source");
    fs::write(root.path().join("second.adoc"), "second\n").expect("second source");
    let session = policy(root.path(), 100).session().expect("session");
    let checks = std::cell::Cell::new(0_usize);

    let result = session.discover_adoc_paths_with_control(
        |_, _| false,
        || {
            checks.set(checks.get() + 1);
            checks.get() > 2
        },
    );

    assert_eq!(
        result,
        Err(ResourceError::Unverifiable(
            "local filesystem scan was cancelled".to_owned()
        ))
    );
}

#[test]
fn a_cancelled_discovery_keeps_the_directory_work_it_performed() {
    let root = TestDir::new("meter-scan-cancellation");
    fs::write(root.path().join("a.adoc"), "a").expect("first source");
    fs::write(root.path().join("b.adoc"), "b").expect("second source");
    let session = policy(root.path(), 100).session().expect("session");
    let meter = session.state.meter.clone();
    let checks = std::cell::Cell::new(0_usize);

    let result = LocalFilesystemView {
        state: &session.state,
        job: None,
    }
    .discover_adoc_paths_with_control(
        LocalFilesystemSession::MAX_SCAN_ENTRIES,
        |_, _| false,
        || {
            checks.set(checks.get() + 1);
            checks.get() > 1
        },
    );

    assert_eq!(
        result,
        Err(ResourceError::Unverifiable(
            "local filesystem scan was cancelled".to_owned()
        ))
    );
    assert_eq!(
        meter.usage(),
        FilesystemIoUsage {
            read_operations: 0,
            read_bytes: 0,
            directory_read_operations: 1,
            directory_entries: 0,
        }
    );
}

/// A directory that disappears between being queued and being opened fails
/// the scan on every platform. The failed enumeration is still counted,
/// because the work of attempting it was performed. Each implementation's
/// stable error category and the affected path are asserted separately.
#[test]
fn a_failed_directory_enumeration_still_counts_the_attempt() {
    let root = TestDir::new("meter-directory-enumeration-failure");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("nested directory");
    fs::write(nested.join("a.adoc"), "abc").expect("source");
    let session = policy(root.path(), 100).session().expect("session");
    let meter = session.state.meter.clone();

    let result = LocalFilesystemView {
        state: &session.state,
        job: None,
    }
    .discover_adoc_paths_with_control(
        LocalFilesystemSession::MAX_SCAN_ENTRIES,
        |scan_root, relative| {
            assert_eq!(scan_root, root.path());
            assert_eq!(relative, Path::new("nested"));
            // The scan has queued `nested` and is about to enumerate it.
            fs::remove_dir_all(&nested).expect("remove the queued directory");
            false
        },
        || false,
    );

    #[cfg(target_os = "linux")]
    assert_eq!(result, Err(ResourceError::Missing(nested.clone())));
    #[cfg(not(target_os = "linux"))]
    assert!(
        matches!(
            &result,
            Err(ResourceError::Inspect { path, .. }) if path == &nested
        ),
        "a vanished directory must report an inspection failure for its path: {result:?}"
    );
    assert_eq!(meter.usage().directory_read_operations, 2);
}

/// Entries are counted as the iterator yields them, before anything decides
/// what they are. On Linux that includes `.` and `..`, which the scan then
/// skips, so the entry count is higher there than the four visible names.
#[test]
fn a_scan_counts_directory_entries_and_the_files_it_reads() {
    let root = TestDir::new("meter-scan-usage");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("nested directory");
    fs::write(root.path().join("a.adoc"), "abc").expect("first source");
    fs::write(root.path().join("ignored.txt"), "ignored").expect("ignored source");
    fs::write(nested.join("b.adoc"), "de").expect("second source");
    let session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);

    let loaded = draft.scan_utf8(path_source_id).expect("scan");

    assert_eq!(loaded.len(), 2);
    let usage = meter.usage();
    assert_eq!(usage.directory_read_operations, 2);
    #[cfg(target_os = "linux")]
    assert_eq!(usage.directory_entries, 8);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(usage.directory_entries, 4);
    assert_eq!(usage.read_operations, 2);
    assert_eq!(usage.read_bytes, 5);
}

#[test]
fn legacy_scan_keeps_a_disappearing_file_as_an_error() {
    let root = TestDir::new("scan-vanished-file");
    let path = root.path().join("vanished.adoc");
    fs::write(&path, "text").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let mut draft = session.draft(&unbounded_job()).expect("draft");
    let meter = draft_meter(&draft);

    let result = draft.scan_utf8(|candidate| {
        assert_eq!(candidate, path);
        fs::remove_file(candidate).expect("remove discovered source");
        path_source_id(candidate)
    });

    assert_eq!(
        result,
        Err(FilesystemDraftError::Resource(ResourceError::Missing(path)))
    );
    assert_eq!(draft.budget().files(), 0);
    assert_eq!(meter.usage().read_operations, 1);
    assert!(matches!(
        draft.prepare_commit(&mut session),
        Err(FilesystemDraftError::PoisonedDraft)
    ));
    assert_eq!(session.budget().files(), 0);
}

#[test]
fn source_ids_and_platform_capability_are_explicit() {
    assert!(matches!(
        LogicalSourceId::new(""),
        Err(ResourceError::InvalidSourceId)
    ));
    assert!(matches!(
        LogicalSourceId::new("bad\nid"),
        Err(ResourceError::InvalidSourceId)
    ));
    let root = TestDir::new("capability");
    let session = policy(root.path(), 100).session().expect("session");
    #[cfg(target_os = "linux")]
    assert_eq!(
        session.race_resistance(),
        FilesystemRaceResistance::HandleRelative
    );
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        session.race_resistance(),
        FilesystemRaceResistance::StaticSnapshotOnly
    );
    let mut session = policy(root.path(), 100).session().expect("session");
    assert!(matches!(
        session.read_utf8(source_id(), Path::new("relative.adoc")),
        Err(ResourceError::PathNotAbsolute(_))
    ));
}

#[test]
fn legacy_and_outcome_read_signatures_remain_distinct() {
    let _: fn(
        &mut LocalFilesystemSession,
        LogicalSourceId,
        &Path,
    ) -> Result<LoadedFilesystemSource, ResourceError> = LocalFilesystemSession::read_utf8;
    let _: fn(
        &mut LocalFilesystemSession,
        LogicalSourceId,
        &Path,
    ) -> Result<FilesystemReadOutcome, ResourceError> = LocalFilesystemSession::read_utf8_outcome;
    let _: fn(
        &mut LocalFilesystemDraft,
        LogicalSourceId,
        &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> = LocalFilesystemDraft::read_utf8;
    let _: fn(
        &mut LocalFilesystemDraft,
        LogicalSourceId,
        &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> =
        LocalFilesystemDraft::read_utf8_outcome;
    let _: fn(
        &mut LocalFilesystemSession,
        LogicalSourceId,
        &Path,
    ) -> Result<LoadedFilesystemSource, ResourceError> = LocalFilesystemSession::reread_utf8;
    let _: fn(
        &mut LocalFilesystemSession,
        LogicalSourceId,
        &Path,
    ) -> Result<FilesystemReadOutcome, ResourceError> = LocalFilesystemSession::reread_utf8_outcome;
    let _: fn(
        &mut LocalFilesystemSession,
        LogicalSourceId,
        &Path,
        &str,
    ) -> Result<LoadedFilesystemSource, ResourceError> = LocalFilesystemSession::read_target_utf8;
    let _: fn(
        &mut LocalFilesystemSession,
        LogicalSourceId,
        &Path,
        &str,
    ) -> Result<FilesystemReadOutcome, ResourceError> =
        LocalFilesystemSession::read_target_utf8_outcome;
    let _: fn(
        &mut LocalFilesystemDraft,
        LogicalSourceId,
        &Path,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> = LocalFilesystemDraft::reread_utf8;
    let _: fn(
        &mut LocalFilesystemDraft,
        LogicalSourceId,
        &Path,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> =
        LocalFilesystemDraft::reread_utf8_outcome;
    let _: fn(
        &mut LocalFilesystemDraft,
        LogicalSourceId,
        &Path,
        &str,
    ) -> Result<LoadedFilesystemSource, FilesystemDraftError> =
        LocalFilesystemDraft::read_target_utf8;
    let _: fn(
        &mut LocalFilesystemDraft,
        LogicalSourceId,
        &Path,
        &str,
    ) -> Result<FilesystemReadOutcome, FilesystemDraftError> =
        LocalFilesystemDraft::read_target_utf8_outcome;
}

#[test]
fn failed_global_budget_charge_is_not_bypassed_by_retrying_the_same_path() {
    let first_root = TestDir::new("file-budget-first");
    let second_root = TestDir::new("file-budget-second");
    let first = first_root.path().join("first.adoc");
    let second = second_root.path().join("second.adoc");
    fs::write(&first, "a").expect("first source");
    fs::write(&second, "b").expect("second source");
    let policy = LocalFilesystemPolicy::new(
        [first_root.path().to_owned(), second_root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 1,
            max_total_bytes: 2,
            max_resource_bytes: 2,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");
    session.read_utf8(source_id(), &first).expect("first read");
    for _ in 0..2 {
        assert_eq!(
            session.read_utf8(source_id(), &second),
            Err(ResourceError::FileLimit { limit: 1 })
        );
    }

    let first_root = TestDir::new("byte-budget-first");
    let second_root = TestDir::new("byte-budget-second");
    let first = first_root.path().join("first.adoc");
    let second = second_root.path().join("second.adoc");
    fs::write(&first, "ab").expect("first source");
    fs::write(&second, "cd").expect("second source");
    let policy = LocalFilesystemPolicy::new(
        [first_root.path().to_owned(), second_root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 2,
            max_total_bytes: 3,
            max_resource_bytes: 2,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");
    session.read_utf8(source_id(), &first).expect("first read");
    for _ in 0..2 {
        assert_eq!(
            session.read_utf8(source_id(), &second),
            Err(ResourceError::ByteLimit)
        );
    }
}

#[test]
fn reread_replaces_and_release_removes_charges_transactionally() {
    let root = TestDir::new("replacement-budget");
    let first = root.path().join("first.adoc");
    let second = root.path().join("second.adoc");
    fs::write(&first, "1234").expect("first source");
    fs::write(&second, "1234").expect("second source");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 2,
            max_total_bytes: 6,
            max_resource_bytes: 6,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");

    session
        .read_utf8(source_id(), &first)
        .expect("initial read");
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));

    fs::write(&first, "12").expect("shrink first");
    session
        .reread_utf8(source_id(), &first)
        .expect("shrunk reread");
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 2));

    session
        .read_utf8(source_id(), &second)
        .expect("second read");
    assert_eq!((session.budget().files(), session.budget().bytes()), (2, 6));

    fs::write(&first, "123").expect("grow first");
    assert_eq!(
        session.reread_utf8(source_id(), &first),
        Err(ResourceError::ByteLimit)
    );
    assert_eq!((session.budget().files(), session.budget().bytes()), (2, 6));

    fs::write(&second, "1").expect("shrink second");
    let second_binding = session
        .reread_utf8(source_id(), &second)
        .expect("shrunk second");
    session
        .reread_utf8(source_id(), &first)
        .expect("grown first");
    assert_eq!((session.budget().files(), session.budget().bytes()), (2, 4));

    fs::remove_file(&second).expect("delete second");
    release_binding(&mut session, second_binding.binding());
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
}

#[cfg(target_os = "linux")]
#[test]
fn release_uses_the_candidate_after_the_opened_file_is_renamed() {
    let root = TestDir::new("release-renamed-candidate");
    let candidate = root.path().join("source.adoc");
    let renamed = root.path().join("renamed.adoc");
    fs::write(&candidate, "text").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");

    let loaded = session
        .read_utf8_after_open(source_id(), &candidate, || {
            fs::rename(&candidate, &renamed).expect("rename opened source");
        })
        .expect("read renamed source");
    assert_eq!(loaded.canonical_path(), renamed);
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));

    release_binding(&mut session, loaded.binding());
    assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
}

#[cfg(target_os = "linux")]
#[test]
fn release_reclaims_a_nested_parent_component_alias() {
    let root = TestDir::new("release-parent-alias");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("nested directory");
    let source = root.path().join("source.adoc");
    let alias = nested.join("..").join("source.adoc");
    fs::write(&source, "text").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");

    let loaded = session.read_utf8(source_id(), &alias).expect("alias read");
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));
    release_binding(&mut session, loaded.binding());
    assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
}

#[cfg(target_os = "linux")]
#[test]
fn release_reclaims_a_symbolic_link_alias() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("release-symlink-alias");
    let source = root.path().join("source.adoc");
    let alias = root.path().join("alias.adoc");
    fs::write(&source, "text").expect("source");
    symlink("source.adoc", &alias).expect("source alias");
    let mut session = policy(root.path(), 100).session().expect("session");

    let loaded = session.read_utf8(source_id(), &alias).expect("alias read");
    release_binding(&mut session, loaded.binding());
    assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
}

#[cfg(target_os = "linux")]
#[test]
fn the_last_alias_release_reclaims_the_shared_file_limit() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("release-last-alias");
    let source = root.path().join("source.adoc");
    let first_alias = source.clone();
    let second_alias = root.path().join("alias.adoc");
    let replacement = root.path().join("replacement.adoc");
    fs::write(&source, "text").expect("source");
    fs::write(&replacement, "new").expect("replacement");
    symlink("source.adoc", &second_alias).expect("second alias");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 2,
            max_total_bytes: 8,
            max_resource_bytes: 4,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");

    let first_loaded = session
        .read_utf8(source_id(), &first_alias)
        .expect("first alias");
    let second_loaded = session
        .read_utf8(source_id(), &second_alias)
        .expect("second alias");
    assert_eq!(
        first_loaded.canonical_path(),
        second_loaded.canonical_path()
    );
    release_binding(&mut session, first_loaded.binding());
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 4));

    release_binding(&mut session, second_loaded.binding());
    assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
    session
        .read_utf8(source_id(), &replacement)
        .expect("released file slot");
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 3));
}

#[test]
fn rejected_new_reread_releases_only_its_candidate_inspection() {
    let root = TestDir::new("reread-candidate-rollback");
    let first = root.path().join("first.adoc");
    let second = root.path().join("second.adoc");
    let third = root.path().join("third.adoc");
    fs::write(&first, "a").expect("first source");
    fs::write(&second, "bb").expect("second source");
    fs::write(&third, "b").expect("third source");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 2,
            max_total_bytes: 2,
            max_resource_bytes: 2,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");

    session.read_utf8(source_id(), &first).expect("first read");
    let inspected = session.state.sessions[0].inspected_paths();
    assert_eq!(
        session.reread_utf8(source_id(), &second),
        Err(ResourceError::ByteLimit)
    );
    assert_eq!(session.state.sessions[0].inspected_paths(), inspected);
    session
        .reread_utf8(source_id(), &third)
        .expect("third read after rejected candidate");
    assert_eq!((session.budget().files(), session.budget().bytes()), (2, 2));
}

#[test]
fn shared_byte_capacity_accepts_the_boundary_across_roots() {
    let first_root = TestDir::new("shared-byte-boundary-first");
    let second_root = TestDir::new("shared-byte-boundary-second");
    let first = first_root.path().join("first.adoc");
    let second = second_root.path().join("second.adoc");
    let third = second_root.path().join("third.adoc");
    fs::write(&first, "12").expect("first source");
    fs::write(&second, "34").expect("second source");
    fs::write(&third, "5").expect("third source");
    let policy = LocalFilesystemPolicy::new(
        [first_root.path().to_owned(), second_root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 3,
            max_total_bytes: 4,
            max_resource_bytes: 4,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");

    session.read_utf8(source_id(), &first).expect("first read");
    session
        .read_utf8(source_id(), &second)
        .expect("boundary read");
    assert_eq!(
        session.read_utf8(source_id(), &third),
        Err(ResourceError::ByteLimit)
    );
    assert_eq!((session.budget().files(), session.budget().bytes()), (2, 4));
}

#[test]
fn replacement_receives_its_previous_charge_before_the_bounded_read() {
    let root = TestDir::new("replacement-capacity");
    let path = root.path().join("document.adoc");
    fs::write(&path, "1234").expect("initial source");
    let policy = LocalFilesystemPolicy::new(
        [root.path().to_owned()],
        FilesystemReadLimits {
            max_files: 1,
            max_total_bytes: 6,
            max_resource_bytes: 5,
        },
    )
    .expect("policy");
    let mut session = policy.session().expect("session");
    session.read_utf8(source_id(), &path).expect("initial read");

    fs::write(&path, "12345").expect("replacement source");
    session
        .reread_utf8(source_id(), &path)
        .expect("replacement uses released charge");
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 5));
    fs::write(&path, "123456").expect("oversized replacement");
    assert_eq!(
        session.reread_utf8(source_id(), &path),
        Err(ResourceError::ResourceTooLarge(path))
    );
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 5));
}

#[test]
fn budget_rejects_without_partially_charging() {
    let limits = FilesystemReadLimits {
        max_files: 1,
        max_total_bytes: 3,
        max_resource_bytes: 3,
    };
    let mut budget = ResourceBudget::default();
    budget.charge(Path::new("a"), 3, limits).expect("boundary");
    assert_eq!((budget.files(), budget.bytes()), (1, 3));
    assert_eq!(
        budget.charge(Path::new("b"), 1, limits),
        Err(ResourceError::FileLimit { limit: 1 })
    );
    assert_eq!((budget.files(), budget.bytes()), (1, 3));
}

#[test]
fn filesystem_session_identity_never_wraps() {
    assert_eq!(next_session_id(u64::MAX - 1), Some(u64::MAX));
    assert_eq!(next_session_id(u64::MAX), None);
}

#[test]
fn policy_rejects_files_outside_roots_and_directories() {
    let root = TestDir::new("root");
    let outside = TestDir::new("outside");
    let outside_file = outside.path().join("outside.adoc");
    fs::write(&outside_file, "outside").expect("write outside file");
    let mut session = policy(root.path(), 100).session().expect("session");

    assert!(matches!(
        session.read_utf8(source_id(), &outside_file),
        Err(ResourceError::OutsideRoots(_))
    ));
    assert!(matches!(
        session.read_utf8(source_id(), root.path()),
        Err(ResourceError::NotRegularFile(_))
    ));
}

#[cfg(unix)]
#[test]
fn policy_rejects_symlinks_that_escape_roots() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("symlink-root");
    let outside = TestDir::new("symlink-outside");
    let outside_file = outside.path().join("outside.adoc");
    fs::write(&outside_file, "outside").expect("write outside file");
    let link = root.path().join("escape.adoc");
    symlink(&outside_file, &link).expect("create symlink");

    assert!(matches!(
        policy(root.path(), 100)
            .session()
            .expect("session")
            .read_utf8(source_id(), &link),
        Err(ResourceError::OutsideRoots(_))
    ));
}

#[cfg(unix)]
#[test]
fn draft_policy_read_rejects_a_symlink_that_stays_inside_the_root() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("draft-policy-symlink");
    let target = root.path().join("target.toml");
    let link = root.path().join(".adocweave.toml");
    fs::write(&target, "schema-version = 1\n").expect("target");
    symlink(&target, &link).expect("policy symlink");
    let session = policy(root.path(), 100).session().expect("session");
    let job = unbounded_job();
    let mut draft = session.draft(&job).expect("draft");

    assert!(
        draft
            .read_utf8_no_symlinks_outcome(source_id(), &link)
            .is_err()
    );
    let usage = job.usage().expect("job usage");
    assert_eq!(usage.read_operations, 1);
    assert_eq!(usage.read_bytes, 0);
    assert_eq!(usage.candidate_changes, 0);
}

#[cfg(unix)]
#[test]
fn deepest_root_rejects_a_symlink_back_into_an_allowed_parent_root() {
    use std::os::unix::fs::symlink;

    let outer = TestDir::new("nested-outer");
    let inner = outer.path().join("inner");
    fs::create_dir(&inner).expect("inner root");
    let outer_file = outer.path().join("outer.adoc");
    fs::write(&outer_file, "outer").expect("outer file");
    let link = inner.join("escape.adoc");
    symlink(&outer_file, &link).expect("cross-boundary symlink");
    let policy = LocalFilesystemPolicy::new(
        [outer.path().to_owned(), inner],
        FilesystemReadLimits::default(),
    )
    .expect("policy");

    assert!(matches!(
        policy
            .session()
            .expect("session")
            .read_utf8(source_id(), &link),
        Err(ResourceError::OutsideRoots(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlinks_between_allowed_roots_remain_confined_to_the_selected_root() {
    use std::os::unix::fs::symlink;

    let first = TestDir::new("cross-root-first");
    let second = TestDir::new("cross-root-second");
    let second_file = second.path().join("second.adoc");
    fs::write(&second_file, "second").expect("second file");
    let link = first.path().join("escape.adoc");
    symlink(&second_file, &link).expect("cross-root symlink");
    let policy = LocalFilesystemPolicy::new(
        [first.path().to_owned(), second.path().to_owned()],
        FilesystemReadLimits::default(),
    )
    .expect("policy");

    assert!(matches!(
        policy
            .session()
            .expect("session")
            .read_utf8(source_id(), &link),
        Err(ResourceError::OutsideRoots(_))
    ));
}

#[test]
fn missing_and_permission_errors_keep_typed_identity() {
    let missing = PathBuf::from("missing.adoc");
    let denied = PathBuf::from("denied.adoc");
    assert_eq!(
        ResourceError::from(LocalTargetError::Missing(missing.clone())),
        ResourceError::Missing(missing)
    );
    assert_eq!(
        ResourceError::from(LocalTargetError::PermissionDenied(denied.clone())),
        ResourceError::PermissionDenied(denied)
    );
}

#[test]
fn policy_constructor_preserves_public_root_error_categories() {
    let root = TestDir::new("policy-constructor-errors");
    let file = root.path().join("file.adoc");
    let missing = root.path().join("missing");
    fs::write(&file, "text").expect("regular file");

    assert!(matches!(
        LocalFilesystemPolicy::new([missing], FilesystemReadLimits::default()),
        Err(ResourceError::Missing(_))
    ));
    assert!(matches!(
        LocalFilesystemPolicy::new([file], FilesystemReadLimits::default()),
        Err(ResourceError::InvalidRoot)
    ));
}

#[cfg(unix)]
#[test]
fn authored_target_read_keeps_the_opened_file_when_the_leaf_is_replaced() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("target-race-root");
    let outside = TestDir::new("target-race-outside");
    let candidate = root.path().join("part.adoc");
    let moved = root.path().join("opened.adoc");
    let outside_file = outside.path().join("outside.adoc");
    fs::write(&candidate, "inside").expect("inside file");
    fs::write(&outside_file, "outside").expect("outside file");
    let mut session = policy(root.path(), 100).session().expect("session");

    let loaded = session
        .read_target_utf8_after_open(source_id(), root.path(), "part.adoc", || {
            fs::rename(&candidate, &moved).expect("retain opened file");
            symlink(&outside_file, &candidate).expect("replace with outside symlink");
        })
        .expect("opened source remains valid");

    assert_eq!(loaded.source(), "inside");
    #[cfg(target_os = "linux")]
    assert_eq!(loaded.canonical_path(), moved);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(loaded.canonical_path(), candidate);
}

#[test]
fn validated_target_enforces_encoding_and_per_resource_limit() {
    let root = TestDir::new("read");
    let invalid = root.path().join("invalid.adoc");
    let oversized = root.path().join("oversized.adoc");
    fs::write(&invalid, [0xff]).expect("write invalid UTF-8");
    fs::write(&oversized, "1234").expect("write oversized file");
    let mut session = policy(root.path(), 3).session().expect("session");

    assert!(matches!(
        session.read_utf8(source_id(), &invalid),
        Err(ResourceError::InvalidUtf8 { .. })
    ));
    assert!(matches!(
        session.read_utf8(source_id(), &oversized),
        Err(ResourceError::ResourceTooLarge(_))
    ));
}

#[cfg(unix)]
#[test]
fn validated_target_owns_the_bytes_captured_before_a_path_is_replaced() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("captured-root");
    let outside = TestDir::new("captured-outside");
    let candidate = root.path().join("part.adoc");
    let outside_file = outside.path().join("outside.adoc");
    fs::write(&candidate, "inside").expect("inside source");
    fs::write(&outside_file, "outside").expect("outside source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let loaded = session
        .read_utf8(source_id(), &candidate)
        .expect("loaded target");

    fs::remove_file(&candidate).expect("replace candidate");
    symlink(&outside_file, &candidate).expect("outside symlink");

    assert_eq!(loaded.source(), "inside");
    assert_eq!(loaded.canonical_path(), candidate);
    assert_eq!(loaded.source_id().as_str(), "test-source");
}

#[cfg(target_os = "linux")]
#[test]
fn shared_session_keeps_the_opened_file_when_an_ancestor_is_replaced() {
    use std::os::unix::fs::symlink;

    let root = TestDir::new("session-race-root");
    let outside = TestDir::new("session-race-outside");
    let directory = root.path().join("parts");
    let moved = root.path().join("parts-opened");
    fs::create_dir(&directory).expect("inside directory");
    fs::write(directory.join("part.adoc"), "inside").expect("inside source");
    fs::write(outside.path().join("part.adoc"), "outside").expect("outside source");
    let mut session = policy(root.path(), 100).session().expect("session");

    let loaded = session
        .read_utf8_after_open(source_id(), &directory.join("part.adoc"), || {
            fs::rename(&directory, &moved).expect("move opened ancestor");
            symlink(outside.path(), &directory).expect("replace ancestor with symlink");
        })
        .expect("opened file remains readable");

    assert_eq!(loaded.source(), "inside");
    assert_ne!(loaded.source(), "outside");
}

#[cfg(target_os = "linux")]
#[test]
fn unlinked_file_does_not_charge_or_cache_a_literal_deleted_suffix_path() {
    let root = TestDir::new("deleted-suffix-budget");
    let candidate = root.path().join("part.adoc");
    let suffix = root.path().join("part.adoc (deleted)");
    fs::write(&candidate, "opened").expect("opened source");
    fs::write(&suffix, "literal suffix").expect("suffix source");
    let mut session = policy(root.path(), 100).session().expect("session");

    let error = session
        .read_utf8_after_open(source_id(), &candidate, || {
            fs::remove_file(&candidate).expect("unlink opened source");
        })
        .expect_err("unlinked identity must fail closed");

    assert!(matches!(error, ResourceError::Unverifiable(_)));
    assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));

    let loaded = session
        .read_utf8(source_id(), &suffix)
        .expect("literal suffix remains independently readable");
    assert_eq!(loaded.canonical_path(), suffix);
    assert_eq!(loaded.source(), "literal suffix");
    assert_eq!(
        (session.budget().files(), session.budget().bytes()),
        (1, 14)
    );
}

#[test]
fn drafts_share_job_usage_across_sessions_and_discarded_candidates() {
    let root = TestDir::new("job-shared-read-budget");
    let first_path = root.path().join("first.adoc");
    let second_path = root.path().join("second.adoc");
    fs::write(&first_path, "abc").expect("first source");
    fs::write(&second_path, "de").expect("second source");
    let policy = policy(root.path(), 100);
    let first_session = policy.session().expect("first session");
    let second_session = policy.session().expect("second session");
    let job = FilesystemJobCoordinator::new(job_limits(4, 100)).expect("job");

    let mut discarded = first_session.draft(&job).expect("first draft");
    discarded
        .read_utf8(source_id(), &first_path)
        .expect("first read");
    drop(discarded);

    let mut rejected = second_session.draft(&job).expect("second draft");
    assert_eq!(
        rejected.read_utf8(source_id(), &second_path),
        Err(FilesystemDraftError::Job(crate::FilesystemJobError::Limit(
            crate::FilesystemJobLimit::ReadBytes { limit: 4 }
        )))
    );
    assert_eq!(
        job.usage().expect("usage"),
        crate::FilesystemJobUsage {
            read_operations: 2,
            read_bytes: 4,
            read_probe_bytes: 1,
            candidate_changes: 1,
            sessions: 2,
            ..crate::FilesystemJobUsage::default()
        }
    );
}

#[test]
fn prepared_commit_cannot_publish_after_the_job_is_cancelled() {
    let root = TestDir::new("job-cancel-before-commit");
    let path = root.path().join("source.adoc");
    fs::write(&path, "source").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    let job = FilesystemJobCoordinator::new(job_limits(100, 100)).expect("job");
    let mut draft = session.draft(&job).expect("draft");
    draft.read_utf8(source_id(), &path).expect("read");
    let prepared = draft.prepare_commit(&mut session).expect("prepare");

    job.cancel().expect("cancel");
    assert_eq!(
        prepared.commit(),
        Err(FilesystemDraftError::Job(
            crate::FilesystemJobError::Cancelled
        ))
    );
    assert_eq!((session.budget().files(), session.budget().bytes()), (0, 0));
}

#[test]
fn missing_draft_read_keeps_the_job_operation_without_bytes() {
    let root = TestDir::new("job-missing-read");
    let session = policy(root.path(), 100).session().expect("session");
    let job = FilesystemJobCoordinator::new(job_limits(10, 10)).expect("job");
    let mut draft = session.draft(&job).expect("draft");

    assert!(matches!(
        draft
            .read_utf8_outcome(source_id(), &root.path().join("missing.adoc"))
            .expect("missing outcome"),
        FilesystemReadOutcome::NotFound { .. }
    ));
    assert_eq!(job.usage().expect("usage").read_operations, 1);
    assert_eq!(job.usage().expect("usage").read_bytes, 0);
}

#[test]
fn candidate_change_limit_rejects_not_found_without_changing_live_state() {
    let root = TestDir::new("job-candidate-change-limit");
    let path = root.path().join("source.adoc");
    fs::write(&path, "source").expect("source");
    let mut session = policy(root.path(), 100).session().expect("session");
    session
        .read_utf8(source_id(), &path)
        .expect("initial live read");
    fs::remove_file(&path).expect("remove source");
    let mut limits = job_limits(100, 100);
    limits.max_candidate_changes = 0;
    let job = FilesystemJobCoordinator::new(limits).expect("job");
    let mut draft = session.draft(&job).expect("draft");

    assert_eq!(
        draft.read_utf8_outcome(source_id(), &path),
        Err(FilesystemDraftError::Job(crate::FilesystemJobError::Limit(
            crate::FilesystemJobLimit::CandidateChanges { limit: 0 }
        )))
    );
    drop(draft);
    assert_eq!((session.budget().files(), session.budget().bytes()), (1, 6));
    assert_eq!(job.usage().expect("usage").candidate_changes, 0);
}

#[test]
fn directory_job_probe_distinguishes_exact_boundary_from_excess() {
    let root = TestDir::new("job-directory-boundary");
    fs::write(root.path().join("a.adoc"), "a").expect("first source");
    fs::write(root.path().join("b.adoc"), "b").expect("second source");
    let session = policy(root.path(), 100).session().expect("session");
    let exact_job = FilesystemJobCoordinator::new(job_limits(100, 2)).expect("exact job");
    let exact = session.draft(&exact_job).expect("exact draft");
    assert_eq!(
        exact
            .discover_adoc_paths_with_control(|_, _| false, || false)
            .expect("exact discovery")
            .len(),
        2
    );
    assert_eq!(exact_job.usage().expect("usage").directory_entries, 2);
    assert_eq!(exact_job.usage().expect("usage").directory_probe_entries, 0);
    drop(exact);

    fs::write(root.path().join("c.adoc"), "c").expect("third source");
    let excess_job = FilesystemJobCoordinator::new(job_limits(100, 2)).expect("excess job");
    let excess = session.draft(&excess_job).expect("excess draft");
    assert_eq!(
        excess.discover_adoc_paths_with_control(|_, _| false, || false),
        Err(FilesystemDraftError::Job(crate::FilesystemJobError::Limit(
            crate::FilesystemJobLimit::DirectoryEntries { limit: 2 }
        )))
    );
    assert_eq!(
        excess_job.usage().expect("usage").directory_probe_entries,
        1
    );
}

#[test]
fn directory_reservation_wait_observes_scan_cancellation() {
    let root = TestDir::new("job-directory-wait-cancellation");
    let session = policy(root.path(), 100).session().expect("session");
    let job = FilesystemJobCoordinator::new(job_limits(100, 1)).expect("job");
    let draft = session.draft(&job).expect("draft");
    let mut holder = job.begin_directory_read(session.id()).expect("holder");
    let reservation = holder.reserve_entry().expect("held reservation");
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker = std::thread::spawn(move || {
        draft.discover_adoc_paths_with_control(
            |_, _| false,
            || worker_cancelled.load(Ordering::Acquire),
        )
    });

    while job.usage().expect("usage").waiting_reservations == 0 {
        std::thread::yield_now();
    }
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        worker.join().expect("worker"),
        Err(FilesystemDraftError::Job(
            crate::FilesystemJobError::Cancelled
        ))
    );
    drop(reservation);
    drop(holder);
    assert_eq!(job.cancel(), Ok(()));
}
