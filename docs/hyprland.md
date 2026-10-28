# Hyprland setup

Hyprland is the primary target, and it implements the Wayland layer-shell protocol
that Velnopt uses. I have tested on **Hyprland 0.56.2**, including actual rendering,
reload, resize, scaling, and monitor lifecycle. Other versions and other compositors
have not had the same integration testing.

## Build and install from a checkout

Run these in the repository root:

```sh
cargo install --path crates/velnopt-cli --locked --root "$HOME/.local"
mkdir -p "$HOME/.config/velnopt"
# Preserve an existing configuration if prompted.
cp -i examples/hyprland.toml "$HOME/.config/velnopt/panel.toml"
"$HOME/.local/bin/velnopt" check "$HOME/.config/velnopt/panel.toml"
"$HOME/.local/bin/velnopt" run "$HOME/.config/velnopt/panel.toml"
```

Run this inside your Hyprland session. It needs no background service and no
elevated privileges. Close it with Ctrl+C. The process prints its PID, which you can
use for targeted reloads. To run `velnopt` by name, make sure the local bin directory
is in your PATH.

If an earlier checkout installed the package named `velnopt`, add `--force` to the
install command when replacing it with `velnopt-cli`. Otherwise Cargo rejects the
executable-name conflict between the old and renamed packages. The executable name
and your existing panel configuration stay the same.

## Session startup

For the hyprlang configuration that the tested 0.56.2 installation supports, add this
line to `~/.config/hypr/hyprland.conf`:

```ini
exec-once = ~/.local/bin/velnopt run ~/.config/velnopt/panel.toml
```

`exec-once` runs when the session starts, not on a Hyprland config reload, so start
Velnopt by hand to try it in the current session. Each invocation creates another
panel instance, so keep only one startup entry.

Newer Hyprland documentation uses Lua configuration. If your installation uses
`hyprland.lua`, use its startup event instead:

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("~/.local/bin/velnopt run ~/.config/velnopt/panel.toml")
end)
```

The Lua example follows [current Hyprland autostart documentation](https://wiki.hypr.land/configuring/core/autostart/);
the integration test uses the hyprlang config that the installed 0.56.2 accepts.
Velnopt needs no special window rules. Its layer namespace is `velnopt`, and you can
inspect it with `hyprctl layers`.

## Choose an output

Run `hyprctl monitors`, then set the exact monitor name under `[panel]`, for example
`output = "DP-1"`. Leave it out to get one panel on every monitor. A named monitor
must be connected at startup and when you apply a new configuration. Once the panel
is running, unplugging and reconnecting that monitor removes and restores its panel.

The panel reserves its configured height. Other bars on the same edge can reserve
extra space or cover each other, so disable the old bar if Velnopt is replacing it.
Velnopt does not modify or stop other desktop programs.

## Keyboard access

Panels ask for no keyboard focus while idle. Keyboard interaction starts with one of
two signals, sent by Hyprland binds. Add these to `hyprland.conf` (change the keys to
taste):

```ini
bind = SUPER, F10, exec, pkill -USR1 -x velnopt
bind = SUPER, D, exec, pkill -USR2 -x velnopt
```

`SIGUSR1` focuses the controls of the panel on the focused monitor; `SIGUSR2` opens
the application launcher there. The keys inside each mode are listed in
[the configuration reference](configuration.md#input). `pkill -x velnopt` signals
every process named `velnopt` that your user owns, so keep a single instance running.
Velnopt has no other IPC listener, and only the same user's processes can signal it.

The desktop widgets in [`examples/desktop.toml`](../examples/desktop.toml) use the
system bus (NetworkManager, BlueZ, UPower), the session bus (tray), and the
PulseAudio protocol. Each is optional: a missing service shows an explicit state and
the rest of the panel keeps working. The launcher and tray menu draw in the panel
strip. Applications started from the launcher inherit the panel's environment.

## Reload

For live workspace and title information, use `examples/live-hyprland.toml`.
`HYPRLAND_INSTANCE_SIGNATURE`, `XDG_RUNTIME_DIR`, and `WAYLAND_DISPLAY` must identify
the same session. The client connects to the current instance's `.socket.sock`
and `.socket2.sock` under `$XDG_RUNTIME_DIR/hypr/`; it does not search legacy global
`/tmp` directories or invoke `hyprctl` subprocesses. It sends four read-only
queries, `j/workspaces`, `j/monitors`, `j/activeworkspace`, and `j/activewindow`,
refreshing after relevant events. Its only write is `dispatch workspace <id>`, sent
when a workspace control is activated, for IDs 1 to 2147483647. See
[the IPC design](decisions/0002-hyprland-ipc.md) for reconnects and bounds.

For an instance started from a shell:

```sh
velnopt run ~/.config/velnopt/panel.toml &
velnopt_pid=$!
kill -HUP "$velnopt_pid"
kill -TERM "$velnopt_pid"
```

For an instance started by Hyprland, use the PID from its startup log or from the
layer entry in `hyprctl layers`. Signal that instance, not every process with the same
name. A rejected reload reports the reason and keeps the current panel.

## Install a locally generated archive

`./scripts/package.sh` creates the archive and checksum in `target/dist`. Verify
the checksum from that directory with `sha256sum -c <archive>.sha256`, extract the
archive, and install its `bin/velnopt` under your local bin directory. The sample
configuration is at `share/velnopt/panel.toml`, with the opt-in live example at
`share/velnopt/live-hyprland.toml`; documentation, every example (including `desktop.toml`), and third-party
notices are at `share/doc/velnopt`.

Archives target the build machine's architecture and libc environment, so they are
not universal static Linux executables. Packaging needs Python 3 and GNU build
utilities as well as Rust. Local packaging grants no redistribution rights; Velnopt
remains all rights reserved.

## Integration test

Besides the rendering checks, `scripts/smoke-hyprland.py --live-widgets --interactive`
verifies pointer clicks, keyboard navigation, the launcher, the tray, unavailable
service states, and the AT-SPI tree inside the nested compositor. Build the pointer
helper first:

```sh
cargo build --release --locked -p velnopt-cli
cargo build --release --locked -p velnopt-wayland --example virtual-pointer
python3 scripts/smoke-hyprland.py --live-widgets --interactive
```

It needs `wtype`, `dbus-daemon`, `busctl`, `foot`, `at-spi2-core`, and `python-gobject`
and uses private D-Bus buses, a dead `PULSE_SERVER`, and a mock tray item, so it
reads no host service. Never run the virtual pointer helper against a real session.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Cannot connect | Launch in Hyprland with `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` present |
| Layer-shell unavailable | Check that the environment selects Hyprland rather than an unrelated nested compositor |
| No matching output | Use the exact connected name from `hyprctl monitors` |
| No text/fonts | Install DejaVu Sans, Noto Sans, or another system font |
| Text clipped | Increase height, reduce text/font size, or reduce padding/gap |
| Fractional scale looks different | This version uses integer raster buffers with compositor resampling |
| Reload does nothing | Send SIGHUP to the correct PID and inspect its standard-error log |
| Live widgets say unavailable | Launch with the matching Hyprland session environment and inspect the IPC diagnostic on standard error |
| Keyboard bind does nothing | Run `pgrep -x velnopt`; bind `pkill -USR1 -x velnopt` and check the panel has a `workspaces`, `bluetooth`, `volume`, `launcher`, or `tray` widget |
| Launcher lacks an application | Terminal applications (`Terminal=true`) and hidden entries are not listed |
| Tray item shows its name | Its icon is an SVG, which is unsupported, or it supplied none |
| `Network unavailable` and similar | The service or bus is not running; the panel retries every 2 seconds |
| Extra reserved space | Check for duplicate Velnopt instances or another bar on the same edge |
| Frame limit error | Reduce height or output scale; buffers are bounded before allocation |
