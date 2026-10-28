# Configuration reference

Configuration is UTF-8 TOML, limited to 1 MiB. Every table rejects unknown keys, and
missing required fields, duplicate keys, invalid types, and unknown widget kinds are
errors. The schema is still experimental.

Both [the minimal example](../examples/panel.toml) and
[the full Hyprland example](../examples/hyprland.toml) are tested.
The [live example](../examples/live-hyprland.toml) adds workspace and window data, and
the [desktop example](../examples/desktop.toml) adds controls and desktop services.

## Root and panel

| Field | Required/default | Meaning |
| --- | --- | --- |
| `version` | Required: `1` | Schema version |
| `panel.edge` | Required: `"top"` or `"bottom"` | Horizontal screen edge |
| `panel.height` | Required: integer 1–65535 | Logical panel height |
| `panel.output` | All connected outputs | One exact output name from `hyprctl monitors` |
| `panel.widgets` | Velnopt label on the left, local clock on the right | Ordered widgets, at most 24 |

A height can parse fine and still exceed the runtime frame limits for the selected
monitor width and scale. Startup and reload reject such requests. For readable text,
28–48 logical pixels is typical. An empty `widgets = []` gives you a deliberately blank
strip, with no periodic timer.

An output name must be 1–256 bytes with no control characters. Startup or reload with
an explicitly named monitor that isn't present is rejected. Once running, unplugging
the monitor removes its surface and plugging it back in restores it.

## Appearance

All fields in `[panel.appearance]` are optional:

| Field | Default | Accepted values |
| --- | --- | --- |
| `background` | `"#152c2b"` | Opaque color in strict `#RRGGBB` notation |
| `foreground` | `"#e4ede6"` | Default text color in `#RRGGBB` notation |
| `font` | `"sans-serif"` | System family name, or `sans-serif`, `serif`, `monospace`; 1–128 bytes |
| `font_size` | `14` | Integer 4–128 logical pixels |
| `padding` | `16` | Integer 0–256 logical pixels, horizontal outer inset |
| `gap` | `24` | Integer 0–256 logical pixels between widgets within a group |

The full example uses height 36 and padding 18. Font names cannot contain control
characters. A missing font family falls back to the system's choice, and if no fonts
are installed at all, the runtime reports an error. Font files are not bundled.

Colors are opaque. Transparency and named CSS colors are unsupported. When you pick
custom colors, keep a text/background contrast of at least 4.5:1.

## Widgets

Each `[[panel.widgets]]` table has `kind`, optional `position` (default `"left"`),
and optional `color` (default shared foreground). Positions are `"left"`,
`"center"`, or `"right"`. Widgets within a group preserve declaration order.

Text widgets require `text`, up to 1024 UTF-8 bytes without control characters:

```toml
[[panel.widgets]]
kind = "text"
text = "Velnopt"
position = "left"
color = "#96d1bd"
```

Clock widgets show local time as `HH:MM`. Set `utc = true` for `HH:MM UTC`:

```toml
[[panel.widgets]]
kind = "clock"
position = "right"
utc = false
```

Live widgets require the matching Hyprland session's `XDG_RUNTIME_DIR` and
`HYPRLAND_INSTANCE_SIGNATURE`:

```toml
[[panel.widgets]]
kind = "workspaces"
position = "left"

[[panel.widgets]]
kind = "window-title"
position = "center"
```

Both accept only the common `position` and `color` options. `workspaces` lists the
regular workspaces Hyprland reports on the panel's own monitor, sorted by ID, and
brackets that monitor's active workspace, for example `1  [work]  3`. Special
workspaces (negative IDs) are not listed. A panel on an output that doesn't match a
Hyprland monitor lists every regular workspace and brackets the focused one. Each
listed workspace is a control; see [Input](#input).

`window-title` shows the focused title, `No focused window` when none is focused, or
`Untitled window` for an empty title. Both live widgets show `Hyprland unavailable` on
IPC failure and clear stale data, while other widgets keep working. Socket errors retry
every 500 ms. A missing or invalid environment means restarting from the correct
session. The client runs only while a selected surface has a live widget.

Replies are bounded to 1 MiB each, and snapshots to 256 workspaces and 32 monitors.
Names are capped at 128 UTF-8 bytes, and titles and combined workspace labels at 1024.
Control characters and line separators become spaces, and truncation preserves UTF-8
and adds an ellipsis. No payload executes commands. `velnopt check` validates these
widgets without IPC.

### Desktop widgets

These widgets accept only `kind`, `position`, and `color`. Each one runs only the
[desktop service](#desktop-services) it needs, and only while the configuration
contains it. They never run configured commands: a widget performs its fixed action on
explicit input and nothing else.

| Kind | Shows | Activation |
| --- | --- | --- |
| `network` | NetworkManager's primary connection | None (display-only) |
| `bluetooth` | The first BlueZ adapter | Toggles adapter power |
| `volume` | The default audio output | Toggles mute; wheel or Up/Down changes volume by 5 % |
| `battery` | UPower's aggregate battery | None (display-only) |
| `launcher` | An `Apps` button | Opens the [launcher](#launcher) |
| `tray` | System tray items | Activates the item; secondary click or keyboard opens its [menu](#tray) |

```toml
[[panel.widgets]]
kind = "tray"
position = "right"

[[panel.widgets]]
kind = "volume"
position = "right"
```

The full set is in [`examples/desktop.toml`](../examples/desktop.toml). The texts
below are exact.

| Widget | Text |
| --- | --- |
| `network`, connected | `Wi-Fi <name> <strength>%`, `Wi-Fi <name>`, `Ethernet`, `<name>` for another link, or `Connected` when it has no name |
| `network`, other | `<link> (limited)` with no verified internet, `Connecting…`, `Offline` |
| `bluetooth` | `Bluetooth <n>` with `n` connected devices, `Bluetooth on`, `Bluetooth off`, `Bluetooth blocked`; ` (not permitted)` is appended after a refused toggle |
| `volume` | `Volume <n>%` or `Muted`; ` (not permitted)` is appended after a refused change |
| `battery` | `Battery <n>%`, with ` charging`, ` full`, or ` not charging` |
| `tray` | One button per item, drawn as its icon, or its title when no icon is available. Nothing is drawn for an empty tray |

A service that cannot provide data replaces its text with one of these states:

| State | Text (`<Service>` is `Network`, `Bluetooth`, `Audio`, `Battery`, or `Tray`) |
| --- | --- |
| Not yet read | `<Service>…` |
| Bus or server not running or unreachable | `<Service> unavailable` |
| Read refused | `<Service>: no permission` |
| No device | `No network device`, `No bluetooth`, `No audio output`, `No battery`, `No tray` |

A bluetooth adapter blocked by rfkill is shown but cannot be toggled. Volume above
100 % is displayed (up to 150 %) but never raised by Velnopt; changes clamp to 0–100 %.

### Input

- Left click activates the control under the pointer.
- Right click on a tray item opens its menu, or sends the item's secondary
  activation when it has none.
- The wheel over `volume` changes the volume by 5 % per step; up raises it.
- Panels request no keyboard focus while idle, so clicking never takes focus from
  the user's window.

Keyboard access uses two signals, normally sent from Hyprland binds (see
[Hyprland setup](hyprland.md#keyboard-access)):

- `SIGUSR1` enters keyboard mode on the panel of Hyprland's focused monitor (the first
  panel without a match). The panel takes exclusive keyboard focus and draws a focus
  ring on the focused control. Tab, Right, and Down move forward; Shift+Tab, Left, and
  Up move back; Home and End jump to the first and last control; Enter or Space
  activates; on a focused `volume`, Up and Down change the volume instead. Escape
  leaves, and so does losing focus. Held keys do not repeat; the panel does no
  client-side key repeat.
- `SIGUSR2` opens the launcher on that panel.

Signals come from processes of the same user. A panel with no controls ignores
`SIGUSR1`.

### Launcher

The launcher replaces the panel's widgets in the same strip with a prompt (`Run: `)
and up to 16 ranked matches. It reads desktop entries from the XDG data directories
each time it opens. Type to search (up to 128 bytes), Backspace edits,
the arrow keys and Tab move through matches, Enter launches the focused match, and
Escape closes it. Name prefixes rank before word prefixes, name substrings, then
generic names and keywords. Clicking a match launches it.

Entries are skipped when `Hidden` or `NoDisplay` is set, when `OnlyShowIn`/`NotShowIn`
excludes this desktop, when `TryExec` is missing, when `Exec` is invalid, and when
`Terminal=true` (Velnopt does not choose a terminal emulator for you). Applications start
directly from their parsed `Exec` argument vector, without a shell; field codes such
as `%u` are dropped. See [the services decision](decisions/0003-interaction-and-services.md).

### Tray

The tray hosts StatusNotifierItem applications on the session bus. If no other
process provides `org.kde.StatusNotifierWatcher`, Velnopt provides it. At most 32
items are shown. A menu replaces the panel's widgets with up to 64 entries from the
item's dbusmenu: checkable entries show `✓`, entries with a submenu end in `▸`, and
disabled entries cannot be chosen. Enter or a click chooses an entry; Escape closes
the menu. SVG icons are not decoded, so such items show their title.

### Desktop services

| Module | Bus or protocol | Notes |
| --- | --- | --- |
| Network, Bluetooth, battery | System bus: NetworkManager, BlueZ, UPower | One worker. Reading never starts a missing service |
| Audio | PulseAudio protocol (`pipewire-pulse` works) | One worker thread; honors `PULSE_SERVER` |
| Tray | Session bus | One worker thread |

Modules for widgets you haven't configured open no connection and start no thread.
Services are event driven and never poll. A bus failure retries at most every 2 seconds
and drops requests made during the outage rather than replaying them. Calls time out
after 2 seconds (750 ms for audio I/O), at most 8 requests queue per audio or tray
worker, Bluetooth counts at most 64 devices, and service strings are single-line and
at most 64 UTF-8 bytes. Tray icons are at most 256×256 pixels and PNG files at most
1 MiB. Controls run only on explicit input.

### Layout and accessibility

Three equal-width groups keep widgets from overlapping. A label that is too long gets
an ellipsis, and an oversized group is clipped to its own area. Workspaces and tray
items sit closer together than other widgets. There is no scrolling, wrapping, command
execution, or scripting.

When the configuration contains `workspaces`, `bluetooth`, `volume`, `launcher`, or
`tray`, each panel registers with AT-SPI through AccessKit. A display-only
configuration creates no adapter. Roles: window; tab list and tab (with selection)
for workspaces; switch for Bluetooth; slider with increment and decrement for
volume; button for the launcher and tray items; search input and list box options
for the launcher; menu items (check box items when checkable) for tray menus; and
labels for text. Click and Focus actions are exposed; Focus enters keyboard mode.

## Check, run, reload

```sh
velnopt check ~/.config/velnopt/panel.toml
velnopt run ~/.config/velnopt/panel.toml
```

`check` never connects to a compositor. Both commands need a regular file and share
the same bounded reader and parser. Symlinks to regular files work. Pipes, devices,
directories, invalid UTF-8, and oversized files are rejected.

Send SIGHUP to the specific running process to apply changes. File watching is not
implemented. A rejected configuration leaves the current state intact, and an accepted
reload updates styling, widgets, output selection, edge, and height. If resource
allocation or the compositor fails after a configuration is accepted, the process
reports an error and exits instead of pretending it recovered.

Exit status is 0 on successful checking, help/version, or orderly signal shutdown;
1 for configuration, file, or runtime errors; and 2 for invalid CLI arguments.
