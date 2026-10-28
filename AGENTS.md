# Velnopt repository instructions

## Context7

Use Context7 MCP to fetch current documentation whenever the user asks about a
library, framework, SDK, API, CLI tool, or cloud service, including API syntax,
configuration, version migration, platform-specific debugging, and CLI usage.
Prefer it over web search even for familiar libraries.

Do not use it for general programming concepts or refactoring, debugging, and
review that do not depend on a library or platform API.

1. Start with `resolve-library-id` using the library name and documentation task,
   unless the user provides an exact `/org/project` library ID.
2. Choose by exact name, description relevance, snippet coverage, source
   reputation, and benchmark score. Refine poor matches; use version-specific IDs
   when the user specifies a version.
3. Use `query-docs` with the selected ID and a specific concept. Query distinct
   concepts separately unless the question concerns their interaction.
4. Use the fetched documentation in the implementation or answer.

## Implementation

- Read `docs/architecture.md` and trace the behavior before editing.
- Use `velnopt-*` for every workspace package and its matching folder under
  `crates/`. The `velnopt-cli` package builds the executable named `velnopt`.
- Keep configuration file access in `velnopt-cli`, compositor resources and the
  event loop in `velnopt-wayland`, drawing in `velnopt-ui`, widget presentation in
  `velnopt-widgets`, and parsing invariants in `velnopt-config`. Follow
  `docs/crates.md`; do not add placeholder crates for planned features.
- Keep Hyprland IPC transport, data validation, and reconnects in
  `velnopt-hyprland`. Preserve bounded I/O, cancellation, and read-only state
  queries; the only write is the validated `dispatch workspace <id>` request.
  Do not log window titles or raw IPC responses.
- Inherit workspace metadata, dependency versions, and lints in every crate.
- Keep unsafe code forbidden. Do not claim dependencies are free of unsafe code.
- Validate user input at boundaries and preserve actionable error messages.
- Keep changes focused and update relevant tests, examples, and documentation.
- Run `./scripts/check.sh` for code and manifest changes. For documentation-only
  edits, check links and `git diff --check` instead.
- Hyprland is the primary runtime target. Run the isolated native smoke test after
  runtime changes when a suitable graphics environment is available. Record any
  integration checks that could not run.
- Do not claim Velnopt is faster or safer than another project without evidence.
- Keep the all-rights-reserved policy and `publish = false` unless the owner
  explicitly changes them. Do not publish, tag, or deploy as part of routine work.
