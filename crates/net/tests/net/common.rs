use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PEER_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub target: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct Connection {
    stream: TcpStream,
    recorder: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl Connection {
    pub fn read_request(&mut self) -> RecordedRequest {
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout is settable");
        let mut buf = Vec::new();
        let head_end = loop {
            if let Some(pos) = find_head_end(&buf) {
                break pos;
            }
            assert!(buf.len() <= 64 * 1024, "request head exceeded 64 KiB");
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk).expect("peer readable");
            assert_ne!(n, 0, "peer closed mid-request-head");
            buf.extend_from_slice(&chunk[..n]);
        };

        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_owned();
        let mut parts = request_line.splitn(3, ' ');
        let method = parts.next().unwrap_or_default().to_owned();
        let target = parts.next().unwrap_or_default().to_owned();
        let version = parts.next().unwrap_or_default().to_owned();

        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                panic!("malformed header line in canned capture: {line:?}");
            };
            headers.push((name.to_owned(), value.trim_start_matches(' ').to_owned()));
        }

        let content_length = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[head_end + 4..].to_vec();
        while body.len() < content_length {
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk).expect("peer readable");
            assert_ne!(n, 0, "peer closed mid-request-body");
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(content_length);

        let request = RecordedRequest {
            method,
            target,
            version,
            headers,
            body,
        };
        self.recorder
            .lock()
            .expect("recorder poisoned")
            .push(request.clone());
        request
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    #[must_use]
    pub fn await_peer_close(&mut self) -> bool {
        let deadline = Instant::now() + PEER_CLOSE_TIMEOUT;
        self.stream
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("read timeout is settable");
        while Instant::now() < deadline {
            match self.stream.read(&mut [0u8; 1]) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => return true,
            }
        }
        false
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

pub struct TestServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    failures: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TestServer {
    pub fn start(handler: impl Fn(&mut Connection) + Send + Sync + 'static) -> Self {
        Self::bind("127.0.0.1:0", handler)
    }

    pub fn start_v6(handler: impl Fn(&mut Connection) + Send + Sync + 'static) -> Self {
        Self::bind("[::1]:0", handler)
    }

    fn bind(addr: &str, handler: impl Fn(&mut Connection) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind(addr).expect("loopback bind succeeds");
        let addr = listener.local_addr().expect("local addr known");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);

        let requests_clone = Arc::clone(&requests);
        let failures_clone = Arc::clone(&failures);
        let shutdown_clone = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            loop {
                if shutdown_clone.load(Ordering::Acquire) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if shutdown_clone.load(Ordering::Acquire) {
                            drop(stream);
                            break;
                        }
                        let mut conn = Connection {
                            stream,
                            recorder: Arc::clone(&requests_clone),
                        };
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                handler(&mut conn);
                            }));
                        if let Err(panic) = outcome {
                            let message = panic
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                                .unwrap_or_else(|| "handler panicked".to_owned());
                            failures_clone
                                .lock()
                                .expect("failures poisoned")
                                .push(message);
                        }
                    }
                    Err(err) => {
                        failures_clone
                            .lock()
                            .expect("failures poisoned")
                            .push(format!("accept failed: {err}"));
                        break;
                    }
                }
            }
        });

        Self {
            addr,
            requests,
            failures,
            shutdown,
            handle: Some(handle),
        }
    }

    #[must_use]
    pub fn url(&self, path: &str) -> url::Url {
        url::Url::parse(&format!("http://{}{path}", self.addr)).expect("server URL is absolute")
    }

    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("recorder poisoned").clone()
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn assert_clean(&self) {
        let failures = self.failures.lock().expect("failures poisoned");
        assert!(failures.is_empty(), "server-side failures: {failures:?}");
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn canned_ok(headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = b"HTTP/1.1 200 OK\r\n".to_vec();
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out
}

pub fn canned_redirect(status: u16, location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

pub fn scripted(replies: impl IntoIterator<Item = Vec<u8>> + Send + 'static) -> TestServer {
    let replies = Mutex::new(replies.into_iter().collect::<VecDeque<_>>());
    TestServer::start(move |connection| {
        connection.read_request();
        let reply = replies
            .lock()
            .expect("scripted replies")
            .pop_front()
            .expect("scripted reply for request");
        connection.write_all(&reply).expect("scripted response");
    })
}

pub async fn read_body(mut body: net::Body) -> Result<Vec<u8>, net::NetError> {
    let mut out = Vec::new();
    while let Some(chunk) = body.read_chunk().await? {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
