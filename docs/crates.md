# Crate plan

Every workspace package and its folder use `velnopt-*`. The application package
is `crates/velnopt-cli`, and its executable is named `velnopt`. All members inherit
workspace metadata and lints, forbid unsafe code, and disable publishing.

## Implemented

| Crate | Owns | Internal dependencies |
| --- | --- | --- |
| `velnopt-cli` | CLI, errors, regular-file reads, startup loading and reload callback | config, wayland |
| `velnopt-config` | Strict schema, versioning, value validation, typed settings | None |
| `velnopt-wayland` | Surfaces, outputs, buffers, seat input, accessibility adapters, signals, event loop, reload and subscription lifecycle | config, hyprland, services, ui, widgets |
| `velnopt-hyprland` | Read-only queries, the workspace switch, bounded snapshots, event framing, reconnects, cancellation | None |
| `velnopt-services` | Modules `apps`, `audio`, `dbus`, `tray` (and tray `icon` decoding): NetworkManager, BlueZ, UPower, PulseAudio, system tray, desktop entries | None |
| `velnopt-ui` | Text label primitives, icons, hit spans, focus ring, validated frame geometry, layout, glyph caches, clipped pixel rendering | config |
| `velnopt-widgets` | Resolve widgets and service state into items with roles, names, and actions; overlays; accessibility trees; minute update timing | config, hyprland, services, ui |

The backend calls the reload function the CLI gives it and validates the resulting
settings before replacing the active configuration. It never imports the CLI or opens
configuration files. The configuration crate stays free of operating system access,
drawing, and widget execution. Widgets depend on UI primitives, but UI does not depend
on widgets or on clock behavior.

## Approved future boundaries

These are planned ownership boundaries. They are not empty packages to add right now.

| Proposed name or feature | Plan |
| --- | --- |
| `velnopt-core` | Shared shell state and update logic once independent of compositor protocol state; not added because no such state exists yet, and it would become a catch-all |
| `velnopt-bluetooth`, `velnopt-network` | Extract from services if their implemented size or dependency isolation warrants it |
| `velnopt-ipc` | Velnopt's own external control protocol if introduced; Hyprland's IPC belongs in its integration crate |
| `velnopt-diagnostics` | Consolidated crash/debug reporting if shared reporting behavior warrants a crate |
| `velnopt-xtask` | Rust maintenance automation if existing scripts become insufficient |

Each integration keeps its own command authorization, input validation,
cancellation, and error handling. Service state is separate from the widgets that
display it. The launcher UI lives in widgets, while application discovery and
launching live in services. Configuration cannot name commands.

## Consolidated names

- `window` belongs in `velnopt-wayland` for Velnopt's own surface lifecycle.
- `windowmanager` belongs in `velnopt-hyprland` for client-side window/workspace
  operations; Hyprland remains the compositor and window manager.
- `dbus` is a services module, not a wrapper crate around a D-Bus library.
- `io` stays as helpers within the subsystem that owns the resource.
- `build` uses package-local `build.rs` where required and existing scripts.
- `crash` and `debug` can share the diagnostics boundary described above.
- `x11` is out of scope for a Hyprland-focused backend. Running XWayland applications
  under Hyprland doesn't need an X11 rendering backend in Velnopt.

Live Hyprland workspace and window information, workspace switching, input,
accessibility, and the services above are all implemented. Velnopt IPC and
notifications need real features and tests of their own before their crates or modules
are added.
