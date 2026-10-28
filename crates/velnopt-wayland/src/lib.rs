//! The Hyprland layer-shell lifecycle, with one event loop for rendering, input, and signals.

mod access;
mod input;

use std::{path::PathBuf, sync::Arc, time::SystemTime};

use accesskit::{Action as AccessAction, NodeId};
use calloop::{
    EventLoop, RegistrationToken,
    channel::{Event as ChannelEvent, channel},
    signals::{Signal, Signals},
    timer::{TimeoutAction, Timer},
};
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::client::{
        Connection, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_keyboard, wl_output, wl_pointer, wl_shm, wl_surface},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{SeatState, keyboard::Keysym},
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use velnopt_config::{Config, Edge, Widget};

use velnopt_hyprland::{Snapshot, Subscription};
use velnopt_services::{
    Enabled, Request, Services, Snapshot as ServiceState, Status,
    apps::{self, App as Application},
};
use velnopt_ui::{Geometry, Renderer, Span};
use velnopt_widgets::{Action, Overlay, State, next_clock_update};

use crate::{access::Access, input::Key};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Two buffers per surface, within a 64 MiB total shared-memory budget.
const MAX_POOL_BYTES: usize = 64 * 1024 * 1024;

/// Run the native panel until it is shut down or the compositor errors.
///
/// `path` names the configuration in diagnostics. The caller owns file access and
/// passes `reload`, which runs on SIGHUP. Before applying a replacement, the backend
/// validates output selection and frame budgets. SIGUSR1 moves keyboard focus to the
/// controls of the panel on Hyprland's focused monitor, and SIGUSR2 opens the
/// application launcher there.
///
/// # Errors
///
/// Returns an error if setup, resource allocation, rendering, or compositor I/O
/// fails. A reload error is reported, and the working configuration stays in use.
pub fn run(
    path: PathBuf,
    config: Config,
    mut reload: impl FnMut() -> Result<Config>,
) -> Result<()> {
    // set the signal masks before font discovery or any dependency can start threads
    let signals = Signals::new(&[
        Signal::SIGINT,
        Signal::SIGTERM,
        Signal::SIGHUP,
        Signal::SIGUSR1,
        Signal::SIGUSR2,
        Signal::SIGCHLD,
    ])?;
    let connection = Connection::connect_to_env().map_err(|error| {
        format!(
            "cannot connect to Hyprland's Wayland socket: {error}; run inside your Hyprland session"
        )
    })?;
    let (globals, mut queue) = registry_queue_init(&connection)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|error| format!("layer-shell is unavailable: {error}; Velnopt requires Hyprland or another layer-shell compositor"))?;
    let shm = Shm::bind(&globals, &qh)?;
    let (access_requests, access_source) = channel();
    let mut app = App {
        seat: SeatState::new(&globals, &qh),
        pointer: None,
        keyboard: None,
        shift: false,
        access_requests,
        next_surface: 0,
        focus_requested: false,
        launcher_requested: false,
        services: None,
        service_state: Arc::new(ServiceState::default()),
        service_enabled: Enabled::default(),
        apps: Vec::new(),
        launcher: None,
        menu_panel: None,
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        qh,
        renderer: Renderer::new()?,
        config,
        path,
        panels: Vec::new(),
        outputs_changed: true,
        reload: false,
        quit: false,
        hyprland: Arc::new(Snapshot::default()),
        subscription: None,
    };
    queue.roundtrip(&mut app)?;
    app.validate_resources(&app.config, true)?;
    app.reconcile();

    let mut event_loop = EventLoop::<App>::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(connection.clone(), queue).insert(handle.clone())?;
    handle.insert_source(signals, |event, _, app| match event.signal() {
        Signal::SIGHUP => app.reload = true,
        Signal::SIGINT | Signal::SIGTERM => app.quit = true,
        Signal::SIGUSR1 => app.focus_requested = true,
        Signal::SIGUSR2 => app.launcher_requested = true,
        Signal::SIGCHLD => apps::reap(),
        _ => {}
    })?;
    handle.insert_source(access_source, |event, _, app| {
        if let ChannelEvent::Msg((surface, request)) = event {
            app.access_request(surface, request.action, request.target_node);
        }
    })?;
    let mut clock: Option<RegistrationToken> = None;
    let mut hyprland_source: Option<RegistrationToken> = None;
    let mut services_source: Option<RegistrationToken> = None;
    eprintln!(
        "Velnopt running (pid {}); send SIGHUP to reload {}",
        std::process::id(),
        app.path.display()
    );

    loop {
        if app.quit {
            break;
        }
        if app.reload {
            app.reload = false;
            match reload().and_then(|candidate| {
                app.validate_resources(&candidate, true)?;
                Ok(candidate)
            }) {
                Ok(candidate) => {
                    app.config = candidate;
                    app.renderer.clear_cache();
                    app.outputs_changed = true;
                    let interactive = interactive(&app.config);
                    for panel in &mut app.panels {
                        if !interactive {
                            panel.keyboard_mode = false;
                            panel.focus = None;
                        }
                        configure_layer(&panel.layer, &app.config, panel.keyboard_mode);
                        if interactive != panel.access.is_some() {
                            panel.access = interactive
                                .then(|| Access::new(panel.id, app.access_requests.clone()));
                        }
                        panel.dirty = true;
                    }
                    eprintln!("reloaded {}", app.path.display());
                }
                Err(error) => eprintln!(
                    "{}: reload rejected; keeping current configuration: {error}",
                    app.path.display()
                ),
            }
        }
        if app.outputs_changed {
            app.validate_resources(&app.config, false)?;
            app.reconcile();
        }
        if std::mem::take(&mut app.focus_requested)
            && let Some(index) = app.focused_panel()
        {
            app.enter_keyboard_mode(index, None);
        }
        if std::mem::take(&mut app.launcher_requested)
            && let Some(index) = app.focused_panel()
        {
            app.open_launcher(index);
        }
        let enabled = if app.panels.is_empty() {
            Enabled::default()
        } else {
            services_enabled(&app.config)
        };
        if enabled != app.service_enabled {
            if let Some(token) = services_source.take() {
                handle.remove(token);
            }
            // dropping the handle cancels and joins its workers before the restart
            app.services = None;
            app.service_state = Arc::new(ServiceState::default());
            app.service_enabled = enabled;
            if enabled != Enabled::default() {
                let (services, source) = Services::new(enabled)?;
                app.services = Some(services);
                services_source = Some(handle.insert_source(source, |(), _, app| {
                    if let Some(services) = &app.services {
                        let snapshot = services.snapshot();
                        if snapshot != app.service_state {
                            app.service_state = snapshot;
                            // tray titles and menus are arbitrary text, so drop the old glyphs
                            app.renderer.clear_cache();
                            for panel in &mut app.panels {
                                panel.dirty = true;
                            }
                        }
                    }
                })?);
            }
            for panel in &mut app.panels {
                panel.dirty = true;
            }
        }
        let needs_hyprland = !app.panels.is_empty()
            && app.config.panel().widgets().iter().any(|widget| {
                matches!(
                    widget,
                    Widget::Workspaces { .. } | Widget::WindowTitle { .. }
                )
            });
        if needs_hyprland && app.subscription.is_none() {
            let (subscription, source) = Subscription::new()?;
            app.subscription = Some(subscription);
            hyprland_source = Some(handle.insert_source(source, |(), _, app| {
                if let Some(subscription) = &app.subscription {
                    let snapshot = subscription.snapshot();
                    if snapshot != app.hyprland {
                        if snapshot.error() != app.hyprland.error()
                            && let Some(error) = snapshot.error()
                        {
                            eprintln!("Hyprland IPC unavailable: {error}");
                        }
                        if snapshot.connected() && !app.hyprland.connected() {
                            eprintln!("Hyprland IPC connected");
                        }
                        app.hyprland = snapshot;
                        app.renderer.clear_cache();
                        for panel in &mut app.panels {
                            panel.dirty = true;
                        }
                    }
                }
            })?);
        } else if !needs_hyprland && let Some(token) = hyprland_source.take() {
            handle.remove(token);
            app.subscription = None;
            app.hyprland = Arc::new(Snapshot::default());
        }
        let needs_clock = !app.panels.is_empty()
            && app
                .config
                .panel()
                .widgets()
                .iter()
                .any(|widget| matches!(widget, Widget::Clock { .. }));
        if needs_clock && clock.is_none() {
            clock = Some(handle.insert_source(
                Timer::from_duration(next_clock_update(SystemTime::now())),
                |_, _, app| {
                    for panel in &mut app.panels {
                        panel.dirty = true;
                    }
                    TimeoutAction::ToDuration(next_clock_update(SystemTime::now()))
                },
            )?);
        } else if !needs_clock && let Some(token) = clock.take() {
            handle.remove(token);
        }
        app.draw()?;
        connection.flush()?;
        event_loop.dispatch(None, &mut app)?;
    }
    // LayerSurface and the wl_surface it owns are destroyed by their RAII owners
    app.panels.clear();
    connection.flush()?;
    Ok(())
}

struct App {
    registry: RegistryState,
    seat: SeatState,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    shift: bool,
    access_requests: calloop::channel::Sender<access::Request>,
    next_surface: u64,
    focus_requested: bool,
    launcher_requested: bool,
    services: Option<Services>,
    service_state: Arc<ServiceState>,
    service_enabled: Enabled,
    /// Applications found the last time the launcher opened.
    apps: Vec<Application>,
    launcher: Option<Launcher>,
    /// The panel that asked for the tray menu in the service snapshot.
    menu_panel: Option<u64>,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<Self>,
    renderer: Renderer,
    config: Config,
    path: PathBuf,
    panels: Vec<Surface>,
    outputs_changed: bool,
    reload: bool,
    quit: bool,
    hyprland: Arc<Snapshot>,
    subscription: Option<Subscription>,
}

struct Surface {
    id: u64,
    output: wl_output::WlOutput,
    layer: LayerSurface,
    /// Exclusive keyboard focus was requested so the controls can be navigated.
    keyboard_mode: bool,
    /// The compositor gave this surface keyboard focus.
    keyboard_focus: bool,
    /// Focused control, tracked by identity so live updates don't move focus.
    focus: Option<Action>,
    /// Controls of the last drawn frame: visible spans, reading order,
    /// the current choice, and accessibility node ids.
    hits: Vec<(Span, Action)>,
    controls: Vec<Action>,
    selected: Option<Action>,
    nodes: Vec<(NodeId, Action)>,
    access: Option<Access>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    dirty: bool,
    frame_pending: bool,
    backing: Option<Backing>,
}

/// The launcher shown in place of one panel's widgets.
struct Launcher {
    panel: u64,
    query: String,
    results: Vec<usize>,
}

const MAX_QUERY_BYTES: usize = 128;
const MAX_RESULTS: usize = 16;
const VOLUME_STEP: i8 = 5;

struct Backing {
    geometry: Geometry,
    pool: SlotPool,
    buffers: [Buffer; 2],
}

impl Backing {
    fn new(geometry: Geometry, shm: &Shm) -> Result<Self> {
        let mut pool = SlotPool::new(geometry.byte_len() * 2, shm)?;
        let (first, _) = pool.create_buffer(
            geometry.width(),
            geometry.height(),
            geometry.stride(),
            wl_shm::Format::Xrgb8888,
        )?;
        let (second, _) = pool.create_buffer(
            geometry.width(),
            geometry.height(),
            geometry.stride(),
            wl_shm::Format::Xrgb8888,
        )?;
        Ok(Self {
            geometry,
            pool,
            buffers: [first, second],
        })
    }
}

/// Whether the configuration has any controls that take input.
fn interactive(config: &Config) -> bool {
    config.panel().widgets().iter().any(|widget| {
        matches!(
            widget,
            Widget::Workspaces { .. }
                | Widget::Bluetooth { .. }
                | Widget::Volume { .. }
                | Widget::Launcher { .. }
                | Widget::Tray { .. }
        )
    })
}

/// The desktop-service modules the configured widgets need.
fn services_enabled(config: &Config) -> Enabled {
    let mut enabled = Enabled::default();
    for widget in config.panel().widgets() {
        match widget {
            Widget::Network { .. } => enabled.network = true,
            Widget::Bluetooth { .. } => enabled.bluetooth = true,
            Widget::Volume { .. } => enabled.audio = true,
            Widget::Battery { .. } => enabled.battery = true,
            Widget::Tray { .. } => enabled.tray = true,
            _ => {}
        }
    }
    enabled
}

fn configure_layer(layer: &LayerSurface, config: &Config, keyboard: bool) {
    let panel = config.panel();
    let edge = match panel.edge() {
        Edge::Top => Anchor::TOP,
        Edge::Bottom => Anchor::BOTTOM,
    };
    layer.set_anchor(edge | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, u32::from(panel.height().get()));
    layer.set_exclusive_zone(i32::from(panel.height().get()));
    layer.set_keyboard_interactivity(if keyboard {
        KeyboardInteractivity::Exclusive
    } else {
        KeyboardInteractivity::None
    });
    layer.commit();
}

impl App {
    fn selected_outputs(&self, config: &Config) -> Vec<wl_output::WlOutput> {
        self.outputs
            .outputs()
            .filter(|output| {
                self.outputs.info(output).is_some_and(|info| {
                    config
                        .panel()
                        .output()
                        .is_none_or(|wanted| info.name.as_deref() == Some(wanted))
                })
            })
            .collect()
    }

    fn validate_resources(&self, config: &Config, require_output: bool) -> Result<()> {
        let outputs = self.selected_outputs(config);
        if require_output && outputs.is_empty() {
            return Err(format!("no matching output is connected (requested {}); inspect names with hyprctl monitors", config.panel().output().unwrap_or("all outputs")).into());
        }
        let mut total = 0;
        for output in outputs {
            let Some(info) = self.outputs.info(&output) else {
                continue;
            };
            let width = self
                .panels
                .iter()
                .find(|panel| panel.output == output)
                .map(|panel| panel.width)
                .or_else(|| info.logical_size.map(|(width, _)| width.max(1) as u32))
                .unwrap_or(1920);
            let geometry = Geometry::new(
                width,
                u32::from(config.panel().height().get()),
                info.scale_factor,
            )?;
            total += geometry.byte_len() * 2;
            if total > MAX_POOL_BYTES {
                return Err("panels exceed the aggregate 64 MiB shared-memory limit".into());
            }
        }
        Ok(())
    }

    fn reconcile(&mut self) {
        self.outputs_changed = false;
        let selected = self.selected_outputs(&self.config);
        self.panels.retain(|panel| selected.contains(&panel.output));
        for output in selected {
            if self.panels.iter().any(|panel| panel.output == output) {
                continue;
            }
            let surface = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh,
                surface,
                Layer::Top,
                Some("velnopt"),
                Some(&output),
            );
            configure_layer(&layer, &self.config, false);
            let scale = self
                .outputs
                .info(&output)
                .map_or(1, |info| info.scale_factor);
            let id = self.next_surface;
            self.next_surface += 1;
            self.panels.push(Surface {
                id,
                output,
                layer,
                keyboard_mode: false,
                keyboard_focus: false,
                focus: None,
                hits: Vec::new(),
                controls: Vec::new(),
                selected: None,
                nodes: Vec::new(),
                access: interactive(&self.config)
                    .then(|| Access::new(id, self.access_requests.clone())),
                width: 1920,
                height: u32::from(self.config.panel().height().get()),
                scale,
                configured: false,
                dirty: true,
                frame_pending: false,
                backing: None,
            });
        }
    }

    /// Run a control activated on the panel at `index`. Each request is
    /// validated by the code that owns it.
    fn activate(&mut self, index: usize, action: Action) {
        match action {
            Action::Workspace(id) => match &self.subscription {
                Some(subscription) => {
                    if let Err(error) = subscription.switch_workspace(id) {
                        eprintln!("workspace switch rejected: {error}");
                    }
                }
                None => eprintln!("workspace switch unavailable: Hyprland IPC is not running"),
            },
            Action::ToggleBluetooth => self.request(Request::ToggleBluetooth),
            Action::ToggleMute => self.request(Request::ToggleMute),
            Action::OpenLauncher => self.open_launcher(index),
            Action::Launch(app) => {
                if let Some(app) = self.apps.get(app) {
                    match apps::launch(app) {
                        Ok(()) => eprintln!("launched {}", app.id()),
                        Err(error) => eprintln!("cannot launch {}: {error}", app.id()),
                    }
                }
                self.close_overlay();
            }
            Action::Tray(item) => self.tray(index, item, false),
            Action::MenuEntry(entry) => {
                let Some(menu) = self.service_state.menu.clone() else {
                    return;
                };
                match menu.entries.iter().find(|candidate| candidate.id == entry) {
                    Some(found) if found.submenu => self.request(Request::TrayMenu {
                        item: menu.item.clone(),
                        parent: entry,
                    }),
                    Some(_) => {
                        self.request(Request::MenuEvent {
                            item: menu.item.clone(),
                            entry,
                        });
                        self.close_overlay();
                    }
                    None => {}
                }
            }
        }
    }

    fn request(&self, request: Request) {
        match &self.services {
            Some(services) => services.request(request),
            None => eprintln!("desktop services are not running"),
        }
    }

    /// Activate a tray item. It opens its menu on this panel instead if the item is
    /// menu-only, if `menu` is set (secondary click), or if the keyboard did it.
    fn tray(&mut self, index: usize, item: usize, menu: bool) {
        // the index points into the frame the user saw; turn it into the item's id now,
        // so a later request can't hit a different item
        let Status::Ready(items) = &self.service_state.tray else {
            return;
        };
        let Some(item) = items.get(item) else {
            return;
        };
        let keyboard = self
            .panels
            .get(index)
            .is_some_and(|panel| panel.keyboard_mode);
        let id = item.id.clone();
        if item.has_menu && (menu || keyboard || item.item_is_menu) {
            self.launcher = None;
            self.menu_panel = self.panels.get(index).map(|panel| panel.id);
            self.request(Request::TrayMenu {
                item: id,
                parent: 0,
            });
            self.request_keyboard(index, None);
        } else if menu {
            self.request(Request::TraySecondary(id));
        } else {
            self.request(Request::TrayActivate(id));
        }
    }

    /// Show the launcher on a panel. Applications are rediscovered every time.
    fn open_launcher(&mut self, index: usize) {
        let Some(panel) = self.panels.get(index) else {
            eprintln!("launcher request ignored: no panel is shown");
            return;
        };
        if self.menu_panel.take().is_some() {
            self.request(Request::CloseMenu);
        }
        // shortcut: scans desktop entries on the event loop (bounded to 4096 files
        // of 64 KiB); move to a worker if slow home directories make this visible.
        self.apps = apps::discover();
        self.launcher = Some(Launcher {
            panel: panel.id,
            query: String::new(),
            results: apps::search(&self.apps, "", MAX_RESULTS),
        });
        self.update_launcher();
        self.request_keyboard(index, self.panels[index].focus);
    }

    /// Re-rank for the current query and focus the best match.
    fn update_launcher(&mut self) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        launcher.results = apps::search(&self.apps, &launcher.query, MAX_RESULTS);
        let focus = launcher.results.first().map(|&index| Action::Launch(index));
        if let Some(panel) = self
            .panels
            .iter_mut()
            .find(|panel| panel.id == launcher.panel)
        {
            panel.focus = focus;
            panel.dirty = true;
        }
    }

    /// Close the launcher or tray menu and give up keyboard focus.
    fn close_overlay(&mut self) {
        let panel = self
            .launcher
            .take()
            .map(|launcher| launcher.panel)
            .or(self.menu_panel);
        if self.menu_panel.take().is_some() {
            self.request(Request::CloseMenu);
        }
        if let Some(surface) = panel
            .and_then(|id| self.panels.iter().find(|panel| panel.id == id))
            .map(|panel| panel.layer.wl_surface().clone())
        {
            self.leave_keyboard_mode(Some(surface));
        }
    }

    /// Handle typed text and editing keys while the launcher has focus.
    /// Returns whether the key was used.
    fn launcher_text(&mut self, keysym: Keysym, text: Option<&str>) -> bool {
        let Some(launcher) = &mut self.launcher else {
            return false;
        };
        if !self
            .panels
            .iter()
            .any(|panel| panel.id == launcher.panel && panel.keyboard_focus)
        {
            return false;
        }
        if keysym == Keysym::BackSpace {
            launcher.query.pop();
        } else if let Some(text) = text
            && (keysym == Keysym::space || Key::from_keysym(keysym, false).is_none())
            && !text.is_empty()
            && !text.chars().any(char::is_control)
        {
            if launcher.query.len() + text.len() > MAX_QUERY_BYTES {
                return true;
            }
            launcher.query.push_str(text);
        } else {
            return false;
        }
        self.update_launcher();
        true
    }

    /// The panel on Hyprland's focused monitor, or the first panel.
    fn focused_panel(&self) -> Option<usize> {
        let focused = self
            .hyprland
            .monitors()
            .iter()
            .find(|monitor| monitor.focused())
            .map(|monitor| monitor.name().to_owned());
        let index = self
            .panels
            .iter()
            .position(|panel| {
                focused.is_some()
                    && self.outputs.info(&panel.output).and_then(|info| info.name) == focused
            })
            .unwrap_or(0);
        (!self.panels.is_empty()).then_some(index)
    }

    /// Ask for exclusive keyboard focus for a panel's controls.
    fn enter_keyboard_mode(&mut self, index: usize, focus: Option<Action>) {
        let Some(panel) = self.panels.get_mut(index) else {
            eprintln!("keyboard focus request ignored: no panel is shown");
            return;
        };
        if panel.controls.is_empty() {
            eprintln!("keyboard focus request ignored: the panel has no controls");
            return;
        }
        let focus = focus.or(panel.focus).or(panel.selected);
        self.request_keyboard(index, focus);
    }

    /// Ask for exclusive keyboard focus, overlays on display-only panels included.
    fn request_keyboard(&mut self, index: usize, focus: Option<Action>) {
        let Some(panel) = self.panels.get_mut(index) else {
            return;
        };
        panel.focus = focus;
        if !panel.keyboard_mode {
            panel.keyboard_mode = true;
            configure_layer(&panel.layer, &self.config, true);
        }
        panel.dirty = true;
    }

    /// Release keyboard focus on one surface, or on every panel when `None`.
    /// Overlays on panels that lose focus close.
    fn leave_keyboard_mode(&mut self, surface: Option<wl_surface::WlSurface>) {
        let released = |panel: &Surface| {
            surface
                .as_ref()
                .is_none_or(|surface| panel.layer.wl_surface() == surface)
        };
        if let Some(launcher) = &self.launcher
            && self
                .panels
                .iter()
                .any(|panel| panel.id == launcher.panel && released(panel))
        {
            self.launcher = None;
        }
        if let Some(id) = self.menu_panel
            && self
                .panels
                .iter()
                .any(|panel| panel.id == id && released(panel))
        {
            self.menu_panel = None;
            self.request(Request::CloseMenu);
        }
        for panel in &mut self.panels {
            if surface
                .as_ref()
                .is_some_and(|surface| panel.layer.wl_surface() != surface)
            {
                continue;
            }
            if panel.keyboard_focus
                && let Some(access) = &mut panel.access
            {
                access.set_focused(false);
            }
            panel.keyboard_focus = false;
            if panel.keyboard_mode {
                panel.keyboard_mode = false;
                panel.focus = None;
                configure_layer(&panel.layer, &self.config, false);
                panel.dirty = true;
            }
        }
    }

    /// Handle a key on the panel that has keyboard focus.
    fn key(&mut self, key: Key) {
        let Some(index) = self
            .panels
            .iter()
            .position(|panel| panel.keyboard_focus && panel.keyboard_mode)
        else {
            return;
        };
        let overlay = self.launcher.is_some() || self.menu_panel.is_some();
        let key = match key {
            // Up and Down adjust a focused volume slider, and navigate otherwise
            Key::Up | Key::Down if self.panels[index].focus == Some(Action::ToggleMute) => {
                let step = if key == Key::Up {
                    VOLUME_STEP
                } else {
                    -VOLUME_STEP
                };
                self.request(Request::AdjustVolume(step));
                return;
            }
            Key::Up => Key::Previous,
            Key::Down => Key::Next,
            key => key,
        };
        let panel = &mut self.panels[index];
        let count = panel.controls.len();
        let current = panel
            .focus
            .and_then(|focus| panel.controls.iter().position(|action| *action == focus));
        let next = match key {
            Key::Leave if overlay => {
                self.close_overlay();
                return;
            }
            Key::Leave => {
                let surface = panel.layer.wl_surface().clone();
                self.leave_keyboard_mode(Some(surface));
                return;
            }
            Key::Activate => {
                if let Some(action) = panel.focus {
                    self.activate(index, action);
                }
                return;
            }
            _ if count == 0 => return,
            Key::Next => current.map_or(0, |index| (index + 1) % count),
            Key::Previous => current.map_or(count - 1, |index| (index + count - 1) % count),
            Key::First => 0,
            Key::Last => count - 1,
            Key::Up | Key::Down => unreachable!("mapped to navigation above"),
        };
        panel.focus = Some(panel.controls[next]);
        panel.dirty = true;
    }

    /// Carry out an assistive technology's request on a panel control.
    fn access_request(&mut self, surface: u64, action: AccessAction, node: NodeId) {
        let Some(index) = self.panels.iter().position(|panel| panel.id == surface) else {
            return;
        };
        let Some(target) = self.panels[index]
            .nodes
            .iter()
            .find(|(id, _)| *id == node)
            .map(|(_, action)| *action)
        else {
            return;
        };
        match (action, target) {
            (AccessAction::Click, _) => self.activate(index, target),
            (AccessAction::Focus, _) => self.enter_keyboard_mode(index, Some(target)),
            (AccessAction::Increment, Action::ToggleMute) => {
                self.request(Request::AdjustVolume(VOLUME_STEP));
            }
            (AccessAction::Decrement, Action::ToggleMute) => {
                self.request(Request::AdjustVolume(-VOLUME_STEP));
            }
            _ => {}
        }
    }

    fn draw(&mut self) -> Result<()> {
        let mut total = 0;
        for panel in &self.panels {
            if panel.configured {
                total += Geometry::new(panel.width, panel.height, panel.scale)?.byte_len() * 2;
                if total > MAX_POOL_BYTES {
                    return Err(
                        "configured surfaces exceed the aggregate 64 MiB shared-memory limit"
                            .into(),
                    );
                }
            }
        }
        // drop every outdated client mapping before allocating new ones, so a resize that
        // grows one surface and shrinks another stays within the total budget
        for panel in &mut self.panels {
            if panel.configured
                && panel.backing.as_ref().is_some_and(|backing| {
                    Geometry::new(panel.width, panel.height, panel.scale)
                        .is_ok_and(|geometry| backing.geometry != geometry)
                })
            {
                panel.backing = None;
            }
        }
        for panel in &mut self.panels {
            if !panel.configured || !panel.dirty || panel.frame_pending {
                continue;
            }
            let geometry = Geometry::new(panel.width, panel.height, panel.scale)?;
            if panel.backing.is_none() {
                panel.backing = Some(Backing::new(geometry, &self.shm)?);
            }
            let Some(backing) = &mut panel.backing else {
                continue;
            };
            let Some(buffer) = backing
                .buffers
                .iter()
                .find(|buffer| buffer.canvas(&mut backing.pool).is_some())
            else {
                continue;
            };
            let Some(canvas) = buffer.canvas(&mut backing.pool) else {
                continue;
            };
            let output = self.outputs.info(&panel.output).and_then(|info| info.name);
            let results: Vec<(usize, &Application)>;
            let overlay = match (&self.launcher, &self.service_state.menu) {
                (Some(launcher), _) if launcher.panel == panel.id => {
                    results = launcher
                        .results
                        .iter()
                        .filter_map(|&index| Some((index, self.apps.get(index)?)))
                        .collect();
                    Some(Overlay::Launcher {
                        query: &launcher.query,
                        results: &results,
                    })
                }
                (_, Some(menu)) if self.menu_panel == Some(panel.id) => Some(Overlay::Menu(menu)),
                _ => None,
            };
            let state = State {
                hyprland: &self.hyprland,
                services: &self.service_state,
                output: output.as_deref(),
                overlay,
            };
            let mut items = velnopt_widgets::items(&self.config, SystemTime::now(), state, None);
            let mut focus = panel
                .focus
                .and_then(|focus| items.iter().position(|item| item.action == Some(focus)));
            if panel.keyboard_mode && focus.is_none() {
                // the focused control is gone or was never set: use the current choice,
                // else the first control
                let order = velnopt_widgets::focusable(&items);
                focus = order
                    .iter()
                    .copied()
                    .find(|&index| items[index].selected)
                    .or(order.first().copied());
                panel.focus = focus.and_then(|index| items[index].action);
            }
            if !panel.keyboard_mode {
                focus = None;
            }
            if let Some(index) = focus {
                items[index].label.focused = true;
            }
            let labels: Vec<_> = items.iter().map(|item| item.label.clone()).collect();
            let spans = self
                .renderer
                .draw(canvas, geometry, &self.config, &labels)?;
            panel.hits = items
                .iter()
                .zip(&spans)
                .filter_map(|(item, span)| Some(((*span)?, item.action?)))
                .collect();
            panel.controls = velnopt_widgets::focusable(&items)
                .into_iter()
                .filter_map(|index| items[index].action)
                .collect();
            panel.selected = items
                .iter()
                .find(|item| item.selected)
                .and_then(|item| item.action);
            panel.nodes = items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    Some((velnopt_widgets::node_id(index, item), item.action?))
                })
                .collect();
            if let Some(access) = &mut panel.access {
                let title = match &output {
                    Some(name) => format!("Velnopt panel on {name}"),
                    None => "Velnopt panel".to_owned(),
                };
                access.update(velnopt_widgets::accessibility(
                    &title,
                    &items,
                    &spans,
                    panel.scale,
                    f64::from(panel.height),
                    focus,
                ));
            }
            let surface = panel.layer.wl_surface();
            surface.set_buffer_scale(panel.scale);
            surface.damage_buffer(0, 0, geometry.width(), geometry.height());
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            buffer.attach_to(surface)?;
            panel.layer.commit();
            panel.frame_pending = true;
            panel.dirty = false;
        }
        Ok(())
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Some(panel) = self
            .panels
            .iter_mut()
            .find(|panel| panel.layer.wl_surface() == surface)
        {
            panel.scale = factor;
            panel.dirty = true;
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(panel) = self
            .panels
            .iter_mut()
            .find(|panel| panel.layer.wl_surface() == surface)
        {
            panel.frame_pending = false;
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed = true;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed = true;
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed = true;
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.panels.retain(|panel| &panel.layer != layer);
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(panel) = self.panels.iter_mut().find(|panel| &panel.layer == layer) {
            panel.width = if configure.new_size.0 == 0 {
                panel.width
            } else {
                configure.new_size.0
            };
            panel.height = if configure.new_size.1 == 0 {
                u32::from(self.config.panel().height().get())
            } else {
                configure.new_size.1
            };
            panel.configured = true;
            panel.dirty = true;
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);
impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
smithay_client_toolkit::delegate_dispatch2!(App);
