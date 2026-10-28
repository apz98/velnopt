use super::*;
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::DirBuilderExt, net::UnixListener},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[test]
fn parses_sorted_state_and_sanitizes_untrusted_display_text() {
    let snapshot = Snapshot::from_json(
        br#"[{"id":2,"name":"work\nspace","extra":true,"monitor":"DP-1"},{"id":1,"name":"1"}]"#,
        br#"[{"name":"eDP-1","focused":false,"activeWorkspace":{"id":1,"name":"1"}},{"name":"DP-1","focused":true,"activeWorkspace":{"id":2,"name":"work"}}]"#,
        br#"{"id":2,"name":"work"}"#,
        br#"{"title":"Private\u001b[31m\ntitle,>>value","extra":123}"#,
    )
    .unwrap();
    assert!(snapshot.connected());
    assert_eq!(snapshot.workspaces()[0].id(), 1);
    assert_eq!(snapshot.workspaces()[1].name(), "work space");
    assert_eq!(snapshot.active_workspace(), Some(2));
    assert_eq!(snapshot.workspaces()[1].monitor(), Some("DP-1"));
    assert_eq!(snapshot.workspaces()[0].monitor(), None);
    assert_eq!(snapshot.monitors()[0].name(), "DP-1");
    assert!(snapshot.monitor("DP-1").unwrap().focused());
    assert_eq!(
        snapshot.monitor("eDP-1").unwrap().active_workspace(),
        Some(1)
    );
    assert!(snapshot.monitor("HDMI-A-1").is_none());
    assert_eq!(snapshot.window_title(), Some("Private [31m title,>>value"));
    let desktop = Snapshot::from_json(b"[]", b"[]", b"{}", b"{}").unwrap();
    assert!(desktop.connected());
    assert_eq!(desktop.window_title(), None);
    let active = Snapshot::from_json(b"[]", b"[]", br#"{"id":7,"name":"7"}"#, b"{}").unwrap();
    assert_eq!(active.workspaces()[0].id(), 7);
}

#[test]
fn enforces_response_limits_types_and_workspace_uniqueness() {
    for workspaces in [
        b"not JSON".as_slice(),
        b"{}",
        br#"[{"id":1,"name":"1"},{"id":1,"name":"duplicate"}]"#,
    ] {
        assert!(Snapshot::from_json(workspaces, b"[]", b"{}", b"{}").is_err());
    }
    assert!(Snapshot::from_json(b"[]", b"[]", b"[]", b"{}").is_err());
    assert!(Snapshot::from_json(b"[]", b"[]", b"{}", b"[null]").is_err());
    assert!(Snapshot::from_json(b"[]", b"{}", b"{}", b"{}").is_err());
    assert!(Snapshot::from_json(b"[]", br#"[{"name":"A"},{"name":"A"}]"#, b"{}", b"{}").is_err());
    let monitors = (0..33)
        .map(|index| serde_json::json!({"name":format!("M-{index}")}))
        .collect::<Vec<_>>();
    assert!(
        Snapshot::from_json(b"[]", &serde_json::to_vec(&monitors).unwrap(), b"{}", b"{}").is_err()
    );
    assert!(Snapshot::from_json(&vec![b' '; MAX_REPLY_BYTES + 1], b"[]", b"{}", b"{}").is_err());
    let workspaces = (1..=257)
        .map(|id| serde_json::json!({"id":id,"name":id.to_string()}))
        .collect::<Vec<_>>();
    assert!(
        Snapshot::from_json(
            &serde_json::to_vec(&workspaces).unwrap(),
            b"[]",
            b"{}",
            b"{}"
        )
        .is_err()
    );
}

#[test]
fn truncation_preserves_utf8_and_bounds() {
    let title = "🦀".repeat(1000);
    let names = serde_json::to_vec(&serde_json::json!([{"id":1,"name":title}])).unwrap();
    let window = serde_json::to_vec(&serde_json::json!({"title":title})).unwrap();
    let state = Snapshot::from_json(&names, b"[]", b"{}", &window).unwrap();
    assert!(state.workspaces()[0].name().len() <= 128);
    assert!(state.window_title().unwrap().len() <= 1024);
    assert!(state.window_title().unwrap().ends_with('…'));
}

#[test]
fn rejects_invalid_session_paths() {
    for signature in ["", "..", "../other", "a/b", "a\n", "a\0"] {
        assert!(Endpoints::new(Path::new("/run/user/1000"), signature).is_err());
    }
    assert!(Endpoints::new(Path::new("relative"), "valid_123").is_err());
    assert!(Endpoints::new(Path::new("/run/user/1000"), "abc-123_456").is_ok());
}

fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for mock IPC state"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Clone)]
struct Replies {
    workspaces: Vec<u8>,
    active: Vec<u8>,
    window: Vec<u8>,
    dispatch: Vec<u8>,
    stall: bool,
}
impl Default for Replies {
    fn default() -> Self {
        Self {
            workspaces: br#"[{"id":1,"name":"1"},{"id":2,"name":"2"}]"#.to_vec(),
            active: br#"{"id":1,"name":"1"}"#.to_vec(),
            window: br#"{"title":"Initial window"}"#.to_vec(),
            dispatch: b"ok".to_vec(),
            stall: false,
        }
    }
}

struct Server {
    directory: PathBuf,
    replies: Arc<Mutex<Replies>>,
    events: Arc<Mutex<Vec<UnixStream>>>,
    requests: Arc<AtomicUsize>,
    dispatched: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl Server {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            env::temp_dir().join(format!("velnopt-hypr-{}-{nonce:x}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let command = UnixListener::bind(directory.join("command")).unwrap();
        let event = UnixListener::bind(directory.join("events")).unwrap();
        command.set_nonblocking(true).unwrap();
        event.set_nonblocking(true).unwrap();
        let replies = Arc::new(Mutex::new(Replies::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let dispatch_log = Arc::clone(&dispatched);
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        let shutdown = Arc::clone(&stop);
        let data = Arc::clone(&replies);
        let request_count = Arc::clone(&requests);
        workers.push(thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match command.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(200)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_millis(200)))
                            .unwrap();
                        let mut request = Vec::new();
                        let commands = [
                            b"j/workspaces".as_slice(),
                            b"j/monitors",
                            b"j/activeworkspace",
                            b"j/activewindow",
                        ];
                        loop {
                            let mut bytes = [0; 64];
                            match stream.read(&mut bytes) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => request.extend_from_slice(&bytes[..count]),
                            }
                            if commands.contains(&request.as_slice())
                                // the client sends each short request in a single write
                                || request.starts_with(b"dispatch ")
                                || request.len() > 64
                            {
                                break;
                            }
                        }
                        request_count.fetch_add(1, Ordering::Relaxed);
                        while data.lock().unwrap().stall && !shutdown.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(2));
                        }
                        let data = data.lock().unwrap().clone();
                        let reply = match request.as_slice() {
                            b"j/workspaces" => data.workspaces,
                            b"j/monitors" => br#"[{"name":"TEST-1","focused":true}]"#.to_vec(),
                            b"j/activeworkspace" => data.active,
                            b"j/activewindow" => data.window,
                            dispatch if dispatch.starts_with(b"dispatch ") => {
                                dispatch_log.lock().unwrap().push(dispatch.to_vec());
                                data.dispatch
                            }
                            _ => b"{}".to_vec(),
                        };
                        let _ = stream.write_all(&reply);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("mock command socket failed: {error}"),
                }
            }
        }));
        let shutdown = Arc::clone(&stop);
        let clients = Arc::clone(&events);
        workers.push(thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match event.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_write_timeout(Some(Duration::from_millis(200)))
                            .unwrap();
                        clients.lock().unwrap().push(stream);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("mock event socket failed: {error}"),
                }
            }
        }));
        Self {
            directory,
            replies,
            events,
            requests,
            dispatched,
            stop,
            workers,
        }
    }

    fn subscribe(&self) -> Subscription {
        Subscription::start(Ok(Endpoints {
            command: self.directory.join("command"),
            events: self.directory.join("events"),
        }))
        .unwrap()
        .0
    }

    fn event(&self, bytes: &[u8]) {
        eventually(|| !self.events.lock().unwrap().is_empty());
        self.events
            .lock()
            .unwrap()
            .retain_mut(|stream| stream.write_all(bytes).is_ok());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.events.lock().unwrap().clear();
        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn loads_updates_coalesces_and_reconnects_without_a_consumer_queue() {
    let server = Server::new();
    let subscription = server.subscribe();
    eventually(|| subscription.snapshot().window_title() == Some("Initial window"));
    server.replies.lock().unwrap().window =
        br#"{"title":"Updated, title >> still valid"}"#.to_vec();
    server.event(b"windowti");
    server.event(b"tlev2>>abc,ignored,payload\n");
    eventually(|| subscription.snapshot().window_title() == Some("Updated, title >> still valid"));
    server.replies.lock().unwrap().window = br#"{"title":"Latest state"}"#.to_vec();
    server.event(&b"activewindowv2>>abc\n".repeat(500));
    eventually(|| subscription.snapshot().window_title() == Some("Latest state"));
    server.events.lock().unwrap().clear();
    eventually(|| !subscription.snapshot().connected());
    assert_eq!(subscription.snapshot().window_title(), None);
    server.replies.lock().unwrap().window = br#"{"title":"After reconnect"}"#.to_vec();
    eventually(|| subscription.snapshot().window_title() == Some("After reconnect"));
}

#[test]
fn failed_queries_clear_stale_state_and_recover() {
    let server = Server::new();
    let subscription = server.subscribe();
    eventually(|| subscription.snapshot().connected());
    server.replies.lock().unwrap().workspaces =
        b"not JSON private-title-must-not-be-logged".to_vec();
    server.event(b"workspace>>2\n");
    eventually(|| !subscription.snapshot().connected());
    assert!(subscription.snapshot().workspaces().is_empty());
    assert!(
        !subscription
            .snapshot()
            .error()
            .unwrap()
            .contains("private-title")
    );
    server.replies.lock().unwrap().workspaces = Replies::default().workspaces;
    eventually(|| subscription.snapshot().connected());
}

#[test]
fn oversized_reply_is_rejected_and_worker_can_stop() {
    let server = Server::new();
    server.replies.lock().unwrap().workspaces = vec![b'x'; MAX_REPLY_BYTES + 1];
    let subscription = server.subscribe();
    eventually(|| {
        subscription
            .snapshot()
            .error()
            .is_some_and(|error| error.contains("1 MiB"))
    });
}

#[test]
fn cancellation_interrupts_a_stalled_request_and_idle_wait() {
    let server = Server::new();
    server.replies.lock().unwrap().stall = true;
    let subscription = server.subscribe();
    eventually(|| server.requests.load(Ordering::Relaxed) > 0);
    let started = Instant::now();
    drop(subscription);
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "cancellation waited for the request timeout"
    );
    let (subscription, _source) = Subscription::start(Err(invalid("missing session"))).unwrap();
    eventually(|| subscription.snapshot().error().is_some());
    let started = Instant::now();
    drop(subscription);
    assert!(started.elapsed() < Duration::from_millis(750));
}

#[test]
fn stalled_request_times_out_without_blocking_snapshot_reads() {
    let server = Server::new();
    server.replies.lock().unwrap().stall = true;
    let subscription = server.subscribe();
    eventually(|| {
        subscription
            .snapshot()
            .error()
            .is_some_and(|error| error.contains("timed out"))
    });
    assert!(!subscription.snapshot().connected());
}

#[test]
fn switches_only_validated_workspaces() {
    let server = Server::new();
    let subscription = server.subscribe();
    eventually(|| subscription.snapshot().connected());
    assert_eq!(
        subscription
            .snapshot()
            .monitor("TEST-1")
            .map(Monitor::focused),
        Some(true)
    );
    for id in [0, -1, -99, i64::from(i32::MAX) + 1] {
        assert_eq!(
            subscription.switch_workspace(id).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    subscription.switch_workspace(3).unwrap();
    eventually(|| !server.dispatched.lock().unwrap().is_empty());
    assert_eq!(
        server.dispatched.lock().unwrap().as_slice(),
        [b"dispatch workspace 3".to_vec()]
    );
    // a rejected command gets reported, and the session stays up
    server.replies.lock().unwrap().dispatch = b"invalid".to_vec();
    subscription.switch_workspace(2147483647).unwrap();
    eventually(|| server.dispatched.lock().unwrap().len() == 2);
    assert_eq!(
        server.dispatched.lock().unwrap()[1],
        b"dispatch workspace 2147483647"
    );
    assert!(subscription.snapshot().connected());
}
