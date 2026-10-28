//! Tray tests, using mock StatusNotifierItems, watchers, and dbusmenu on a private bus.

use std::{
    collections::HashMap,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_io::block_on;
use zbus::{
    Connection, connection, interface,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, Structure, Value},
};

use super::{PrivateBus, eventually, path};
use crate::{
    Enabled, EntryKind, ItemStatus, Menu, MenuEntry, Request, Snapshot, Status, TrayItem,
    tray::Watcher,
};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "/StatusNotifierItem";

type Log = Arc<Mutex<Vec<String>>>;

struct Item {
    title: String,
    status: &'static str,
    pixmaps: Vec<(i32, i32, Vec<u8>)>,
    icon_name: String,
    theme_path: String,
    is_menu: bool,
    menu: &'static str,
    log: Log,
}

impl Item {
    fn new(title: &str, log: &Log) -> Self {
        Self {
            title: title.into(),
            status: "Active",
            pixmaps: Vec::new(),
            icon_name: String::new(),
            theme_path: String::new(),
            is_menu: false,
            menu: "/",
            log: Arc::clone(log),
        }
    }
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn id(&self) -> String {
        "mock-id".into()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        self.title.clone()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        self.status.into()
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.icon_name.clone()
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        self.pixmaps.clone()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        self.theme_path.clone()
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        self.is_menu
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        path(self.menu)
    }
    fn activate(&self, x: i32, y: i32) {
        self.log.lock().unwrap().push(format!("Activate {x} {y}"));
    }
    fn secondary_activate(&self, x: i32, y: i32) {
        self.log
            .lock()
            .unwrap()
            .push(format!("SecondaryActivate {x} {y}"));
    }
    #[zbus(signal)]
    async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// A property value for the mock menu.
enum Prop {
    Text(&'static str),
    Flag(bool),
    Number(i32),
}

fn node(id: i32, properties: &[(&str, Prop)]) -> Value<'static> {
    let properties: HashMap<String, Value<'static>> = properties
        .iter()
        .map(|(name, value)| {
            let value = match value {
                Prop::Text(text) => Value::from(*text),
                Prop::Flag(flag) => Value::from(*flag),
                Prop::Number(number) => Value::from(*number),
            };
            ((*name).to_owned(), value)
        })
        .collect();
    Value::from(Structure::from((
        id,
        properties,
        Vec::<Value<'static>>::new(),
    )))
}

type Layout = (
    u32,
    (i32, HashMap<String, Value<'static>>, Vec<Value<'static>>),
);

struct DbusMenu {
    log: Log,
}

#[interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    fn about_to_show(&self, id: i32) -> bool {
        self.log.lock().unwrap().push(format!("AboutToShow {id}"));
        false
    }
    fn get_layout(&self, parent: i32, depth: i32, _names: Vec<String>) -> Layout {
        self.log
            .lock()
            .unwrap()
            .push(format!("GetLayout {parent} {depth}"));
        let children = match parent {
            0 => vec![
                node(1, &[("label", Prop::Text("_Open"))]),
                node(2, &[("type", Prop::Text("separator"))]),
                node(
                    3,
                    &[
                        ("label", Prop::Text("Check")),
                        ("toggle-type", Prop::Text("checkmark")),
                        ("toggle-state", Prop::Number(1)),
                    ],
                ),
                node(
                    4,
                    &[
                        ("label", Prop::Text("More")),
                        ("children-display", Prop::Text("submenu")),
                    ],
                ),
                node(
                    5,
                    &[
                        ("label", Prop::Text("Hidden")),
                        ("visible", Prop::Flag(false)),
                    ],
                ),
                node(
                    6,
                    &[
                        ("label", Prop::Text("Ra__dio")),
                        ("toggle-type", Prop::Text("radio")),
                        ("toggle-state", Prop::Number(0)),
                        ("enabled", Prop::Flag(false)),
                    ],
                ),
            ],
            4 => vec![node(41, &[("label", Prop::Text("Child"))])],
            _ => Vec::new(),
        };
        (1, (parent, HashMap::new(), children))
    }
    fn event(&self, id: i32, event: &str, data: Value<'_>, timestamp: u32) {
        self.log
            .lock()
            .unwrap()
            .push(format!("Event {id} {event} {data:?} {timestamp}"));
    }
    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

fn tray() -> Enabled {
    Enabled {
        tray: true,
        ..Enabled::default()
    }
}

fn plain(bus: &PrivateBus) -> connection::Builder<'static> {
    connection::Builder::address(bus.address.as_str()).unwrap()
}

fn items(snapshot: &Snapshot) -> Option<Vec<TrayItem>> {
    match &snapshot.tray {
        Status::Ready(items) => Some(items.clone()),
        _ => None,
    }
}

fn titles(snapshot: &Snapshot) -> Option<Vec<String>> {
    items(snapshot).map(|items| items.into_iter().map(|item| item.title).collect())
}

/// Call the watcher, retrying until our worker has claimed its name.
fn register(connection: &Connection, argument: &str) {
    let mut last = String::new();
    for _ in 0..200 {
        let reply = block_on(connection.call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &argument,
        ));
        match reply {
            Ok(_) => return,
            Err(error) => last = format!("{error:?}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("could not register {argument}: {last}");
}

fn log() -> Log {
    Log::default()
}

fn argb(alpha: u8, red: u8, green: u8, blue: u8, pixels: usize) -> Vec<u8> {
    [alpha, red, green, blue].repeat(pixels)
}

#[test]
fn our_watcher_collects_items_registered_by_name_and_by_path() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    eventually(&services, |s| {
        (s.tray == Status::Ready(Vec::new())).then_some(())
    });

    let calls = log();
    let mut named = Item::new("Alpha", &calls);
    named.status = "NeedsAttention";
    named.pixmaps = vec![
        (16, 16, argb(255, 1, 2, 3, 256)),
        // closest to 32 px, with a pixel order we can recognize
        (30, 30, argb(0x80, 10, 20, 30, 900)),
        (48, 48, argb(255, 4, 5, 6, 2304)),
        // wrong data length: rejected even though it is exactly 32 px
        (32, 32, vec![0; 10]),
    ];
    let alpha = block_on(
        bus.serve("org.example.Alpha")
            .serve_at(ITEM, named)
            .unwrap()
            .build(),
    )
    .unwrap();
    register(&alpha, "org.example.Alpha");
    let found = eventually(&services, |s| items(s).filter(|items| !items.is_empty()));
    let first = &found[0];
    assert_eq!(first.id, "org.example.Alpha/StatusNotifierItem");
    assert_eq!(first.title, "Alpha");
    assert_eq!(first.status, ItemStatus::NeedsAttention);
    assert!(!first.item_is_menu && !first.has_menu);
    let icon = first.icon.as_ref().expect("pixmap icon");
    assert_eq!((icon.width, icon.height), (30, 30));
    assert_eq!(&icon.rgba[..4], &[10, 20, 30, 0x80], "ARGB becomes RGBA");
    assert_eq!(icon.rgba.len(), 30 * 30 * 4);

    // registered by object path, so the bus name is the sender's unique name
    let beta = block_on(
        plain(&bus)
            .serve_at("/org/example/Beta", Item::new("Beta", &calls))
            .unwrap()
            .build(),
    )
    .unwrap();
    register(&beta, "/org/example/Beta");
    eventually(&services, |s| {
        (titles(s)? == ["Alpha", "Beta"]).then_some(())
    });
    let unique = beta.unique_name().unwrap().to_string();
    let found = items(&services.snapshot()).unwrap();
    assert_eq!(found[1].id, format!("{unique}/org/example/Beta"));

    // registering the same item again changes nothing
    register(&alpha, "org.example.Alpha");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(titles(&services.snapshot()).unwrap().len(), 2);

    // the host is registered with our watcher
    let registered: bool = block_on(async {
        let reply = beta
            .call_method(
                Some(WATCHER),
                WATCHER_PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(WATCHER, "IsStatusNotifierHostRegistered"),
            )
            .await
            .unwrap();
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize().unwrap();
        bool::try_from(value).unwrap()
    });
    assert!(registered);
}

#[test]
fn title_signals_update_and_closing_removes_items() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    let mock = block_on(
        bus.serve("org.example.Title")
            .serve_at(ITEM, Item::new("Before", &log()))
            .unwrap()
            .build(),
    )
    .unwrap();
    register(&mock, "org.example.Title");
    eventually(&services, |s| (titles(s)? == ["Before"]).then_some(()));
    block_on(async {
        let item = mock
            .object_server()
            .interface::<_, Item>(ITEM)
            .await
            .unwrap();
        item.get_mut().await.title = "After".into();
        Item::new_title(item.signal_emitter()).await.unwrap();
    });
    eventually(&services, |s| (titles(s)? == ["After"]).then_some(()));
    drop(mock);
    eventually(&services, |s| {
        (s.tray == Status::Ready(Vec::new())).then_some(())
    });
}

#[test]
fn an_external_watcher_is_used_instead_of_ours() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let mock = block_on(
        bus.serve("org.example.Ext")
            .serve_at(ITEM, Item::new("External", &log()))
            .unwrap()
            .build(),
    )
    .unwrap();
    let (items_seen, hosts) = (
        Arc::new(Mutex::new(vec![
            "org.example.Ext/StatusNotifierItem".to_owned(),
        ])),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (changed, _unused) = async_channel::bounded(1);
    let _watcher = block_on(
        bus.serve(WATCHER)
            .serve_at(
                WATCHER_PATH,
                Watcher {
                    items: Arc::clone(&items_seen),
                    hosts: Arc::clone(&hosts),
                    changed,
                },
            )
            .unwrap()
            .build(),
    )
    .unwrap();
    let services = bus.services(tray());
    eventually(&services, |s| (titles(s)? == ["External"]).then_some(()));
    let hosts = hosts.lock().unwrap().clone();
    assert_eq!(
        hosts,
        [format!("org.kde.StatusNotifierHost-{}", std::process::id())]
    );
    drop(mock);
}

#[test]
fn activation_calls_the_item_or_opens_its_menu() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    let calls = log();
    let plain_item = block_on(
        bus.serve("org.example.Plain")
            .serve_at(ITEM, Item::new("Plain", &calls))
            .unwrap()
            .build(),
    )
    .unwrap();
    let mut menu_only = Item::new("MenuOnly", &calls);
    menu_only.is_menu = true;
    menu_only.menu = "/Menu";
    let menu_item = block_on(
        bus.serve("org.example.MenuOnly")
            .serve_at(ITEM, menu_only)
            .unwrap()
            .serve_at(
                "/Menu",
                DbusMenu {
                    log: Arc::clone(&calls),
                },
            )
            .unwrap()
            .build(),
    )
    .unwrap();
    register(&plain_item, "org.example.Plain");
    register(&menu_item, "org.example.MenuOnly");
    eventually(&services, |s| (titles(s)?.len() == 2).then_some(()));

    services.request(Request::TrayActivate(
        "org.example.Plain/StatusNotifierItem".into(),
    ));
    wait_for_log(&calls, "Activate 0 0");
    services.request(Request::TraySecondary(
        "org.example.Plain/StatusNotifierItem".into(),
    ));
    wait_for_log(&calls, "SecondaryActivate 0 0");

    // a menu-only item opens its menu and doesn't get activated
    services.request(Request::TrayActivate(
        "org.example.MenuOnly/StatusNotifierItem".into(),
    ));
    let menu = eventually(&services, |s| s.menu.clone());
    assert_eq!(menu.item, "org.example.MenuOnly/StatusNotifierItem");
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("Activate"))
            .count(),
        1
    );

    // unknown ids are ignored
    services.request(Request::TrayActivate("nobody/StatusNotifierItem".into()));
    services.request(Request::TrayMenu {
        item: "nobody/StatusNotifierItem".into(),
        parent: 0,
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(services.snapshot().menu, Some(menu));
}

fn wait_for_log(log: &Log, expected: &str) {
    for _ in 0..200 {
        if log.lock().unwrap().iter().any(|line| line == expected) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("never saw {expected:?}; log: {:?}", log.lock().unwrap());
}

#[test]
fn menus_publish_entries_events_and_submenus() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    let calls = log();
    let mut with_menu = Item::new("Menu", &calls);
    with_menu.menu = "/Menu";
    let mock = block_on(
        bus.serve("org.example.Menu")
            .serve_at(ITEM, with_menu)
            .unwrap()
            .serve_at(
                "/Menu",
                DbusMenu {
                    log: Arc::clone(&calls),
                },
            )
            .unwrap()
            .build(),
    )
    .unwrap();
    register(&mock, "org.example.Menu");
    let item = "org.example.Menu/StatusNotifierItem".to_owned();
    eventually(&services, |s| items(s)?.first()?.has_menu.then_some(()));

    services.request(Request::TrayMenu {
        item: item.clone(),
        parent: 0,
    });
    let menu = eventually(&services, |s| s.menu.clone());
    let entry = |id, label: &str, enabled, kind, submenu| MenuEntry {
        id,
        label: label.into(),
        enabled,
        kind,
        submenu,
    };
    assert_eq!(
        menu,
        Menu {
            item: item.clone(),
            parent: 0,
            entries: vec![
                entry(1, "Open", true, EntryKind::Normal, false),
                entry(2, "", true, EntryKind::Separator, false),
                entry(3, "Check", true, EntryKind::Check(true), false),
                entry(4, "More", true, EntryKind::Normal, true),
                entry(6, "Ra_dio", false, EntryKind::Radio(false), false),
            ],
        }
    );
    assert!(calls.lock().unwrap().contains(&"AboutToShow 0".to_owned()));
    assert!(calls.lock().unwrap().contains(&"GetLayout 0 1".to_owned()));

    // a submenu opens with its node as the parent
    services.request(Request::TrayMenu {
        item: item.clone(),
        parent: 4,
    });
    let sub = eventually(&services, |s| s.menu.clone().filter(|m| m.parent == 4));
    assert_eq!(
        sub.entries,
        [entry(41, "Child", true, EntryKind::Normal, false)]
    );
    services.request(Request::TrayMenu {
        item: item.clone(),
        parent: 0,
    });
    eventually(&services, |s| s.menu.clone().filter(|m| m.parent == 0));

    // disabled, hidden, separator, and unknown entries are never sent
    for entry in [6, 5, 2, 99] {
        services.request(Request::MenuEvent {
            item: item.clone(),
            entry,
        });
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|line| !line.starts_with("Event"))
    );
    assert!(services.snapshot().menu.is_some());

    services.request(Request::MenuEvent {
        item: item.clone(),
        entry: 3,
    });
    wait_for_log(&calls, "Event 3 clicked I32(0) 0");
    eventually(&services, |s| s.menu.is_none().then_some(()));

    // closing without choosing, and events for a closed menu
    services.request(Request::TrayMenu {
        item: item.clone(),
        parent: 0,
    });
    eventually(&services, |s| s.menu.clone());
    services.request(Request::CloseMenu);
    eventually(&services, |s| s.menu.is_none().then_some(()));
    services.request(Request::MenuEvent { item, entry: 1 });
    std::thread::sleep(Duration::from_millis(200));
    let events = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("Event"))
        .count();
    assert_eq!(events, 1);
}

#[test]
fn layout_updates_refetch_the_open_menu_only() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    let calls = log();
    let mut with_menu = Item::new("Menu", &calls);
    with_menu.menu = "/Menu";
    let mock = block_on(
        bus.serve("org.example.Live")
            .serve_at(ITEM, with_menu)
            .unwrap()
            .serve_at(
                "/Menu",
                DbusMenu {
                    log: Arc::clone(&calls),
                },
            )
            .unwrap()
            .build(),
    )
    .unwrap();
    register(&mock, "org.example.Live");
    eventually(&services, |s| items(s)?.first()?.has_menu.then_some(()));
    let fetches = |calls: &Log| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("GetLayout"))
            .count()
    };
    let emit = || {
        block_on(async {
            let menu = mock
                .object_server()
                .interface::<_, DbusMenu>("/Menu")
                .await
                .unwrap();
            DbusMenu::layout_updated(menu.signal_emitter(), 2, 0)
                .await
                .unwrap();
        });
    };
    emit();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fetches(&calls), 0, "no menu is open");
    services.request(Request::TrayMenu {
        item: "org.example.Live/StatusNotifierItem".into(),
        parent: 0,
    });
    eventually(&services, |s| s.menu.clone());
    assert_eq!(fetches(&calls), 1);
    emit();
    for _ in 0..200 {
        if fetches(&calls) == 2 {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("open menu was not re-fetched");
}

#[test]
fn at_most_thirty_two_items_are_kept() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    let calls = log();
    let mut builder = bus.serve("org.example.Many");
    for index in 0..33 {
        builder = builder
            .serve_at(
                format!("/Item{index:02}"),
                Item::new(&format!("Item {index:02}"), &calls),
            )
            .unwrap();
    }
    let many = block_on(builder.build()).unwrap();
    for index in 0..33 {
        register(&many, &format!("org.example.Many/Item{index:02}"));
    }
    let found = eventually(&services, |s| items(s).filter(|items| items.len() == 32));
    assert_eq!(found[0].title, "Item 00");
    assert_eq!(found[31].title, "Item 31");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(items(&services.snapshot()).unwrap().len(), 32);
}

#[test]
fn png_icons_come_from_the_item_theme_path() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let services = bus.services(tray());
    let folder = std::env::temp_dir().join(format!("vst-{}", std::process::id()));
    let nested = folder.join("hicolor/48x48/apps");
    fs::create_dir_all(&nested).unwrap();
    let pixels: Vec<u8> = [200, 100, 50, 255].repeat(4);
    fs::write(
        nested.join("themed.png"),
        png_bytes(2, 2, png::ColorType::Rgba, &pixels),
    )
    .unwrap();
    fs::write(
        folder.join("flat.png"),
        png_bytes(2, 2, png::ColorType::Rgba, &pixels),
    )
    .unwrap();
    let calls = log();
    let mut flat = Item::new("Flat", &calls);
    flat.icon_name = "flat".into();
    flat.theme_path = folder.display().to_string();
    let mut themed = Item::new("Themed", &calls);
    themed.icon_name = "themed".into();
    themed.theme_path = folder.display().to_string();
    let mut missing = Item::new("Missing", &calls);
    missing.icon_name = "no-such-icon-anywhere".into();
    missing.theme_path = folder.display().to_string();
    let mock = block_on(
        bus.serve("org.example.Icons")
            .serve_at("/Flat", flat)
            .unwrap()
            .serve_at("/Themed", themed)
            .unwrap()
            .serve_at("/Missing", missing)
            .unwrap()
            .build(),
    )
    .unwrap();
    for name in ["Flat", "Themed", "Missing"] {
        register(&mock, &format!("org.example.Icons/{name}"));
    }
    let found = eventually(&services, |s| items(s).filter(|items| items.len() == 3));
    for item in &found[..2] {
        let icon = item
            .icon
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no icon", item.title));
        assert_eq!((icon.width, icon.height), (2, 2));
        assert_eq!(&icon.rgba[..4], &[200, 100, 50, 255]);
    }
    assert_eq!(found[2].icon, None);
    let _ = fs::remove_dir_all(&folder);
}

/// Encode a PNG of the given color type.
pub(crate) fn png_bytes(width: u32, height: u32, color: png::ColorType, pixels: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels).unwrap();
    writer.finish().unwrap();
    bytes
}
