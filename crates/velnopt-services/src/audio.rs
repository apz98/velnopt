//! The default sink, over the PulseAudio native protocol (pipewire-pulse speaks it too).
//!
//! Two connections. One only receives subscription events. The other is used
//! synchronously for queries and changes, so a reply never gets mixed in with
//! events. The worker blocks in `poll` on the event socket and a wake socket;
//! nothing polls on a timer.

use std::{
    collections::VecDeque,
    ffi::CString,
    fs::File,
    io::{self, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use pulseaudio::protocol::{
    self, AuthParams, ChannelVolume, Command, CommandReply, ProtocolError, PulseError,
    SetDeviceMuteParams, SetDeviceVolumeParams, SinkInfo, SubscriptionEventFacility,
    SubscriptionMask, Volume,
};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::{Audio, MAX_TEXT, Request, Shared, Status, sanitize};

const RETRY: Duration = Duration::from_secs(2);
/// Limit for a server that stalls mid-message. Idle waits use `poll` instead.
const IO_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_QUEUED: usize = 8;
/// PulseAudio cookies are 256 bytes.
const MAX_COOKIE: u64 = 256;
const MAX_VOLUME_PERCENT: u32 = 150;
const NORM: u64 = 0x1_0000;

/// Where the server is, and the authentication cookie to send it.
pub(crate) struct Server {
    socket: Option<PathBuf>,
    cookie: Vec<u8>,
}

impl Server {
    pub(crate) fn from_env() -> Self {
        let mut cookie = Vec::new();
        if let Some(file) = pulseaudio::cookie_path_from_env().and_then(|p| File::open(p).ok()) {
            let _ = file.take(MAX_COOKIE).read_to_end(&mut cookie);
        }
        // pipewire-pulse rejects any other length. A server that authenticates by
        // socket credentials ignores the content.
        cookie.resize(MAX_COOKIE as usize, 0);
        Self {
            socket: pulseaudio::socket_path_from_env(),
            cookie,
        }
    }

    #[cfg(test)]
    pub(crate) fn at(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
            cookie: vec![0; MAX_COOKIE as usize],
        }
    }
}

/// The worker thread, its request queue, and the stream that wakes it.
pub(crate) struct Handle {
    queue: Arc<Mutex<VecDeque<Request>>>,
    wake: UnixStream,
    worker: Option<JoinHandle<()>>,
}

impl Handle {
    pub(crate) fn start(server: Server, shared: Shared) -> io::Result<Self> {
        let (wake, woken) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        woken.set_nonblocking(true)?;
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let requests = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name("velnopt-audio".into())
            .spawn(move || run(&server, &woken, &requests, &shared))?;
        Ok(Self {
            queue,
            wake,
            worker: Some(worker),
        })
    }

    pub(crate) fn request(&self, request: Request) {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if queue.len() < MAX_QUEUED {
            queue.push_back(request);
            drop(queue);
            // a full wake socket already holds an unread notification
            let _ = (&self.wake).write(&[1]);
        }
    }

    /// Closing the writing end wakes the worker with an end-of-file.
    pub(crate) fn cancel(&self) {
        let _ = self.wake.shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Failure {
    Stopped,
    Denied,
    Unavailable(String),
}

impl From<ProtocolError> for Failure {
    fn from(error: ProtocolError) -> Self {
        match error {
            ProtocolError::ServerError(PulseError::AccessDenied | PulseError::AuthKey) => {
                Self::Denied
            }
            // name the kind of failure only; payloads may carry private data
            ProtocolError::ServerError(error) => Self::Unavailable(format!("PulseAudio {error:?}")),
            ProtocolError::Io(error) => Self::from(error),
            ProtocolError::Timeout => Self::Unavailable("PulseAudio timed out".into()),
            _ => Self::Unavailable("PulseAudio protocol error".into()),
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Unavailable(sanitize(
            &format!("PulseAudio {:?}", error.kind()),
            MAX_TEXT,
        ))
    }
}

fn run(server: &Server, woken: &UnixStream, queue: &Mutex<VecDeque<Request>>, shared: &Shared) {
    loop {
        let Err(failure) = session(server, woken, queue, shared);
        let status = match failure {
            Failure::Stopped => return,
            Failure::Denied => Status::PermissionDenied,
            Failure::Unavailable(reason) => Status::Unavailable(reason),
        };
        shared.update(|snapshot| snapshot.audio = status);
        // don't replay clicks made while the server was unreachable
        queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        if matches!(wait(&[], woken, Some(RETRY)), Err(Failure::Stopped)) {
            return;
        }
    }
}

enum Never {}

fn session(
    server: &Server,
    woken: &UnixStream,
    queue: &Mutex<VecDeque<Request>>,
    shared: &Shared,
) -> Result<Never, Failure> {
    let socket = server
        .socket
        .as_ref()
        .ok_or_else(|| Failure::Unavailable("PulseAudio socket not found".into()))?;
    let mut control = Client::connect(socket, &server.cookie)?;
    let mut events = Client::connect(socket, &server.cookie)?;
    events.acknowledge(&Command::Subscribe(
        SubscriptionMask::SINK | SubscriptionMask::SERVER,
    ))?;
    let mut denied = false;
    publish(shared, control.read(denied)?);
    loop {
        let ready = wait(&[events.stream()], woken, None)?;
        if ready[0] {
            let mut changed = false;
            // events already buffered won't make the socket readable again
            loop {
                if let Command::SubscribeEvent(event) = events.next_command()? {
                    changed |= matches!(
                        event.event_facility,
                        SubscriptionEventFacility::Sink | SubscriptionEventFacility::Server
                    );
                }
                if events.reader.buffer().is_empty() {
                    break;
                }
            }
            if changed {
                publish(shared, control.read(denied)?);
            }
        }
        let requests: Vec<_> = queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect();
        if !requests.is_empty() {
            for request in requests {
                denied = match control.apply(request) {
                    Ok(()) => false,
                    Err(ProtocolError::ServerError(PulseError::AccessDenied)) => true,
                    Err(error) => return Err(error.into()),
                };
            }
            publish(shared, control.read(denied)?);
        }
    }
}

fn publish(shared: &Shared, status: Status<Audio>) {
    shared.update(|snapshot| snapshot.audio = status);
}

/// Block until a stream is readable, `timeout` passes, or the handle is cancelled.
fn wait(
    streams: &[&UnixStream],
    woken: &UnixStream,
    timeout: Option<Duration>,
) -> Result<Vec<bool>, Failure> {
    let started = Instant::now();
    loop {
        let mut fds = vec![PollFd::new(woken, PollFlags::IN)];
        fds.extend(
            streams
                .iter()
                .map(|stream| PollFd::new(*stream, PollFlags::IN)),
        );
        let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
        let remaining = remaining
            .map(Timespec::try_from)
            .transpose()
            .map_err(|_| Failure::Unavailable("invalid wait duration".into()))?;
        match poll(&mut fds, remaining.as_ref()) {
            Ok(_) => {
                if !fds[0].revents().is_empty() {
                    // drain the wake bytes; end-of-file means the handle was dropped
                    let mut bytes = [0; 64];
                    loop {
                        match (&*woken).read(&mut bytes) {
                            Ok(0) => return Err(Failure::Stopped),
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
                return Ok(fds[1..].iter().map(|fd| !fd.revents().is_empty()).collect());
            }
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(io::Error::from(error).into()),
        }
    }
}

/// One authenticated protocol connection.
struct Client {
    reader: BufReader<UnixStream>,
    version: u16,
    sequence: u32,
}

impl Client {
    fn connect(socket: &PathBuf, cookie: &[u8]) -> Result<Self, Failure> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut client = Self {
            reader: BufReader::new(stream),
            version: protocol::MAX_VERSION,
            sequence: 0,
        };
        let auth = AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie: cookie.to_vec(),
        };
        let reply: protocol::AuthReply = client.call(&Command::Auth(auth))?;
        client.version = protocol::MAX_VERSION.min(reply.version);
        let mut props = protocol::Props::new();
        props.set(
            protocol::Prop::ApplicationName,
            CString::new("velnopt").expect("no NUL"),
        );
        let _: protocol::SetClientNameReply = client.call(&Command::SetClientName(props))?;
        Ok(client)
    }

    fn stream(&self) -> &UnixStream {
        self.reader.get_ref()
    }

    fn send(&mut self, command: &Command) -> Result<(), ProtocolError> {
        self.sequence += 1;
        protocol::write_command_message(self.reader.get_mut(), self.sequence, command, self.version)
    }

    fn call<R: CommandReply>(&mut self, command: &Command) -> Result<R, ProtocolError> {
        self.send(command)?;
        protocol::read_reply_message(&mut self.reader, self.version).map(|(_, reply)| reply)
    }

    fn acknowledge(&mut self, command: &Command) -> Result<(), ProtocolError> {
        self.send(command)?;
        protocol::read_ack_message(&mut self.reader).map(drop)
    }

    fn next_command(&mut self) -> Result<Command, ProtocolError> {
        protocol::read_command_message(&mut self.reader, self.version).map(|(_, command)| command)
    }

    /// The default sink, or `None` if the server has none.
    fn sink(&mut self) -> Result<Option<SinkInfo>, ProtocolError> {
        let info: protocol::ServerInfo = self.call(&Command::GetServerInfo)?;
        let Some(name) = info.default_sink_name.filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        match self.call(&Command::GetSinkInfo(protocol::GetSinkInfo {
            index: None,
            name: Some(name),
        })) {
            Ok(sink) => Ok(Some(sink)),
            // a default that names no usable sink counts as none (pipewire-pulse without a
            // session manager reports the literal alias)
            Err(ProtocolError::ServerError(PulseError::NoEntity | PulseError::Invalid)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read(&mut self, control_denied: bool) -> Result<Status<Audio>, Failure> {
        Ok(match self.sink()? {
            None => Status::Missing,
            Some(sink) => Status::Ready(Audio {
                volume: percent(sink.cvolume.channels().iter().map(Volume::as_u32)),
                muted: sink.muted,
                control_denied,
            }),
        })
    }

    fn apply(&mut self, request: Request) -> Result<(), ProtocolError> {
        let Some(sink) = self.sink()? else {
            return Ok(());
        };
        match request {
            Request::ToggleMute => self.acknowledge(&Command::SetSinkMute(SetDeviceMuteParams {
                device_index: None,
                device_name: Some(sink.name),
                mute: !sink.muted,
            })),
            Request::AdjustVolume(delta) => {
                let current = percent(sink.cvolume.channels().iter().map(Volume::as_u32));
                let target = Volume::from_u32_clamped(raw(adjust(current, delta)));
                let mut volume = ChannelVolume::empty();
                for _ in sink.cvolume.channels() {
                    volume.push(target);
                }
                self.acknowledge(&Command::SetSinkVolume(SetDeviceVolumeParams {
                    device_index: None,
                    device_name: Some(sink.name),
                    volume,
                }))
            }
            _ => Ok(()),
        }
    }
}

/// Average of the channel volumes, as a rounded and clamped percent of normal.
fn percent(volumes: impl ExactSizeIterator<Item = u32>) -> u8 {
    let count = volumes.len() as u64;
    if count == 0 {
        return 0;
    }
    let total: u64 = volumes.map(u64::from).sum();
    let percent = (total * 100 + count * NORM / 2) / (count * NORM);
    percent.min(u64::from(MAX_VOLUME_PERCENT)) as u8
}

/// Add a percent delta, keeping the result within 0..=100.
fn adjust(current: u8, delta: i8) -> u8 {
    (i16::from(current) + i16::from(delta)).clamp(0, 100) as u8
}

/// Raw protocol volume for a percent of normal.
fn raw(percent: u8) -> u32 {
    (u64::from(percent) * NORM / 100) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_averages_and_clamps() {
        assert_eq!(percent([].into_iter()), 0);
        assert_eq!(percent([0x1_0000].into_iter()), 100);
        assert_eq!(percent([0x8000, 0x1_0000].into_iter()), 75);
        assert_eq!(percent([0x1_0000 * 3].into_iter()), 150);
        assert_eq!(percent([u32::MAX, u32::MAX].into_iter()), 150);
    }

    #[test]
    fn adjust_clamps_to_normal_volume() {
        assert_eq!(adjust(50, 5), 55);
        assert_eq!(adjust(98, 5), 100);
        assert_eq!(adjust(3, -5), 0);
        assert_eq!(adjust(120, -5), 100);
        assert_eq!(adjust(100, i8::MIN), 0);
    }

    #[test]
    fn raw_round_trips_through_percent() {
        for value in 0..=100 {
            assert_eq!(percent([raw(value)].into_iter()), value);
        }
    }
}
