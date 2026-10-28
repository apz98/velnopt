//! The system tray: a StatusNotifierWatcher (when none exists), a host, items,
//! and dbusmenu, on the session bus with one worker thread.
//!
//! Everything is event driven: item, watcher, dbusmenu, and `NameOwnerChanged`
//! signals each trigger a re-read of whatever changed. Calls to applications time
//! out after two seconds, so a stalled application can delay the tray but never
//! another module.

use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use async_channel::{Receiver, Sender};
use async_io::Timer;
use futures_lite::{StreamExt, future::or};
use zbus::{
    Connection, MatchRule, Message, MessageStream, fdo, interface,
    message::{Header, Type},
    names::BusName,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, OwnedValue, Structure, Value},
};

use crate::{
    EntryKind, ItemStatus, MAX_TEXT, Menu, MenuEntry, Request, Shared, Status, TrayItem,
    dbus::{self, Bus, Properties},
    icon, sanitize,
};

const RETRY: Duration = Duration::from_secs(2);
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";
/// Most items shown.
const MAX_ITEMS: usize = 32;
/// Most items our own watcher remembers.
const MAX_REGISTERED: usize = 64;
const MAX_HOSTS: usize = 16;
const MAX_KEY: usize = 256;
const MAX_ENTRIES: usize = 64;

/// The worker thread, plus its cancellation and request channels.
pub(crate) struct Handle {
    cancel: Sender<()>,
    requests: Sender<Request>,
    worker: Option<JoinHandle<()>>,
}

impl Handle {
    pub(crate) fn start(bus: Bus, shared: Shared) -> io::Result<Self> {
        let (cancel, cancelled) = async_channel::bounded::<()>(1);
        let (requests, incoming) = async_channel::bounded(8);
        let worker = thread::Builder::new()
            .name("velnopt-tray".into())
            .spawn(move || {
                // dropping the losing future abandons any stalled bus call
                async_io::block_on(or(
                    async {
                        let _ = cancelled.recv().await;
                    },
                    run(bus, shared, incoming),
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

async fn run(bus: Bus, shared: Shared, requests: Receiver<Request>) {
    loop {
        if let Err(error) = session(&bus, &shared, &requests).await {
            let reason = dbus::failure::<()>(&error);
            shared.update(|snapshot| {
                snapshot.tray = match reason {
                    Status::Unavailable(reason) => Status::Unavailable(reason),
                    _ => Status::PermissionDenied,
                };
                snapshot.menu = None;
            });
        }
        Timer::after(RETRY).await;
        // don't replay clicks made while the bus was unreachable
        while requests.try_recv().is_ok() {}
    }
}

/// The watcher service we provide when nobody else does.
pub(crate) struct Watcher {
    pub(crate) items: Arc<Mutex<Vec<String>>>,
    pub(crate) hosts: Arc<Mutex<Vec<String>>>,
    pub(crate) changed: Sender<()>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let sender = header.sender().map(ToString::to_string);
        let key = item_key(service, sender.as_deref())
            .ok_or_else(|| fdo::Error::InvalidArgs("invalid item".into()))?;
        let (bus, _) = split(&key).ok_or_else(|| fdo::Error::InvalidArgs("invalid item".into()))?;
        let name =
            BusName::try_from(bus).map_err(|_| fdo::Error::InvalidArgs("bus name".into()))?;
        // nobody owns that name, so it could never be unregistered
        if !fdo::DBusProxy::new(emitter.connection())
            .await?
            .name_has_owner(name)
            .await?
        {
            return Err(fdo::Error::InvalidArgs("unowned bus name".into()));
        }
        let added = {
            let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
            if items.contains(&key) {
                false
            } else if items.len() >= MAX_REGISTERED {
                return Err(fdo::Error::LimitsExceeded("too many tray items".into()));
            } else {
                items.push(key.clone());
                true
            }
        };
        if added {
            Self::status_notifier_item_registered(&emitter, &key).await?;
            let _ = self.changed.try_send(());
        }
        Ok(())
    }

    async fn register_status_notifier_host(
        &self,
        service: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        BusName::try_from(service).map_err(|_| fdo::Error::InvalidArgs("bus name".into()))?;
        let added = {
            let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            if hosts.iter().any(|host| host == service) {
                false
            } else if hosts.len() >= MAX_HOSTS {
                return Err(fdo::Error::LimitsExceeded("too many hosts".into()));
            } else {
                hosts.push(service.to_owned());
                true
            }
        };
        if added {
            Self::status_notifier_host_registered(&emitter).await?;
        }
        Ok(())
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        !self
            .hosts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Our watcher's registries, kept so vanished names can be dropped.
struct Own {
    items: Arc<Mutex<Vec<String>>>,
    hosts: Arc<Mutex<Vec<String>>>,
}

/// Normalize a `RegisterStatusNotifierItem` argument to `"<bus><path>"`.
///
/// A leading `/` means an object path on the sender's connection. Anything else is
/// a bus name, optionally followed by a path.
fn item_key(argument: &str, sender: Option<&str>) -> Option<String> {
    let key = if argument.starts_with('/') {
        format!("{}{argument}", sender?)
    } else if argument.contains('/') {
        argument.to_owned()
    } else {
        format!("{argument}{DEFAULT_ITEM_PATH}")
    };
    let (bus, path) = split(&key)?;
    (key.len() <= MAX_KEY
        && BusName::try_from(bus).is_ok()
        && zbus::zvariant::ObjectPath::try_from(path).is_ok())
    .then_some(key)
}

/// Split `"<bus><path>"` at the first `/`; the path keeps its leading slash.
fn split(key: &str) -> Option<(&str, &str)> {
    let at = key.find('/')?;
    (at > 0).then(|| key.split_at(at))
}

struct Entry {
    key: String,
    owner: String,
    path: OwnedObjectPath,
    menu: Option<OwnedObjectPath>,
    view: TrayItem,
}

struct Tray<'a> {
    conn: &'a Connection,
    daemon: fdo::DBusProxy<'a>,
    shared: &'a Shared,
    own: Option<Own>,
    items: Vec<Entry>,
    menu: Option<Menu>,
}

async fn session(bus: &Bus, shared: &Shared, requests: &Receiver<Request>) -> zbus::Result<()> {
    let conn = dbus::connect(bus).await?;
    let mut messages = MessageStream::from(&conn);
    messages.set_max_queued(dbus::MAX_QUEUED);
    let daemon = fdo::DBusProxy::new(&conn).await?;
    for rule in rules()? {
        daemon.add_match_rule(rule).await?;
    }
    let (changed, wake) = async_channel::bounded(1);
    let own = provide_watcher(&conn, &daemon, changed).await?;
    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    conn.request_name(host.as_str()).await?;
    conn.call_method(
        Some(WATCHER),
        WATCHER_PATH,
        Some(WATCHER),
        "RegisterStatusNotifierHost",
        &host,
    )
    .await?;
    let mut tray = Tray {
        conn: &conn,
        daemon,
        shared,
        own,
        items: Vec::new(),
        menu: None,
    };
    tray.reconcile().await?;
    loop {
        let event = or(
            async { Event::Message(messages.next().await) },
            or(
                async {
                    let _ = wake.recv().await;
                    Event::Wake
                },
                async { Event::Request(requests.recv().await.ok()) },
            ),
        )
        .await;
        match event {
            Event::Message(Some(Ok(message))) => {
                if tray.message(&message).await? {
                    // the watcher's owner changed: serve it ourselves or follow the new owner
                    return Ok(());
                }
            }
            // a closed or lagging stream may have dropped signals, so start over
            Event::Message(Some(Err(error))) => return Err(error),
            Event::Message(None) => return Err(zbus::Error::Failure("bus closed".into())),
            Event::Wake => tray.reconcile().await?,
            Event::Request(Some(request)) => tray.request(request).await,
            Event::Request(None) => {}
        }
    }
}

enum Event {
    Message(Option<zbus::Result<Message>>),
    Wake,
    Request(Option<Request>),
}

/// Serve `org.kde.StatusNotifierWatcher`, unless another process already owns it.
async fn provide_watcher(
    conn: &Connection,
    daemon: &fdo::DBusProxy<'_>,
    changed: Sender<()>,
) -> zbus::Result<Option<Own>> {
    if daemon.name_has_owner(WATCHER.try_into()?).await? {
        return Ok(None);
    }
    let own = Own {
        items: Arc::default(),
        hosts: Arc::default(),
    };
    conn.object_server()
        .at(
            WATCHER_PATH,
            Watcher {
                items: Arc::clone(&own.items),
                hosts: Arc::clone(&own.hosts),
                changed,
            },
        )
        .await?;
    use zbus::fdo::RequestNameFlags::DoNotQueue;
    match conn
        .request_name_with_flags(WATCHER, DoNotQueue.into())
        .await
    {
        Ok(_) => Ok(Some(own)),
        // someone else got there first: use their watcher
        Err(zbus::Error::NameTaken) => Ok(None),
        Err(error) => Err(error),
    }
}

fn rules() -> zbus::Result<Vec<MatchRule<'static>>> {
    Ok(vec![
        MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(ITEM_INTERFACE)?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(WATCHER)?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(MENU_INTERFACE)?
            .member("LayoutUpdated")?
            .build(),
    ])
}

impl Tray<'_> {
    /// React to one signal. Returns `true` when the session has to restart.
    async fn message(&mut self, message: &Message) -> zbus::Result<bool> {
        let header = message.header();
        if header.message_type() != Type::Signal {
            return Ok(false);
        }
        let interface = header.interface().map(|i| i.as_str());
        let member = header.member().map(|m| m.as_str());
        let sender = header.sender().map(ToString::to_string);
        let path = header.path().map(|p| p.as_str().to_owned());
        match (interface, member) {
            // any client can emit this signal; only the bus's own copy is real
            (Some("org.freedesktop.DBus"), Some("NameOwnerChanged"))
                if sender.as_deref() == Some("org.freedesktop.DBus") =>
            {
                let Ok((name, _, owner)) = message.body().deserialize::<(String, String, String)>()
                else {
                    return Ok(false);
                };
                if name == WATCHER {
                    // us acquiring the name doesn't count as an owner change
                    let ours = self.own.is_some()
                        && self
                            .conn
                            .unique_name()
                            .is_some_and(|me| me.as_str() == owner);
                    return Ok(!ours);
                }
                if owner.is_empty() {
                    self.forget(&name).await;
                    self.reconcile().await?;
                }
            }
            (Some(WATCHER), _) => self.reconcile().await?,
            (Some(ITEM_INTERFACE), _) => {
                let found = self.items.iter().position(|entry| {
                    Some(&entry.owner) == sender.as_ref()
                        && Some(entry.path.as_str()) == path.as_deref()
                });
                if let Some(index) = found {
                    self.refresh(index).await;
                    self.publish();
                }
            }
            (Some(MENU_INTERFACE), _) => {
                let open = self.menu.as_ref().and_then(|menu| {
                    let entry = self.items.iter().find(|entry| entry.key == menu.item)?;
                    (Some(&entry.owner) == sender.as_ref())
                        .then(|| (menu.item.clone(), menu.parent))
                });
                if let Some((item, parent)) = open {
                    self.open_menu(&item, parent).await;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    /// Drop registrations in our own watcher whose owner has gone away.
    async fn forget(&mut self, name: &str) {
        let Some(own) = &self.own else { return };
        own.hosts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|host| host != name);
        let gone: Vec<String> = {
            let mut items = own.items.lock().unwrap_or_else(|p| p.into_inner());
            let gone = items
                .iter()
                .filter(|key| split(key).is_some_and(|(bus, _)| bus == name))
                .cloned()
                .collect();
            items.retain(|key| split(key).is_none_or(|(bus, _)| bus != name));
            gone
        };
        if gone.is_empty() {
            return;
        }
        if let Ok(interface) = self
            .conn
            .object_server()
            .interface::<_, Watcher>(WATCHER_PATH)
            .await
        {
            for key in gone {
                let _ =
                    Watcher::status_notifier_item_unregistered(interface.signal_emitter(), &key)
                        .await;
            }
        }
    }

    /// The registered items, according to our watcher or the external one.
    async fn registered(&self) -> zbus::Result<Vec<String>> {
        if let Some(own) = &self.own {
            return Ok(own.items.lock().unwrap_or_else(|p| p.into_inner()).clone());
        }
        let properties = dbus::get_all(self.conn, WATCHER, WATCHER_PATH, WATCHER).await?;
        Ok(dbus::value(&properties, "RegisteredStatusNotifierItems").unwrap_or_default())
    }

    /// Make the item list match the watcher's, in the same order.
    async fn reconcile(&mut self) -> zbus::Result<()> {
        let mut wanted = Vec::new();
        for key in self.registered().await? {
            if item_key(&key, None).as_deref() == Some(key.as_str()) && !wanted.contains(&key) {
                wanted.push(key);
            }
        }
        wanted.truncate(MAX_ITEMS);
        let mut items = Vec::new();
        for key in wanted {
            match self.items.iter().position(|entry| entry.key == key) {
                Some(index) => items.push(self.items.swap_remove(index)),
                None => {
                    if let Ok(entry) = self.read(&key).await {
                        items.push(entry);
                    }
                }
            }
        }
        self.items = items;
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| !self.items.iter().any(|entry| entry.key == menu.item))
        {
            self.menu = None;
        }
        self.publish();
        Ok(())
    }

    async fn read(&self, key: &str) -> zbus::Result<Entry> {
        let (bus, path) = split(key).ok_or_else(|| zbus::Error::Failure("item key".into()))?;
        let owner = self
            .daemon
            .get_name_owner(BusName::try_from(bus)?)
            .await
            .map_err(zbus::Error::from)?
            .to_string();
        let properties = dbus::get_all(self.conn, bus, path, ITEM_INTERFACE).await?;
        Ok(Entry {
            key: key.to_owned(),
            owner,
            path: OwnedObjectPath::try_from(path)?,
            menu: dbus::target(&properties, "Menu"),
            view: view(key, &properties),
        })
    }

    /// Re-read one item. If it stops answering, drop it.
    async fn refresh(&mut self, index: usize) {
        let key = self.items[index].key.clone();
        match self.read(&key).await {
            Ok(entry) => self.items[index] = entry,
            Err(_) => {
                self.items.remove(index);
                if self.menu.as_ref().is_some_and(|menu| menu.item == key) {
                    self.menu = None;
                }
            }
        }
    }

    fn publish(&self) {
        let items: Vec<_> = self.items.iter().map(|entry| entry.view.clone()).collect();
        let menu = self.menu.clone();
        self.shared.update(|snapshot| {
            snapshot.tray = Status::Ready(items);
            snapshot.menu = menu;
        });
    }

    async fn request(&mut self, request: Request) {
        match request {
            Request::TrayActivate(item) => {
                let Some(entry) = self.items.iter().find(|entry| entry.key == item) else {
                    return;
                };
                if entry.view.item_is_menu {
                    self.open_menu(&item, 0).await;
                } else {
                    self.call_item(&item, "Activate").await;
                }
            }
            Request::TraySecondary(item) => self.call_item(&item, "SecondaryActivate").await,
            Request::TrayMenu { item, parent } => self.open_menu(&item, parent).await,
            Request::MenuEvent { item, entry } => self.menu_event(&item, entry).await,
            Request::CloseMenu => {
                self.menu = None;
                self.publish();
            }
            Request::ToggleBluetooth | Request::ToggleMute | Request::AdjustVolume(_) => {}
        }
    }

    async fn call_item(&self, item: &str, method: &str) {
        let Some(entry) = self.items.iter().find(|entry| entry.key == item) else {
            return;
        };
        let Some((bus, _)) = split(&entry.key) else {
            return;
        };
        // a failure is the application's problem; the tray state stays as it was
        let _ = self
            .conn
            .call_method(
                Some(bus),
                &entry.path,
                Some(ITEM_INTERFACE),
                method,
                &(0_i32, 0_i32),
            )
            .await;
    }

    /// Fetch the children of dbusmenu node `parent` and publish them.
    async fn open_menu(&mut self, item: &str, parent: i32) {
        let menu = match self.items.iter().find(|entry| entry.key == item) {
            Some(entry) => self.fetch_menu(entry, parent).await.ok(),
            None => return,
        };
        self.menu = menu;
        self.publish();
    }

    async fn fetch_menu(&self, entry: &Entry, parent: i32) -> zbus::Result<Menu> {
        let (Some(menu), Some((bus, _))) = (&entry.menu, split(&entry.key)) else {
            return Err(zbus::Error::Failure("no menu".into()));
        };
        // some applications fill their menus lazily, so a refusal or failure isn't an error
        let _ = self
            .conn
            .call_method(
                Some(bus),
                menu,
                Some(MENU_INTERFACE),
                "AboutToShow",
                &(parent,),
            )
            .await;
        let reply = self
            .conn
            .call_method(
                Some(bus),
                menu,
                Some(MENU_INTERFACE),
                "GetLayout",
                &(parent, 1_i32, Vec::<&str>::new()),
            )
            .await?;
        let (_revision, (_id, _properties, children)): (u32, Node) = reply.body().deserialize()?;
        let mut entries = Vec::new();
        for child in children {
            if entries.len() == MAX_ENTRIES {
                break;
            }
            let node = Value::from(child)
                .downcast::<Structure>()
                .ok()
                .and_then(|structure| Node::try_from(structure).ok());
            let Some((id, properties, _)) = node else {
                continue;
            };
            entries.extend(menu_entry(id, &properties));
        }
        Ok(Menu {
            item: entry.key.clone(),
            parent,
            entries,
        })
    }

    /// Click an entry in the open menu, then close it.
    async fn menu_event(&mut self, item: &str, id: i32) {
        // only an enabled, visible entry of the menu on screen can be chosen
        let chosen = self.menu.as_ref().is_some_and(|menu| {
            menu.item == item
                && menu.entries.iter().any(|entry| {
                    entry.id == id && entry.enabled && entry.kind != EntryKind::Separator
                })
        });
        let target = self.items.iter().find(|entry| entry.key == item);
        if let (true, Some(entry)) = (chosen, target)
            && let (Some(menu), Some((bus, _))) = (&entry.menu, split(&entry.key))
        {
            let _ = self
                .conn
                .call_method(
                    Some(bus),
                    menu,
                    Some(MENU_INTERFACE),
                    "Event",
                    &(id, "clicked", Value::from(0_i32), 0_u32),
                )
                .await;
            self.menu = None;
            self.publish();
        }
    }
}

/// A dbusmenu layout node: id, properties, and children (variants that hold nodes).
type Node = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn view(key: &str, properties: &Properties) -> TrayItem {
    let text = |name: &str| dbus::value::<String>(properties, name).unwrap_or_default();
    let id = sanitize(&text("Id"), MAX_TEXT);
    let title = sanitize(&text("Title"), MAX_TEXT);
    let icon = dbus::value::<Vec<(i32, i32, Vec<u8>)>>(properties, "IconPixmap")
        .and_then(icon::from_pixmaps)
        .or_else(|| icon::from_theme(&text("IconName"), &text("IconThemePath")));
    TrayItem {
        id: key.to_owned(),
        title: if title.trim().is_empty() { id } else { title },
        status: match text("Status").as_str() {
            "Passive" => ItemStatus::Passive,
            "NeedsAttention" => ItemStatus::NeedsAttention,
            _ => ItemStatus::Active,
        },
        icon,
        item_is_menu: dbus::value(properties, "ItemIsMenu").unwrap_or(false),
        has_menu: dbus::target(properties, "Menu").is_some(),
    }
}

/// One visible entry, or `None` if it is hidden.
fn menu_entry(id: i32, properties: &Properties) -> Option<MenuEntry> {
    let flag = |name: &str| dbus::value::<bool>(properties, name);
    let text = |name: &str| dbus::value::<String>(properties, name).unwrap_or_default();
    if flag("visible") == Some(false) {
        return None;
    }
    let state = dbus::value::<i32>(properties, "toggle-state") == Some(1);
    let kind = if text("type") == "separator" {
        EntryKind::Separator
    } else {
        match text("toggle-type").as_str() {
            "checkmark" => EntryKind::Check(state),
            "radio" => EntryKind::Radio(state),
            _ => EntryKind::Normal,
        }
    };
    Some(MenuEntry {
        id,
        label: sanitize(&strip_mnemonic(&text("label")), MAX_TEXT),
        enabled: flag("enabled").unwrap_or(true),
        kind,
        submenu: text("children-display") == "submenu",
    })
}

/// `_File` becomes `File`; `__` becomes `_`.
fn strip_mnemonic(label: &str) -> String {
    let mut result = String::with_capacity(label.len());
    let mut characters = label.chars();
    while let Some(character) = characters.next() {
        if character != '_' {
            result.push(character);
        } else if let Some(next) = characters.next() {
            result.push(next);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonics_are_stripped() {
        assert_eq!(strip_mnemonic("_File"), "File");
        assert_eq!(strip_mnemonic("Snap__shot"), "Snap_shot");
        assert_eq!(strip_mnemonic("E_xit_"), "Exit");
        assert_eq!(strip_mnemonic("plain"), "plain");
    }

    #[test]
    fn registration_arguments_normalize() {
        assert_eq!(
            item_key("org.app", Some(":1.2")).as_deref(),
            Some("org.app/StatusNotifierItem")
        );
        assert_eq!(
            item_key("/org/ayatana/Item", Some(":1.2")).as_deref(),
            Some(":1.2/org/ayatana/Item")
        );
        assert_eq!(
            item_key("org.app/Custom", None).as_deref(),
            Some("org.app/Custom")
        );
        assert_eq!(item_key("/path", None), None);
        assert_eq!(item_key("not a name", None), None);
        assert_eq!(item_key(&format!("a.b/{}", "x".repeat(300)), None), None);
    }
}
