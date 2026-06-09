# Usage

## Setup

Rust is pinned to nightly in `rust-toolchain.toml`. After cloning the repository, rustup will install the toolchain automatically.

[Task](https://taskfile.dev/) and [Lefthook](https://github.com/evilmartians/lefthook) are optional but supported.

```bash
task              # list available tasks
task build        # cargo build --workspace --locked
task test         # run all tests
task lint         # clippy -D warnings
task fmt          # format
task check        # fmt, lint, test, doc, build
```

You can also use Cargo directly.

```bash
cargo build --workspace --locked
cargo test --workspace --all-targets --all-features --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

A release build creates the `spindle` binary under `target/release/`.

```bash
cargo build --workspace --release --locked
```

## Basic usage

For experiments, use an explicit state directory.

```bash
export SPINDLE_STATE_DIR=/tmp/spindle
mkdir -p "$SPINDLE_STATE_DIR"
chmod 700 "$SPINDLE_STATE_DIR"
```

Append an event.

```bash
cargo run -p spindle -- emit \
  --type agent.status.changed \
  --source pi \
  --data '{"state":"working","message":"cargo test"}'
```

Read events with `query events`.

```bash
cargo run -p spindle -- query events --type agent.status.changed
cargo run -p spindle -- query events --source pi --limit 5
```

Prepare installed extensions, then start the daemon to use the same request contract over a Unix socket.

```bash
cargo run -p spindle -- bootstrap \
  --extension-dir /path/to/packages \
  --trust-runtime
cargo run -p spindle -- daemon
```

Send a JSONL request from another shell.

```bash
cargo run -p spindle -- send \
  --request '{"command":"emit","type":"agent.status.changed","source":"pi","data":{"state":"testing"}}'
```

The default socket path is `<state-dir>/spindle.sock`. Use `--socket` to set it explicitly.

## Trusted local automation model

Installing an extension package is an explicit trust decision. `spindle` stages and runs extension executables with the current user's normal OS permissions. Use OS, container, or other external sandbox boundaries when you need stronger isolation.

Local clients that can access the user's private spindle socket may emit events. Event `source` is a routing label, not an authentication boundary. Manifest route `source` values are still validated against installed event-owning extensions. Keep the socket and state directory private, and connect only trusted same-user clients/extensions.

Capability-bearing action execution is controlled by installed extension routes and continuation grants:

- Route declarations installed from trusted extension packages may grant capabilities to their target actions.
- Direct invokes do not carry capability grants. A direct invoke can run only actions that require no capabilities.
- Continuation invokes reuse the core-validated capabilities from the original route/action invocation and fail closed when invalid, expired, or under-capable.

`bootstrap` removes stale legacy `capabilities.json` files. No policy file is required for normal desktop bootstrap.

## Direct action invocation

Direct invocation is useful for no-capability actions.

```bash
cargo run -p spindle -- invoke \
  --action local-tool.item.refresh \
  --source launcher \
  --args '{"name":"dev"}'
```

If the target action declares required capabilities, direct invoke fails with a missing capability error. Wire capability-requiring actions through installed routes or continuations instead.

## State files

The state directory mainly contains these files:

- `events.jsonl` — append-only event log
- `extensions.json` — installed extensions
- `extensions/{id}/` — staged extension packages (`extension.json`, `bin/...`)
- `spindle.sock` — daemon Unix socket

The default state directory is `$SPINDLE_STATE_DIR` when set, otherwise `$HOME/.local/state/spindle`.
