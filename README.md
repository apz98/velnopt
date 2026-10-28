# Velnopt

Velnopt is a small native panel for **Hyprland**, written in Rust. It draws a strip of
text along the edge of your screen: some configurable Unicode text, a clock, your
workspaces, the focused window's title, network, Bluetooth, volume and battery status,
an application launcher, and a system tray. You can place and style it per monitor,
and it reloads its configuration while it runs.

Longer term, I want this to grow into an alternative to
[Quickshell](https://quickshell.org/). It's not there yet. What you get today is a
working desktop panel with a fixed set of controls: no QML, no arbitrary UI scripting,
no configurable commands, no notifications, and no Quickshell API compatibility. I
also haven't shown that it is faster or safer than anything else. There are no
comparative benchmarks and no independent audits, so please don't read either into it.

## Run

You'll need Linux, [Rust through rustup](https://rustup.rs/), a native linker, system
fonts, libxkbcommon (for keyboard input), and a running Hyprland session. The desktop
widgets talk to NetworkManager, BlueZ, UPower, and a PulseAudio-compatible server
(such as pipewire-pulse) if they're there. All of them are optional. If one is
missing, no worries, the widget just shows an explicit state for it. Rust 1.99.1,
Clippy, and rustfmt get selected automatically. Rendering is done in software, so
there are no Qt, FreeType, or Wayland development packages to install. On Arch,
`base-devel` and a font such as `ttf-dejavu` cover the build tools and text fonts; on
Ubuntu the build tools come from `build-essential`.

```sh
cargo run -p velnopt-cli --release --locked -- check examples/hyprland.toml
cargo run -p velnopt-cli --release --locked -- run examples/hyprland.toml
```

Run it from inside Hyprland, so that `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` point at
the right session. By default you get a panel on every connected monitor, with desktop
space reserved at the top. `Ctrl+C` closes it cleanly.

[The Hyprland setup guide](docs/hyprland.md) covers installation, starting it with
your session, monitor selection, and troubleshooting.

For live workspaces and the focused window's title, use the opt-in example:

```sh
cargo run -p velnopt-cli --release --locked -- run examples/live-hyprland.toml
```

Workspaces are listed on the left with the active one in brackets, and the focused
window's title sits in the center. Updates come over read-only IPC, no reload needed.
If IPC isn't available the panel says so and stays usable. Existing configurations
keep working.

For the controls and desktop services, use [the desktop example](examples/desktop.toml):

```sh
cargo run -p velnopt-cli --release --locked -- run examples/desktop.toml
```

You can click a workspace, the Bluetooth or volume status, `Apps`, or a tray item, and
scroll over the volume to change it. For keyboard access, add these binds to
`hyprland.conf`:

```ini
bind = SUPER, F10, exec, pkill -USR1 -x velnopt
bind = SUPER, D, exec, pkill -USR2 -x velnopt
```

The first one focuses the panel's controls and the second opens the launcher. More in
[the Hyprland guide](docs/hyprland.md#keyboard-access).

## Configure and reload

The original minimal configuration still works. It gives you the default styling, a
Velnopt label, and a local-time clock:

```toml
version = 1

[panel]
edge = "top"
height = 32
```

[The full example](examples/hyprland.toml) adds a font, colors, spacing, and explicit
left, center, and right widgets. It's a good one to copy, so feel free to play with
it! Every supported field and limit is in
[the configuration reference](docs/configuration.md).

To reload an instance running in the background:

```sh
target/release/velnopt run examples/hyprland.toml &
velnopt_pid=$!
# Edit the configuration, then apply it:
kill -HUP "$velnopt_pid"
# Close this instance:
kill -TERM "$velnopt_pid"
```

A bad reload prints an error and the panel keeps its current configuration. Reloading
is always explicit; nothing watches your files. The runtime never runs commands from
the configuration. Controls only do their fixed action when you click or press them,
and panels only ask for keyboard focus in keyboard mode or while the launcher or a
tray menu is open. Interactive configurations expose an AT-SPI accessibility tree.

## Development

```sh
./scripts/check.sh
python3 scripts/smoke-hyprland.py
./scripts/package.sh
```

The input, accessibility, and desktop-service checks are described in
[the development guide](CONTRIBUTING.md#commands).

The check script runs formatting, Clippy, the tests, API docs, and a release build.
The smoke test needs Hyprland, `hyprctl`, `grim`, and either a running Wayland session
or `labwc` (used headless when no session is running). It starts a throwaway nested
Hyprland and checks rendering, reload, resize, scaling, monitor lifecycle, and
shutdown, with captures and reports under `target/hyprland-smoke`. CI only runs the
checks that don't need a display; the Hyprland smoke test is a separate local
integration check. A few flags add more:

- `--clock-tick` waits for a visible update across a minute boundary.
- `--live-widgets` checks real workspace and title updates, plus offline and teardown
  behavior. It needs `foot`, because it launches a controlled terminal in the nested
  compositor (the panel itself doesn't need `foot`).
- `--interactive` checks pointer, keyboard, launcher, tray, service states, and AT-SPI.
  It needs more tools; see the development guide.

The package script builds a native Linux archive and a SHA-256 checksum under
`target/dist`, with the executable, the example, the docs, and the dependency license
notices. It builds for the current host and doesn't publish anything. Maintenance and
release policy are in [the development guide](CONTRIBUTING.md).

## Workspace

| Path | Responsibility |
| --- | --- |
| `crates/velnopt-cli` | The `velnopt` executable, arguments, and bounded configuration file access |
| `crates/velnopt-config` | Strict configuration parsing and validated settings |
| `crates/velnopt-hyprland` | Hyprland IPC queries, the workspace switch, bounded snapshots, reconnects, and cancellation |
| `crates/velnopt-services` | Network, Bluetooth, battery, audio, system tray, and desktop-entry launching |
| `crates/velnopt-wayland` | Layer-shell surfaces, outputs, buffers, input, accessibility, event loop, and signals |
| `crates/velnopt-ui` | Validated frame geometry, software rendering, and text layout |
| `crates/velnopt-widgets` | Widget presentation, controls, roles, overlays, and clock update timing |
| `examples` | Minimal, styled, live, and desktop panel configurations |
| `scripts` | Checks, native integration test, and local packaging |
| `docs` | Architecture, configuration, Hyprland setup, and roadmap |

Under the hood it's Smithay Client Toolkit for layer-shell and shared-memory buffers,
COSMIC Text for finding fonts and shaping text, and Calloop for a single event loop.
The project's own crates forbid unsafe Rust. Dependencies can still contain unsafe
code, so this is not a whole-program safety proof.

The panel tries hard not to do work when nothing changes. Static panels have no
periodic redraw timer, and clock panels update at minute boundaries. Live widgets share
one cancellable IPC worker across all outputs and coalesce its notifications; while
IPC is connected and idle it makes no periodic state queries. Desktop widgets start one
worker per service they need (system bus, session bus, audio), and none of them polls.

If you want to dig deeper: [the architecture](docs/architecture.md),
[the roadmap](docs/roadmap.md), [the security policy](SECURITY.md), and
[the performance method](docs/performance.md). The [crate plan](docs/crates.md)
records the agreed crate boundaries and the crates that are deferred.

## License

All rights reserved. Velnopt is not currently offered under an open-source license; see
[LICENSE](LICENSE). Workspace packages have `publish = false`, and dependencies keep
their own licenses. Velnopt is independent of Quickshell and Hyprland.
