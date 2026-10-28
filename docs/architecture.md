# Architecture

## Scope and ownership

Velnopt is a native panel that targets Hyprland. Hyprland is a Wayland compositor,
and layer-shell is the protocol Velnopt uses to place panels and reserve desktop
space. Other layer-shell compositors may work, but they are not the main tested
target. See [the backend decision](decisions/0001-panel-runtime.md).

`velnopt-config` owns strict TOML parsing, version checks, style and widget limits,
and a validated `Config`. It has no operating system or compositor access.
Its fields are private, so the only way to get a `Config` is through the parser.

The executable package is `velnopt-cli`; its binary remains named `velnopt`.
Its `config_file` module opens a regular file in nonblocking mode, bounds the byte
read, validates UTF-8, and hands the text to the parser. Checking, startup, and reload
all use that one implementation, so a FIFO can't block a reload while it waits for a
writer.

`velnopt-wayland` owns compositor connections, outputs, layer surfaces, shared-memory
buffers, the event loop, signals, and configuration replacement. The CLI supplies
a reload callback, so the backend never depends on CLI code or on reading
configuration files. Before applying what the callback returns, the backend validates
its output selection and resource budget.

`velnopt-ui` owns physical frame bounds, text layout, cached glyph rasterization,
clipping, and opaque pixel compositing. Public frame geometry has private fields
and can only be built through validation. Drawing takes a validated `Config` and
checks the caller's buffer length before it touches any pixels.

`velnopt-widgets` resolves text and clock content into UI labels and calculates the
next clock update. It creates no surfaces, timers, or threads, and it doesn't load
configuration files. Local time conversion uses the system timezone through Chrono.
Widgets build on UI primitives, and the renderer knows nothing about widget kinds or
time.

`velnopt-hyprland` owns Hyprland IPC and immutable desktop snapshots: four read-only
queries and one write, `dispatch workspace <id>`. Wayland owns its optional
subscription and notification source; widgets format the latest snapshot into
labels. The client depends on neither configuration nor rendering.
See [the IPC decision](decisions/0002-hyprland-ipc.md).

`velnopt-services` owns desktop integrations: NetworkManager, BlueZ, and UPower on the
system bus, the system tray on the session bus, the PulseAudio protocol for audio, and
desktop-entry discovery and launching (modules `dbus`, `tray`, `audio`, `icon`, `apps`).
Workers publish the latest snapshot and wake the event loop; widgets present it and
Wayland sends requests. See [the services decision](decisions/0003-interaction-and-services.md).

Dependencies flow downward, with no cycles:

```text
velnopt-cli -> velnopt-wayland -> velnopt-widgets -> velnopt-ui -> velnopt-config
```

`velnopt-wayland` and `velnopt-widgets` also depend directly on `velnopt-hyprland` and
`velnopt-services`, which depend on no other project crate.

The CLI, Wayland, and widgets also consume validated configuration directly, and
Wayland uses UI geometry and rendering directly. See the [crate plan](crates.md) for
future boundaries. A crate is added when it owns implemented behavior, so future
services are not stubs. Wayland and widgets also depend on `velnopt-hyprland` for its
IPC data model and subscription, and on `velnopt-services` for service state and
requests.

## Runtime flow

```text
CLI -> bounded regular-file read -> strict parser -> validated configuration
                                                    |
                                                    v
Hyprland globals -> outputs -> one selected layer surface per output
                                      |
                                      v
                         configure / resize / scale
                                      |
                                      v
                         bounded software frame -> commit

Calloop: compositor events + seat input + optional minute timer
         + IPC and service notifications + signals
```

The initial file is validated before Velnopt connects to Hyprland. Startup needs at
least one matching output and installed system fonts. The layer namespace is
`velnopt`, on the top layer. Each panel anchors to left/right and its configured
top or bottom edge, and reserves its height. It requests no keyboard interaction
while idle; keyboard mode and the launcher or a tray menu request exclusive focus
until they end.

Each surface has two reusable shared-memory buffers. Drawing only uses a buffer the
compositor has released, and waits for the previous frame callback before committing
another. Dirty surfaces are looked at again after event dispatch, buffer releases
included. A panel that hasn't changed requests no more frames.

Output add and remove events reconcile the selected surfaces. Removed surfaces
release their client resources. If no selected outputs remain, the process waits for
one to reconnect, with no clock timer running. A layer surface the compositor closes
is removed, and it is not recreated over and over in response to the close event.

## Reload and shutdown

Linux signals arrive through Calloop's signalfd source. The signal masks are installed
before font initialization, so any threads created by dependencies inherit them. There
are no custom signal handlers. Live widgets are opt-in, and they add one cancellable
IPC worker after the masks are installed. Nonblocking sockets and request deadlines
keep requests from stalling the rendering loop, and no snapshot lock is held during
socket I/O. A static or clock-only configuration starts no worker. Desktop widgets
start only the service workers they need, also after the masks. Those workers are
restarted (the old ones cancelled and joined) only when a reload changes which modules
are needed, and none run while no panel is shown.

`SIGUSR1` enters keyboard mode on the panel of Hyprland's focused monitor and `SIGUSR2`
opens the launcher; `SIGCHLD` reaps launched applications. `SIGHUP` rereads and
validates the entire file, checks output selection and resource bounds, and only then
replaces the configuration. Syntax, schema, file, unknown-output, and resource-limit
errors keep the previous configuration. A successful change updates layer properties
and marks surfaces dirty. Glyph caches are cleared on every accepted reload, so
repeated text and style changes don't pile up old glyphs.

Rejecting a configuration is transactional: it either applies fully or not at all.
Unrecoverable allocation or compositor failures end the process with an error. I make
no claim that Velnopt recovers from arbitrary operating system failure after a valid
configuration has been accepted.

`SIGINT` and `SIGTERM` stop dispatch, destroy layer surfaces, flush the connection,
and return success. A disconnected compositor returns a failure to the CLI.

## Text, scaling, and resource bounds

COSMIC Text discovers system fonts, does the Unicode shaping and fallback, and
rasterizes glyphs. Three equal-width groups keep the left, center, and right
information from overlapping. A label that is too long gets an ellipsis, and any
excess group content is clipped.

## Input, overlays, and accessibility

SCTK's seat handling delivers pointer and keyboard events. For each frame, widgets
build a list of items, each with a role, an accessible name, and optionally an action.
After drawing, Wayland keeps that frame's visible hit spans, the controls in reading
order, and the AT-SPI node identities, and resolves clicks, keys, and
assistive-technology requests against them. Focus is kept by action identity, so live
updates don't move it. Left click activates; right click on a tray item opens its
menu; the wheel over the volume changes it by 5 %. In keyboard mode Tab, arrows,
Home, End, Enter, Space, and Escape operate controls. Held keys do not repeat.

The launcher and tray menus are overlays. They replace the panel's widgets in the
same strip, with the same label layout and focus ring, on one panel at a time.
Applications are rediscovered whenever the launcher opens.

When the configuration contains a workspace, Bluetooth, volume, launcher, or tray
widget, each panel registers an AccessKit adapter that publishes the tree of its last
drawn frame to AT-SPI. Display-only configurations create none.

Buffers hold opaque native-endian XRGB8888 pixels. An integer output scale multiplies
logical metrics before rasterization. At fractional scales the compositor resamples an
integer-scale buffer, because the native fractional-scale and viewport protocols are
not implemented. Rotated outputs are handled by the compositor's transformation, not
by pre-transformed client buffers.

Physical geometry is checked before anything is allocated: positive dimensions,
integer scale 1–8, at most 32 MiB per frame, and an aggregate 64 MiB of current client
shared-memory mappings across double-buffered surfaces. Outdated client mappings are
dropped before their replacements are allocated. The compositor may keep old buffer
backing around until it is done with it, so these limits are not a cap on total process
or compositor RSS. Font discovery and text caches use more memory on top.

The clock follows local system time by default, or UTC when configured. A timer is
installed only while a selected surface has a clock, and it redraws at minute
boundaries. Deadlines use monotonic scheduling calculated from wall time, so a
wall-clock change shows up at the next scheduled update, within a minute.

Project crates inherit `unsafe_code = "forbid"`. Dependencies can use unsafe code.
There is no Velnopt IPC listener, no plugin loading, no configurable command, and no
scripting runtime. The Hyprland client sends four fixed read-only queries and one
workspace switch for validated IDs. The launcher starts only applications from desktop
entries, directly and without a shell, on explicit input. The set of controls is fixed:
Bluetooth power, mute and volume, tray activation, and menu choices.
