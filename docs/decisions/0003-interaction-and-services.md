# 0003: Input, inline overlays, accessibility, and desktop services

Status: accepted for the v0.1 release candidate.

## Context

The panel used to be display-only. The release candidate adds workspace switching, a
launcher, a system tray, and network, Bluetooth, volume, and battery status. Those
need input, keyboard access, assistive-technology support, and new trust boundaries
(D-Bus peers, desktop entries, tray items). I also want to keep startup and idle cost
low and the configuration declarative and command-free.

## Decision

### Input and keyboard focus

Smithay Client Toolkit's seat handling provides pointer and keyboard (`xkbcommon`).
Left click activates; right click opens a tray item's menu; the wheel adjusts volume
by 5 %. Layer-shell keyboard interactivity is `None` while idle, so clicking never
steals focus, and `Exclusive` only in keyboard mode or while the launcher or a tray
menu is open. Leaving returns it to `None`.

Keyboard entry uses signals from Hyprland binds: `SIGUSR1` enters keyboard mode on the
panel of the focused monitor, and `SIGUSR2` opens the launcher. Signals already flow
through Calloop's signalfd. A control socket would add a listener, a protocol, and an
authentication question, and signals add none of those. Only processes of the same
user can send them, which is the trust boundary I already have.

Held keys do not repeat. The panel sets up no client-side repeat, and repeating an
activation would act repeatedly; the compositor's own key binds cover any need for
repeat.

### Inline overlays

The launcher and tray menus replace the panel's widgets in the same strip, reusing
label layout, hit spans, and the focus ring. Popup surfaces would need
`xdg_popup`/positioner handling, separate buffers against the shared-memory budget,
and a second accessibility tree. The cost is that a menu or launcher hides the other
widgets while it is open, and only one panel can show it at a time.

### Interaction model and ownership

`velnopt-widgets` turns configuration and state into items with roles, accessible
names, and actions; `velnopt-wayland` owns mode and focus state and executes actions.
`velnopt-services` owns state and controls for the desktop integrations. I did not add
a `velnopt-core` crate: there is still no application state independent of the
protocol lifecycle, and a crate with no such owner would become a catch-all.

### Accessibility

AccessKit with its Unix (AT-SPI) adapter publishes one tree per panel surface, only
when the configuration contains an interactive widget. Display-only configurations
create no adapter. Roles are window, tab list and tab (with selection), switch, slider
(increment and decrement), button, search input, list box option, menu item (check
box when checkable), and label. Click and Focus requests are forwarded to the event
loop and checked against the controls of the last drawn frame.

### Services thread model

`velnopt-services` runs only the modules whose widgets are configured: one zbus 5
worker on the system bus for NetworkManager, BlueZ, and UPower; one thread speaking the
PulseAudio native protocol (pipewire-pulse works) for audio; and one zbus worker on the
session bus for the tray. Each publishes the latest immutable snapshot into a slot and
wakes the event loop through a coalescing ping, as `velnopt-hyprland` does. Workers are
driven by D-Bus signals or server events and never poll. Failures retry no more often
than every 2 seconds. Reading never starts a missing service, because the worker
checks that the name has an owner first. Each module reports `Pending`, `Unavailable`,
`PermissionDenied`, `Missing`, or `Ready`, and widgets show explicit text for each.
Dropping the services cancels and joins the workers.

Controls change host state only on explicit input: toggle Bluetooth power, toggle
mute, adjust volume, activate a tray item, and choose a menu entry. Requests go through
bounded queues and are dropped, not replayed, while a service is down. Network and
battery are display-only.

### Launcher safety

Desktop entries are untrusted input. Scanning is bounded (4096 files, 64 KiB each,
2048 applications), strings are sanitized, and `Exec` is tokenized by the Desktop
Entry quoting rules into an argument vector. Field codes are dropped, `%%` is a
literal `%`, and the program is started directly from `PATH` with its arguments, never
through a shell. Standard streams are closed, the directory is `$HOME`, and the child
runs in its own process group. Entries marked `Terminal=true` are hidden because
Velnopt does not choose a terminal emulator. `SIGCHLD` reaps finished children. The
child inherits the panel's environment.

### Tray watcher and host

Velnopt provides `org.kde.StatusNotifierWatcher` only when no other process owns the
name, registers a host, and mirrors the registered items otherwise. A registration
must name a bus name that currently has an owner, and entries disappear when the owner
does. At most 32 items are shown, 64 are remembered, and its watcher accepts at most
16 hosts. Item properties and dbusmenu layouts are sanitized and bounded (64 UTF-8
bytes per string, 64 menu entries, icons at most 256×256 and PNG files at most 1 MiB,
icon names without path separators). Item and menu calls time out after two seconds,
so a stalled application delays only the tray. SVG icons are not decoded.

## Consequences

Idle cost stays low: no module runs unless configured, nothing polls, and an idle
panel holds neither keyboard focus nor an accessibility adapter unless a control
exists. The inline overlays are simple but cover other widgets. Controls are fixed
built-in actions; there is still no configurable command, plugin, scripting, or
Velnopt IPC listener. Dependencies (zbus, AccessKit, the PulseAudio protocol crate)
may contain unsafe code even though project crates forbid it. These choices have no
comparative measurements; see [the performance method](../performance.md).
