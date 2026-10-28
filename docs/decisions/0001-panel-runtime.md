# 0001: Native software-rendered Hyprland panel

Status: accepted for the first panel implementation.

## Context

I had a working panel with text, a clock, styling, and reload, and Hyprland as the
target. The workspace has a strict TOML parser and forbids unsafe code in project
crates. I don't need QML compatibility.

## Decision

I use Smithay Client Toolkit 0.21 for Wayland output management, layer-shell, and
shared-memory buffers; COSMIC Text 0.19 for system fonts, Unicode shaping, and
software rasterization; and Calloop 0.14 for events, timers, and Linux signals.
Chrono provides local timezone conversion. The initial executable modules have since
been split by what they own into CLI, Wayland, UI, and widget crates; see
[the crate plan](../crates.md). The dependency choices are unchanged.

I pinned COSMIC Text to upstream revision `b07cb90a872cb9fec7fea1c72d734f81907fe3ad`
until a published release contains its fontdb 0.24 migration. The published 0.19.0
still requires the unmaintained `ttf-parser` through fontdb 0.23 (RUSTSEC-2026-0192),
and upstream replaces that parser with maintained fontations code. An exact Git pin
adds a source-fetch requirement and uses unreleased code; tests and real Hyprland
verification cover this choice. I'll return to a registry release once it includes the
migration. No advisory is ignored. The lockfile unifies skrifa at 0.44 so the shaper
and rasterizer share compatible font types; keep that consistent when updating the
font stack.

Configuration stays declarative and display-only. Reload goes through SIGHUP, with
full validation before the new configuration replaces the old one. This scope needs
no filesystem watcher, IPC server, command runner, plugin interface, or async
runtime.

## Alternatives

Qt/QML has mature UI components, but it brings a different runtime and authoring
model before any compatibility requirement exists. A GPU renderer needs graphics
lifecycle and driver complexity that a text panel doesn't need. Writing the protocol
and font handling myself would duplicate maintained dependencies.

## Consequences

The client needs no Qt or native font-rendering libraries. Static content does not
redraw continuously, and buffers are reused. Software rendering is not proof of
better performance, and richer widgets may change the tradeoff.

The initial panel had no interactive widgets or accessibility tree; input,
accessibility, and desktop services came later
([0003](0003-interaction-and-services.md)). Fractional scales rely on compositor
resampling, and layer-shell alone supplies no Hyprland workspace data. Dependencies
may use unsafe code even though project crates forbid it.
