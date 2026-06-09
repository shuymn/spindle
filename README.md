# spindle

[日本語版](README.ja.md)

`spindle` is a small harness for connecting local macOS automation through events, actions, and extensions.

The core is intentionally small: it stores events, validates extension registrations, and dispatches routes from events to actions. App integrations, external tool integrations, workflow logic, and agent hooks live outside the kernel as extensions.

```text
receive an event
  -> store it in an append-only JSONL log
  -> find installed routes
  -> invoke extension actions
  -> dispatch events produced by those actions
```

## Status

Experimental. The repository currently contains the daemon kernel and the extension SDK.

## Repository layout

```text
spindle/
  crates/spindle/                    daemon kernel, CLI, socket server
  crates/spindle-extension-sdk/      typed SDK for stdio JSONL extension hosts
  crates/spindle-extension-example/  minimal stdio JSONL extension example
  docs/                              concepts, usage, and development notes
```

## Quick start

Rust is pinned by `rust-toolchain.toml`.

```bash
cargo build --workspace --locked
cargo test --workspace --all-targets --all-features --locked
```

Task is supported when available:

```bash
task build
task test
task check
```

For experiments, use an explicit state directory.

```bash
export SPINDLE_STATE_DIR=/tmp/spindle
mkdir -p "$SPINDLE_STATE_DIR"
chmod 700 "$SPINDLE_STATE_DIR"

cargo run -p spindle -- emit \
  --type agent.status.changed \
  --source pi \
  --data '{"state":"working","message":"cargo test"}'

cargo run -p spindle -- query events --type agent.status.changed
```

## Security model

Installing an extension package means trusting its executable code to run as your user. Local clients that can access the user's spindle socket may emit events; event `source` is a routing label, not an authentication boundary. Manifest route `source` values are still validated against installed event-owning extensions. Capability-bearing action execution is authorized by installed route declarations and continuation grants, not by a separate policy file.

## Documentation

- [Concepts](docs/concepts.md) — kernel responsibilities, extension boundaries, and what stays out of core
- [Usage](docs/usage.md) — setup, CLI examples, trusted local automation model, and state files
- [Extensions](docs/extensions.md) — manifests, registration surface, routes, continuations, and stdio JSONL hosts
- [Extension SDK README](crates/spindle-extension-sdk/README.md) — SDK package notes

Agent-facing development notes:

- [Coding guidelines](docs/coding.md)
- [Testing guidelines](docs/testing.md)
- [Tooling guidelines](docs/tooling.md)
- [Review guidelines](docs/review.md)

## License

MIT
