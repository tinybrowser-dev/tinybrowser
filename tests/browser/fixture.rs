use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

pub(crate) fn read_request_head(stream: &mut impl Read) -> Option<String> {
    let head = read_request_head_bytes(stream)?;
    Some(String::from_utf8_lossy(&head).into_owned())
}

fn read_request_head_bytes(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 512];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if Instant::now() >= deadline {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => {
                eprintln!("test server read failed: {error}");
                return None;
            }
        }
    }
    Some(head)
}

pub(crate) fn server() -> (TcpListener, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server bind");
    let address = listener.local_addr().expect("server addr");
    (listener, address)
}

#[derive(Clone)]
pub(crate) struct Response {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    fn not_found() -> Self {
        Self {
            status: 404,
            reason: "Not Found",
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn render(&self, head: &str, cors: bool) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status,
            self.reason,
            self.body.len()
        );
        for (name, value) in &self.headers {
            let _ = write!(response, "{name}: {value}\r\n");
        }
        if cors {
            match request_header(head, "origin") {
                Some(origin) => {
                    let _ = write!(
                        response,
                        "Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Credentials: true\r\nVary: Origin\r\n"
                    );
                }
                None => response.push_str("Access-Control-Allow-Origin: *\r\n"),
            }
        }
        response.push_str("\r\n");
        let mut response = response.into_bytes();
        response.extend_from_slice(&self.body);
        response
    }
}

pub(crate) fn ok(content_type: &str, body: &[u8]) -> Response {
    Response {
        status: 200,
        reason: "OK",
        headers: vec![("Content-Type".to_owned(), content_type.to_owned())],
        body: body.to_vec(),
    }
}

pub(crate) fn redirect(location: &str) -> Response {
    Response {
        status: 302,
        reason: "Found",
        headers: vec![("Location".to_owned(), location.to_owned())],
        body: Vec::new(),
    }
}

pub(crate) const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xcf, 0xc0, 0x50,
    0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

pub(crate) struct RecordedRequest {
    pub(crate) path: String,
    pub(crate) head: String,
}

pub(crate) struct TestServer {
    address: std::net::SocketAddr,
    receiver: Mutex<mpsc::Receiver<RecordedRequest>>,
    history: Mutex<Vec<RecordedRequest>>,
    released: Arc<(Mutex<bool>, Condvar)>,
}

impl TestServer {
    pub(crate) fn start(routes: Vec<(&'static str, Response)>, hold: Option<&'static str>) -> Self {
        Self::with_options(routes, hold, true, Duration::ZERO)
    }

    // https://fetch.spec.whatwg.org/#cors-check
    pub(crate) fn start_without_cors(routes: Vec<(&'static str, Response)>) -> Self {
        Self::with_options(routes, None, false, Duration::ZERO)
    }

    pub(crate) fn serve_on(listener: TcpListener, routes: Vec<(&'static str, Response)>) -> Self {
        Self::serve(listener, routes, None, true, Duration::ZERO, None)
    }

    pub(crate) fn serve_on_after(
        listener: TcpListener,
        routes: Vec<(&'static str, Response)>,
        delay: Duration,
    ) -> Self {
        Self::serve(listener, routes, None, true, delay, None)
    }

    fn with_options(
        routes: Vec<(&'static str, Response)>,
        hold: Option<&'static str>,
        cors: bool,
        delay: Duration,
    ) -> Self {
        let (listener, _) = server();
        Self::serve(listener, routes, hold, cors, delay, None)
    }

    pub(crate) fn start_tls(
        routes: Vec<(&'static str, Response)>,
        identity: native_tls::Identity,
    ) -> Self {
        let (listener, _) = server();
        let acceptor = native_tls::TlsAcceptor::new(identity).expect("tls acceptor");
        Self::serve(listener, routes, None, true, Duration::ZERO, Some(acceptor))
    }

    fn serve(
        listener: TcpListener,
        routes: Vec<(&'static str, Response)>,
        hold: Option<&'static str>,
        cors: bool,
        delay: Duration,
        tls: Option<native_tls::TlsAcceptor>,
    ) -> Self {
        let address = listener.local_addr().expect("server addr");
        let (sender, receiver) = mpsc::channel();
        let released: Arc<(Mutex<bool>, Condvar)> = Arc::default();
        let flag = Arc::clone(&released);
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let sender = sender.clone();
                let flag = Arc::clone(&flag);
                let routes = routes.clone();
                let tls = tls.clone();
                thread::spawn(move || {
                    if let Some(acceptor) = tls {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                        let Ok(mut stream) = acceptor.accept(stream) else {
                            return;
                        };
                        let _ = stream
                            .get_ref()
                            .set_read_timeout(Some(Duration::from_millis(100)));
                        answer(&mut stream, &routes, hold, cors, delay, &sender, &flag);
                        return;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    answer(&mut stream, &routes, hold, cors, delay, &sender, &flag);
                });
            }
        });
        Self {
            address,
            receiver: Mutex::new(receiver),
            history: Mutex::new(Vec::new()),
            released,
        }
    }

    pub(crate) fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    pub(crate) fn wait(&self, path: &str) -> String {
        poll_until(&format!("a request for {path}"), || {
            self.drain();
            self.history
                .lock()
                .expect("request history")
                .iter()
                .rev()
                .find(|request| request.path == path)
                .map(|request| request.head.clone())
        })
    }

    pub(crate) fn requested(&self, path: &str) -> bool {
        self.drain();
        self.history
            .lock()
            .expect("request history")
            .iter()
            .any(|request| request.path == path)
    }

    pub(crate) fn wait_count(&self, path: &str, count: usize) -> Vec<String> {
        poll_until(&format!("{count} requests for {path}"), || {
            let heads = self.history(path);
            (heads.len() >= count).then_some(heads)
        })
    }

    pub(crate) fn history(&self, path: &str) -> Vec<String> {
        self.drain();
        self.history
            .lock()
            .expect("request history")
            .iter()
            .filter(|request| request.path == path)
            .map(|request| request.head.clone())
            .collect()
    }

    fn drain(&self) {
        let receiver = self.receiver.lock().expect("request channel");
        while let Ok(recorded) = receiver.try_recv() {
            self.history.lock().expect("request history").push(recorded);
        }
    }

    pub(crate) fn release(&self) {
        let (lock, condvar) = &*self.released;
        *lock.lock().expect("release flag") = true;
        condvar.notify_all();
    }
}

fn answer(
    stream: &mut (impl Read + Write),
    routes: &[(&'static str, Response)],
    hold: Option<&str>,
    cors: bool,
    delay: Duration,
    sender: &mpsc::Sender<RecordedRequest>,
    flag: &(Mutex<bool>, Condvar),
) {
    let Some(head) = read_request_head(stream) else {
        return;
    };
    let Some(path) = request_target(&head) else {
        return;
    };
    let _ = sender.send(RecordedRequest {
        path: path.to_owned(),
        head: head.clone(),
    });
    if hold == Some(path) {
        let (lock, condvar) = flag;
        let mut released = lock.lock().expect("release flag");
        while !*released {
            released = condvar.wait(released).expect("release wait");
        }
    }
    if !delay.is_zero() {
        thread::sleep(delay);
    }
    let response = routes
        .iter()
        .find(|(route, _)| *route == path)
        .map_or_else(Response::not_found, |(_, response)| response.clone());
    let _ = stream.write_all(&response.render(&head, cors));
}

pub(crate) fn poll_until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(answer) = probe() {
            return answer;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    std::path::Path::new("/proc").join(pid.to_string()).exists()
}

fn request_target(head: &str) -> Option<&str> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    (!method.is_empty() && version.starts_with("HTTP/")).then_some(target)
}

fn request_header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

pub(crate) fn load_page(client: &mut cdp::Client, url: &str, timeout: Duration) -> String {
    let created = client
        .call("Target.createTarget", &json!({"url": "about:blank"}), None)
        .expect("create");
    let target_id = created["targetId"].as_str().expect("id").to_owned();
    let session = attach(client, &target_id);
    client
        .call("Page.navigate", &json!({"url": url}), Some(&session))
        .expect("navigate");
    wait_for_load(client, &session, timeout);
    session
}

pub(crate) fn attach(client: &mut cdp::Client, target_id: &str) -> String {
    let attached = client
        .call(
            "Target.attachToTarget",
            &json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .expect("attach");
    let session = attached["sessionId"].as_str().expect("session").to_owned();
    client
        .call("Page.enable", &json!({}), Some(&session))
        .expect("enable Page events");
    session
}

pub(crate) fn wait_for_load(client: &mut cdp::Client, session: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let event = client
            .read_event(Duration::from_millis(500))
            .expect("read event");
        if let Some(event) = event
            && event["method"] == json!("Page.loadEventFired")
            && event["sessionId"] == json!(session)
        {
            return;
        }
        assert!(Instant::now() < deadline, "no load event for {session}");
    }
}

pub(crate) fn evaluate(
    client: &mut cdp::Client,
    session: &str,
    expression: &str,
) -> serde_json::Value {
    client
        .call(
            "Runtime.evaluate",
            &json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
            Some(session),
        )
        .expect("evaluate")["result"]["value"]
        .clone()
}

pub(crate) fn open_page(prefix: &str, url: &str) -> (Fixture, cdp::Client, String) {
    let mut fixture = Fixture::new(prefix);
    fixture.spawn_daemon();
    fixture.wait_json();
    let mut client = fixture.connect();
    let session = load_page(&mut client, url, Duration::from_secs(10));
    (fixture, client, session)
}

pub struct Fixture {
    runtime: PathBuf,
    data: PathBuf,
    children: Vec<Child>,
}

impl Fixture {
    pub fn runtime(&self) -> &std::path::Path {
        &self.runtime
    }

    pub fn data(&self) -> &std::path::Path {
        &self.data
    }

    pub fn children(&mut self) -> &mut [Child] {
        &mut self.children
    }

    pub fn append_child(&mut self, child: Child) {
        self.children.push(child);
    }

    pub fn new(prefix: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{prefix}-{stamp}"));
        let runtime = root.join("run");
        let data = root.join("data");
        std::fs::create_dir_all(&runtime).expect("runtime");
        std::fs::create_dir_all(&data).expect("data");
        Self {
            runtime,
            data,
            children: Vec::new(),
        }
    }

    pub fn spawn_daemon(&mut self) -> u32 {
        self.spawn_daemon_with(&[])
    }

    pub fn spawn_daemon_with_env(&mut self, key: &str, value: &str) -> u32 {
        self.spawn_daemon_with(&[(key, value)])
    }

    pub fn spawn_daemon_with(&mut self, env: &[(&str, &str)]) -> u32 {
        let child = Command::new(env!("CARGO_BIN_EXE_tinybrowser"))
            .args(["--profile=default"])
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_DATA_HOME", &self.data)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");
        let id = child.id();
        self.children.push(child);
        id
    }

    pub fn wait_json(&self) -> serde_json::Value {
        let path = self.runtime.join("tinybrowser/default/daemon.json");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            {
                return value;
            }
            assert!(Instant::now() < deadline, "daemon.json missing");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn endpoint(&self) -> std::net::SocketAddr {
        let info = self.wait_json();
        let port = u16::try_from(info["port"].as_u64().expect("port")).expect("port");
        format!("127.0.0.1:{port}").parse().expect("addr")
    }

    pub fn connect(&self) -> cdp::Client {
        cdp::Client::connect(self.endpoint()).expect("cdp connect")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(text) =
            std::fs::read_to_string(self.runtime.join("tinybrowser/default/daemon.json"))
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(pid) = value["pid"].as_u64()
        {
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        if let Some(root) = self.runtime.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
