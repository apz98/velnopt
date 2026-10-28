# Performance measurement

I have one matched idle-panel comparison with Quickshell (below). There is no
frame-time or interaction benchmark yet. Configuration validation is not a
meaningful substitute for a desktop-shell workload, so don't use validator timings
to claim anything about shell performance.

## Results: 2026-10-27 idle panel

`scripts/measure.py --runs 5 --idle 30` (raw data:
[measurements/2026-10-27.json](measurements/2026-10-27.json)). Host: Intel
i3-1315U laptop, Linux 7.2.11, Hyprland 0.56.2 nested in a Hyprland session with a
headless 1920x1080 output at scale 1 plus the nested window's output (each
workload draws one panel per output), Velnopt release build from this tree,
Quickshell 0.3.2 from Arch Linux.

Workloads: `velnopt-static` runs `examples/hyprland.toml` (three text groups and
a minute clock). `quickshell-static` runs `scripts/quickshell-static/shell.qml`,
which draws the same panel: same height, colors, font family and size, texts, and
minute-aligned clock. `velnopt-desktop` runs `examples/desktop.toml` with a private
D-Bus bus that has no services and no PulseAudio server, so service widgets show
their unavailable states; it has no Quickshell counterpart.

| Workload | Startup ms (min–max) | RSS MiB | PSS MiB | Threads | Idle CPU in 30 s | Context switches in 30 s |
| --- | --- | --- | --- | --- | --- | --- |
| velnopt-static | 16.1 (15.3–25.2) | 9.5 | 6.6 | 1 | 0 ms | 3 |
| quickshell-static | 74.4 (72.4–75.6) | 117.0 | 84.2 | 18 | 0 ms (one run 10 ms) | 0 (one run 58) |
| velnopt-desktop | 16.8 (15.1–25.5) | 13.7 | 10.8 | 11 | 10 ms | 136 |

Medians of five runs. Startup is process launch until `hyprctl layers` lists the
process's layer surface, polled every 5 ms; it includes hyprctl latency and is
not the first presented frame. CPU time comes from `/proc` at 10 ms clock-tick
resolution, so both static panels are idle below what this method can resolve.
Context switches are summed over all threads.

What this supports: for this idle static panel on this machine, Velnopt reserved
its layer surface sooner and used about one-twelfth of Quickshell's proportional
memory. It shows nothing about frame time, interaction latency, richer widgets,
other hardware, or Quickshell configurations that differ from the QML above.
Quickshell renders through Qt's scene graph while Velnopt renders in software, and
that difference is part of what was measured. The desktop workload's background
activity comes from its D-Bus, accessibility, and service threads.

## Comparable workloads

Start with an empty horizontal panel, then add the same text, update frequency, and
interactions in both projects. Match output size and scale, compositor, rendering
backend where possible, fonts, and animations. Write down the differences you can't
remove instead of implying the two are equivalent.

Record the hardware, OS, compositor and version, graphics driver, display refresh
rate, power policy, Rust toolchain, build profile, dependency lockfile, project
revisions, and exact launch commands. Keep raw results alongside the method when
publishing a comparison. Report cold and warm starts separately.

## Metrics

| Metric | Definition |
| --- | --- |
| Startup | Process launch to first presented frame, including compositor acknowledgment |
| Idle memory | Resident and proportional set size after a documented settling period |
| Idle activity | CPU time and wakeups over the same fixed interval |
| Frame time | Median and tail latency during a repeatable update or interaction workload |
| Lifecycle stability | Memory trend and resource behavior across repeated reload and output changes |

Use release builds. Run enough repetitions to report the sample count, median, and
spread, and include tail percentiles for frame measurements. Keep logging and
profiling overhead out of the reported run unless the same overhead is part of both
workloads. Store profiling captures outside version control unless they are
sanitized, small artifacts that you attach to a report on purpose.

## Regression policy

Get a repeatable baseline before setting budgets. Ordinary hosted CI is fine for
correctness, but noisy shared runners can't establish a performance advantage.
Automated benchmark thresholds come in only once a controlled runner and stable
workloads make a failure something you can act on.
