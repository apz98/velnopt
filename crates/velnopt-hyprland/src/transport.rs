//! Nonblocking Unix socket transport with deadlines and explicit cancellation.

use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with, sockopt},
};

use super::{Endpoints, MAX_REPLY_BYTES, Snapshot, invalid};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_EVENT_BYTES: usize = 64 * 1024;

/// The latest requested workspace, plus the socket that signals a new request.
pub(super) struct Requests {
    pub(super) pending: Arc<Mutex<Option<i64>>>,
    pub(super) wake: UnixStream,
}

impl Requests {
    /// Clear wake notifications and take the latest request.
    fn take(&self) -> Option<i64> {
        let mut bytes = [0; 64];
        while matches!((&self.wake).read(&mut bytes), Ok(count) if count > 0) {}
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

pub(super) fn run(
    endpoints: io::Result<Endpoints>,
    cancel: UnixStream,
    requests: Requests,
    mut publish: impl FnMut(Snapshot),
) {
    let endpoints = match endpoints {
        Ok(endpoints) => endpoints,
        Err(error) => {
            publish(Snapshot::unavailable(&error));
            let _ = wait(None, &cancel, None);
            return;
        }
    };
    loop {
        let result = session(&endpoints, &cancel, &requests, &mut publish);
        if let Err(error) = result {
            if error.kind() == io::ErrorKind::Interrupted {
                return;
            }
            publish(Snapshot::unavailable(&error));
        }
        // don't replay clicks made while the compositor was unreachable
        if requests.take().is_some() {
            eprintln!("Hyprland workspace switch discarded: IPC is disconnected");
        }
        if wait(None, &cancel, Some(RETRY_DELAY)).is_err() {
            return;
        }
    }
}

fn session(
    endpoints: &Endpoints,
    cancel: &UnixStream,
    requests: &Requests,
    publish: &mut impl FnMut(Snapshot),
) -> io::Result<()> {
    // subscribe first, so changes that land during the snapshot queries stay queued
    let mut events = open(&endpoints.events, cancel, Instant::now() + REQUEST_TIMEOUT)?;
    publish(snapshot(endpoints, cancel)?);
    let mut decoder = Events::default();
    loop {
        let ready = wait_any(
            &[(&events, PollFlags::IN), (&requests.wake, PollFlags::IN)],
            cancel,
            None,
        )?;
        if ready[1]
            && let Some(id) = requests.take()
        {
            // the workspace event that follows refreshes state; a failure keeps the session
            match query(
                &endpoints.command,
                format!("dispatch workspace {id}").as_bytes(),
                cancel,
            ) {
                Ok(reply) if reply.trim_ascii() == b"ok" => {}
                Ok(_) => eprintln!("Hyprland rejected a workspace switch"),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(error) => eprintln!("Hyprland workspace switch failed: {error}"),
            }
        }
        if !ready[0] {
            continue;
        }
        let mut refresh = false;
        let mut consumed = 0;
        loop {
            // keep cancellation responsive even during a constant stream of events
            wait(None, cancel, Some(Duration::ZERO))?;
            let mut bytes = [0; 4096];
            match events.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Hyprland event socket disconnected",
                    ));
                }
                Ok(count) => {
                    refresh |= decoder.feed(&bytes[..count])?;
                    consumed += count;
                    if consumed >= MAX_EVENT_BYTES {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if refresh {
            publish(snapshot(endpoints, cancel)?);
        }
    }
}

fn snapshot(endpoints: &Endpoints, cancel: &UnixStream) -> io::Result<Snapshot> {
    let workspaces = query(&endpoints.command, b"j/workspaces", cancel)?;
    let monitors = query(&endpoints.command, b"j/monitors", cancel)?;
    let active = query(&endpoints.command, b"j/activeworkspace", cancel)?;
    let window = query(&endpoints.command, b"j/activewindow", cancel)?;
    Snapshot::from_json(&workspaces, &monitors, &active, &window)
}

fn open(path: &Path, cancel: &UnixStream, deadline: Instant) -> io::Result<UnixStream> {
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    let address = SocketAddrUnix::new(path)?;
    let stream = UnixStream::from(socket);
    match connect(&stream, &address) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::INPROGRESS => {
            ready(&stream, PollFlags::OUT, cancel, deadline)?;
            sockopt::socket_error(&stream)??;
        }
        Err(error) => return Err(error.into()),
    }
    if sockopt::socket_peercred(&stream)?.uid != rustix::process::getuid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Hyprland IPC peer belongs to a different user",
        ));
    }
    Ok(stream)
}

fn query(path: &Path, command: &[u8], cancel: &UnixStream) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut stream = open(path, cancel, deadline)?;
    let mut remaining = command;
    while !remaining.is_empty() {
        ready(&stream, PollFlags::OUT, cancel, deadline)?;
        match stream.write(remaining) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "Hyprland request write failed",
                ));
            }
            Ok(count) => remaining = &remaining[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    let mut reply = Vec::new();
    loop {
        ready(&stream, PollFlags::IN, cancel, deadline)?;
        let mut bytes = [0; 4096];
        match stream.read(&mut bytes) {
            Ok(0) => return Ok(reply),
            Ok(count) => {
                if reply.len() + count > MAX_REPLY_BYTES {
                    return Err(invalid("Hyprland reply exceeds 1 MiB"));
                }
                reply.extend_from_slice(&bytes[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

fn ready(
    stream: &UnixStream,
    interest: PollFlags,
    cancel: &UnixStream,
    deadline: Instant,
) -> io::Result<()> {
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Hyprland IPC request timed out"))?;
    if wait(Some((stream, interest)), cancel, Some(timeout))? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Hyprland IPC request timed out",
        ))
    }
}

/// If the cancellation socket is readable it wins over any ready data.
fn wait(
    stream: Option<(&UnixStream, PollFlags)>,
    cancel: &UnixStream,
    timeout: Option<Duration>,
) -> io::Result<bool> {
    let streams: &[(&UnixStream, PollFlags)] = match &stream {
        Some(stream) => std::slice::from_ref(stream),
        None => &[],
    };
    Ok(wait_any(streams, cancel, timeout)?.first() == Some(&true))
}

/// Report which streams are ready, in order. Cancellation wins.
fn wait_any(
    streams: &[(&UnixStream, PollFlags)],
    cancel: &UnixStream,
    timeout: Option<Duration>,
) -> io::Result<Vec<bool>> {
    let started = Instant::now();
    loop {
        let mut fds = vec![PollFd::new(cancel, PollFlags::IN)];
        for (stream, interest) in streams {
            fds.push(PollFd::new(*stream, *interest));
        }
        let remaining = timeout.map(|duration| duration.saturating_sub(started.elapsed()));
        let timeout = remaining
            .map(Timespec::try_from)
            .transpose()
            .map_err(|_| invalid("invalid IPC wait duration"))?;
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_) => {
                if !fds[0].revents().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Hyprland subscription stopped",
                    ));
                }
                return Ok(fds[1..].iter().map(|fd| !fd.revents().is_empty()).collect());
            }
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[derive(Default)]
struct Events {
    partial: Vec<u8>,
}

impl Events {
    fn feed(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let mut refresh = false;
        for &byte in bytes {
            if byte == b'\n' {
                if !self.partial.is_empty() {
                    let delimiter = self
                        .partial
                        .windows(2)
                        .position(|bytes| bytes == b">>")
                        .ok_or_else(|| invalid("malformed Hyprland event frame"))?;
                    let name = std::str::from_utf8(&self.partial[..delimiter])
                        .map_err(|_| invalid("invalid Hyprland event name"))?;
                    refresh |= matches!(
                        name,
                        "workspace"
                            | "workspacev2"
                            | "focusedmon"
                            | "focusedmonv2"
                            | "activewindow"
                            | "activewindowv2"
                            | "windowtitle"
                            | "windowtitlev2"
                            | "openwindow"
                            | "closewindow"
                            | "movewindow"
                            | "movewindowv2"
                            | "createworkspace"
                            | "createworkspacev2"
                            | "destroyworkspace"
                            | "destroyworkspacev2"
                            | "renameworkspace"
                            | "moveworkspace"
                            | "moveworkspacev2"
                            | "monitoradded"
                            | "monitoraddedv2"
                            | "monitorremoved"
                            | "monitorremovedv2"
                            | "activespecial"
                            | "activespecialv2"
                    );
                }
                self.partial.clear();
            } else {
                if self.partial.len() == MAX_EVENT_BYTES {
                    return Err(invalid("Hyprland event exceeds 64 KiB"));
                }
                self.partial.push(byte);
            }
        }
        Ok(refresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_fragmented_and_batched_event_frames() {
        let mut decoder = Events::default();
        assert!(!decoder.feed(b"worksp").unwrap());
        assert!(
            decoder
                .feed(b"acev2>>2,work\nwindowtitlev2>>abc,a,b>>c\n")
                .unwrap()
        );
        assert!(!decoder.feed(b"unknown>>value\n").unwrap());
        assert!(!decoder.feed(b"activewindow>>partial").unwrap());
        assert!(decoder.feed(b" title\n").unwrap());
    }

    #[test]
    fn rejects_oversized_and_malformed_frames() {
        assert!(
            Events::default()
                .feed(&vec![b'x'; MAX_EVENT_BYTES + 1])
                .is_err()
        );
        assert!(Events::default().feed(b"bad frame\n").is_err());
    }
}
