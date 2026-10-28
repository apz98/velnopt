# Desktop panel

MODE: operate

THESIS: Keep a small, readable strip of desktop information visible while the
user works in Hyprland. Configuration changes must preserve the working panel
when rejected.

OWN-WORLD: A flat canopy-green reading surface, pale text, and a mint identity
label. These are configurable implementation defaults, not user-approved brand
commitments. The panel borrows the clarity and grouping of public wayfinding:
one line, generous insets, and distinct information positions.

STORY: Identity at the left, the configured desktop label in the center, and
local time at the right. No controls or advertisements. Changes happen through
the configuration file and a reload signal.

FIRST VIEWPORT: The full example is a 36-logical-pixel strip spanning the selected
output, with an 18-pixel horizontal inset, 14-pixel text, and three independent
horizontal groups. The minimal schema retains a 32-pixel panel and 16-pixel inset.
Narrow widths keep the clock and other groups separate through clipping and
text ellipsis. Integer HiDPI uses scaled rasterization; fractional scale is
resampled by the compositor.

FORM: Native Hyprland layer-shell status panel. Code-led implementation; no image
comp or raster assets. Direction seed:61e997942e974ee9. The assigned seventh
grounded direction contributes palette and reading density; native status-panel
organization takes priority over decorative source material.

Required review captures are the actual Hyprland screenshots in
target/hyprland-smoke: desktop.png, panel.png, narrow.png, hidpi.png,
fractional.png, invalid-reload.png, and reloaded.png. No browser detector applies
to the native software-rendered surface. Initial and changed configurations are
explicitly named by the capture filenames.

## Live Hyprland widget extension

The confirmed next milestone adds global workspace information at the left and
the focused window title at the center, with the clock retained at the right.
The original minimal/styled examples stay unchanged. The live example preserves
the same palette, type, insets, equal-width groups, and ellipsis. Active workspaces
use brackets as a textual marker. No input controls or new visual tokens are added.
Empty/offline states use explicit labels in their existing groups.

Actual native captures for this extension are target/hyprland-smoke/live-title.png,
live-workspaces.png, live-narrow.png, live-no-window.png, and live-offline.png.
The window titles in these captures come from a controlled test terminal.
