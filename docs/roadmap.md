# Roadmap

The first version is done: a Hyprland panel with text, a clock, styling,
configuration reload, local packaging, and checks. Broader shell functionality is
future work. There are no promised dates and no comparative performance claims.

## Completed: repository and panel

- [x] Rust workspace, pinned toolchain, lockfile, inherited lints, and reserved rights.
- [x] Strict configuration checking and an executable panel runtime.
- [x] Configurable Unicode text, local/UTC clock, font, colors, and spacing.
- [x] Top/bottom placement, exclusive desktop space, and output selection.
- [x] Validate-before-apply reload through SIGHUP.
- [x] Dirty-only rendering with reusable buffers and an optional minute timer.
- [x] Resize, output lifecycle, integer scaling, and compositor-resampled fractional scaling.
- [x] Graceful shutdown and compositor-disconnect error reporting.
- [x] Isolated Hyprland smoke tests, screenshots, unit/CLI tests, CI, and audits.
- [x] Local Linux archive packaging with checksum and third-party license notices.
- [x] Hyprland setup, configuration reference, architecture, and security documentation.
- [x] Implemented crate boundaries for CLI, configuration, Wayland, UI, and widgets.
- [x] `velnopt-hyprland`: bounded read-only IPC, reconnects, and cancellable I/O.
- [x] Live workspace/title widgets with native update, clipping, offline, and teardown tests.

## Completed: interaction and desktop services

- [x] Per-monitor, clickable workspaces through one validated `dispatch workspace` write.
- [x] Pointer and keyboard input; keyboard mode and launcher through `SIGUSR1`/`SIGUSR2`.
- [x] AT-SPI accessibility through AccessKit for interactive configurations.
- [x] `velnopt-services`: NetworkManager, BlueZ, UPower, PulseAudio-protocol audio, tray.
- [x] `network`, `bluetooth`, `volume`, `battery`, `launcher`, and `tray` widgets.
- [x] Application launcher from desktop entries, started without a shell.
- [x] Tray items and dbusmenu menus, drawn inline in the panel strip.
- [x] Interactive and desktop-service checks in the native smoke test.

## Next: broader desktop shell

The [crate plan](crates.md) records the approved names and who owns what. Shared
application state can move to `velnopt-core` once it exists independently of the
Wayland protocol lifecycle.

Known limits of the current release candidate:

- [ ] SVG tray icons are not decoded; those items show their title.
- [ ] No client-side key repeat in keyboard mode.
- [ ] Desktop entries with `Terminal=true` are hidden; no terminal emulator is chosen.
- [ ] `network` and `battery` are display-only; there are no network controls.
- [ ] The launcher and tray menus replace the panel's widgets instead of using popups.
- [ ] Notifications and other interactive components need requirements first.
- [ ] Decide whether a richer authoring model is needed beyond declarative TOML.
- [ ] Add native fractional-scale and viewport handling if measurements justify it.

Configurable process execution, scripting, Velnopt IPC, and plugins each need an
explicit permission and resource model before I build them. The launcher starts only
desktop-entry applications, and only on explicit input. I'm not promising QML
compatibility.

## Performance and hardening

- [x] Matched idle static panel for Velnopt and Quickshell, with repeatable startup,
  CPU, context-switch, and memory measurements (`scripts/measure.py`; results and
  their limits in [performance](performance.md)).
- [ ] Matched interactive workloads and frame-time measurements.
- [x] Reload stress in the native smoke test (20 reloads; workers joined, no thread
  or descriptor growth).
- [ ] Add parser fuzzing and longer-running lifecycle stress tests.
- [ ] Derive regression budgets from controlled baseline measurements.

Follow [the measurement method](performance.md). Writing it in Rust does not by
itself show that Velnopt is faster or safer than an alternative.

## Public distribution

- [ ] Decide redistribution rights and stable release scope explicitly.
- [ ] Define configuration compatibility and upgrade guarantees.
- [ ] Add signed releases and platform packages when distribution is authorized.

The local package script does not publish, tag, grant redistribution rights, or
stand in for a release review.
