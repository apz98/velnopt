# Development

Velnopt is a solo developer's monorepo, so the process is deliberately light: no
release team, no custom build framework, no mandatory tickets. Keep changes easy to
review and easy to run. All rights are reserved, and this guide does not give you
permission to redistribute the code. If you want to contribute, agree it with me
before you submit anything.

## Daily workflow

1. Make a focused change. A short-lived branch helps for larger work; as the only
   maintainer I don't require a pull request.
2. Change behavior, tests, and documentation together.
3. Run `./scripts/check.sh`. CI runs the same script on Linux.
4. Read through `git diff` and `git diff --check` before committing, including any
   dependency and lockfile changes.

Write clear commit subjects, like `Add configuration validation` or
`Fix panel height parsing`. No commit message tooling is required. Keep `main`
buildable. Once the repo is hosted on GitHub, enable Dependabot and private
vulnerability reporting, and make the `Quality` and `Dependency audit` jobs required
checks. Those are repository settings that the owner has to apply; the files in the
repo don't turn them on.

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo run -p velnopt-cli --locked -- check examples/panel.toml
cargo build -p velnopt-cli --release --locked
```

The release executable is `target/release/velnopt`. `velnopt check <path>` validates a
configuration without needing a display; `velnopt run <path>` runs the panel inside
Hyprland. Rendering needs system fonts. It does not need Wayland development
libraries.

After a runtime change, build the executable and run the isolated integration test:

```sh
python3 scripts/smoke-hyprland.py
```

This needs Hyprland, `hyprctl`, `grim`, Python 3, and either a running Wayland
session or `labwc`. Without a session (for example from a console), it hosts the
nested compositor in a headless `labwc`; cage, sway, and weston lack the
`xdg_wm_base` version 6 that Hyprland's Wayland backend needs. The script launches a
nested compositor with private configuration and disables session environment
imports. It configures only that instance and replaces the window output with
headless test outputs, so nothing appears on your desktop and the host pointer
cannot move focus. It captures the panels and cleans up every process it started.
Reports and screenshots land in `target/hyprland-smoke`. CI does not run this
graphics check, so record your local results for rendering or runtime lifecycle
changes.

`--live-widgets` adds real IPC workspace/title updates, offline behavior, and
subscription teardown. `--interactive` adds pointer, keyboard, launcher, tray,
unavailable-service, and AT-SPI checks. Build the pointer helper first:

```sh
cargo build --release --locked -p velnopt-wayland --example virtual-pointer
python3 scripts/smoke-hyprland.py --live-widgets --interactive
```

`--interactive` also needs `wtype`, `dbus-daemon`, `busctl`, `at-spi2-core`, and
`python-gobject`. It runs against private D-Bus buses and a dead `PULSE_SERVER`, so it
touches no host service. The services tests also use `dbus-daemon` for private
buses, and `pipewire`, `pipewire-pulse`, and `pactl` for an optional isolated audio
test that is skipped when they are missing. Never run the virtual pointer helper
against a real session; it will happily click whatever is under your actual cursor.
It requires `foot` for a controlled terminal in the nested compositor. Add
`--clock-tick` to get a visible minute update. Normal Cargo tests use
mock Unix servers to cover malformed/oversized replies, reconnects, deadlines,
and cancellation, so they don't need a graphical session.

`./scripts/package.sh` builds a Linux archive and checksum under `target/dist`.
It needs Python 3 to collect dependency notices, plus the standard GNU utilities.
It does not install the application or publish the archive.

To run the same dependency audit as CI:

```sh
cargo install cargo-audit --version 0.22.2 --locked
cargo audit --deny warnings
```

The audit needs network access to the RustSec advisory database. Read the advisories
and warnings instead of quietly adding ignore entries. There are three ignores right
now, and `SECURITY.md` explains each of them. An advisory scan is not a
complete security review.

## Workspace rules

- Inherit package metadata, dependency versions, and lints from the workspace.
  Every package must opt in with `[lints] workspace = true`.
- Keep parsing independent of file access, environment variables, rendering, and
  process execution. Desktop entries, D-Bus replies, and tray data are untrusted:
  bound and sanitize them in `velnopt-services`, and never pass them to a shell.
  Validated configuration owns its invariants. `docs/architecture.md` says who owns
  file access, the runtime, and the renderer.
- Prefer concrete types and existing dependencies. Add a crate when it owns a
  real boundary, not for a subsystem you expect to need later.
- Project crates forbid unsafe Rust. Dependencies may contain unsafe code and
  need separate review. Don't weaken the lint to make an implementation fit.
- Return useful errors at boundaries. Avoid panics for user input or operating
  system failures. Tests may use `expect` to explain a failed invariant.
- Add tests for behavior and failure boundaries. Don't test formatting, and don't
  duplicate the implementation in assertions.
- Keep secrets, build output, profiling captures, and machine-specific settings
  out of version control. Never commit access tokens.

## Dependencies and toolchain

`Cargo.lock` is committed because this workspace ships an application. Use
`--locked` in validation and release builds. To change a dependency, edit the root
dependency declaration, then read the resulting lockfile diff. Dependabot groups Rust
updates weekly and proposes action updates separately.

COSMIC Text currently uses an exact upstream Git revision, to get rid of an
unmaintained transitive font parser. See `docs/decisions/0001-panel-runtime.md`.
Check that pin by hand during dependency maintenance, and move back to a published
release once it includes fontdb 0.24. Don't add a RustSec ignore to hide the
warning.

The pinned toolchain and the declared minimum Rust version are both 1.99.1. I'm not
committing to support older compilers. When updating Rust, change
`rust-toolchain.toml` and `workspace.package.rust-version` together, update the
README, and run the full checks. Also look at the pinned cargo-audit version during
toolchain maintenance, because Dependabot does not update that shell command.

## Releases

There is no automated publishing or release workflow yet. Keep changes under
`Unreleased` in the changelog. Before a release, run the full checks and audit,
review dependency licenses, update the shared version and changelog, and verify
the executable from a clean checkout. Tag and distribute only after the release
scope and distribution rights have been decided explicitly. Publishing to crates.io
is disabled for every current package.
