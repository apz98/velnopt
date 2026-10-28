//! Read-only Hyprland state, delivered through a coalescing Calloop ping source.
//!
//! One cancellable worker thread owns the IPC sockets. Consumers keep only the
//! latest bounded snapshot, so talking to the compositor never blocks rendering.

mod transport;

use std::{
    env, io,
    io::Write,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use calloop::ping::{Ping, PingSource, make_ping};
use serde::Deserialize;

const MAX_REPLY_BYTES: usize = 1024 * 1024;
const MAX_WORKSPACES: usize = 256;
const MAX_MONITORS: usize = 32;

/// A workspace as Hyprland reports it, with its name cut down to bounded single-line text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    id: i64,
    name: String,
    monitor: Option<String>,
}

impl Workspace {
    /// Hyprland's workspace identifier.
    pub fn id(&self) -> i64 {
        self.id
    }
    /// Sanitized name, limited to 128 UTF-8 bytes.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Name of the monitor showing this workspace, when Hyprland reports one.
    pub fn monitor(&self) -> Option<&str> {
        self.monitor.as_deref()
    }
}

/// A Hyprland monitor and the workspace it currently shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Monitor {
    name: String,
    active_workspace: Option<i64>,
    focused: bool,
}

impl Monitor {
    /// Hyprland's monitor name, which matches the Wayland output name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The regular workspace visible on this monitor.
    pub fn active_workspace(&self) -> Option<i64> {
        self.active_workspace
    }
    /// Whether this monitor has input focus.
    pub fn focused(&self) -> bool {
        self.focused
    }
}

/// The latest desktop state. An unavailable snapshot carries no stale data.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    connected: bool,
    workspaces: Vec<Workspace>,
    monitors: Vec<Monitor>,
    active_workspace: Option<i64>,
    window_title: Option<String>,
    error: Option<String>,
}

impl Snapshot {
    /// Whether all three state queries completed successfully.
    pub fn connected(&self) -> bool {
        self.connected
    }
    /// Workspaces sorted by identifier, including the active workspace.
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }
    /// Monitors sorted by name.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }
    /// Look up a monitor by its exact output name.
    pub fn monitor(&self, name: &str) -> Option<&Monitor> {
        self.monitors.iter().find(|monitor| monitor.name == name)
    }
    /// Workspace on the currently focused monitor, when reported.
    pub fn active_workspace(&self) -> Option<i64> {
        self.active_workspace
    }
    /// Focused window title, or `None` for no focused window.
    pub fn window_title(&self) -> Option<&str> {
        self.window_title.as_deref()
    }
    /// Connection or query failure. Never contains raw window titles or response contents.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Decode the replies to `j/workspaces`, `j/monitors`, `j/activeworkspace`, and
    /// `j/activewindow`.
    ///
    /// Unknown object fields are ignored, so extra fields from the compositor don't break
    /// parsing. Display strings are sanitized and truncated at UTF-8 boundaries before
    /// they are published.
    ///
    /// # Errors
    ///
    /// Fails on replies over 1 MiB, invalid JSON or types, duplicate workspace IDs,
    /// more than 256 workspaces, and more than 32 monitors. The JSON decoder's
    /// recursion limit stays on.
    pub fn from_json(
        workspaces: &[u8],
        monitors: &[u8],
        active: &[u8],
        window: &[u8],
    ) -> io::Result<Self> {
        let raw: Vec<RawWorkspace> = decode(workspaces, b'[')?;
        if raw.len() > MAX_WORKSPACES {
            return Err(invalid("too many Hyprland workspaces"));
        }
        let raw_monitors: Vec<RawMonitor> = decode(monitors, b'[')?;
        if raw_monitors.len() > MAX_MONITORS {
            return Err(invalid("too many Hyprland monitors"));
        }
        let mut monitors: Vec<_> = raw_monitors
            .into_iter()
            .map(|monitor| Monitor {
                name: sanitize(&monitor.name, 128),
                active_workspace: monitor.active_workspace.and_then(|workspace| workspace.id),
                focused: monitor.focused,
            })
            .collect();
        monitors.sort_by(|left, right| left.name.cmp(&right.name));
        if monitors.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(invalid("duplicate Hyprland monitor name"));
        }
        let active: RawActive = decode(active, b'{')?;
        let window: RawWindow = decode(window, b'{')?;
        let mut workspaces: Vec<_> = raw
            .into_iter()
            .map(|workspace| workspace.into_workspace())
            .collect();
        if let Some(id) = active.id
            && !workspaces.iter().any(|workspace| workspace.id == id)
        {
            if workspaces.len() == MAX_WORKSPACES {
                return Err(invalid("too many Hyprland workspaces"));
            }
            workspaces.push(
                RawWorkspace {
                    id,
                    name: active.name.unwrap_or_else(|| id.to_string()),
                    monitor: active.monitor,
                }
                .into_workspace(),
            );
        }
        workspaces.sort_by_key(|workspace| workspace.id);
        if workspaces.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(invalid("duplicate Hyprland workspace identifier"));
        }
        let window_title = window.title.map(|title| sanitize(&title, 1024));
        Ok(Self {
            connected: true,
            workspaces,
            monitors,
            active_workspace: active.id,
            window_title,
            error: None,
        })
    }

    fn unavailable(error: &io::Error) -> Self {
        Self {
            error: Some(sanitize(&error.to_string(), 512)),
            ..Self::default()
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8], opening: u8) -> io::Result<T> {
    if bytes.len() > MAX_REPLY_BYTES {
        return Err(invalid("Hyprland reply exceeds 1 MiB"));
    }
    if bytes
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .copied()
        != Some(opening)
    {
        return Err(invalid("invalid Hyprland JSON response type"));
    }
    // the response may hold private text, so it stays out of the error
    serde_json::from_slice(bytes).map_err(|_| invalid("invalid Hyprland JSON response"))
}

#[derive(Deserialize)]
struct RawWorkspace {
    id: i64,
    name: String,
    #[serde(default)]
    monitor: Option<String>,
}
impl RawWorkspace {
    fn into_workspace(self) -> Workspace {
        let name = sanitize(&self.name, 128);
        Workspace {
            id: self.id,
            name: if name.trim().is_empty() {
                self.id.to_string()
            } else {
                name
            },
            monitor: self.monitor.map(|monitor| sanitize(&monitor, 128)),
        }
    }
}
#[derive(Deserialize)]
struct RawActive {
    id: Option<i64>,
    name: Option<String>,
    #[serde(default)]
    monitor: Option<String>,
}
#[derive(Deserialize)]
struct RawMonitor {
    name: String,
    #[serde(default)]
    focused: bool,
    #[serde(default, rename = "activeWorkspace")]
    active_workspace: Option<RawActive>,
}
#[derive(Deserialize)]
struct RawWindow {
    title: Option<String>,
}

fn sanitize(source: &str, limit: usize) -> String {
    let mut result = String::with_capacity(source.len().min(limit));
    for character in source.chars() {
        let character = if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            ' '
        } else {
            character
        };
        if result.len() + character.len_utf8() > limit {
            result.truncate(result.floor_char_boundary(limit - 3));
            result.push('…');
            break;
        }
        result.push(character);
    }
    result
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Endpoints {
    command: PathBuf,
    events: PathBuf,
}

impl Endpoints {
    fn from_env() -> io::Result<Self> {
        let runtime =
            env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| invalid("XDG_RUNTIME_DIR is missing"))?;
        let signature = env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| {
            invalid("HYPRLAND_INSTANCE_SIGNATURE is missing or invalid; launch from Hyprland")
        })?;
        Self::new(Path::new(&runtime), &signature)
    }

    fn new(runtime: &Path, signature: &str) -> io::Result<Self> {
        if !runtime.is_absolute()
            || signature.is_empty()
            || signature.len() > 255
            || !signature
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(invalid(
                "invalid Hyprland runtime directory or instance signature",
            ));
        }
        let directory = runtime.join("hypr").join(signature);
        Ok(Self {
            command: directory.join(".socket.sock"),
            events: directory.join(".socket2.sock"),
        })
    }
}

/// A live subscription with one IPC worker and one latest-state slot.
///
/// Dropping it interrupts any connect, read, or retry wait and joins the worker.
pub struct Subscription {
    state: Arc<Mutex<Arc<Snapshot>>>,
    pending: Arc<Mutex<Option<i64>>>,
    cancel: UnixStream,
    commands: UnixStream,
    worker: Option<JoinHandle<()>>,
}

impl Subscription {
    /// Subscribe using the current Hyprland session environment.
    ///
    /// Insert the returned source into Calloop and call [`Self::snapshot`] when it
    /// pings. Repeated changes coalesce, so there is no unbounded event queue. If IPC
    /// is missing or down, the snapshot says so and the panel keeps running. Socket
    /// failures retry every 500 ms.
    ///
    /// # Errors
    ///
    /// Returns an error if the notification or cancellation resources, or the worker
    /// thread, cannot be created. Set up the application's signal masks first.
    pub fn new() -> io::Result<(Self, PingSource)> {
        Self::start(Endpoints::from_env())
    }

    fn start(endpoints: io::Result<Endpoints>) -> io::Result<(Self, PingSource)> {
        let (ping, source) = make_ping()?;
        let (cancel, wake) = UnixStream::pair()?;
        let (commands, command_wake) = UnixStream::pair()?;
        for socket in [&cancel, &wake, &commands, &command_wake] {
            socket.set_nonblocking(true)?;
        }
        let state = Arc::new(Mutex::new(Arc::new(Snapshot::default())));
        let pending = Arc::new(Mutex::new(None));
        let published = Arc::clone(&state);
        let requests = transport::Requests {
            pending: Arc::clone(&pending),
            wake: command_wake,
        };
        let worker = thread::Builder::new()
            .name("velnopt-hyprland".into())
            .spawn(move || {
                transport::run(endpoints, wake, requests, |snapshot| {
                    publish(&published, &ping, snapshot)
                });
            })?;
        Ok((
            Self {
                state,
                pending,
                cancel,
                commands,
                worker: Some(worker),
            },
            source,
        ))
    }

    /// Clone the latest immutable state without waiting for socket I/O.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(
            &self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

impl Subscription {
    /// Ask Hyprland to show a regular workspace, without waiting for socket I/O.
    ///
    /// Only positive 32-bit identifiers are accepted; special and named-only
    /// workspaces can't be addressed. The single pending slot keeps the latest
    /// request, and requests made while IPC is disconnected are dropped.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] for an identifier outside
    /// `1..=2147483647`.
    pub fn switch_workspace(&self, id: i64) -> io::Result<()> {
        if !(1..=i64::from(i32::MAX)).contains(&id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace identifier must be a positive 32-bit integer",
            ));
        }
        *self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(id);
        // a full wake socket already holds an unread notification
        let _ = (&self.commands).write(&[1]);
        Ok(())
    }
}

fn publish(state: &Mutex<Arc<Snapshot>>, ping: &Ping, snapshot: Snapshot) {
    let mut current = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if **current != snapshot {
        *current = Arc::new(snapshot);
        drop(current);
        ping.ping();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = (&self.cancel).write(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
