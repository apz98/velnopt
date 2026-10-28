# Changelog

This file lists changes that affect users or development. No stable release has
been published.

## Unreleased

### Added

- Rust 2024 workspace with a pinned toolchain, shared metadata and lints, and a
  committed dependency lockfile.
- `velnopt check <path>` with strict, bounded TOML configuration validation.
- Configuration examples, library and CLI tests, and API documentation.
- Shared local checks, GitHub Actions CI, dependency audits, and Dependabot.
- Architecture, roadmap, performance methodology, development, and security docs.
- Native Hyprland panel with shaped Unicode text, local/UTC clock, configurable
  styling, monitor selection, top/bottom placement, and reserved desktop space.
- Validated SIGHUP reload, reusable buffers, bounded geometry, event-driven redraw,
  graceful shutdown, output hotplug, scaling, and compositor-disconnect handling.
- Isolated Hyprland smoke tests and local Linux packaging with license notices.
- `velnopt-hyprland`: bounded read-only state queries, event updates, same-user
  socket checks, reconnects, and cancellable background I/O.
- Opt-in `workspaces`/`window-title` widgets and a live Hyprland example.
- Mock IPC failure/recovery tests and native workspace/title/offline smoke checks.
- Pointer and keyboard input: clickable per-monitor workspaces (one validated
  `dispatch workspace` write), tray right click, and wheel volume. `SIGUSR1` enters
  keyboard mode and `SIGUSR2` opens the launcher; idle panels request no keyboard focus.
- AT-SPI accessibility through AccessKit for configurations with interactive widgets.
- `velnopt-services` with NetworkManager, BlueZ, UPower, PulseAudio-protocol audio, and
  system tray (StatusNotifierItem and dbusmenu) modules, with explicit pending,
  unavailable, permission-denied, and missing states.
- Widgets `network`, `bluetooth`, `volume`, `battery`, `launcher`, and `tray`, and
  `examples/desktop.toml`.
- Application launcher over desktop entries, started without a shell; the launcher
  and tray menus draw inline in the panel strip.
- Smoke test `--interactive` for input, launcher, tray, service states, and AT-SPI,
  with `examples/virtual-pointer.rs`, `scripts/atspi-probe.py`, and
  `scripts/mock-tray-item.py`.
- Decision record 0003 for input, overlays, accessibility, and services.

### Changed

- Hyprland IPC adds the read-only `j/monitors` query and the workspace switch write;
  workspaces are listed per monitor and special workspaces are no longer listed.
- Docs now describe controls, services, and their trust boundaries. The display-only
  and no-accessibility statements no longer apply.
- Left and right widget groups keep their natural width and the center group takes
  the remaining space, instead of each group getting a fixed third of the panel.
- The dependency audit ignores three "unmaintained" zbus 5 advisories that
  AccessKit's AT-SPI adapter still needs (see SECURITY.md).

- Renamed the executable package and folder to `velnopt-cli`, preserving the
  `velnopt` command, help, version output, and configuration format.
- Extracted implemented rendering, widget presentation, and platform lifecycle
  into `velnopt-ui`, `velnopt-widgets`, and `velnopt-wayland`.
- Frame geometry stays validated across crate boundaries, and the rendering API now
  has checked pixel buffer lengths.

### Security

- Tray icon files are opened without blocking, so an item whose `IconThemePath`
  points at a FIFO or other special file can no longer hang the tray worker.
- `NameOwnerChanged` is only trusted when the bus itself sends it. Before, any
  session client could forge it and make the panel drop tray items or refresh
  service state.
