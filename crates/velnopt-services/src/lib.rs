//! Network, Bluetooth, audio, battery, and tray state, plus a few explicit controls.
//!
//! One D-Bus worker (NetworkManager, BlueZ, UPower) and one PulseAudio-protocol
//! worker publish the latest bounded [`Snapshot`] and wake the caller with a
//! coalescing Calloop ping. It's all event driven, nothing polls, and nothing
//! changes the host's state unless the caller sends a [`Request`].

pub mod apps;
mod audio;
mod dbus;
mod icon;
mod tray;

use std::{
    io,
    sync::{Arc, Mutex},
};

use calloop::ping::{Ping, PingSource, make_ping};

/// Longest display string we take from a service, in UTF-8 bytes.
const MAX_TEXT: usize = 64;

/// Which modules to run. A disabled module opens no connections.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Enabled {
    /// NetworkManager on the system bus.
    pub network: bool,
    /// BlueZ on the system bus.
    pub bluetooth: bool,
    /// The default PulseAudio (or pipewire-pulse) sink.
    pub audio: bool,
    /// UPower's display device on the system bus.
    pub battery: bool,
    /// The system tray (StatusNotifierItem and dbusmenu) on the session bus.
    pub tray: bool,
}

/// The state of one module.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Status<T> {
    /// Not queried yet. A disabled module stays in this state for good.
    #[default]
    Pending,
    /// The service or bus isn't running or can't be reached. Holds a sanitized reason.
    Unavailable(String),
    /// The service refused the read.
    PermissionDenied,
    /// The service is running but has no device (no adapter, battery, or sink).
    Missing,
    /// Current state.
    Ready(T),
}

/// How well the network is connected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connectivity {
    /// No active connection.
    Disconnected,
    /// A connection is being established.
    Connecting,
    /// Connected with working internet access.
    Connected,
    /// Connected locally, behind a portal, or without internet access.
    Limited,
}

impl Connectivity {
    /// Map NetworkManager's `State` and `Connectivity` enumerations.
    fn from_nm(state: u32, connectivity: u32) -> Self {
        match state {
            40 => Self::Connecting,
            // local and site-only connections never have internet access
            50 | 60 => Self::Limited,
            // connectivity 1 (none), 2 (portal), and 3 (limited) all mean no verified
            // access; 0 (unknown) means checking is turned off
            70 if matches!(connectivity, 1..=3) => Self::Limited,
            70 => Self::Connected,
            _ => Self::Disconnected,
        }
    }
}

/// The kind of the primary connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Link {
    /// An 802.11 wireless connection.
    Wifi,
    /// An 802.3 wired connection.
    Ethernet,
    /// Anything else (VPN, bridge, modem, or no connection).
    Other,
}

impl Link {
    fn from_nm(kind: &str) -> Self {
        match kind {
            "802-11-wireless" => Self::Wifi,
            "802-3-ethernet" => Self::Ethernet,
            _ => Self::Other,
        }
    }
}

/// NetworkManager's primary connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Network {
    /// Overall connectivity.
    pub state: Connectivity,
    /// Kind of the primary connection.
    pub kind: Link,
    /// Sanitized connection name, empty when disconnected.
    pub name: String,
    /// Wi-Fi signal strength in percent, when connected over Wi-Fi.
    pub strength: Option<u8>,
}

/// The first Bluetooth adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bluetooth {
    /// Whether the adapter is powered.
    pub powered: bool,
    /// Whether rfkill (or some other block) stops it from being powered.
    pub blocked: bool,
    /// Connected devices, counting at most 64.
    pub connected: u16,
    /// The last power toggle was refused. Cleared when the adapter state changes.
    pub control_denied: bool,
}

/// The default audio sink.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Audio {
    /// Average channel volume in percent of normal, at most 150.
    pub volume: u8,
    /// Whether the sink is muted.
    pub muted: bool,
    /// The last volume or mute change was refused.
    pub control_denied: bool,
}

/// What a battery is doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Charge {
    /// Charging.
    Charging,
    /// Running on battery.
    Discharging,
    /// Fully charged.
    Full,
    /// Plugged in but not charging, or about to start.
    NotCharging,
    /// Anything else.
    Unknown,
}

impl Charge {
    fn from_upower(state: u32) -> Self {
        match state {
            1 => Self::Charging,
            2 => Self::Discharging,
            4 => Self::Full,
            5 | 6 => Self::NotCharging,
            _ => Self::Unknown,
        }
    }
}

/// UPower's aggregated battery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Battery {
    /// Charge level, 0 to 100.
    pub percent: u8,
    /// Charging state.
    pub state: Charge,
}

/// The latest state of every module.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    /// Network state.
    pub network: Status<Network>,
    /// Bluetooth state.
    pub bluetooth: Status<Bluetooth>,
    /// Audio state.
    pub audio: Status<Audio>,
    /// Battery state.
    pub battery: Status<Battery>,
    /// Tray items in registration order, at most 32.
    pub tray: Status<Vec<TrayItem>>,
    /// The open tray menu, if any.
    pub menu: Option<Menu>,
}

/// What a tray item wants the user to know.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemStatus {
    /// Nothing to show.
    Passive,
    /// Normal.
    Active,
    /// Needs the user's attention.
    NeedsAttention,
}

/// A decoded icon: straight (non-premultiplied) RGBA8, at most 256x256.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Icon {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// `width * height * 4` bytes, row-major.
    pub rgba: Arc<[u8]>,
}

/// One StatusNotifierItem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrayItem {
    /// `"<bus name><object path>"`, at most 256 bytes; pass it back in requests.
    pub id: String,
    /// Sanitized title, falling back to the item's `Id`.
    pub title: String,
    /// Item status.
    pub status: ItemStatus,
    /// Pixmap or PNG theme icon. `None` if there is only an SVG, or nothing.
    pub icon: Option<Icon>,
    /// The item shows a menu instead of handling `Activate`.
    pub item_is_menu: bool,
    /// The item exports a dbusmenu.
    pub has_menu: bool,
}

/// What a menu entry is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A plain entry.
    Normal,
    /// A divider.
    Separator,
    /// A check box and whether it is on.
    Check(bool),
    /// A radio button and whether it is selected.
    Radio(bool),
}

/// One visible dbusmenu entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuEntry {
    /// The dbusmenu item id, for [`Request::MenuEvent`].
    pub id: i32,
    /// Sanitized label without mnemonic markers.
    pub label: String,
    /// Whether the entry can be chosen.
    pub enabled: bool,
    /// Entry kind.
    pub kind: EntryKind,
    /// Choosing it opens a submenu. Send [`Request::TrayMenu`] with `parent` set to `id`.
    pub submenu: bool,
}

/// The open tray menu: the children of dbusmenu node `parent`, at most 64.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Menu {
    /// The [`TrayItem::id`] it belongs to.
    pub item: String,
    /// The dbusmenu node shown (0 for the root).
    pub parent: i32,
    /// Visible entries.
    pub entries: Vec<MenuEntry>,
}

/// A control the caller may ask for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// Switch the Bluetooth adapter on if it is off, and off if it is on.
    ToggleBluetooth,
    /// Toggle mute on the default sink.
    ToggleMute,
    /// Add a percent delta to the default sink's volume. The result is clamped
    /// to 0..=100 and applied to every channel equally.
    AdjustVolume(i8),
    /// Activate a tray item, or open its menu if it only has a menu.
    TrayActivate(String),
    /// Give a tray item its secondary activation.
    TraySecondary(String),
    /// Open the menu of a tray item at dbusmenu node `parent` (0 for the root).
    TrayMenu {
        /// A [`TrayItem::id`].
        item: String,
        /// The node whose children to show.
        parent: i32,
    },
    /// Choose an entry of the open menu, then close it.
    MenuEvent {
        /// The item whose menu is open.
        item: String,
        /// A [`MenuEntry::id`] of the open menu.
        entry: i32,
    },
    /// Close the open menu without choosing anything.
    CloseMenu,
}

/// Handle to the running workers. Dropping it cancels and joins them.
pub struct Services {
    state: Arc<Mutex<Arc<Snapshot>>>,
    dbus: Option<dbus::Handle>,
    audio: Option<audio::Handle>,
    tray: Option<tray::Handle>,
}

impl Services {
    /// Start the enabled modules.
    ///
    /// Insert the returned source into Calloop and call [`Self::snapshot`] when it
    /// pings. Changes coalesce, so there is no unbounded queue. A missing service shows
    /// up in the snapshot rather than as an error. Bus failures retry at most once every
    /// 2 seconds.
    ///
    /// # Errors
    ///
    /// Returns an error if the ping source or a worker thread can't be created.
    pub fn new(enabled: Enabled) -> io::Result<(Self, PingSource)> {
        Self::start(
            enabled,
            dbus::Bus::System,
            dbus::Bus::Session,
            audio::Server::from_env(),
        )
    }

    fn start(
        enabled: Enabled,
        system: dbus::Bus,
        session: dbus::Bus,
        server: audio::Server,
    ) -> io::Result<(Self, PingSource)> {
        let (ping, source) = make_ping()?;
        let shared = Shared {
            state: Arc::new(Mutex::new(Arc::new(Snapshot::default()))),
            ping,
        };
        let dbus = (enabled.network || enabled.bluetooth || enabled.battery)
            .then(|| dbus::Handle::start(system, enabled, shared.clone()))
            .transpose()?;
        let audio = enabled
            .audio
            .then(|| audio::Handle::start(server, shared.clone()))
            .transpose()?;
        let tray = enabled
            .tray
            .then(|| tray::Handle::start(session, shared.clone()))
            .transpose()?;
        Ok((
            Self {
                state: shared.state,
                dbus,
                audio,
                tray,
            },
            source,
        ))
    }

    /// Clone the latest state without waiting for I/O.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(
            &self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Ask a module to do something, without waiting. A request is dropped, not
    /// replayed later, if the module is disabled, its queue of eight is full, or the
    /// service is disconnected.
    pub fn request(&self, request: Request) {
        match request {
            Request::ToggleBluetooth => {
                if let Some(dbus) = &self.dbus {
                    dbus.request(request);
                }
            }
            Request::ToggleMute | Request::AdjustVolume(_) => {
                if let Some(audio) = &self.audio {
                    audio.request(request);
                }
            }
            tray_request => {
                if let Some(tray) = &self.tray {
                    tray.request(tray_request);
                }
            }
        }
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        // signal every worker before the fields join them, so the waits overlap
        self.dbus.iter().for_each(dbus::Handle::cancel);
        self.audio.iter().for_each(audio::Handle::cancel);
        self.tray.iter().for_each(tray::Handle::cancel);
    }
}

/// The published snapshot and the wake-up that every worker shares.
#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<Arc<Snapshot>>>,
    ping: Ping,
}

impl Shared {
    /// Apply `change`, and wake the caller only if the snapshot differs.
    fn update(&self, change: impl FnOnce(&mut Snapshot)) {
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = (**current).clone();
        change(&mut next);
        if next != **current {
            *current = Arc::new(next);
            drop(current);
            self.ping.ping();
        }
    }
}

/// Cut a service string down to one line of at most `limit` UTF-8 bytes.
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

#[cfg(test)]
mod tests;
