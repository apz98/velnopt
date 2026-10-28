//! Mapping tests, plus integration tests against mock services on a private bus
//! and an isolated pipewire-pulse. Nothing here touches the host's services.

pub(crate) mod tray;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use async_io::block_on;
use zbus::{Connection, connection, fdo, interface, zvariant::OwnedObjectPath};

use crate::{
    Audio, Battery, Bluetooth, Charge, Connectivity, Enabled, Link, Network, Request, Services,
    Snapshot, Status, audio, dbus, sanitize,
};

// ---- mappings and bounds ---------------------------------------------------

#[test]
fn network_state_mapping() {
    use Connectivity::*;
    for (state, connectivity, expected) in [
        (10, 0, Disconnected),
        (20, 4, Disconnected),
        (30, 0, Disconnected),
        (40, 0, Connecting),
        (50, 4, Limited),
        (60, 4, Limited),
        (70, 0, Connected),
        (70, 4, Connected),
        (70, 1, Limited),
        (70, 2, Limited),
        (70, 3, Limited),
        (0, 0, Disconnected),
    ] {
        assert_eq!(
            Connectivity::from_nm(state, connectivity),
            expected,
            "{state}/{connectivity}"
        );
    }
}

#[test]
fn link_and_charge_mapping() {
    assert_eq!(Link::from_nm("802-11-wireless"), Link::Wifi);
    assert_eq!(Link::from_nm("802-3-ethernet"), Link::Ethernet);
    assert_eq!(Link::from_nm("vpn"), Link::Other);
    for (state, expected) in [
        (1, Charge::Charging),
        (2, Charge::Discharging),
        (3, Charge::Unknown),
        (4, Charge::Full),
        (5, Charge::NotCharging),
        (6, Charge::NotCharging),
        (0, Charge::Unknown),
        (99, Charge::Unknown),
    ] {
        assert_eq!(Charge::from_upower(state), expected);
    }
}

#[test]
fn sanitize_is_single_line_and_bounded() {
    assert_eq!(sanitize("a\nb\tc\u{2028}d", 64), "a b c d");
    let long = "é".repeat(100);
    let clean = sanitize(&long, 64);
    assert!(clean.len() <= 64 && clean.ends_with('…'));
    assert_eq!(sanitize("short", 64), "short");
}

#[test]
fn namespaces_match_whole_path_components() {
    assert!(dbus::in_namespace("/org/bluez", "/org/bluez"));
    assert!(dbus::in_namespace("/org/bluez/hci0", "/org/bluez"));
    assert!(!dbus::in_namespace("/org/bluez2", "/org/bluez"));
}

#[test]
fn signals_are_attributed_to_their_module() {
    let signal = |path: &str, interface: &str, member: &str| {
        zbus::Message::signal(path, interface, member)
            .unwrap()
            .build(&())
            .unwrap()
    };
    let properties = "org.freedesktop.DBus.Properties";
    assert_eq!(
        dbus::classify(&signal(
            "/org/freedesktop/NetworkManager/AccessPoint/3",
            properties,
            "PropertiesChanged"
        )),
        Some(dbus::Module::Network)
    );
    assert_eq!(
        dbus::classify(&signal("/org/bluez/hci0", properties, "PropertiesChanged")),
        Some(dbus::Module::Bluetooth)
    );
    assert_eq!(
        dbus::classify(&signal(
            "/",
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesAdded"
        )),
        Some(dbus::Module::Bluetooth)
    );
    assert_eq!(
        dbus::classify(&signal(
            "/org/freedesktop/UPower/devices/DisplayDevice",
            properties,
            "PropertiesChanged"
        )),
        Some(dbus::Module::Battery)
    );
    assert_eq!(
        dbus::classify(&signal("/other", properties, "PropertiesChanged")),
        None
    );
    let owner = |sender: &str| {
        zbus::Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameOwnerChanged",
        )
        .unwrap()
        .sender(sender)
        .unwrap()
        .build(&("org.bluez", "", ":1.5"))
        .unwrap()
    };
    assert_eq!(
        dbus::classify(&owner("org.freedesktop.DBus")),
        Some(dbus::Module::Bluetooth)
    );
    // a client forging the bus's signal is ignored
    assert_eq!(dbus::classify(&owner(":1.42")), None);
}

#[test]
fn errors_become_names_and_denials() {
    let denied = zbus::Error::FDO(Box::new(fdo::Error::AccessDenied("secret text".into())));
    assert_eq!(dbus::failure::<()>(&denied), Status::PermissionDenied);
    let other = zbus::Error::FDO(Box::new(fdo::Error::ServiceUnknown("secret text".into())));
    assert_eq!(
        dbus::failure::<()>(&other),
        Status::Unavailable("org.freedesktop.DBus.Error.ServiceUnknown".into())
    );
}

// ---- private bus -----------------------------------------------------------

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A `dbus-daemon` with no service directories, so nothing can get activated.
struct PrivateBus {
    child: Child,
    address: String,
    directory: PathBuf,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let directory = std::env::temp_dir().join(format!(
            "vsb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let config = directory.join("bus.conf");
        fs::write(
            &config,
            format!(
                "<busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                directory.display()
            ),
        )
        .unwrap();
        let mut child = match Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                eprintln!("skipped: dbus-daemon is unavailable ({error})");
                return None;
            }
        };
        let mut address = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut address,
        )
        .unwrap();
        Some(Self {
            child,
            address: address.trim().into(),
            directory,
        })
    }

    fn serve(&self, name: &str) -> connection::Builder<'static> {
        connection::Builder::address(self.address.as_str())
            .unwrap()
            .name(name.to_owned())
            .unwrap()
    }

    fn services(&self, enabled: Enabled) -> Services {
        Services::start(
            enabled,
            dbus::Bus::Address(self.address.clone()),
            dbus::Bus::Address(self.address.clone()),
            audio::Server::at(PathBuf::new()),
        )
        .unwrap()
        .0
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Wait until `check` accepts the snapshot. Panics with the last one if it never does.
fn eventually<T>(services: &Services, check: impl Fn(&Snapshot) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = services.snapshot();
        if let Some(found) = check(&snapshot) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; last snapshot: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn path(text: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(text).unwrap()
}

// ---- mock UPower -----------------------------------------------------------

struct Display {
    percentage: f64,
    state: u32,
    kind: u32,
    present: bool,
}

#[interface(name = "org.freedesktop.UPower.Device")]
impl Display {
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.percentage
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property, name = "Type")]
    fn kind(&self) -> u32 {
        self.kind
    }
    #[zbus(property)]
    fn is_present(&self) -> bool {
        self.present
    }
}

const DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

fn upower(bus: &PrivateBus, display: Display) -> Connection {
    block_on(
        bus.serve("org.freedesktop.UPower")
            .serve_at(DISPLAY, display)
            .unwrap()
            .build(),
    )
    .unwrap()
}

#[test]
fn battery_reports_changes_and_missing_hardware() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = upower(
        &bus,
        Display {
            percentage: 80.4,
            state: 2,
            kind: 2,
            present: true,
        },
    );
    let services = bus.services(Enabled {
        battery: true,
        ..Enabled::default()
    });
    eventually(&services, |s| {
        (s.battery
            == Status::Ready(Battery {
                percent: 80,
                state: Charge::Discharging,
            }))
        .then_some(())
    });
    block_on(async {
        let interface = mock
            .object_server()
            .interface::<_, Display>(DISPLAY)
            .await
            .unwrap();
        interface.get_mut().await.state = 1;
        interface.get_mut().await.percentage = 55.0;
        let emitter = interface.signal_emitter();
        interface.get().await.state_changed(emitter).await.unwrap();
        interface
            .get()
            .await
            .percentage_changed(emitter)
            .await
            .unwrap();
    });
    eventually(&services, |s| {
        (s.battery
            == Status::Ready(Battery {
                percent: 55,
                state: Charge::Charging,
            }))
        .then_some(())
    });
    // modules that weren't enabled stay pending
    assert_eq!(services.snapshot().network, Status::Pending);
}

#[test]
fn battery_missing_when_no_battery_present() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _mock = upower(
        &bus,
        Display {
            percentage: 0.0,
            state: 0,
            kind: 0,
            present: false,
        },
    );
    let services = bus.services(Enabled {
        battery: true,
        ..Enabled::default()
    });
    eventually(&services, |s| (s.battery == Status::Missing).then_some(()));
}

#[test]
fn service_disappearing_is_unavailable_until_it_returns() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(Enabled {
        battery: true,
        ..Enabled::default()
    });
    // not running at startup
    eventually(&services, |s| {
        matches!(s.battery, Status::Unavailable(_)).then_some(())
    });
    let mock = upower(
        &bus,
        Display {
            percentage: 90.0,
            state: 4,
            kind: 2,
            present: true,
        },
    );
    eventually(&services, |s| {
        matches!(s.battery, Status::Ready(_)).then_some(())
    });
    drop(mock);
    eventually(&services, |s| {
        matches!(s.battery, Status::Unavailable(_)).then_some(())
    });
    let _mock = upower(
        &bus,
        Display {
            percentage: 20.0,
            state: 2,
            kind: 2,
            present: true,
        },
    );
    eventually(&services, |s| {
        (s.battery
            == Status::Ready(Battery {
                percent: 20,
                state: Charge::Discharging,
            }))
        .then_some(())
    });
}

// ---- mock NetworkManager ---------------------------------------------------

struct Manager {
    state: u32,
    connectivity: u32,
    primary: OwnedObjectPath,
}

#[interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        self.connectivity
    }
    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        self.primary.clone()
    }
}

struct Active(String);

#[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn id(&self) -> String {
        self.0.clone()
    }
    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "802-11-wireless".into()
    }
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![path("/org/freedesktop/NetworkManager/Devices/1")]
    }
}

struct Wireless;

#[interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        path("/org/freedesktop/NetworkManager/AccessPoint/1")
    }
}

struct Point(u8);

#[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl Point {
    #[zbus(property)]
    fn strength(&self) -> u8 {
        self.0
    }
}

const NM: &str = "/org/freedesktop/NetworkManager";

fn network_manager(bus: &PrivateBus, state: u32, name: &str, strength: u8) -> Connection {
    let primary = if state >= 50 {
        path("/org/freedesktop/NetworkManager/ActiveConnection/1")
    } else {
        path("/")
    };
    block_on(
        bus.serve("org.freedesktop.NetworkManager")
            .serve_at(
                NM,
                Manager {
                    state,
                    connectivity: 4,
                    primary,
                },
            )
            .unwrap()
            .serve_at(
                "/org/freedesktop/NetworkManager/ActiveConnection/1",
                Active(name.into()),
            )
            .unwrap()
            .serve_at("/org/freedesktop/NetworkManager/Devices/1", Wireless)
            .unwrap()
            .serve_at(
                "/org/freedesktop/NetworkManager/AccessPoint/1",
                Point(strength),
            )
            .unwrap()
            .build(),
    )
    .unwrap()
}

#[test]
fn network_reports_wifi_changes_and_disconnect() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = network_manager(&bus, 70, "Home", 61);
    let services = bus.services(Enabled {
        network: true,
        ..Enabled::default()
    });
    eventually(&services, |s| {
        (s.network
            == Status::Ready(Network {
                state: Connectivity::Connected,
                kind: Link::Wifi,
                name: "Home".into(),
                strength: Some(61),
            }))
        .then_some(())
    });
    block_on(async {
        let point = mock
            .object_server()
            .interface::<_, Point>("/org/freedesktop/NetworkManager/AccessPoint/1")
            .await
            .unwrap();
        point.get_mut().await.0 = 33;
        point
            .get()
            .await
            .strength_changed(point.signal_emitter())
            .await
            .unwrap();
        let manager = mock
            .object_server()
            .interface::<_, Manager>(NM)
            .await
            .unwrap();
        manager.get_mut().await.connectivity = 2;
        manager
            .get()
            .await
            .connectivity_changed(manager.signal_emitter())
            .await
            .unwrap();
    });
    eventually(&services, |s| match &s.network {
        Status::Ready(n) => {
            (n.strength == Some(33) && n.state == Connectivity::Limited).then_some(())
        }
        _ => None,
    });
    block_on(async {
        let manager = mock
            .object_server()
            .interface::<_, Manager>(NM)
            .await
            .unwrap();
        manager.get_mut().await.state = 20;
        manager.get_mut().await.primary = path("/");
        manager
            .get()
            .await
            .state_changed(manager.signal_emitter())
            .await
            .unwrap();
        manager
            .get()
            .await
            .primary_connection_changed(manager.signal_emitter())
            .await
            .unwrap();
    });
    eventually(&services, |s| {
        (s.network
            == Status::Ready(Network {
                state: Connectivity::Disconnected,
                kind: Link::Other,
                name: String::new(),
                strength: None,
            }))
        .then_some(())
    });
}

#[test]
fn network_names_are_sanitized() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let hostile = format!("line one\nline two {}", "x".repeat(200));
    let _mock = network_manager(&bus, 70, &hostile, 120);
    let services = bus.services(Enabled {
        network: true,
        ..Enabled::default()
    });
    let network = eventually(&services, |s| match &s.network {
        Status::Ready(n) => Some(n.clone()),
        _ => None,
    });
    assert!(
        network.name.len() <= 64 && !network.name.contains('\n'),
        "{:?}",
        network.name
    );
    assert_eq!(network.strength, Some(100));
}

// ---- mock BlueZ ------------------------------------------------------------

struct Adapter {
    powered: bool,
    state: &'static str,
    deny: bool,
}

#[interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered
    }
    #[zbus(property)]
    fn set_powered(&mut self, value: bool) -> fdo::Result<()> {
        if self.deny {
            return Err(fdo::Error::AccessDenied("rfkill".into()));
        }
        self.powered = value;
        Ok(())
    }
    #[zbus(property)]
    fn power_state(&self) -> String {
        self.state.into()
    }
}

struct Device(bool);

#[interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.0
    }
}

const ADAPTER: &str = "/org/bluez/hci0";

fn bluez(bus: &PrivateBus, adapter: Option<Adapter>, devices: &[bool]) -> Connection {
    let mut builder = bus
        .serve("org.bluez")
        .serve_at("/", fdo::ObjectManager)
        .unwrap();
    if let Some(adapter) = adapter {
        builder = builder.serve_at(ADAPTER, adapter).unwrap();
    }
    for (index, connected) in devices.iter().enumerate() {
        builder = builder
            .serve_at(format!("{ADAPTER}/dev_{index:03}"), Device(*connected))
            .unwrap();
    }
    block_on(builder.build()).unwrap()
}

fn adapter(powered: bool) -> Adapter {
    Adapter {
        powered,
        state: if powered { "on" } else { "off" },
        deny: false,
    }
}

fn bluetooth() -> Enabled {
    Enabled {
        bluetooth: true,
        ..Enabled::default()
    }
}

fn ready(snapshot: &Snapshot) -> Option<Bluetooth> {
    match &snapshot.bluetooth {
        Status::Ready(value) => Some(value.clone()),
        _ => None,
    }
}

#[test]
fn bluetooth_counts_connected_devices_and_follows_changes() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = bluez(&bus, Some(adapter(true)), &[true, false]);
    let services = bus.services(bluetooth());
    eventually(&services, |s| {
        (ready(s)
            == Some(Bluetooth {
                powered: true,
                blocked: false,
                connected: 1,
                control_denied: false,
            }))
        .then_some(())
    });
    block_on(async {
        let device = mock
            .object_server()
            .interface::<_, Device>(format!("{ADAPTER}/dev_001"))
            .await
            .unwrap();
        device.get_mut().await.0 = true;
        device
            .get()
            .await
            .connected_changed(device.signal_emitter())
            .await
            .unwrap();
    });
    eventually(&services, |s| (ready(s)?.connected == 2).then_some(()));
}

#[test]
fn bluetooth_counts_at_most_sixty_four_devices() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _mock = bluez(&bus, Some(adapter(true)), &[true; 70]);
    let services = bus.services(bluetooth());
    assert_eq!(eventually(&services, ready).connected, 64);
}

#[test]
fn bluetooth_blocked_and_missing() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = bluez(
        &bus,
        Some(Adapter {
            powered: false,
            state: "off-blocked",
            deny: false,
        }),
        &[],
    );
    let services = bus.services(bluetooth());
    let value = eventually(&services, ready);
    assert!(value.blocked && !value.powered);
    drop(mock);
    drop(services);
    let _mock = bluez(&bus, None, &[]);
    let services = bus.services(bluetooth());
    eventually(&services, |s| {
        (s.bluetooth == Status::Missing).then_some(())
    });
}

#[test]
fn toggle_bluetooth_powers_the_adapter() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _mock = bluez(&bus, Some(adapter(true)), &[]);
    let services = bus.services(bluetooth());
    eventually(&services, |s| ready(s)?.powered.then_some(()));
    services.request(Request::ToggleBluetooth);
    eventually(&services, |s| (!ready(s)?.powered).then_some(()));
    services.request(Request::ToggleBluetooth);
    eventually(&services, |s| ready(s)?.powered.then_some(()));
}

#[test]
fn refused_toggle_sets_control_denied_until_the_state_changes() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = bluez(
        &bus,
        Some(Adapter {
            powered: true,
            state: "on",
            deny: true,
        }),
        &[],
    );
    let services = bus.services(bluetooth());
    eventually(&services, |s| ready(s)?.powered.then_some(()));
    services.request(Request::ToggleBluetooth);
    eventually(&services, |s| ready(s)?.control_denied.then_some(()));
    assert!(
        ready(&services.snapshot()).unwrap().powered,
        "a refused toggle changes nothing"
    );
    block_on(async {
        let adapter = mock
            .object_server()
            .interface::<_, Adapter>(ADAPTER)
            .await
            .unwrap();
        adapter.get_mut().await.powered = false;
        adapter.get_mut().await.state = "off";
        adapter.get_mut().await.deny = false;
        adapter
            .get()
            .await
            .powered_changed(adapter.signal_emitter())
            .await
            .unwrap();
    });
    eventually(&services, |s| {
        let value = ready(s)?;
        (!value.powered && !value.control_denied).then_some(())
    });
}

struct Refusing;

#[interface(name = "org.freedesktop.DBus.ObjectManager")]
impl Refusing {
    fn get_managed_objects(&self) -> fdo::Result<Vec<u8>> {
        Err(fdo::Error::AccessDenied("no".into()))
    }
}

struct Stalled;

#[interface(name = "org.freedesktop.DBus.ObjectManager")]
impl Stalled {
    async fn get_managed_objects(&self) -> fdo::Result<Vec<u8>> {
        std::future::pending().await
    }
}

#[test]
fn refused_read_is_permission_denied() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _mock = block_on(
        bus.serve("org.bluez")
            .serve_at("/", Refusing)
            .unwrap()
            .build(),
    )
    .unwrap();
    let services = bus.services(bluetooth());
    eventually(&services, |s| {
        (s.bluetooth == Status::PermissionDenied).then_some(())
    });
}

#[test]
fn bluez_disappearing_is_unavailable_then_recovers() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = bluez(&bus, Some(adapter(true)), &[]);
    let services = bus.services(bluetooth());
    eventually(&services, ready);
    drop(mock);
    eventually(&services, |s| {
        matches!(s.bluetooth, Status::Unavailable(_)).then_some(())
    });
    let _mock = bluez(&bus, Some(adapter(false)), &[]);
    eventually(&services, |s| (!ready(s)?.powered).then_some(()));
}

#[test]
fn dropping_services_cancels_a_stalled_bus_call() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _mock = block_on(
        bus.serve("org.bluez")
            .serve_at("/", Stalled)
            .unwrap()
            .build(),
    )
    .unwrap();
    let services = bus.services(Enabled {
        bluetooth: true,
        battery: true,
        ..Enabled::default()
    });
    thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    drop(services);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn unreachable_bus_is_unavailable_for_every_dbus_module() {
    let address = format!("unix:path={}/missing", std::env::temp_dir().display());
    let (services, _source) = Services::start(
        Enabled {
            network: true,
            bluetooth: true,
            battery: true,
            audio: false,
            tray: true,
        },
        dbus::Bus::Address(address.clone()),
        dbus::Bus::Address(address),
        audio::Server::at(PathBuf::new()),
    )
    .unwrap();
    eventually(&services, |s| {
        (matches!(s.network, Status::Unavailable(_))
            && matches!(s.bluetooth, Status::Unavailable(_))
            && matches!(s.battery, Status::Unavailable(_))
            && matches!(s.tray, Status::Unavailable(_)))
        .then_some(())
    });
    let started = Instant::now();
    drop(services);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn disabled_modules_open_nothing() {
    let (services, _source) = Services::new(Enabled::default()).unwrap();
    assert!(services.dbus.is_none() && services.audio.is_none());
    assert_eq!(*services.snapshot(), Snapshot::default());
    services.request(Request::ToggleMute);
    services.request(Request::ToggleBluetooth);
}

// ---- isolated pipewire-pulse -----------------------------------------------

/// A private `pipewire` and `pipewire-pulse` pair in a temporary runtime dir.
struct Pulse {
    children: Vec<Child>,
    directory: PathBuf,
}

impl Pulse {
    fn start() -> Option<Self> {
        let directory = std::env::temp_dir().join(format!(
            "vsp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let mut pulse = Self {
            children: Vec::new(),
            directory,
        };
        for program in ["pipewire", "pipewire-pulse"] {
            let mut command = Command::new(program);
            pulse.isolate(&mut command);
            match command.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                Ok(child) => pulse.children.push(child),
                Err(error) => {
                    eprintln!("skipped: {program} is unavailable ({error})");
                    return None;
                }
            }
        }
        let socket = pulse.socket();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            if Instant::now() > deadline {
                eprintln!("skipped: isolated pipewire-pulse did not start");
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let loaded = pulse.pactl(&["load-module", "module-null-sink", "sink_name=velnopt_test"]);
        // with no session manager the server can't set a default sink, but it
        // reports the only sink as the default
        if !loaded {
            eprintln!("skipped: could not create a null sink");
            return None;
        }
        Some(pulse)
    }

    fn isolate(&self, command: &mut Command) {
        command
            .env("XDG_RUNTIME_DIR", &self.directory)
            .env("XDG_CONFIG_HOME", self.directory.join("config"))
            .env("HOME", &self.directory)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/none", self.directory.display()),
            )
            .env_remove("PULSE_SERVER")
            .env_remove("PULSE_RUNTIME_PATH");
    }

    fn socket(&self) -> PathBuf {
        self.directory.join("pulse/native")
    }

    fn pactl(&self, arguments: &[&str]) -> bool {
        let mut command = Command::new("pactl");
        self.isolate(&mut command);
        command
            .env("PULSE_SERVER", format!("unix:{}", self.socket().display()))
            .args(arguments)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

impl Drop for Pulse {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn level(snapshot: &Snapshot) -> Option<Audio> {
    match &snapshot.audio {
        Status::Ready(audio) => Some(audio.clone()),
        _ => None,
    }
}

fn audio_services(socket: &Path) -> Services {
    Services::start(
        Enabled {
            audio: true,
            ..Enabled::default()
        },
        dbus::Bus::System,
        dbus::Bus::System,
        audio::Server::at(socket.to_owned()),
    )
    .unwrap()
    .0
}

#[test]
fn real_server_handshake_and_loss() {
    let Some(mut pulse) = Pulse::start() else {
        return;
    };
    let services = audio_services(&pulse.socket());
    // with no session manager the only default is the literal alias, which names
    // no sink. The handshake and queries still work against pipewire.
    eventually(&services, |s| (s.audio == Status::Missing).then_some(()));
    let mut server = pulse.children.pop().unwrap();
    server.kill().unwrap();
    server.wait().unwrap();
    eventually(&services, |s| {
        matches!(s.audio, Status::Unavailable(_)).then_some(())
    });
    let mut command = Command::new("pipewire-pulse");
    pulse.isolate(&mut command);
    pulse.children.push(
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    eventually(&services, |s| (s.audio == Status::Missing).then_some(()));
    let started = Instant::now();
    drop(services);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn audio_without_a_server_is_unavailable() {
    let services = audio_services(Path::new("/nonexistent/velnopt/pulse"));
    eventually(&services, |s| {
        matches!(s.audio, Status::Unavailable(_)).then_some(())
    });
    let started = Instant::now();
    drop(services);
    assert!(started.elapsed() < Duration::from_secs(1));
}

// ---- mock PulseAudio server --------------------------------------------------

use std::{
    io::BufReader,
    os::unix::net::{UnixListener, UnixStream},
    sync::{Arc, Mutex},
};

use pulseaudio::protocol::{
    self as pa, AuthReply, ChannelVolume, Command as PaCommand, ServerInfo, SetClientNameReply,
    SinkInfo, SubscriptionEvent, SubscriptionEventFacility, SubscriptionEventType, Volume,
};

#[derive(Clone)]
struct Sink {
    volumes: Vec<u32>,
    muted: bool,
    deny: bool,
    has_default: bool,
}

/// Serves one default sink over the native protocol, on a private socket.
struct MockPulse {
    socket: PathBuf,
    sink: Arc<Mutex<Sink>>,
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
}

impl MockPulse {
    fn start() -> Self {
        let socket = std::env::temp_dir().join(format!(
            "vsm-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let sink = Arc::new(Mutex::new(Sink {
            volumes: vec![0x1_0000; 2],
            muted: false,
            deny: false,
            has_default: true,
        }));
        let subscribers = Arc::new(Mutex::new(Vec::new()));
        let (state, listeners) = (Arc::clone(&sink), Arc::clone(&subscribers));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (state, listeners) = (Arc::clone(&state), Arc::clone(&listeners));
                thread::spawn(move || {
                    let _ = serve(stream, &state, &listeners);
                });
            }
        });
        Self {
            socket,
            sink,
            subscribers,
        }
    }

    /// Change the sink the way another client would, and notify subscribers.
    fn change(&self, change: impl FnOnce(&mut Sink)) {
        change(&mut self.sink.lock().unwrap());
        notify(&self.subscribers);
    }
}

impl Drop for MockPulse {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

fn notify(subscribers: &Mutex<Vec<UnixStream>>) {
    for mut stream in subscribers.lock().unwrap().iter() {
        let _ = pa::write_command_message(
            &mut stream,
            u32::MAX,
            &PaCommand::SubscribeEvent(SubscriptionEvent {
                event_facility: SubscriptionEventFacility::Sink,
                event_type: SubscriptionEventType::Changed,
                index: Some(0),
            }),
            pa::MAX_VERSION,
        );
    }
}

fn serve(
    stream: UnixStream,
    sink: &Mutex<Sink>,
    subscribers: &Mutex<Vec<UnixStream>>,
) -> Result<(), pa::ProtocolError> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let version = pa::MAX_VERSION;
    loop {
        let (seq, command) = pa::read_command_message(&mut reader, version)?;
        match command {
            PaCommand::Auth(_) => pa::write_reply_message(
                &mut writer,
                seq,
                &AuthReply {
                    version,
                    use_memfd: false,
                    use_shm: false,
                },
                version,
            )?,
            PaCommand::SetClientName(_) => {
                pa::write_reply_message(
                    &mut writer,
                    seq,
                    &SetClientNameReply { client_id: 1 },
                    version,
                )?;
            }
            PaCommand::Subscribe(_) => {
                subscribers.lock().unwrap().push(writer.try_clone()?);
                pa::write_ack_message(&mut writer, seq)?;
            }
            PaCommand::GetServerInfo => {
                let name = sink.lock().unwrap().has_default.then(|| c"mock".to_owned());
                let info = ServerInfo {
                    default_sink_name: name,
                    ..ServerInfo::default()
                };
                pa::write_reply_message(&mut writer, seq, &info, version)?;
            }
            PaCommand::GetSinkInfo(_) => {
                let state = sink.lock().unwrap().clone();
                let mut info = SinkInfo::new_dummy(0);
                info.name = c"mock".to_owned();
                info.muted = state.muted;
                info.cvolume = ChannelVolume::empty();
                for volume in state.volumes {
                    info.cvolume.push(Volume::from_u32_clamped(volume));
                }
                pa::write_reply_message(&mut writer, seq, &info, version)?;
            }
            PaCommand::SetSinkVolume(_) | PaCommand::SetSinkMute(_) => {
                let mut state = sink.lock().unwrap();
                if state.deny {
                    pa::write_error(&mut writer, seq, &pa::PulseError::AccessDenied)?;
                    continue;
                }
                match command {
                    PaCommand::SetSinkVolume(params) => {
                        state.volumes = params
                            .volume
                            .channels()
                            .iter()
                            .map(Volume::as_u32)
                            .collect();
                    }
                    PaCommand::SetSinkMute(params) => state.muted = params.mute,
                    _ => unreachable!(),
                }
                drop(state);
                pa::write_ack_message(&mut writer, seq)?;
                notify(subscribers);
            }
            _ => pa::write_error(&mut writer, seq, &pa::PulseError::Command)?,
        }
    }
}

#[test]
fn audio_reads_and_controls_the_default_sink() {
    let mock = MockPulse::start();
    let services = audio_services(&mock.socket);
    let first = eventually(&services, level);
    assert_eq!(
        (first.volume, first.muted, first.control_denied),
        (100, false, false)
    );

    services.request(Request::ToggleMute);
    eventually(&services, |s| level(s)?.muted.then_some(()));
    services.request(Request::ToggleMute);
    eventually(&services, |s| (!level(s)?.muted).then_some(()));

    services.request(Request::AdjustVolume(-30));
    eventually(&services, |s| (level(s)?.volume == 70).then_some(()));
    assert_eq!(
        mock.sink.lock().unwrap().volumes,
        vec![0x1_0000 * 70 / 100; 2]
    );
    services.request(Request::AdjustVolume(100));
    eventually(&services, |s| (level(s)?.volume == 100).then_some(()));

    // a change by another client arrives as a subscription event, and uneven
    // channels report their average
    mock.change(|sink| sink.volumes = vec![0x4000, 0xC000]);
    eventually(&services, |s| (level(s)?.volume == 50).then_some(()));
    mock.change(|sink| sink.volumes = vec![0x1_8000; 2]);
    eventually(&services, |s| (level(s)?.volume == 150).then_some(()));

    let started = Instant::now();
    drop(services);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn refused_audio_change_sets_control_denied_until_the_next_success() {
    let mock = MockPulse::start();
    mock.change(|sink| sink.deny = true);
    let services = audio_services(&mock.socket);
    eventually(&services, level);
    services.request(Request::ToggleMute);
    eventually(&services, |s| level(s)?.control_denied.then_some(()));
    assert!(!level(&services.snapshot()).unwrap().muted);
    mock.sink.lock().unwrap().deny = false;
    services.request(Request::ToggleMute);
    eventually(&services, |s| {
        let audio = level(s)?;
        (audio.muted && !audio.control_denied).then_some(())
    });
}

#[test]
fn audio_without_a_default_sink_is_missing_then_follows_it() {
    let mock = MockPulse::start();
    mock.sink.lock().unwrap().has_default = false;
    let services = audio_services(&mock.socket);
    eventually(&services, |s| (s.audio == Status::Missing).then_some(()));
    mock.change(|sink| sink.has_default = true);
    eventually(&services, level);
    // requests with no sink do nothing, and don't fail
    mock.change(|sink| sink.has_default = false);
    eventually(&services, |s| (s.audio == Status::Missing).then_some(()));
    services.request(Request::ToggleMute);
    services.request(Request::AdjustVolume(5));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(services.snapshot().audio, Status::Missing);
}
