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

cat > "$SPINDLE_STATE_DIR/capabilities.json" <<'JSON'
{
  "emits": {
    "pi": ["agent.status.changed"]
  },
  "direct": {},
  "routes": {}
}
JSON
chmod 600 "$SPINDLE_STATE_DIR/capabilities.json"
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

Start the daemon to use the same request contract over a Unix socket.

```bash
cargo run -p spindle -- daemon
```

Send a JSONL request from another shell.

```bash
cargo run -p spindle -- send \
  --request '{"command":"emit","type":"agent.status.changed","source":"pi","data":{"state":"testing"}}'
```

The default socket path is `<state-dir>/spindle.sock`. Use `--socket` to set it explicitly.

## Capability policy

When an action requires capabilities, direct invokes and routes must grant those capabilities. The grants must also be allowed by `capabilities.json` in the state directory.

Route capabilities are checked both during dispatch and when an extension is installed or registered. Route grant policy is keyed by route owner extension ID and specifies the allowed event `source`, event kind, and capabilities.

```json
{
  "emits": {
    "local-tool": ["local-tool.item.changed"]
  },
  "direct": {
    "launcher": ["local-tool.item.write"]
  },
  "routes": {
    "workflow": [
      {
        "source": "local-tool",
        "event": "local-tool.item.changed",
        "capabilities": ["local-tool.item.read"]
      }
    ]
  }
}
```

`capabilities.json` is local grant policy. If you create it manually, keep it private with `chmod 600 "$SPINDLE_STATE_DIR/capabilities.json"`.

Example direct action invocation:

```bash
cargo run -p spindle -- invoke \
  --action local-tool.item.write \
  --source launcher \
  --capability local-tool.item.write \
  --args '{"name":"dev"}'
```

`emit` is also a dispatch entrypoint, so only event kinds allowed by `capabilities.json` under `emits[source]` are accepted.

A policy grantor cannot be `*`. Capability values may be `*`, but this broadens access and should be limited to trusted sources/extensions.

`source` and extension IDs are local logical names, not OS sandboxes. `spindle` is a trusted same-user local automation bus bounded by private state directories, Unix socket permissions, and policy. Peer UID checks are not implemented yet. Keep the socket and state directory private, and connect only trusted same-user clients/extensions.

## State files

The state directory mainly contains these files:

- `events.jsonl` — append-only event log
- `extensions.json` — installed/registered extensions
- `capabilities.json` — emit / direct / route grant policy
- `spindle.sock` — daemon Unix socket

The default state directory is `$SPINDLE_STATE_DIR` when set, otherwise `$HOME/.local/state/spindle`.
