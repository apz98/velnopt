# Product

<!-- impeccable:product-schema 1 -->

## Platform

Native Linux desktop software for Hyprland, which is a Wayland compositor. The
workflow's web/iOS/Android labels don't describe this platform.

## Stack

Rust, in a solo-maintainer Cargo monorepo.

## Product Purpose

Velnopt is meant to be an alternative to Quickshell. The confirmed delivery for
this iteration is a working Hyprland panel with text, clock, styling, and reload,
plus packaging and checks.

The next confirmed milestone adds live workspaces and the focused window's title,
with initial loading, event updates, and reconnect handling. The v0.1 release
candidate adds controls and desktop services: workspace switching, network,
Bluetooth, volume, and battery status, an application launcher, and a system tray,
with keyboard access and an accessibility tree.

## Capabilities and Constraints

- Keep all rights reserved and publishing disabled.
- Configuration is strict TOML. Invalid reloads must retain the current panel.
- Project crates forbid unsafe Rust.
- Desktop information is optional; when IPC or a service is unavailable, the panel
  shows an explicit state and keeps running.
- Controls are fixed built-in actions on explicit input. Configuration names no commands.
- Idle panels request no keyboard focus; keyboard access is through `SIGUSR1`/`SIGUSR2`
  binds, and interactive panels expose AT-SPI semantics.
- Performance and safety advantages are goals, and they need evidence.
- Broader shell features, scripting, and Quickshell compatibility are undecided.

## Users

I'm building Velnopt as a solo Rust project. I haven't done any research on a
broader audience, and I haven't made distribution commitments.

## Brand Commitments

The name is Velnopt. There is no visual brand or set of assets yet.

## Evidence on Hand

The workspace contains a configuration validator and a native panel runtime with
isolated Hyprland integration tests (rendering, input, accessibility, and desktop
service states), a Hyprland IPC client, service modules with mock D-Bus tests, and
mock IPC tests. There are no comparative shell benchmarks or security audits that
would establish superiority over Quickshell.
