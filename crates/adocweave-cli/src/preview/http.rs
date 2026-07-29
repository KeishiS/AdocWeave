use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAX_REQUEST_BYTES: usize = 8192;
const HTTP_WORKERS: usize = 4;
const HTTP_QUEUE_CAPACITY: usize = 28;
const CLIENT_READ_TIMEOUT: Duration = Duration::from_millis(250);
const REQUEST_DEADLINE: Duration = Duration::from_millis(500);
const RESPONSE_DEADLINE: Duration = Duration::from_millis(500);
#[cfg(test)]
type WorkerHook = Arc<dyn Fn() + Send + Sync>;
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

pub(super) struct HttpSnapshot {
    generation: u64,
    html: String,
    diagnostics: String,
    style_origins: BTreeSet<String>,
}

impl HttpSnapshot {
    pub(super) fn new(
        generation: u64,
        html: String,
        diagnostics: String,
        style_origins: BTreeSet<String>,
    ) -> Self {
        Self {
            generation,
            html,
            diagnostics,
            style_origins,
        }
    }

    pub(super) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn failure(&self, generation: u64, html: String, diagnostics: String) -> Self {
        Self {
            generation,
            html,
            diagnostics,
            style_origins: self.style_origins.clone(),
        }
    }
}

struct RequestJob {
    stream: TcpStream,
    snapshot: Arc<HttpSnapshot>,
    local: SocketAddr,
}

pub(super) struct HttpWorkers {
    sender: Option<mpsc::SyncSender<RequestJob>>,
    workers: Vec<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl HttpWorkers {
    pub(super) fn new() -> io::Result<Self> {
        Self::start(
            #[cfg(test)]
            None,
        )
    }

    #[cfg(test)]
    fn with_worker_hook(hook: WorkerHook) -> io::Result<Self> {
        Self::start(Some(hook))
    }

    fn start(#[cfg(test)] hook: Option<WorkerHook>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<RequestJob>(HTTP_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(HTTP_WORKERS);
        for index in 0..HTTP_WORKERS {
            let receiver = Arc::clone(&receiver);
            let shutdown = Arc::clone(&shutdown);
            #[cfg(test)]
            let hook = hook.clone();
            let worker = std::thread::Builder::new()
                .name(format!("adocweave-preview-http-{index}"))
                .spawn(move || {
                    while !shutdown.load(Ordering::Acquire) {
                        let job = {
                            let Ok(receiver) = receiver.lock() else {
                                return;
                            };
                            receiver.recv()
                        };
                        let Ok(job) = job else {
                            return;
                        };
                        #[cfg(test)]
                        if let Some(hook) = &hook {
                            hook();
                        }
                        let _ = respond(job.stream, &job.snapshot, job.local);
                    }
                })?;
            workers.push(worker);
        }
        Ok(Self {
            sender: Some(sender),
            workers,
            shutdown,
        })
    }

    pub(super) fn dispatch(
        &self,
        stream: TcpStream,
        snapshot: &Arc<HttpSnapshot>,
        local: SocketAddr,
    ) {
        let job = RequestJob {
            stream,
            snapshot: Arc::clone(snapshot),
            local,
        };
        match self
            .sender
            .as_ref()
            .expect("HTTP worker sender exists until drop")
            .try_send(job)
        {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job)) => {
                let _ = job.stream.shutdown(Shutdown::Both);
            }
        }
    }
}

impl Drop for HttpWorkers {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn respond(mut stream: TcpStream, snapshot: &HttpSnapshot, local: SocketAddr) -> io::Result<()> {
    let request = match read_request_headers(&mut stream, REQUEST_DEADLINE) {
        Ok(request) => request,
        Err(RequestReadError::TooLarge) => {
            return write_response(
                &mut stream,
                "GET",
                431,
                "text/plain; charset=utf-8",
                "request headers too large\n",
                &BTreeSet::new(),
            );
        }
        Err(RequestReadError::Timeout) => {
            return write_response(
                &mut stream,
                "GET",
                408,
                "text/plain; charset=utf-8",
                "request timeout\n",
                &BTreeSet::new(),
            );
        }
        Err(RequestReadError::Invalid) => {
            return write_response(
                &mut stream,
                "GET",
                400,
                "text/plain; charset=utf-8",
                "invalid request\n",
                &BTreeSet::new(),
            );
        }
    };
    let Some(request) = parse_request(&request) else {
        return write_response(
            &mut stream,
            "GET",
            400,
            "text/plain; charset=utf-8",
            "invalid request headers\n",
            &BTreeSet::new(),
        );
    };
    if !host_allowed(request.host, local) {
        return write_response(
            &mut stream,
            request.method,
            400,
            "text/plain",
            "invalid host\n",
            &BTreeSet::new(),
        );
    }
    if !matches!(request.method, "GET" | "HEAD") {
        return write_response(
            &mut stream,
            request.method,
            405,
            "text/plain",
            "method not allowed\n",
            &snapshot.style_origins,
        );
    }
    let (status, content_type, body) = match request.path {
        "/" => (200, "text/html; charset=utf-8", shell()),
        "/document" => (200, "text/html; charset=utf-8", snapshot.html.clone()),
        "/client.js" => (200, "text/javascript; charset=utf-8", CLIENT_JS.to_owned()),
        "/events" => (
            200,
            "application/json",
            format!("{{\"generation\":{}}}\n", snapshot.generation),
        ),
        "/diagnostics" => (200, "application/json", snapshot.diagnostics.clone()),
        _ => (404, "text/plain; charset=utf-8", "not found\n".to_owned()),
    };
    write_response(
        &mut stream,
        request.method,
        status,
        content_type,
        &body,
        &snapshot.style_origins,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Request<'request> {
    method: &'request str,
    path: &'request str,
    host: &'request str,
}

fn parse_request(request: &str) -> Option<Request<'_>> {
    let mut lines = request.strip_suffix("\r\n\r\n")?.split("\r\n");
    let mut parts = lines.next()?.split(' ');
    let method = parts.next()?;
    let path = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some()
        || !method.bytes().all(is_http_token_byte)
        || !path.starts_with('/')
        || path.bytes().any(|byte| byte.is_ascii_control())
        || version != "HTTP/1.1"
    {
        return None;
    }
    let mut host = None;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name.bytes().all(is_http_token_byte)
            || value
                .bytes()
                .any(|byte| byte != b'\t' && !(b' '..=b'~').contains(&byte))
        {
            return None;
        }
        if name.eq_ignore_ascii_case("host") && host.replace(value.trim()).is_some() {
            return None;
        }
    }
    Some(Request {
        method,
        path,
        host: host.filter(|host| valid_http_authority(host))?,
    })
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn valid_http_authority(authority: &str) -> bool {
    !authority.is_empty()
        && !authority
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || b"@/?#\\".contains(&byte))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestReadError {
    Invalid,
    Timeout,
    TooLarge,
}

trait HeaderReader: Read {
    fn set_header_timeout(&self, timeout: Duration) -> io::Result<()>;
}

impl HeaderReader for TcpStream {
    fn set_header_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
}

fn read_request_headers(
    stream: &mut impl HeaderReader,
    deadline: Duration,
) -> Result<String, RequestReadError> {
    let started = Instant::now();
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let remaining_time = deadline
            .checked_sub(started.elapsed())
            .ok_or(RequestReadError::Timeout)?;
        stream
            .set_header_timeout(remaining_time.min(CLIENT_READ_TIMEOUT))
            .map_err(|_| RequestReadError::Invalid)?;
        let remaining = MAX_REQUEST_BYTES.saturating_sub(request.len());
        if remaining == 0 {
            return Err(RequestReadError::TooLarge);
        }
        let read_limit = remaining.min(chunk.len());
        match stream.read(&mut chunk[..read_limit]) {
            Ok(0) => return Err(RequestReadError::Invalid),
            Ok(count) => {
                request.extend_from_slice(&chunk[..count]);
                if started.elapsed() >= deadline {
                    return Err(RequestReadError::Timeout);
                }
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    request.truncate(end + 4);
                    return String::from_utf8(request).map_err(|_| RequestReadError::Invalid);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(RequestReadError::Timeout);
            }
            Err(_) => return Err(RequestReadError::Invalid),
        }
    }
}

fn host_allowed(host: &str, local: SocketAddr) -> bool {
    if !valid_http_authority(host) {
        return false;
    }
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
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        400 => "Bad Request",
        _ => "Error",
    };
    let allow = if status == 405 {
        "Allow: GET, HEAD\r\n"
    } else {
        ""
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{allow}Cache-Control: no-store\r\nContent-Security-Policy: {}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len(),
        content_security_policy(style_origins)
    );
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    write_until(stream, headers.as_bytes(), deadline)?;
    if method != "HEAD" {
        write_until(stream, body.as_bytes(), deadline)?;
    }
    Ok(())
}

fn write_until(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "preview response deadline exceeded",
            ));
        }
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "could not write preview response",
                ));
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::net::{Ipv4Addr, TcpListener};

    use super::*;

    struct SegmentedReader {
        segments: VecDeque<io::Result<Vec<u8>>>,
    }

    impl SegmentedReader {
        fn new(segments: impl IntoIterator<Item = io::Result<Vec<u8>>>) -> Self {
            Self {
                segments: segments.into_iter().collect(),
            }
        }
    }

    impl Read for SegmentedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(segment) = self.segments.pop_front() else {
                return Ok(0);
            };
            let segment = segment?;
            assert!(segment.len() <= buffer.len(), "test segment exceeds buffer");
            buffer[..segment.len()].copy_from_slice(&segment);
            Ok(segment.len())
        }
    }

    struct TestHeaderReader<R>(R);

    impl<R: Read> Read for TestHeaderReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl<R: Read> HeaderReader for TestHeaderReader<R> {
        fn set_header_timeout(&self, _timeout: Duration) -> io::Result<()> {
            Ok(())
        }
    }

    fn snapshot(html: String) -> Arc<HttpSnapshot> {
        Arc::new(HttpSnapshot::new(1, html, "[]".to_owned(), BTreeSet::new()))
    }

    #[test]
    fn request_reader_collects_headers_split_across_reads() {
        let mut reader = TestHeaderReader(SegmentedReader::new([
            Ok(b"GET / HTTP/1.1\r\nHo".to_vec()),
            Ok(b"st: 127.0.0.1:4000\r".to_vec()),
            Ok(b"\n\r\nignored body".to_vec()),
        ]));
        let request =
            read_request_headers(&mut reader, Duration::from_secs(1)).expect("complete headers");
        assert_eq!(request, "GET / HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\r\n");
    }

    #[test]
    fn request_reader_rejects_incomplete_oversized_and_timed_out_headers() {
        let mut incomplete =
            TestHeaderReader(Cursor::new(b"GET / HTTP/1.1\r\nHost: localhost".as_slice()));
        assert_eq!(
            read_request_headers(&mut incomplete, Duration::from_secs(1)),
            Err(RequestReadError::Invalid)
        );
        let mut oversized = TestHeaderReader(Cursor::new(vec![b'a'; MAX_REQUEST_BYTES]));
        assert_eq!(
            read_request_headers(&mut oversized, Duration::from_secs(1)),
            Err(RequestReadError::TooLarge)
        );
        let mut timed_out = TestHeaderReader(SegmentedReader::new([Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "test timeout",
        ))]));
        assert_eq!(
            read_request_headers(&mut timed_out, Duration::from_secs(1)),
            Err(RequestReadError::Timeout)
        );
    }

    #[test]
    fn request_reader_applies_one_deadline_to_continuous_small_reads() {
        struct SlowReader;
        impl Read for SlowReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(3));
                buffer[0] = b'a';
                Ok(1)
            }
        }
        assert_eq!(
            read_request_headers(&mut TestHeaderReader(SlowReader), Duration::from_millis(5)),
            Err(RequestReadError::Timeout)
        );
    }

    #[test]
    fn request_reader_rejects_headers_completed_after_the_deadline() {
        struct DelayedCompleteReader;
        impl Read for DelayedCompleteReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(6));
                let request = b"GET / HTTP/1.1\r\nHost: localhost:4000\r\n\r\n";
                buffer[..request.len()].copy_from_slice(request);
                Ok(request.len())
            }
        }
        assert_eq!(
            read_request_headers(
                &mut TestHeaderReader(DelayedCompleteReader),
                Duration::from_millis(5)
            ),
            Err(RequestReadError::Timeout)
        );
    }

    #[test]
    fn request_parser_requires_http_11_and_exactly_one_valid_host() {
        assert_eq!(
            parse_request("HEAD /events HTTP/1.1\r\nHost: localhost:4000\r\n\r\n"),
            Some(Request {
                method: "HEAD",
                path: "/events",
                host: "localhost:4000",
            })
        );
        for invalid in [
            "GET / HTTP/1.0\r\nHost: localhost\r\n\r\n",
            "GET / HTTP/1.1\r\nUser-Agent: test\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: localhost\r\nHost: attacker.example\r\n\r\n",
            "GET / HTTP/1.1\r\nMalformed\r\nHost: localhost\r\n\r\n",
        ] {
            assert!(parse_request(invalid).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn workers_bound_connections_and_close_over_capacity() {
        let active = Arc::new(std::sync::Barrier::new(HTTP_WORKERS + 1));
        let worker_active = Arc::clone(&active);
        let release = Arc::new(std::sync::Barrier::new(HTTP_WORKERS + 1));
        let worker_release = Arc::clone(&release);
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_started = Arc::clone(&started);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let workers = HttpWorkers::with_worker_hook(Arc::new(move || {
            if worker_started.fetch_add(1, Ordering::Relaxed) < HTTP_WORKERS {
                worker_active.wait();
                worker_release.wait();
            }
        }))
        .expect("workers");
        let snapshot = snapshot(String::new());
        let mut clients = Vec::new();
        for _ in 0..HTTP_WORKERS {
            let client = TcpStream::connect(address).expect("active client");
            let (server, _) = listener.accept().expect("active server");
            workers.dispatch(server, &snapshot, address);
            clients.push(client);
        }
        active.wait();
        for _ in 0..HTTP_QUEUE_CAPACITY {
            let client = TcpStream::connect(address).expect("queued client");
            let (server, _) = listener.accept().expect("queued server");
            workers.dispatch(server, &snapshot, address);
            clients.push(client);
        }
        let mut rejected = TcpStream::connect(address).expect("rejected client");
        let (server, _) = listener.accept().expect("rejected server");
        workers.dispatch(server, &snapshot, address);
        let mut response = String::new();
        rejected
            .read_to_string(&mut response)
            .expect("closed response");
        assert!(response.is_empty());
        release.wait();
        drop(clients);
        drop(workers);
    }

    #[test]
    fn csp_lists_only_explicit_stylesheet_origins() {
        let origins = BTreeSet::from(["https://cdn.example".to_owned()]);
        let response = content_security_policy(&origins);
        assert!(response.contains("style-src 'unsafe-inline' https://cdn.example"));
        assert!(!response.contains("style-src *"));
    }

    #[test]
    fn workers_stop_when_a_client_does_not_read_a_large_response() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let workers = HttpWorkers::new().expect("workers");
        let snapshot = snapshot("x".repeat(16 * 1024 * 1024));
        let mut client = TcpStream::connect(address).expect("client");
        let (server, _) = listener.accept().expect("server");
        client
            .write_all(format!("GET /document HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
            .expect("request");
        workers.dispatch(server, &snapshot, address);
        std::thread::sleep(Duration::from_millis(20));
        let stopping = Instant::now();
        drop(workers);
        assert!(stopping.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn response_contract_has_security_headers_and_allow() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let snapshot = snapshot(String::new());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("server");
            respond(stream, &snapshot, address).expect("response");
        });
        let mut client = TcpStream::connect(address).expect("client");
        client
            .write_all(format!("POST / HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
            .expect("request");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("response");
        server.join().expect("server");
        assert!(response.starts_with("HTTP/1.1 405"));
        assert!(response.contains("\r\nAllow: GET, HEAD\r\n"));
        assert!(response.contains("\r\nCache-Control: no-store\r\n"));
        assert!(response.contains("\r\nX-Content-Type-Options: nosniff\r\n"));
    }

    #[test]
    fn host_validation_supports_wildcard_bind_and_rejects_injection() {
        let wildcard = SocketAddr::from(([0, 0, 0, 0], 4000));
        assert!(host_allowed("192.0.2.10:4000", wildcard));
        for invalid in [
            "evil.example:4000",
            "192.0.2.10:4000\r\nX-Evil: yes",
            "user@192.0.2.10:4000",
            "192.0.2.10:4000/path",
            "192.0.2.10:4000?query",
        ] {
            assert!(!host_allowed(invalid, wildcard), "{invalid:?}");
        }
    }
}
