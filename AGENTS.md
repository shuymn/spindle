<!-- Maintenance: update when repository commands, hooks, lint policy, or agent-facing docs change. -->
<!-- Audience: this file and docs/ are for coding agents. Keep instructions direct and compact. -->

## Non-negotiables

- Use Task as the default command interface.
  Prefer `task build`, `task test`, `task lint`, `task fmt`, and `task check` over ad-hoc command sequences.
- Do not use `--no-verify` for commits or pushes. Fix the hook failure.
- Keep this file limited to always-on repository rules; put detailed guidance in `docs/`.
- When writing or updating agent-facing docs, use direct instructions instead of tutorials or human-oriented prose.

## Rust work

- Before modifying Rust code, read `docs/coding.md`.
- Before modifying tests, read `docs/testing.md`.
- Before changing build, CI, hooks, toolchain, or adding tools, read `docs/tooling.md`.
- Before code review work, read `docs/review.md`.
- Before changing kernel/extension boundaries, read `docs/concepts.md`.
- Before changing CLI behavior, state files, or policy behavior, read `docs/usage.md`.
- Before changing extension manifests, routes, continuations, or stdio hosts, read `docs/extensions.md`.
- Read `docs/adr/` only when historical rationale matters to the task.
- `unwrap`, `expect`, `todo`, and `dbg!` are denied across the workspace, including tests.
  Prefer `Result` tests and `?`.
- `unsafe_code` is forbidden in this workspace.

## Commands

- Full local verification: `task check`.
- Fast verification without tests/docs: `task check:fast`.
- Rust-native equivalents are acceptable when Task is unavailable:
  - `cargo build --workspace --locked`
  - `cargo test --workspace --all-targets --all-features --locked`
  - `cargo fmt --all`
  - `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- After changing `Cargo.toml`, run `task build` or `cargo build --workspace --locked`.
