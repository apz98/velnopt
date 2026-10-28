//! NetworkManager, BlueZ, and UPower over one D-Bus connection and one worker.
//!
//! The worker registers match rules for the enabled services, then re-reads a
//! module whenever one of its signals, or its name owner, changes.

use std::{
    collections::HashMap,
    io,
    thread::{self, JoinHandle},
    time::Duration,
};

use async_channel::{Receiver, Sender};
use async_io::Timer;
use futures_lite::{StreamExt, future::or};
use zbus::{
    Connection, MatchRule, Message, MessageStream, connection, fdo,
    message::Type,
    names::InterfaceName,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::{
    Battery, Bluetooth, Charge, Connectivity, Enabled, Link, MAX_TEXT, Network, Request, Shared,
    Status, sanitize,
};

const RETRY: Duration = Duration::from_secs(2);
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DEVICES: usize = 64;
pub(crate) const MAX_QUEUED: usize = 1024;

const NETWORK_SERVICE: &str = "org.freedesktop.NetworkManager";
const NETWORK_ROOT: &str = "/org/freedesktop/NetworkManager";
const BLUEZ_SERVICE: &str = "org.bluez";
const BLUEZ_ROOT: &str = "/org/bluez";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const UPOWER_SERVICE: &str = "org.freedesktop.UPower";
const UPOWER_ROOT: &str = "/org/freedesktop/UPower";
const UPOWER_DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// Which bus a worker connects to.
#[derive(Clone)]
pub(crate) enum Bus {
    System,
    Session,
    /// A private bus, for tests.
    #[cfg(test)]
    Address(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Module {
    Network,
    Bluetooth,
    Battery,
}

impl Module {
    const ALL: [Self; 3] = [Self::Network, Self::Bluetooth, Self::Battery];

    fn service(self) -> &'static str {
        match self {
            Self::Network => NETWORK_SERVICE,
            Self::Bluetooth => BLUEZ_SERVICE,
            Self::Battery => UPOWER_SERVICE,
        }
    }

    fn root(self) -> &'static str {
        match self {
            Self::Network => NETWORK_ROOT,
            Self::Bluetooth => BLUEZ_ROOT,
            Self::Battery => UPOWER_ROOT,
        }
    }

    fn enabled(self, enabled: Enabled) -> bool {
        match self {
            Self::Network => enabled.network,
            Self::Bluetooth => enabled.bluetooth,
            Self::Battery => enabled.battery,
        }
    }
}

/// The worker thread, plus its cancellation and request channels.
pub(crate) struct Handle {
    cancel: Sender<()>,
    requests: Sender<Request>,
    worker: Option<JoinHandle<()>>,
}

impl Handle {
    pub(crate) fn start(bus: Bus, enabled: Enabled, shared: Shared) -> io::Result<Self> {
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
        let (requests, incoming) = async_channel::bounded(8);
        let worker = thread::Builder::new()
            .name("velnopt-dbus".into())
            .spawn(move || {
                // dropping the losing future abandons any stalled bus call
                async_io::block_on(or(
                    async {
                        let _ = cancelled.recv().await;
                    },
                    run(bus, enabled, shared, incoming),
                ));
            })?;
        Ok(Self {
            cancel,
            requests,
            worker: Some(worker),
        })
    }

    pub(crate) fn request(&self, request: Request) {
        let _ = self.requests.try_send(request);
    }

    pub(crate) fn cancel(&self) {
        self.cancel.close();
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

async fn run(bus: Bus, enabled: Enabled, shared: Shared, requests: Receiver<Request>) {
    loop {
        if let Err(error) = session(&bus, enabled, &shared, &requests).await {
            let reason = failure::<()>(&error);
            shared.update(|snapshot| {
                if enabled.network {
                    snapshot.network = reason.convert();
                }
                if enabled.bluetooth {
                    snapshot.bluetooth = reason.convert();
                }
                if enabled.battery {
                    snapshot.battery = reason.convert();
                }
            });
        }
        Timer::after(RETRY).await;
        // don't replay clicks made while the bus was unreachable
        while requests.try_recv().is_ok() {}
    }
}

/// What the worker remembers about the Bluetooth adapter between updates.
#[derive(Default)]
struct Adapter {
    path: Option<OwnedObjectPath>,
    powered: bool,
    blocked: bool,
    denied: bool,
}

async fn session(
    bus: &Bus,
    enabled: Enabled,
    shared: &Shared,
    requests: &Receiver<Request>,
) -> zbus::Result<()> {
    let conn = connect(bus).await?;
    let mut messages = MessageStream::from(&conn);
    messages.set_max_queued(MAX_QUEUED);
    let daemon = fdo::DBusProxy::new(&conn).await?;
    for rule in rules(enabled)? {
        daemon.add_match_rule(rule).await?;
    }
    let mut adapter = Adapter::default();
    for module in Module::ALL.into_iter().filter(|m| m.enabled(enabled)) {
        refresh(module, &conn, &daemon, shared, &mut adapter).await;
    }
    loop {
        let event = or(async { Event::Message(messages.next().await) }, async {
            Event::Request(requests.recv().await.ok())
        })
        .await;
        match event {
            Event::Message(Some(Ok(message))) => {
                if let Some(module) = classify(&message).filter(|m| m.enabled(enabled)) {
                    refresh(module, &conn, &daemon, shared, &mut adapter).await;
                }
            }
            // a closed or lagging stream may have dropped signals, so start over
            Event::Message(Some(Err(error))) => return Err(error),
            Event::Message(None) => return Err(zbus::Error::Failure("bus closed".into())),
            Event::Request(Some(Request::ToggleBluetooth)) if enabled.bluetooth => {
                toggle_bluetooth(&conn, &mut adapter).await;
                refresh(Module::Bluetooth, &conn, &daemon, shared, &mut adapter).await;
            }
            Event::Request(_) => {}
        }
    }
}

/// Connect with the shared method-call timeout.
pub(crate) async fn connect(bus: &Bus) -> zbus::Result<Connection> {
    let builder = match bus {
        Bus::System => connection::Builder::system()?,
        Bus::Session => connection::Builder::session()?,
        #[cfg(test)]
        Bus::Address(address) => connection::Builder::address(address.as_str())?,
    };
    builder.method_timeout(CALL_TIMEOUT).build().await
}

enum Event {
    Message(Option<zbus::Result<Message>>),
    Request(Option<Request>),
}

/// Signals to receive: each enabled service's object tree, plus owner changes.
fn rules(enabled: Enabled) -> zbus::Result<Vec<MatchRule<'static>>> {
    let mut rules = Vec::new();
    for module in Module::ALL.into_iter().filter(|m| m.enabled(enabled)) {
        rules.push(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .arg(0, module.service())?
                .build(),
        );
        rules.push(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .path_namespace(module.root())?
                .build(),
        );
    }
    if enabled.bluetooth {
        // BlueZ announces devices from the root object
        rules.push(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .interface(OBJECT_MANAGER)?
                .path("/")?
                .build(),
        );
    }
    Ok(rules)
}

/// Which module a signal is about, or `None` for unrelated messages.
pub(crate) fn classify(message: &Message) -> Option<Module> {
    let header = message.header();
    if header.message_type() != Type::Signal {
        return None;
    }
    let path = header.path()?.as_str();
    if path == "/"
        && header
            .interface()
            .is_some_and(|i| i.as_str() == OBJECT_MANAGER)
    {
        return Some(Module::Bluetooth);
    }
    if header
        .member()
        .is_some_and(|m| m.as_str() == "NameOwnerChanged")
    {
        // any client can emit this signal; only the bus's own copy is real
        if header.sender()?.as_str() != "org.freedesktop.DBus" {
            return None;
        }
        let (name, _, _): (String, String, String) = message.body().deserialize().ok()?;
        return Module::ALL.into_iter().find(|m| m.service() == name);
    }
    Module::ALL
        .into_iter()
        .find(|m| in_namespace(path, m.root()))
}

pub(crate) fn in_namespace(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

async fn refresh(
    module: Module,
    conn: &Connection,
    daemon: &fdo::DBusProxy<'_>,
    shared: &Shared,
    adapter: &mut Adapter,
) {
    // ask the bus first, because a method call to a name nobody owns would start the service
    let owner = daemon
        .name_has_owner(module.service().try_into().expect("valid bus name"))
        .await;
    let down = match owner {
        Ok(true) => None,
        Ok(false) => Some(Status::<()>::Unavailable(format!(
            "{} is not running",
            module.service()
        ))),
        Err(error) => Some(failure(&zbus::Error::from(error))),
    };
    if let Some(down) = down {
        if module == Module::Bluetooth {
            *adapter = Adapter::default();
        }
        return shared.update(|snapshot| match module {
            Module::Network => snapshot.network = down.convert(),
            Module::Bluetooth => snapshot.bluetooth = down.convert(),
            Module::Battery => snapshot.battery = down.convert(),
        });
    }
    match module {
        Module::Network => {
            let status = settle(network(conn).await);
            shared.update(|snapshot| snapshot.network = status);
        }
        Module::Bluetooth => {
            let status = settle(bluetooth(conn, adapter).await);
            shared.update(|snapshot| snapshot.bluetooth = status);
        }
        Module::Battery => {
            let status = settle(battery(conn).await);
            shared.update(|snapshot| snapshot.battery = status);
        }
    }
}

fn settle<T>(result: zbus::Result<Option<T>>) -> Status<T> {
    match result {
        Ok(Some(value)) => Status::Ready(value),
        Ok(None) => Status::Missing,
        Err(error) => failure(&error),
    }
}

/// Map an error to `PermissionDenied` or `Unavailable(error name)`. Nothing past
/// the D-Bus error name is kept.
pub(crate) fn failure<T>(error: &zbus::Error) -> Status<T> {
    let name = error_name(error);
    if is_denied(&name) {
        Status::PermissionDenied
    } else {
        Status::Unavailable(sanitize(&name, MAX_TEXT))
    }
}

pub(crate) fn error_name(error: &zbus::Error) -> String {
    use zbus::DBusError;
    match error {
        zbus::Error::MethodError(name, ..) => name.to_string(),
        zbus::Error::FDO(error) => error.name().to_string(),
        zbus::Error::InputOutput(_) => "I/O error".into(),
        _ => "D-Bus error".into(),
    }
}

fn is_denied(name: &str) -> bool {
    matches!(
        name,
        "org.freedesktop.DBus.Error.AccessDenied"
            | "org.bluez.Error.NotAuthorized"
            | "org.bluez.Error.NotPermitted"
    )
}

pub(crate) type Properties = HashMap<String, OwnedValue>;

pub(crate) async fn get_all(
    conn: &Connection,
    service: &str,
    path: &str,
    interface: &'static str,
) -> zbus::Result<Properties> {
    fdo::PropertiesProxy::builder(conn)
        .destination(service)?
        .path(path)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?
        .get_all(InterfaceName::from_static_str(interface)?)
        .await
        .map_err(Into::into)
}

pub(crate) fn value<T: TryFrom<OwnedValue>>(properties: &Properties, name: &str) -> Option<T> {
    T::try_from(properties.get(name)?.try_clone().ok()?).ok()
}

/// An object path property that points at something other than `/`.
pub(crate) fn target(properties: &Properties, name: &str) -> Option<OwnedObjectPath> {
    value::<OwnedObjectPath>(properties, name).filter(|path| path.as_str() != "/")
}

async fn network(conn: &Connection) -> zbus::Result<Option<Network>> {
    let manager = get_all(conn, NETWORK_SERVICE, NETWORK_ROOT, NETWORK_SERVICE).await?;
    let state = Connectivity::from_nm(
        value(&manager, "State").unwrap_or(0),
        value(&manager, "Connectivity").unwrap_or(0),
    );
    let mut network = Network {
        state,
        kind: Link::Other,
        name: String::new(),
        strength: None,
    };
    let Some(primary) = target(&manager, "PrimaryConnection") else {
        return Ok(Some(network));
    };
    let active = get_all(
        conn,
        NETWORK_SERVICE,
        &primary,
        "org.freedesktop.NetworkManager.Connection.Active",
    )
    .await?;
    network.name = sanitize(
        &value::<String>(&active, "Id").unwrap_or_default(),
        MAX_TEXT,
    );
    network.kind = Link::from_nm(&value::<String>(&active, "Type").unwrap_or_default());
    if network.kind == Link::Wifi
        && let Some(device) = value::<Vec<OwnedObjectPath>>(&active, "Devices")
            .and_then(|devices| devices.into_iter().next())
    {
        // the access point can vanish between calls; the name is still worth having
        network.strength = strength(conn, &device).await.ok().flatten();
    }
    Ok(Some(network))
}

async fn strength(conn: &Connection, device: &str) -> zbus::Result<Option<u8>> {
    let wireless = get_all(
        conn,
        NETWORK_SERVICE,
        device,
        "org.freedesktop.NetworkManager.Device.Wireless",
    )
    .await?;
    let Some(point) = target(&wireless, "ActiveAccessPoint") else {
        return Ok(None);
    };
    let point = get_all(
        conn,
        NETWORK_SERVICE,
        &point,
        "org.freedesktop.NetworkManager.AccessPoint",
    )
    .await?;
    Ok(value::<u8>(&point, "Strength").map(|percent| percent.min(100)))
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, Properties>>;

async fn bluetooth(conn: &Connection, state: &mut Adapter) -> zbus::Result<Option<Bluetooth>> {
    let objects: Objects = conn
        .call_method(
            Some(BLUEZ_SERVICE),
            "/",
            Some(OBJECT_MANAGER),
            "GetManagedObjects",
            &(),
        )
        .await?
        .body()
        .deserialize()?;
    let mut objects: Vec<_> = objects.into_iter().collect();
    objects.sort_by(|left, right| left.0.cmp(&right.0));
    let adapter = objects
        .iter()
        .find_map(|(path, interfaces)| Some((path, interfaces.get("org.bluez.Adapter1")?)));
    let Some((path, properties)) = adapter else {
        *state = Adapter::default();
        return Ok(None);
    };
    let powered = value::<bool>(properties, "Powered").unwrap_or(false);
    let blocked = value::<String>(properties, "PowerState").as_deref() == Some("off-blocked");
    let connected = objects
        .iter()
        .filter_map(|(_, interfaces)| interfaces.get("org.bluez.Device1"))
        .take(MAX_DEVICES)
        .filter(|device| value::<bool>(device, "Connected") == Some(true))
        .count();
    // once the adapter state changes, the earlier refusal no longer applies
    if state.path.is_some() && (state.powered, state.blocked) != (powered, blocked) {
        state.denied = false;
    }
    *state = Adapter {
        path: Some(path.clone()),
        powered,
        blocked,
        denied: state.denied,
    };
    Ok(Some(Bluetooth {
        powered,
        blocked,
        connected: u16::try_from(connected).unwrap_or(u16::MAX),
        control_denied: state.denied,
    }))
}

async fn toggle_bluetooth(conn: &Connection, adapter: &mut Adapter) {
    let Some(path) = adapter.path.clone() else {
        return;
    };
    let result = async {
        fdo::PropertiesProxy::builder(conn)
            .destination(BLUEZ_SERVICE)?
            .path(path)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?
            .set(
                InterfaceName::from_static_str("org.bluez.Adapter1")?,
                "Powered",
                Value::from(!adapter.powered),
            )
            .await
            .map_err(zbus::Error::from)
    }
    .await;
    match result {
        Ok(()) => adapter.denied = false,
        Err(error) => {
            let name = error_name(&error);
            adapter.denied = is_denied(&name) || name == "org.bluez.Error.Failed";
        }
    }
}

async fn battery(conn: &Connection) -> zbus::Result<Option<Battery>> {
    let device = get_all(
        conn,
        UPOWER_SERVICE,
        UPOWER_DISPLAY,
        "org.freedesktop.UPower.Device",
    )
    .await?;
    // type 2 is a battery; without one, the display device is just a placeholder
    if value(&device, "IsPresent") != Some(true) || value(&device, "Type") != Some(2_u32) {
        return Ok(None);
    }
    let percent: f64 = value(&device, "Percentage").unwrap_or(0.0);
    Ok(Some(Battery {
        percent: percent.round().clamp(0.0, 100.0) as u8,
        state: Charge::from_upower(value(&device, "State").unwrap_or(0)),
    }))
}

impl<T> Status<T> {
    /// Carry a failure (never `Ready`) over to another module's type.
    fn convert<U>(&self) -> Status<U> {
        match self {
            Self::Unavailable(reason) => Status::Unavailable(reason.clone()),
            Self::PermissionDenied => Status::PermissionDenied,
            Self::Missing => Status::Missing,
            Self::Pending | Self::Ready(_) => Status::Pending,
        }
    }
}
