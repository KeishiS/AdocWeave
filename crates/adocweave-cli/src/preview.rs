use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use adocweave::CancellationToken;

const MAX_REQUEST_BYTES: usize = 8192;
const CLIENT_JS: &str = r#"let generation=-1;
async function update(){
  try {
    const event=await fetch('/events',{cache:'no-store'}).then(r=>r.json());
    if(generation>=0&&event.generation!==generation){
      document.querySelector('iframe').contentWindow.location.reload();
    }
    if(generation<0||event.generation!==generation){
      document.querySelector('pre').textContent=await fetch('/diagnostics',{cache:'no-store'}).then(r=>r.text());
    }
    generation=event.generation;
  } catch (_) {}
}
setInterval(update,500); update();
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    pub bind: IpAddr,
    pub port: u16,
    pub debounce: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Build {
    pub html: String,
    pub diagnostics: String,
    dependencies: BTreeMap<PathBuf, Fingerprint>,
    style_origins: BTreeSet<String>,
}

impl Build {
    pub fn new(
        html: String,
        diagnostics: String,
        dependencies: BTreeMap<PathBuf, Fingerprint>,
    ) -> Self {
        Self {
            html,
            diagnostics,
            dependencies,
            style_origins: BTreeSet::new(),
        }
    }

    pub fn failure(message: String, dependencies: BTreeMap<PathBuf, Fingerprint>) -> Self {
        Self::new(
            error_document(&message),
            format!("{message}\n"),
            dependencies,
        )
    }

    pub fn with_style_origins(mut self, origins: BTreeSet<String>) -> Self {
        self.style_origins = origins;
        self
    }

    fn changed(&self) -> bool {
        self.dependencies
            .iter()
            .any(|(path, fingerprint)| Fingerprint::read(path) != *fingerprint)
    }
}

#[derive(Debug)]
pub enum Error {
    Bind {
        address: SocketAddr,
        source: io::Error,
    },
    Io(io::Error),
    Build(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind { address, source } => {
                write!(
                    formatter,
                    "could not bind preview server to {address}: {source}"
                )
            }
            Self::Io(source) => source.fmt(formatter),
            Self::Build(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    modified: Option<SystemTime>,
    len: u64,
    exists: bool,
    kind: FileKind,
    content_hash: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileKind {
    Missing,
    Regular,
    Symlink,
    Directory,
    Other,
    Unreadable,
}

impl Fingerprint {
    pub(crate) fn read(path: &Path) -> Self {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: FileKind::Symlink,
                content_hash: fs::read_link(path).ok().map(|target| hash(&target)),
            },
            Ok(metadata) if metadata.is_dir() => Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: FileKind::Directory,
                content_hash: None,
            },
            Ok(metadata) if metadata.is_file() => match File::open(path) {
                Ok(mut file) => {
                    let opened = file.metadata().unwrap_or(metadata);
                    let mut bytes = Vec::new();
                    let content_hash = file.read_to_end(&mut bytes).ok().map(|_| hash(&bytes));
                    Self {
                        modified: opened.modified().ok(),
                        len: opened.len(),
                        exists: true,
                        kind: if content_hash.is_some() {
                            FileKind::Regular
                        } else {
                            FileKind::Unreadable
                        },
                        content_hash,
                    }
                }
                Err(_) => Self {
                    modified: metadata.modified().ok(),
                    len: metadata.len(),
                    exists: true,
                    kind: FileKind::Unreadable,
                    content_hash: None,
                },
            },
            Ok(metadata) => Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                exists: true,
                kind: FileKind::Other,
                content_hash: None,
            },
            Err(_) => Self {
                modified: None,
                len: 0,
                exists: false,
                kind: FileKind::Missing,
                content_hash: None,
            },
        }
    }

    pub(crate) fn from_open_file(file: &File, bytes: &[u8]) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            exists: true,
            kind: FileKind::Regular,
            content_hash: Some(hash(&bytes)),
        })
    }

    pub(crate) fn from_loaded_bytes(path: &Path, bytes: &[u8]) -> Self {
        let metadata = fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        Self {
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            exists: true,
            kind: FileKind::Regular,
            content_hash: Some(hash(&bytes)),
        }
    }
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn read_dependency(path: &Path) -> io::Result<(Vec<u8>, Fingerprint)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "dependency is not a regular non-symlink file",
        ));
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let fingerprint = Fingerprint::from_open_file(&file, &bytes)?;
    Ok((bytes, fingerprint))
}

#[derive(Clone)]
struct State {
    generation: u64,
    html: String,
    diagnostics: String,
    dependencies: BTreeMap<PathBuf, Fingerprint>,
    style_origins: BTreeSet<String>,
}

impl State {
    fn from_build(generation: u64, build: Build) -> Self {
        Self {
            generation,
            html: build.html,
            diagnostics: build.diagnostics,
            dependencies: build.dependencies,
            style_origins: build.style_origins,
        }
    }

    fn changed(&self) -> bool {
        self.dependencies
            .iter()
            .any(|(path, previous)| Fingerprint::read(path) != *previous)
    }

    fn refresh(&mut self) {
        for (path, fingerprint) in &mut self.dependencies {
            *fingerprint = Fingerprint::read(path);
        }
    }
}

pub fn run(
    options: Options,
    mut build: impl FnMut(&CancellationToken) -> Result<Build, String> + Send,
    shutdown: &AtomicBool,
) -> Result<(), Error> {
    let address = SocketAddr::new(options.bind, options.port);
    let listener = TcpListener::bind(address).map_err(|source| Error::Bind { address, source })?;
    listener.set_nonblocking(true).map_err(Error::Io)?;
    let local = listener.local_addr().map_err(Error::Io)?;
    eprintln!("AdocWeave preview: http://{local}/");

    let first = build(&CancellationToken::new()).map_err(Error::Build)?;
    let mut state = State::from_build(1, first);
    let mut changed_at = None;
    while !shutdown.load(Ordering::Acquire) {
        if state.changed() {
            changed_at.get_or_insert_with(std::time::Instant::now);
        }
        if changed_at.is_some_and(|start| start.elapsed() >= options.debounce) {
            state.refresh();
            let cancellation = CancellationToken::new();
            let mut superseded = false;
            let result = std::thread::scope(|scope| {
                let worker = scope.spawn(|| build(&cancellation));
                while !worker.is_finished() {
                    if shutdown.load(Ordering::Acquire) || state.changed() {
                        cancellation.cancel();
                        superseded = true;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => spawn_response(stream, state.clone(), local),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(error) => return Err(Error::Io(error)),
                    }
                }
                worker
                    .join()
                    .map_err(|_| Error::Build("preview build worker panicked".to_owned()))
            })?;
            if state.changed() {
                superseded = true;
            }
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            if superseded {
                state.refresh();
                changed_at = Some(std::time::Instant::now());
                continue;
            }
            let next_generation = state.generation.saturating_add(1);
            match result {
                Ok(next) if next.changed() => {
                    state.refresh();
                    changed_at = Some(std::time::Instant::now());
                    continue;
                }
                Ok(next) => state = State::from_build(next_generation, next),
                Err(message) => {
                    state.generation = next_generation;
                    state.html = error_document(&message);
                    state.diagnostics = format!("{message}\n");
                    state.refresh();
                }
            }
            changed_at = None;
        }

        match listener.accept() {
            Ok((stream, _)) => spawn_response(stream, state.clone(), local),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(Error::Io(error)),
        }
    }
    Ok(())
}

fn spawn_response(stream: TcpStream, state: State, local: SocketAddr) {
    std::thread::spawn(move || {
        let _ = respond(stream, &state, local);
    });
}

fn respond(mut stream: TcpStream, state: &State, local: SocketAddr) -> Result<(), Error> {
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(Error::Io)?;
    let mut request = [0_u8; MAX_REQUEST_BYTES];
    let count = stream.read(&mut request).map_err(Error::Io)?;
    let request = std::str::from_utf8(&request[..count]).unwrap_or("");
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let host = request
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("host").then_some(value)
        })
        .unwrap_or("")
        .trim();
    if !host_allowed(host, local) {
        return write_response(
            &mut stream,
            method,
            400,
            "text/plain",
            "invalid host\n",
            &BTreeSet::new(),
        );
    }
    if !matches!(method, "GET" | "HEAD") {
        return write_response(
            &mut stream,
            method,
            405,
            "text/plain",
            "method not allowed\n",
            &state.style_origins,
        );
    }
    let (status, content_type, body) = match path {
        "/" => (200, "text/html; charset=utf-8", shell()),
        "/document" => (200, "text/html; charset=utf-8", state.html.clone()),
        "/client.js" => (200, "text/javascript; charset=utf-8", CLIENT_JS.to_owned()),
        "/events" => (
            200,
            "application/json",
            format!("{{\"generation\":{}}}\n", state.generation),
        ),
        "/diagnostics" => (200, "application/json", state.diagnostics.clone()),
        _ => (404, "text/plain; charset=utf-8", "not found\n".to_owned()),
    };
    write_response(
        &mut stream,
        method,
        status,
        content_type,
        &body,
        &state.style_origins,
    )
}

fn host_allowed(host: &str, local: SocketAddr) -> bool {
    let Ok(url) = url::Url::parse(&format!("http://{host}")) else {
        return false;
    };
    if url.port_or_known_default() != Some(local.port()) {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(address)) => local.ip().is_unspecified() || local.ip() == address,
        Some(url::Host::Ipv6(address)) => local.ip().is_unspecified() || local.ip() == address,
        Some(url::Host::Domain(name)) => local.ip().is_loopback() && name == "localhost",
        None => false,
    }
}

fn write_response(
    stream: &mut TcpStream,
    method: &str,
    status: u16,
    content_type: &str,
    body: &str,
    style_origins: &BTreeSet<String>,
) -> Result<(), Error> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        400 => "Bad Request",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: {}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len(),
        content_security_policy(style_origins)
    )
    .map_err(Error::Io)?;
    if method != "HEAD" {
        stream.write_all(body.as_bytes()).map_err(Error::Io)?;
    }
    Ok(())
}

fn content_security_policy(style_origins: &BTreeSet<String>) -> String {
    format!(
        "default-src 'none'; script-src 'self'; frame-src 'self'; style-src 'unsafe-inline'{}",
        style_origins
            .iter()
            .map(|origin| format!(" {origin}"))
            .collect::<String>()
    )
}

fn shell() -> String {
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>AdocWeave preview</title></head><body><iframe title=\"Preview\" sandbox src=\"/document\"></iframe><pre aria-label=\"Diagnostics\"></pre><script src=\"/client.js\"></script></body></html>\n".to_owned()
}

fn error_document(message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Preview error</title></head><body><h1>Preview error</h1><pre>{}</pre></body></html>\n",
        escape_html(message)
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use super::*;
    use adocweave::CancellationCheck;

    fn snapshots(paths: impl IntoIterator<Item = PathBuf>) -> BTreeMap<PathBuf, Fingerprint> {
        paths
            .into_iter()
            .map(|path| {
                let fingerprint = Fingerprint::read(&path);
                (path, fingerprint)
            })
            .collect()
    }

    fn request(address: SocketAddr, path: &str) -> String {
        for _ in 0..100 {
            if let Ok(mut stream) = TcpStream::connect(address) {
                write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\n\r\n").expect("request");
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok() {
                    return response;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("preview server did not start");
    }

    #[test]
    fn error_page_escapes_input() {
        let page = error_document("</pre><script>alert(1)</script>");
        assert!(!page.contains("<script>alert"));
        assert!(page.contains("&lt;/pre&gt;"));
    }

    #[test]
    fn fingerprints_detect_create_change_and_delete() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("dependency.adoc");
        let missing = Fingerprint::read(&path);
        fs::write(&path, "one").expect("create");
        let created = Fingerprint::read(&path);
        fs::write(&path, "two").expect("same-length change");
        let changed = Fingerprint::read(&path);
        fs::remove_file(&path).expect("delete");
        let deleted = Fingerprint::read(&path);
        assert_ne!(missing, created);
        assert_ne!(created, changed);
        assert_ne!(changed, deleted);
    }

    #[test]
    fn fixed_routes_reload_by_generation_and_shutdown_releases_port() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dependency = directory.path().join("dependency.adoc");
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
        let address = reservation.local_addr().expect("address");
        drop(reservation);
        let shutdown = Arc::new(AtomicBool::new(false));
        let builds = Arc::new(AtomicU64::new(0));
        let server_shutdown = Arc::clone(&shutdown);
        let server_builds = Arc::clone(&builds);
        let server_dependency = dependency.clone();
        let thread = std::thread::spawn(move || {
            run(
                Options {
                    bind: address.ip(),
                    port: address.port(),
                    debounce: Duration::from_millis(20),
                },
                |_| {
                    let generation = server_builds.fetch_add(1, Ordering::Relaxed) + 1;
                    Ok(Build::new(
                        format!("<p>{generation}</p>"),
                        "[]".to_owned(),
                        snapshots([server_dependency.clone()]),
                    ))
                },
                &server_shutdown,
            )
        });

        let shell = request(address, "/");
        assert!(shell.starts_with("HTTP/1.1 200"), "{shell:?}");
        assert!(shell.contains("Content-Security-Policy: default-src 'none'"));
        assert!(shell.contains("<iframe"));
        assert!(request(address, "/secret").starts_with("HTTP/1.1 404"));
        fs::write(&dependency, "created").expect("create dependency");
        for _ in 0..100 {
            if request(address, "/events").contains("\"generation\":2") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(builds.load(Ordering::Relaxed), 2);

        shutdown.store(true, Ordering::Release);
        thread.join().expect("server thread").expect("server");
        TcpListener::bind(address).expect("shutdown released port");
    }

    #[test]
    fn occupied_port_reports_the_bind_address() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("occupied port");
        let address = listener.local_addr().expect("address");
        let shutdown = AtomicBool::new(false);
        let error = run(
            Options {
                bind: address.ip(),
                port: address.port(),
                debounce: Duration::from_millis(20),
            },
            |_| unreachable!("binding fails before building"),
            &shutdown,
        )
        .expect_err("bind must fail");
        assert!(matches!(
            error,
            Error::Bind {
                address: failed,
                ..
            } if failed == address
        ));
    }

    #[test]
    fn newer_dependency_change_cancels_an_in_flight_build() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dependency = directory.path().join("dependency.adoc");
        fs::write(&dependency, "one").expect("fixture");
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
        let address = reservation.local_addr().expect("address");
        drop(reservation);
        let shutdown = Arc::new(AtomicBool::new(false));
        let builds = Arc::new(AtomicU64::new(0));
        let server_shutdown = Arc::clone(&shutdown);
        let server_builds = Arc::clone(&builds);
        let server_dependency = dependency.clone();
        let thread = std::thread::spawn(move || {
            run(
                Options {
                    bind: address.ip(),
                    port: address.port(),
                    debounce: Duration::from_millis(20),
                },
                |cancellation| {
                    let build = server_builds.fetch_add(1, Ordering::Relaxed) + 1;
                    if build == 2 {
                        while !cancellation.is_cancelled() {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        return Err("cancelled".to_owned());
                    }
                    Ok(Build::new(
                        format!("<p>{build}</p>"),
                        "[]".to_owned(),
                        snapshots([server_dependency.clone()]),
                    ))
                },
                &server_shutdown,
            )
        });
        request(address, "/events");
        fs::write(&dependency, "two").expect("first change");
        for _ in 0..100 {
            if builds.load(Ordering::Relaxed) >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        fs::write(&dependency, "six").expect("superseding change");
        for _ in 0..100 {
            if builds.load(Ordering::Relaxed) >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(builds.load(Ordering::Relaxed), 3);
        assert!(request(address, "/events").contains("\"generation\":2"));
        shutdown.store(true, Ordering::Release);
        thread.join().expect("server thread").expect("server");
    }

    #[test]
    fn dependency_changed_after_build_is_rejected_before_adoption() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("root.adoc");
        let discovered = directory.path().join("new.adoc");
        fs::write(&root, "one").expect("root");
        fs::write(&discovered, "old").expect("dependency");
        let builds = AtomicU64::new(0);
        let first = Build::new(
            "initial".to_owned(),
            "[]".to_owned(),
            snapshots([root.clone()]),
        );
        fs::write(&root, "two").expect("trigger");
        let second = {
            builds.fetch_add(1, Ordering::Relaxed);
            let build = Build::new(
                "stale".to_owned(),
                "[]".to_owned(),
                snapshots([root, discovered.clone()]),
            );
            fs::write(&discovered, "new").expect("post-build change");
            build
        };
        assert!(first.changed());
        assert!(second.changed());
        assert_eq!(builds.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dependency_snapshot_is_captured_when_the_file_is_read() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dependency = directory.path().join("new.adoc");
        fs::write(&dependency, "old").expect("dependency");
        let (bytes, fingerprint) = read_dependency(&dependency).expect("read dependency");
        assert_eq!(bytes, b"old");

        // This is the deterministic barrier between loading a newly discovered
        // dependency and completing the build.
        fs::write(&dependency, "new").expect("replace after read");
        let build = Build::new(
            "rendered from old".to_owned(),
            "[]".to_owned(),
            BTreeMap::from([(dependency, fingerprint)]),
        );
        assert!(build.changed(), "the stale build must not be adopted");
    }

    #[test]
    fn csp_lists_only_explicit_stylesheet_origins() {
        let origins = BTreeSet::from(["https://cdn.example".to_owned()]);
        let response = content_security_policy(&origins);
        assert!(response.contains("style-src 'unsafe-inline' https://cdn.example"));
        assert!(!response.contains("style-src *"));
    }

    #[test]
    fn host_validation_supports_wildcard_bind_and_rejects_injection() {
        let wildcard = SocketAddr::from(([0, 0, 0, 0], 4000));
        assert!(host_allowed("192.0.2.10:4000", wildcard));
        assert!(!host_allowed("evil.example:4000", wildcard));
        assert!(!host_allowed("192.0.2.10:4000\r\nX-Evil: yes", wildcard));
    }

    #[cfg(unix)]
    #[test]
    fn permission_fingerprint_recovers_after_read_access_returns() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("dependency.adoc");
        fs::write(&path, "content").expect("fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("deny");
        let denied = Fingerprint::read(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore");
        let restored = Fingerprint::read(&path);
        if denied.content_hash.is_none() {
            assert_ne!(denied, restored);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_fingerprint_never_reads_the_target_body() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let secret = outside.path().join("secret.adoc");
        fs::write(&secret, "EXTERNAL_SECRET_BODY").expect("secret");
        let watched = root.path().join("dependency");
        symlink(&secret, &watched).expect("symlink");

        let fingerprint = Fingerprint::read(&watched);
        assert_eq!(fingerprint.kind, FileKind::Symlink);
        assert_eq!(
            fingerprint.content_hash,
            Some(hash(&fs::read_link(&watched).expect("link target")))
        );
        assert_ne!(
            fingerprint.content_hash,
            Some(hash(&b"EXTERNAL_SECRET_BODY".as_slice()))
        );
    }
}
