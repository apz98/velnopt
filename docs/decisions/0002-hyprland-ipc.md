# 0002: Read-only Hyprland state with bounded background I/O

Status: accepted for live workspace and focused-window widgets; amended for
monitors and workspace switching (see [0003](0003-interaction-and-services.md)).

## Context

The milestone needs initial desktop state, event updates, and reconnects. Hyprland's
command socket is synchronous, so a blocking request on the rendering loop would
stall frames, reload, and shutdown. Velnopt's own control IPC is a separate feature.
Workspace switching is the only compositor action.

## Decision

`velnopt-hyprland` subscribes to `.socket2.sock` before querying `j/workspaces`,
`j/monitors`, `j/activeworkspace`, and `j/activewindow` on `.socket.sock`. Events
only invalidate state; full JSON snapshots mean I never parse titles out of
comma-delimited event payloads. The four queries are eventually consistent, not an
atomic compositor snapshot. Queued events trigger another refresh if state changes
during a query.

One worker uses nonblocking Unix sockets, one-second total request deadlines, and a
cancellation socket. A single protected latest-state slot plus Calloop's coalescing
ping source wakes the rendering loop. No lock is held during I/O. Dropping the
subscription interrupts connect/read/idle/retry waits and joins the worker.
Application signal masks are installed before thread creation.

The worker runs only while a selected surface uses a live widget. A connected idle
session makes no periodic queries. Socket failures clear stale state and retry after
500 ms. An invalid session environment reports an error and waits for cancellation;
restart the panel from the correct session to fix it.

Endpoints use the absolute `XDG_RUNTIME_DIR` and a validated instance signature,
with no legacy `/tmp` fallback. Socket peer credentials must match the caller's UID.
The request surface is four fixed read-only queries and one write,
`dispatch workspace <id>`.

The write exists so workspace controls work. `Subscription::switch_workspace` accepts
only IDs `1..=2147483647`, so special and named-only workspaces are never addressed
and no text from Hyprland or the user reaches the command. A single pending slot keeps
the latest request, and a wake socket interrupts the worker, which sends it on the
command socket under the same one-second deadline. A request made while IPC is
disconnected is discarded, not replayed after reconnection.

Replies are limited to 1 MiB, event frames to 64 KiB, and snapshots to 256 workspaces
and 32 monitors. Names and titles become bounded single-line UTF-8. JSON recursion
limits stay enabled, duplicate workspace IDs are rejected, and diagnostics omit raw
response contents. Glyph caches are cleared on snapshot changes, which discards old
arbitrary titles.

## Tradeoffs and verification

The worker adds a thread and its stack only for opted-in widgets. It keeps
synchronous IPC off the rendering loop without an async runtime or a broad compositor
SDK. The window title shows global focus on every output. Workspaces are listed per
monitor with that monitor's active workspace, and special workspaces are not listed.

Mock servers cover initial state, fragmented and batched events, coalescing,
reconnects, malformed and oversized replies, timeouts, and cancellation. Native smoke
tests change workspaces and a controlled terminal title in a disposable Hyprland
session, capture the clipping, empty, and offline states, and verify worker teardown
on reload. These tests read no host-session window data.
