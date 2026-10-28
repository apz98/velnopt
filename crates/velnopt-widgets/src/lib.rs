//! Widget presentation: labels, interactive items, and accessibility trees.
//!
//! This crate turns a validated configuration and the latest live state into the
//! items for one panel. It has no surfaces, timers, threads, or I/O: the caller
//! draws the labels and runs the actions of the items the user activates.

mod status;

use std::{
    borrow::Cow,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use accesskit::{Node, NodeId, Rect, Role as AccessRole, Toggled, TreeId, TreeInfo, TreeUpdate};
use chrono::{DateTime, Local, Utc};
use velnopt_config::{Config, Position, Widget};
use velnopt_hyprland::Snapshot;
use velnopt_services::{Snapshot as Services, Status, apps::App};
use velnopt_ui::{Image, Label, Span};

/// Live state shared by every panel, plus the output this panel is drawn on.
#[derive(Clone, Copy, Debug)]
pub struct State<'a> {
    /// Latest Hyprland snapshot. The default snapshot means there is no IPC.
    pub hyprland: &'a Snapshot,
    /// Latest desktop-service snapshot. Modules that aren't running stay pending.
    pub services: &'a Services,
    /// Wayland output name of the panel, used to pick the workspaces of its monitor.
    pub output: Option<&'a str>,
    /// A launcher or menu shown in place of this panel's widgets.
    pub overlay: Option<Overlay<'a>>,
}

/// Content that takes the place of a panel's widgets while the panel has keyboard focus.
#[derive(Clone, Copy, Debug)]
pub enum Overlay<'a> {
    /// Application search: the query, and the ranked matches with their indices.
    Launcher {
        /// Text typed so far.
        query: &'a str,
        /// Matching applications, best first, with indices into the full list.
        results: &'a [(usize, &'a App)],
    },
    /// The open tray menu.
    Menu(&'a velnopt_services::Menu),
}

/// What activating an item asks for. The caller validates and executes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Show the regular workspace with this Hyprland identifier.
    Workspace(i64),
    /// Toggle the Bluetooth adapter's power.
    ToggleBluetooth,
    /// Toggle mute of the default audio output.
    ToggleMute,
    /// Open the application launcher.
    OpenLauncher,
    /// Launch the application with this index in the discovered list.
    Launch(usize),
    /// Activate the tray item with this index in the tray snapshot.
    Tray(usize),
    /// Activate the entry with this dbusmenu identifier in the open menu.
    MenuEntry(i32),
}

/// Accessibility role of an item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Read-only text.
    Text,
    /// A workspace selector; `selected` marks the visible workspace.
    Tab,
    /// A push button.
    Button,
    /// An on/off control; `toggled` holds its state.
    Switch,
    /// A level control; `value` holds its percentage.
    Slider,
    /// The launcher's search field; `value` text is its query.
    Search,
    /// A launcher result.
    Option,
    /// A menu entry, optionally checkable through `toggled`.
    MenuItem,
}

/// One drawable, optionally actionable element of a panel.
#[derive(Debug)]
pub struct Item<'a> {
    /// What to draw.
    pub label: Label<'a>,
    /// Index of the configured widget that produced the item. Overlays use
    /// `usize::MAX`.
    pub widget: usize,
    /// Accessibility role.
    pub role: Role,
    /// Accessible name, which can be more descriptive than the drawn text.
    pub name: Cow<'a, str>,
    /// Whether the item is the current choice in its group.
    pub selected: bool,
    /// On/off state of switches and checkable menu entries.
    pub toggled: Option<bool>,
    /// Numeric value of sliders, in percent.
    pub value: Option<f64>,
    /// Present for items that respond to pointer clicks and keyboard activation.
    pub action: Option<Action>,
}

const OVERLAY: usize = usize::MAX;
const SEARCH_PROMPT: &str = "Run: ";
const SEARCH_CARET: char = '▏';

/// Resolve the configured widgets into items, keeping their order and style.
///
/// An overlay replaces the widgets. `focus` is an item index, and that item draws
/// a focus ring if it is actionable. This only prepares content.
pub fn items<'a>(
    config: &'a Config,
    now: SystemTime,
    state: State<'a>,
    focus: Option<usize>,
) -> Vec<Item<'a>> {
    let mut items = match state.overlay {
        Some(overlay) => overlay_items(overlay),
        None => widget_items(config, now, state),
    };
    if let Some(item) = focus.and_then(|index| items.get_mut(index)) {
        item.label.focused = item.action.is_some();
    }
    items
}

fn widget_items<'a>(config: &'a Config, now: SystemTime, state: State<'a>) -> Vec<Item<'a>> {
    let mut items = Vec::new();
    for (widget_index, widget) in config.panel().widgets().iter().enumerate() {
        let item = |text: Cow<'a, str>, joined: bool, role: Role| Item {
            name: text.clone(),
            label: Label {
                text,
                position: widget.position(),
                color: widget.color(),
                joined,
                focused: false,
                icon: None,
            },
            widget: widget_index,
            role,
            selected: false,
            toggled: None,
            value: None,
            action: None,
        };
        match widget {
            Widget::Workspaces { .. } if state.hyprland.connected() => {
                let (workspaces, active) = workspaces(state);
                if workspaces.is_empty() {
                    items.push(item(Cow::Borrowed("No workspaces"), false, Role::Text));
                }
                for (index, workspace) in workspaces.into_iter().enumerate() {
                    let current = Some(workspace.id()) == active;
                    let text = if current {
                        Cow::Owned(format!("[{}]", workspace.name()))
                    } else {
                        Cow::Borrowed(workspace.name())
                    };
                    items.push(Item {
                        name: Cow::Owned(format!("Workspace {}", workspace.name())),
                        selected: current,
                        action: Some(Action::Workspace(workspace.id())),
                        ..item(text, index > 0, Role::Tab)
                    });
                }
            }
            Widget::Bluetooth { .. } => {
                let mut bluetooth = item(
                    status::bluetooth(&state.services.bluetooth),
                    false,
                    Role::Text,
                );
                if let Status::Ready(ready) = &state.services.bluetooth
                    && !ready.blocked
                {
                    bluetooth.role = Role::Switch;
                    bluetooth.name = Cow::Borrowed("Bluetooth");
                    bluetooth.toggled = Some(ready.powered);
                    bluetooth.action = Some(Action::ToggleBluetooth);
                }
                items.push(bluetooth);
            }
            Widget::Volume { .. } => {
                let mut volume = item(status::audio(&state.services.audio), false, Role::Text);
                if let Status::Ready(ready) = &state.services.audio {
                    volume.role = Role::Slider;
                    volume.name = Cow::Borrowed(if ready.muted {
                        "Volume, muted"
                    } else {
                        "Volume"
                    });
                    volume.value = Some(f64::from(ready.volume));
                    volume.action = Some(Action::ToggleMute);
                }
                items.push(volume);
            }
            Widget::Network { .. } => {
                items.push(item(
                    status::network(&state.services.network),
                    false,
                    Role::Text,
                ));
            }
            Widget::Battery { .. } => {
                items.push(item(
                    status::battery(&state.services.battery),
                    false,
                    Role::Text,
                ));
            }
            Widget::Launcher { .. } => items.push(Item {
                name: Cow::Borrowed("Applications"),
                action: Some(Action::OpenLauncher),
                ..item(Cow::Borrowed("Apps"), false, Role::Button)
            }),
            Widget::Tray { .. } => match &state.services.tray {
                Status::Ready(tray) => {
                    for (index, tray_item) in tray.iter().enumerate() {
                        let icon = tray_item
                            .icon
                            .as_ref()
                            .and_then(|icon| Image::new(icon.width, icon.height, &icon.rgba));
                        // the icon is drawn instead of the title, but the title stays the accessible name
                        let text = match icon {
                            Some(_) => Cow::Borrowed(""),
                            None => Cow::Borrowed(tray_item.title.as_str()),
                        };
                        let mut button = Item {
                            name: Cow::Borrowed(tray_item.title.as_str()),
                            action: Some(Action::Tray(index)),
                            ..item(text, index > 0, Role::Button)
                        };
                        button.label.icon = icon;
                        items.push(button);
                    }
                }
                other => {
                    if let Some(text) = status::tray(other) {
                        items.push(item(text, false, Role::Text));
                    }
                }
            },
            _ => items.push(item(text(widget, now, state.hyprland), false, Role::Text)),
        }
    }
    items
}

/// Launcher and menu items, drawn left-aligned across the whole panel.
fn overlay_items<'a>(overlay: Overlay<'a>) -> Vec<Item<'a>> {
    let item = |text: Cow<'a, str>, role: Role, joined: bool| Item {
        name: text.clone(),
        label: Label {
            text,
            position: Position::Left,
            color: None,
            joined,
            focused: false,
            icon: None,
        },
        widget: OVERLAY,
        role,
        selected: false,
        toggled: None,
        value: None,
        action: None,
    };
    let mut items = Vec::new();
    match overlay {
        Overlay::Launcher { query, results } => {
            items.push(Item {
                name: Cow::Borrowed("Search applications"),
                ..item(
                    Cow::Owned(format!("{SEARCH_PROMPT}{query}{SEARCH_CARET}")),
                    Role::Search,
                    false,
                )
            });
            if results.is_empty() {
                items.push(item(
                    Cow::Borrowed("No matching applications"),
                    Role::Text,
                    false,
                ));
            }
            for (index, app) in results {
                items.push(Item {
                    action: Some(Action::Launch(*index)),
                    ..item(Cow::Borrowed(app.name()), Role::Option, false)
                });
            }
        }
        Overlay::Menu(menu) => {
            use velnopt_services::EntryKind;
            let entries: Vec<_> = menu
                .entries
                .iter()
                .filter(|entry| entry.kind != EntryKind::Separator)
                .collect();
            if entries.is_empty() {
                items.push(item(Cow::Borrowed("Empty menu"), Role::Text, false));
            }
            for entry in entries {
                let (prefix, toggled) = match entry.kind {
                    EntryKind::Check(on) | EntryKind::Radio(on) => {
                        (if on { "✓ " } else { "  " }, Some(on))
                    }
                    _ => ("", None),
                };
                let suffix = if entry.submenu { " ▸" } else { "" };
                let text = Cow::Owned(format!("{prefix}{}{suffix}", entry.label));
                items.push(Item {
                    name: Cow::Borrowed(entry.label.as_str()),
                    toggled,
                    action: entry.enabled.then_some(Action::MenuEntry(entry.id)),
                    ..item(text, Role::MenuItem, false)
                });
            }
        }
    }
    items
}

/// Regular workspaces for the panel's monitor and the one it shows.
///
/// If no Hyprland monitor matches (an unnamed output, say), every regular
/// workspace is listed and the globally focused one counts as current.
/// Special workspaces have negative identifiers and can't be switched to this way.
fn workspaces<'a>(state: State<'a>) -> (Vec<&'a velnopt_hyprland::Workspace>, Option<i64>) {
    let hyprland = state.hyprland;
    let monitor = state.output.and_then(|output| hyprland.monitor(output));
    let workspaces = hyprland
        .workspaces()
        .iter()
        .filter(|workspace| workspace.id() > 0)
        .filter(|workspace| {
            monitor.is_none_or(|monitor| workspace.monitor() == Some(monitor.name()))
        })
        .collect();
    let active = match monitor {
        Some(monitor) => monitor.active_workspace(),
        None => hyprland.active_workspace(),
    };
    (workspaces, active)
}

/// Indices of the items that take focus and activation, in reading order.
pub fn focusable(items: &[Item<'_>]) -> Vec<usize> {
    let mut indices: Vec<_> = (0..items.len())
        .filter(|&index| items[index].action.is_some())
        .collect();
    // items come grouped left, center, right; keep declaration order inside each group
    indices.sort_by_key(|&index| items[index].label.position as u8);
    indices
}

/// Accessibility node of `items[index]`. A workspace node keeps its identity for
/// as long as the workspace exists, so assistive technology doesn't see a new
/// control on every refresh.
pub fn node_id(index: usize, item: &Item<'_>) -> NodeId {
    match item.action {
        Some(Action::Workspace(id)) => NodeId(0x1_0000_0000 | (id as u64 & 0xffff_ffff)),
        _ => NodeId(1000 + index as u64),
    }
}

/// Root node identity of every panel tree.
pub const ROOT: NodeId = NodeId(0);

/// The container role and label that group consecutive items of one role.
fn container(role: Role) -> Option<(AccessRole, &'static str)> {
    match role {
        Role::Tab => Some((AccessRole::TabList, "Workspaces")),
        Role::Option => Some((AccessRole::ListBox, "Applications")),
        Role::MenuItem => Some((AccessRole::Menu, "Menu")),
        _ => None,
    }
}

/// Build a complete accessibility tree for one panel.
///
/// `spans` and `scale` come from the renderer's last frame, and bounds are in
/// surface-local logical pixels. Workspace tabs, launcher results, and menu
/// entries each get a container. Focus is on the focused item, or else the panel.
pub fn accessibility(
    title: &str,
    items: &[Item<'_>],
    spans: &[Option<Span>],
    scale: i32,
    height: f64,
    focus: Option<usize>,
) -> TreeUpdate {
    let scale = f64::from(scale.max(1));
    let mut nodes = Vec::with_capacity(items.len() + 2);
    let mut root_children = Vec::new();
    // (widget, role, container id, children)
    let mut group: Option<(usize, Role, NodeId, Vec<NodeId>)> = None;
    let flush = |group: &mut Option<(usize, Role, NodeId, Vec<NodeId>)>,
                 nodes: &mut Vec<(NodeId, Node)>,
                 root_children: &mut Vec<NodeId>| {
        if let Some((_, role, id, children)) = group.take()
            && let Some((container_role, label)) = container(role)
        {
            let mut list = Node::new(container_role);
            list.set_label(label);
            list.set_children(children);
            nodes.push((id, list));
            root_children.push(id);
        }
    };
    for (index, item) in items.iter().enumerate() {
        let id = node_id(index, item);
        let mut node = Node::new(match item.role {
            Role::Text => AccessRole::Label,
            Role::Tab => AccessRole::Tab,
            Role::Button => AccessRole::Button,
            Role::Switch => AccessRole::Switch,
            Role::Slider => AccessRole::Slider,
            Role::Search => AccessRole::SearchInput,
            Role::Option => AccessRole::ListBoxOption,
            Role::MenuItem => match item.toggled {
                Some(_) => AccessRole::MenuItemCheckBox,
                None => AccessRole::MenuItem,
            },
        });
        node.set_label(item.name.as_ref());
        match item.role {
            // AT-SPI takes the name of static text from its value, not its label
            Role::Text => node.set_value(item.name.as_ref()),
            Role::Tab => node.set_selected(item.selected),
            Role::Option => node.set_selected(focus == Some(index)),
            Role::Search => node.set_value(
                item.label
                    .text
                    .strip_prefix(SEARCH_PROMPT)
                    .unwrap_or_default()
                    .trim_end_matches(SEARCH_CARET),
            ),
            _ => {}
        }
        if let Some(toggled) = item.toggled {
            node.set_toggled(if toggled {
                Toggled::True
            } else {
                Toggled::False
            });
        }
        if let Some(value) = item.value {
            node.set_numeric_value(value);
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(150.0);
            node.add_action(accesskit::Action::Increment);
            node.add_action(accesskit::Action::Decrement);
        }
        if item.action.is_some() {
            node.add_action(accesskit::Action::Click);
            node.add_action(accesskit::Action::Focus);
        }
        if let Some(Some(span)) = spans.get(index) {
            node.set_bounds(Rect {
                x0: f64::from(span.start) / scale,
                y0: 0.0,
                x1: f64::from(span.end) / scale,
                y1: height,
            });
        }
        nodes.push((id, node));
        if container(item.role).is_some() {
            if group
                .as_ref()
                .is_none_or(|(widget, role, _, _)| *widget != item.widget || *role != item.role)
            {
                flush(&mut group, &mut nodes, &mut root_children);
                // container ids below 1000 can't collide with item nodes
                let container_id = match item.widget {
                    OVERLAY => NodeId(999),
                    widget => NodeId(1 + widget as u64),
                };
                group = Some((item.widget, item.role, container_id, Vec::new()));
            }
            if let Some((_, _, _, children)) = &mut group {
                children.push(id);
            }
        } else {
            flush(&mut group, &mut nodes, &mut root_children);
            root_children.push(id);
        }
    }
    flush(&mut group, &mut nodes, &mut root_children);
    let mut root = Node::new(AccessRole::Window);
    root.set_label(title);
    root.set_children(root_children);
    nodes.push((ROOT, root));
    let focus = focus
        .and_then(|index| items.get(index).map(|item| node_id(index, item)))
        .unwrap_or(ROOT);
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo {
            root: ROOT,
            toolkit_name: Some("Velnopt".into()),
            toolkit_version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Resolve the text a widget shows at the given time.
///
/// Static text is borrowed, not copied. Clocks show local `HH:MM` or `HH:MM UTC`,
/// depending on their configuration.
fn text<'a>(widget: &'a Widget, now: SystemTime, hyprland: &'a Snapshot) -> Cow<'a, str> {
    match widget {
        Widget::Text { text, .. } => Cow::Borrowed(text),
        Widget::Clock { utc, .. } => Cow::Owned(clock_text(now, *utc)),
        // workspaces turn into individual items in `items`
        Widget::Workspaces { .. } => Cow::Borrowed("Hyprland unavailable"),
        Widget::WindowTitle { .. } if !hyprland.connected() => {
            Cow::Borrowed("Hyprland unavailable")
        }
        Widget::WindowTitle { .. } => Cow::Borrowed(match hyprland.window_title() {
            None => "No focused window",
            Some("") => "Untitled window",
            Some(title) => title,
        }),
        // service, launcher, and tray widgets are resolved in `widget_items`
        _ => Cow::Borrowed(""),
    }
}

fn clock_text(now: SystemTime, utc: bool) -> String {
    if utc {
        DateTime::<Utc>::from(now).format("%H:%M UTC").to_string()
    } else {
        DateTime::<Local>::from(now).format("%H:%M").to_string()
    }
}

/// Time left until the next wall-clock minute.
///
/// The caller schedules this delay on its own event loop, since this crate makes no
/// timers or threads. Recalculate after each update so changes to the wall clock
/// are picked up.
pub fn next_clock_update(now: SystemTime) -> Duration {
    let elapsed = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    Duration::from_secs(60) - Duration::new(elapsed.as_secs() % 60, elapsed.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn services() -> &'static Services {
        static SERVICES: OnceLock<Services> = OnceLock::new();
        SERVICES.get_or_init(Services::default)
    }

    fn labels<'a>(config: &'a Config, now: SystemTime, hyprland: &'a Snapshot) -> Vec<Label<'a>> {
        let state = State {
            services: services(),
            overlay: None,
            hyprland,
            output: None,
        };
        items(config, now, state, None)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn resolves_configured_widgets_to_ui_labels() {
        let config = velnopt_config::parse("version=1\n[panel]\nedge='top'\nheight=32\n[[panel.widgets]]\nkind='clock'\nposition='right'\nutc=true\n[[panel.widgets]]\nkind='text'\ntext='مرحبا'\ncolor='#96d1bd'").expect("valid widgets");
        let state = Snapshot::default();
        let labels = labels(&config, UNIX_EPOCH + Duration::from_secs(86399), &state);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].text, "23:59 UTC");
        assert_eq!(labels[0].position, Position::Right);
        assert_eq!(labels[1].text, "مرحبا");
        assert_eq!(labels[1].position, Position::Left);
        assert_eq!(
            labels[1].color.expect("custom color").rgb(),
            [0x96, 0xd1, 0xbd]
        );
    }

    #[test]
    fn preserves_unicode_text() {
        let widget = Widget::Text {
            text: "مرحبا 🦀".into(),
            position: Position::Left,
            color: None,
        };
        assert_eq!(text(&widget, UNIX_EPOCH, &Snapshot::default()), "مرحبا 🦀");
    }

    #[test]
    fn live_widgets_show_focused_workspace_and_title_or_explicit_empty_states() {
        let config =
            velnopt_config::parse(include_str!("../../../examples/live-hyprland.toml")).unwrap();
        let state = Snapshot::from_json(
            br#"[{"id":2,"name":"work"},{"id":1,"name":"1"},{"id":-98,"name":"special:magic"}]"#,
            b"[]",
            br#"{"id":2}"#,
            br#"{"title":"Focused, title >> value"}"#,
        )
        .unwrap();
        let resolved = labels(&config, UNIX_EPOCH, &state);
        assert_eq!(resolved[0].text, "1");
        assert!(!resolved[0].joined);
        assert_eq!(resolved[1].text, "[work]");
        assert!(resolved[1].joined);
        assert_eq!(resolved[2].text, "Focused, title >> value");
        let empty = Snapshot::from_json(b"[]", b"[]", b"{}", b"{}").unwrap();
        let resolved = labels(&config, UNIX_EPOCH, &empty);
        assert_eq!(resolved[0].text, "No workspaces");
        assert_eq!(resolved[1].text, "No focused window");
        let offline = Snapshot::default();
        let resolved = labels(&config, UNIX_EPOCH, &offline);
        assert_eq!(resolved[0].text, "Hyprland unavailable");
        assert_eq!(resolved[1].text, "Hyprland unavailable");
    }

    const TWO_MONITORS: (&[u8], &[u8]) = (
        br#"[{"id":1,"name":"1","monitor":"eDP-1"},{"id":2,"name":"web","monitor":"DP-1"},{"id":3,"name":"3","monitor":"eDP-1"}]"#,
        br#"[{"name":"eDP-1","focused":true,"activeWorkspace":{"id":3,"name":"3"}},{"name":"DP-1","focused":false,"activeWorkspace":{"id":2,"name":"web"}}]"#,
    );

    #[test]
    fn workspaces_follow_each_monitor_and_expose_actions() {
        let config =
            velnopt_config::parse(include_str!("../../../examples/live-hyprland.toml")).unwrap();
        let snapshot =
            Snapshot::from_json(TWO_MONITORS.0, TWO_MONITORS.1, br#"{"id":3}"#, b"{}").unwrap();
        let laptop = items(
            &config,
            UNIX_EPOCH,
            State {
                services: services(),
                overlay: None,
                hyprland: &snapshot,
                output: Some("eDP-1"),
            },
            Some(1),
        );
        let texts: Vec<_> = laptop.iter().map(|item| item.label.text.as_ref()).collect();
        assert_eq!(texts[..2], ["1", "[3]"]);
        assert_eq!(laptop[0].action, Some(Action::Workspace(1)));
        assert_eq!(laptop[1].role, Role::Tab);
        assert!(laptop[1].selected && laptop[1].label.focused);
        assert_eq!(laptop[1].name, "Workspace 3");
        assert!(!laptop[0].label.focused);
        assert_eq!(laptop[2].action, None, "titles are not controls");
        assert_eq!(focusable(&laptop), [0, 1]);
        let external = items(
            &config,
            UNIX_EPOCH,
            State {
                services: services(),
                overlay: None,
                hyprland: &snapshot,
                output: Some("DP-1"),
            },
            None,
        );
        assert_eq!(external[0].label.text, "[web]");
        assert_eq!(external[0].action, Some(Action::Workspace(2)));
        // an unknown output falls back to every regular workspace and global focus
        let unknown = items(
            &config,
            UNIX_EPOCH,
            State {
                services: services(),
                overlay: None,
                hyprland: &snapshot,
                output: Some("HEADLESS-9"),
            },
            None,
        );
        assert_eq!(focusable(&unknown).len(), 3);
        assert_eq!(unknown[2].label.text, "[3]");
        // focus on something that isn't a control draws no ring
        let title = items(
            &config,
            UNIX_EPOCH,
            State {
                services: services(),
                overlay: None,
                hyprland: &snapshot,
                output: Some("DP-1"),
            },
            Some(1),
        );
        assert!(title.iter().all(|item| !item.label.focused));
    }

    #[test]
    fn accessibility_tree_groups_tabs_and_tracks_focus_and_bounds() {
        let config =
            velnopt_config::parse(include_str!("../../../examples/live-hyprland.toml")).unwrap();
        let snapshot =
            Snapshot::from_json(TWO_MONITORS.0, TWO_MONITORS.1, br#"{"id":3}"#, b"{}").unwrap();
        let state = State {
            services: services(),
            overlay: None,
            hyprland: &snapshot,
            output: Some("eDP-1"),
        };
        let panel = items(&config, UNIX_EPOCH, state, Some(1));
        let spans = vec![Some(Span { start: 20, end: 40 }), None, None, None];
        let tree = accessibility("Velnopt panel", &panel, &spans, 2, 36.0, Some(1));
        let node = |id: NodeId| &tree.nodes.iter().find(|(node, _)| *node == id).unwrap().1;
        let root = node(ROOT);
        assert_eq!(root.role(), AccessRole::Window);
        assert_eq!(root.label(), Some("Velnopt panel"));
        let list = node(root.children()[0]);
        assert_eq!(list.role(), AccessRole::TabList);
        assert_eq!(list.children().len(), 2);
        let first = node(list.children()[0]);
        assert_eq!(first.role(), AccessRole::Tab);
        assert_eq!(first.label(), Some("Workspace 1"));
        assert_eq!(first.is_selected(), Some(false));
        assert!(first.supports_action(accesskit::Action::Click));
        assert_eq!(first.bounds().unwrap().x0, 10.0);
        assert_eq!(first.bounds().unwrap().x1, 20.0);
        let current = node(list.children()[1]);
        assert_eq!(current.is_selected(), Some(true));
        assert_eq!(tree.focus, list.children()[1]);
        assert_eq!(node_id(1, &panel[1]), tree.focus);
        // workspace node ids survive unrelated changes in item order
        let moved = Snapshot::from_json(
            br#"[{"id":3,"name":"3","monitor":"eDP-1"}]"#,
            TWO_MONITORS.1,
            br#"{"id":3}"#,
            b"{}",
        )
        .unwrap();
        let moved_items = items(
            &config,
            UNIX_EPOCH,
            State {
                services: services(),
                overlay: None,
                hyprland: &moved,
                output: Some("eDP-1"),
            },
            None,
        );
        assert_eq!(node_id(0, &moved_items[0]), tree.focus);
        let labels = node(root.children()[1]);
        assert_eq!(labels.role(), AccessRole::Label);
        assert_eq!(labels.value(), Some("No focused window"));
        assert!(!labels.supports_action(accesskit::Action::Click));
        let unfocused = accessibility("Velnopt panel", &panel, &spans, 1, 36.0, None);
        assert_eq!(unfocused.focus, ROOT);
    }

    #[test]
    fn service_widgets_expose_controls_only_when_ready() {
        use velnopt_services::{Audio, Bluetooth};
        let config = velnopt_config::parse(
            "version=1\n[panel]\nedge='top'\nheight=32\n[[panel.widgets]]\nkind='bluetooth'\n[[panel.widgets]]\nkind='volume'\n[[panel.widgets]]\nkind='launcher'\n[[panel.widgets]]\nkind='network'",
        )
        .unwrap();
        let services = Services {
            bluetooth: Status::Ready(Bluetooth {
                powered: true,
                blocked: false,
                connected: 0,
                control_denied: false,
            }),
            audio: Status::Ready(Audio {
                volume: 40,
                muted: false,
                control_denied: false,
            }),
            ..Services::default()
        };
        let hyprland = Snapshot::default();
        let state = |services| State {
            hyprland: &hyprland,
            services,
            output: None,
            overlay: None,
        };
        let ready = items(&config, UNIX_EPOCH, state(&services), None);
        assert_eq!(ready[0].role, Role::Switch);
        assert_eq!(ready[0].toggled, Some(true));
        assert_eq!(ready[0].action, Some(Action::ToggleBluetooth));
        assert_eq!(ready[1].role, Role::Slider);
        assert_eq!(ready[1].value, Some(40.0));
        assert_eq!(ready[1].label.text, "Volume 40%");
        assert_eq!(ready[2].action, Some(Action::OpenLauncher));
        assert_eq!(ready[2].name, "Applications");
        assert_eq!(ready[3].action, None, "network is display-only");
        assert_eq!(ready[3].label.text, "Network…");
        let tree = accessibility("Velnopt panel", &ready, &[], 1, 32.0, None);
        let slider = &tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == AccessRole::Slider)
            .unwrap()
            .1;
        assert_eq!(slider.numeric_value(), Some(40.0));
        assert!(slider.supports_action(accesskit::Action::Increment));
        let switch = &tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == AccessRole::Switch)
            .unwrap()
            .1;
        assert_eq!(switch.toggled(), Some(Toggled::True));
        let offline = Services {
            bluetooth: Status::Unavailable("org.bluez is not running".into()),
            audio: Status::PermissionDenied,
            ..Services::default()
        };
        let unavailable = items(&config, UNIX_EPOCH, state(&offline), None);
        assert_eq!(unavailable[0].label.text, "Bluetooth unavailable");
        assert_eq!(unavailable[0].action, None);
        assert_eq!(unavailable[1].label.text, "Audio: no permission");
        assert_eq!(unavailable[1].action, None);
    }

    #[test]
    fn launcher_and_menu_overlays_replace_widgets_with_accessible_controls() {
        use velnopt_services::{EntryKind, Menu, MenuEntry};
        let config =
            velnopt_config::parse(include_str!("../../../examples/live-hyprland.toml")).unwrap();
        let hyprland = Snapshot::default();
        let overlay = |overlay| State {
            hyprland: &hyprland,
            services: services(),
            output: None,
            overlay: Some(overlay),
        };
        let empty = items(
            &config,
            UNIX_EPOCH,
            overlay(Overlay::Launcher {
                query: "zz",
                results: &[],
            }),
            None,
        );
        assert_eq!(empty.len(), 2);
        assert_eq!(empty[0].role, Role::Search);
        assert_eq!(empty[0].label.text, "Run: zz▏");
        assert_eq!(empty[1].label.text, "No matching applications");
        let tree = accessibility("Velnopt panel", &empty, &[], 1, 32.0, None);
        let search = &tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == AccessRole::SearchInput)
            .unwrap()
            .1;
        assert_eq!(search.value(), Some("zz"));
        let entry = |id, label: &str, kind, enabled, submenu| MenuEntry {
            id,
            label: label.into(),
            enabled,
            kind,
            submenu,
        };
        let menu = Menu {
            item: "item".into(),
            parent: 0,
            entries: vec![
                entry(1, "Open", EntryKind::Normal, true, false),
                entry(2, "", EntryKind::Separator, true, false),
                entry(3, "Mute", EntryKind::Check(true), true, false),
                entry(4, "More", EntryKind::Normal, true, true),
                entry(5, "Disabled", EntryKind::Normal, false, false),
            ],
        };
        let shown = items(&config, UNIX_EPOCH, overlay(Overlay::Menu(&menu)), Some(0));
        let texts: Vec<_> = shown.iter().map(|item| item.label.text.as_ref()).collect();
        assert_eq!(texts, ["Open", "✓ Mute", "More ▸", "Disabled"]);
        assert!(shown[0].label.focused);
        assert_eq!(shown[1].toggled, Some(true));
        assert_eq!(shown[2].action, Some(Action::MenuEntry(4)));
        assert_eq!(shown[3].action, None);
        assert_eq!(focusable(&shown), [0, 1, 2]);
        let tree = accessibility("Velnopt panel", &shown, &[], 1, 32.0, Some(0));
        let node = |id: NodeId| &tree.nodes.iter().find(|(node, _)| *node == id).unwrap().1;
        let list = node(node(ROOT).children()[0]);
        assert_eq!(list.role(), AccessRole::Menu);
        assert_eq!(list.children().len(), 4);
        assert_eq!(
            node(list.children()[1]).role(),
            AccessRole::MenuItemCheckBox
        );
    }

    #[test]
    fn clock_rolls_over_and_timer_aligns_to_wall_time() {
        let clock = Widget::Clock {
            position: Position::Right,
            color: None,
            utc: true,
        };
        assert_eq!(
            text(
                &clock,
                UNIX_EPOCH + Duration::from_secs(86399),
                &Snapshot::default()
            ),
            "23:59 UTC"
        );
        assert_eq!(
            text(
                &clock,
                UNIX_EPOCH + Duration::from_secs(86400),
                &Snapshot::default()
            ),
            "00:00 UTC"
        );
        assert_eq!(
            next_clock_update(UNIX_EPOCH + Duration::from_millis(59500)),
            Duration::from_millis(500)
        );
        assert_eq!(
            next_clock_update(UNIX_EPOCH + Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }
}
