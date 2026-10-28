# Security

Velnopt is in initial development and isn't ready for production use. There are no
supported stable releases, and security fixes go to the current development branch.

## Reporting

If GitHub's private vulnerability reporting is enabled on the hosted repository, use
that. Otherwise, contact the repository owner through an existing private channel to
arrange disclosure; there is no dedicated security address yet. Please don't post
exploit details, private configuration, or secrets in a public issue.

Include the affected revision, steps to reproduce, the expected behavior, and the
impact. There is no guaranteed response time and no bounty program.

## Current boundaries

- Checking reads only the supplied regular configuration file and does not start
  a compositor connection. Running connects to the caller's Hyprland session and
  allocates layer surfaces and bounded shared-memory buffers.
- Reads are capped at 1 MiB plus one detection byte. Parsing rejects unsupported
  versions, unknown fields, missing fields, and invalid values. Nonblocking open
  and regular-file checks prevent a FIFO from stalling reload.
- Invalid reloads retain the active configuration. Accepted reloads clear old
  glyph caches. Frames are limited to 32 MiB, with an aggregate 64 MiB of current
  double-buffered client mappings. These are not total process RSS limits.
- Widgets display text/time and desktop state. There is no configurable command,
  plugin loader, scripting engine, or Velnopt IPC listener. Controls run fixed
  actions, and only on explicit input: workspace switch, Bluetooth power, mute and
  volume, tray activation and menu choices, and launching a desktop-entry application.
- Panels request no keyboard focus while idle. `SIGUSR1` and `SIGUSR2` start keyboard
  mode and the launcher; they come from processes of the same user, who can already
  signal or replace the panel. Interactive configurations expose an AT-SPI tree whose
  Click and Focus requests resolve only against controls of the last drawn frame.
- Project crates forbid unsafe Rust. This does not establish that dependencies
  are free of unsafe code or vulnerabilities.
- Live widgets use four fixed read-only queries and one workspace switch (IDs
  1 to 2147483647 only) on session-local Hyprland sockets, with validated instance
  signatures and same-UID peer checks. The client has one cancellable worker,
  one-second request deadlines, 1 MiB replies, 64 KiB event frames, and at most 256
  workspaces. Snapshots and notifications coalesce.
- Names/titles are sanitized and bounded before drawing. Failure clears stale
  state. Logs do not echo raw replies or titles. A same-user peer check does not
  authenticate a compromised process running as the user. See the
  [IPC design](docs/decisions/0002-hyprland-ipc.md).
- Desktop services add trust boundaries with D-Bus peers: NetworkManager, BlueZ, and
  UPower on the system bus; tray applications and their menus on the session bus; and
  the PulseAudio-compatible server. Their strings are untrusted. Service text is
  sanitized to single lines of at most 64 bytes, menus to 64 entries, tray items to
  32, icons to 256×256 pixels (PNG files to 1 MiB, icon names without path
  separators), and requests queue in bounded queues that are dropped, not replayed,
  when a service is down. Calls time out after two seconds. Reading D-Bus state never
  starts a missing service. A tray item can still lie about its own title, icon, and menu.
- Desktop entries are treated as untrusted input: scanning is bounded
  to 4096 files of 64 KiB, `Exec` is tokenized into an argument vector without a shell,
  field codes are dropped, and `Terminal=true` entries are hidden. Launched
  applications inherit the panel's environment, run in their own process group with
  closed standard streams, and are not sandboxed. An attacker who can write the user's
  desktop entries can already run code as that user.
- Velnopt serves `org.kde.StatusNotifierWatcher` on the session bus only when no other
  process owns it, and accepts registrations of owned bus names only. Any session-bus
  client can register an item.
- CI audits the committed dependency lockfile against RustSec on changes and
  weekly. `.cargo/audit.toml` ignores three "unmaintained" advisories
  (RUSTSEC-2026-0370, 0371, 0372: zvariant, zbus_names, zvariant_derive, merged into
  zbus 6). They report no vulnerability; AccessKit's AT-SPI adapter still requires
  zbus 5. The exception is removed when AccessKit moves to zbus 6. CI actions are
  pinned to commit hashes, checkout does not persist credentials, and workflow token
  permissions are read-only.

The application runs with the caller's privileges and is not a sandbox. It loads
installed fonts and talks to the compositor, the D-Bus buses, and the audio server
that its environment selects. Dependencies, including zbus, AccessKit, and the
PulseAudio protocol crate, may use unsafe code; project crates forbid it. See the
[services decision](docs/decisions/0003-interaction-and-services.md).
Any future control IPC, scripting, or configurable process execution will need its
own trust boundaries and review before it is built.
