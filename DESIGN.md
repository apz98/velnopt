---
name: Velnopt native panel
description: A flat, configurable reading strip for the Hyprland desktop.
colors:
  canopy-green: "#152c2b"
  pale-foreground: "#e4ede6"
  mint-identity: "#96d1bd"
typography:
  label:
    fontFamily: "sans-serif"
    fontSize: "14px"
    lineHeight: 1.25
spacing:
  default-inset: "16px"
  example-inset: "18px"
  widget-gap: "24px"
components:
  panel-minimal-example:
    backgroundColor: "{colors.canopy-green}"
    textColor: "{colors.pale-foreground}"
    typography: "{typography.label}"
    padding: "0 {spacing.default-inset}"
    height: "32px"
  panel-full-example:
    backgroundColor: "{colors.canopy-green}"
    textColor: "{colors.pale-foreground}"
    typography: "{typography.label}"
    padding: "0 {spacing.example-inset}"
    height: "36px"
  identity-label:
    textColor: "{colors.mint-identity}"
    typography: "{typography.label}"
  text-label:
    textColor: "{colors.pale-foreground}"
    typography: "{typography.label}"
---

# Design System: Velnopt native panel

## Overview

**Creative North Star: "Canopy-green reading surface"**

This describes the native Linux panel as it is implemented today. It is not a
brand a user has approved. The panel is a flat, opaque strip with one line of desktop
information, split into clear horizontal groups. Color and type are configurable
defaults from the implementation, not brand choices.

**Key Characteristics:**

- A continuous dark surface with pale text and a mint identity label.
- Three independently clipped information groups.
- Text and time update from configuration; workspaces, volume, Bluetooth, the launcher,
  and tray items are controls with a focus ring.

The tokens above come from the
[configuration defaults](crates/velnopt-config/src/lib.rs),
the [minimal example](examples/panel.toml), and the
[full example](examples/hyprland.toml). [Rendering](crates/velnopt-ui/src/lib.rs)
decides geometry, line height, shaping, and clipping. Every pixel token is in logical
pixels. The [surface contract](.impeccable/surfaces/panel.md) records the current
direction and the review captures.

## Colors

### Primary

- **Mint identity:** the default Velnopt text widget's color override.

### Neutral

- **Canopy green:** the opaque panel background.
- **Pale foreground:** text and clock color unless a widget supplies an override.

Colors are opaque `#RRGGBB` values in the configuration. These three are defaults,
not a locked palette, and there is no separate palette for selection or interaction.

## Typography

One label role covers the identity label, configured text, and the time. The generic
family resolves to whatever fonts are installed on the user's system, because Velnopt
ships no font. Named families and the generic `serif` and `monospace` families can
also be configured. Don't assume glyphs look the same on every system.

Text gets full Unicode shaping, a fixed line-height multiplier, and a single
unwrapped line with an ellipsis at the end. The line box is centered vertically in the
panel. There is no heading scale, and weight and letter spacing are not configurable.

## Layout

The panel spans its selected output, at the configured top or bottom edge. After the
horizontal insets, the available width is split into three equal groups (left,
center, right) with integer rounding. Widgets keep their declaration order within a
group, separated by the widget-gap token, and each group aligns its contents to its
own side.

The minimal example asks for the smaller panel token and inherits the default inset.
The full example asks for the larger height and inset explicitly. Height is a required
configuration field, not a runtime default. Padding and gap are configurable, and
padding is capped at half the surface width when rendering.

**The Independent Groups Rule.** Left and right groups keep their natural
width. The center group takes the space between them, and sits centered on the panel
when it fits. When both sides overflow, each keeps half the row. Labels get an
ellipsis, and a group that overflows is clipped instead of overlapping its neighbors.
There are no breakpoints and no alternative mobile layout.

An integer output scale multiplies raster dimensions and metrics. Fractional scaling
is the compositor resampling the result. The narrow and HiDPI review captures use a
reloaded configuration with a 44-logical-pixel height. That is a test variant, not a
new default.

## Elevation & Depth

The surface is flat and fully opaque. The renderer has no shadows, transparency,
gradients, or elevation states today. Groups are separated by space and by text
color.

## Shapes

The panel is one rectangular strip with square corners: no decorative border, no
cards, no pills. Text is clipped at the panel and group bounds.

## Components

### Panel

The minimal and full configurations use the same renderer. The full example has the
identity label at left, configured desktop text in the center, and the time at right.
The minimal example has only the identity label and the time. The optional
[live example](examples/live-hyprland.toml) puts workspaces at left and the focused
window title in the center instead, and keeps the clock at right along with the full
example's geometry and label styling.

### Text and identity labels

Both use the same single-line text widget. The identity label is an ordinary text
widget with a mint color override. It is not an icon, a logo asset, or a navigation
control. Other widgets inherit the foreground color or bring their own.

### Live workspace and window labels

These display-only labels get the same single-line shaping, end ellipsis, and
independent group clipping as static text. Workspace names are separated by spaces,
and the active workspace's name is in brackets. The live example uses the mint
override for this group. The brackets add no selection color or interactive state.

The title label shows `No focused window` when none is focused and
`Untitled window` for an empty title. A connected session without workspaces
shows `No workspaces`. Both live widgets show `Hyprland unavailable` while
IPC is unavailable, and the panel and clock keep working. Initial snapshots, desktop
events, and reconnection update the labels without a configuration reload. An
unchanged snapshot does not request another frame.

### Controls and focus

Controls are single-line labels like the others. In keyboard mode the focused control
gets a **focus ring**: a 2 px outline in the label's own color, drawn 4 px outside the
text, clipped to the group. It has no fill. Workspaces and tray items sit in a tight
gap (0.6 times the font size) instead of the widget gap, so they read as one
control group. Tray icons are drawn at line height, centered vertically; an item with
no usable icon shows its title. The launcher button reads `Apps`.

Service widgets state their condition in text (for example `Volume 45%`, `Muted`,
`Network unavailable`); there is no icon-only state and no color-coded status.

### Inline overlays

The launcher and tray menus replace the panel's widgets in the same strip, left-aligned
across the whole width, with the same label style. The launcher shows a `Run: ` prompt
with a caret, then ranked application names. A menu shows entries with `✓` for
checked, `▸` for submenus; separators are omitted. The focused entry takes the focus
ring. There are no popups, shadows, or animation.

### Clock

The display-only clock shows local `HH:MM`, or `HH:MM UTC` when configured, and
updates on minute boundaries. Display-only widgets have no hover, active, animation,
or input states. Static text and styling change through configuration and reload, and
an invalid reload leaves the working panel alone.

The sidecar's HTML/CSS sample is a documentation preview of the full example, not
the native renderer. Its derived tonal ramps are preview metadata. They are not extra
palette tokens or implemented themes.

## Do's and Don'ts

### Do:

- Do preserve independent clipping and single-line text shaping.
- Do distinguish configuration defaults from explicit example values.
- Do test actual native output at narrow widths and integer/fractional scales.
- Do preserve the current panel when a reload is invalid.

### Don't:

- Don't present the default palette as a user-approved brand.
- Don't invent controls or interactive states for display-only widgets
  (`text`, `clock`, `window-title`, `network`, `battery`).
- Don't assume a bundled font or identical glyph metrics across systems.
- Don't treat documentation preview CSS as native rendering behavior.
