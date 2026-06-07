# Extensions

## Installing extensions

Install an extension by passing either a manifest path or a directory containing a manifest.

```bash
cargo run -p spindle -- install /path/to/my-extension/extension.json
cargo run -p spindle -- install /path/to/my-extension
```

Validate a manifest without starting the extension host.

```bash
cargo run -p spindle -- extension validate /path/to/my-extension/extension.json
```

`extension validate` reads only the static manifest and does not run the entrypoint. `install` / `extension register` register only the static manifest surface by default.

For a `stdio-jsonl` extension, pass `--trust-runtime` when you want to start the entrypoint and receive dynamic surface from the `register` request. `--trust-runtime` executes the entrypoint for dynamic surface discovery and records the current entrypoint path / SHA-256 in the registry. To inspect dynamic surface without writing the registry, use `extension surface --trust-runtime <manifest>`.

## Manifest and registration surface

A `stdio-jsonl` manifest contains the extension ID, version, entrypoint, and runtime. For static installation, it also contains the event/action/capability surface.

A manifest with empty surface requires dynamic registration through `install --trust-runtime` or `extension register --trust-runtime`. Static registration does not run the entrypoint during install/register. However, invoking a `stdio-jsonl` extension action does run the entrypoint. If you need entrypoint change detection, register with `--trust-runtime`.

```json
{
  "id": "my-extension",
  "version": "0.1.0",
  "entrypoint": "./target/release/my-extension",
  "runtime": "stdio-jsonl"
}
```

Event/action/capability surface can be written statically in the manifest, but `stdio-jsonl` extensions usually register it from extension code through the SDK.

`emits` are event kinds an extension may observe from external input or IPC and emit. `produces` are event kinds an extension action may return in `ActionOutput`. Both are treated as event surface ownership, so two extensions cannot register the same event kind through either `emits` or `produces`.

```rust
ExtensionRegistration::new()
    .emit("local-tool.item.changed")
    .produce("notifier.message.requested")
    .capability("notifier.message.write")
    .action(
        "notifier.message.send",
        RegistrationAction::new().capability("notifier.message.write"),
    )
```

Routes are small connections from events to actions. A route that grants capabilities must specify `source`. During dispatch, the event payload and the route's static `args` are merged as objects. If a key exists in both, route `args` wins.

```json
{
  "event": "local-tool.item.changed",
  "source": "local-tool",
  "action": "workflow.item.render",
  "capabilities": ["local-tool.item.read"],
  "args": {}
}
```

## Asynchronous continuations

When the daemon invokes an extension action through a route or direct invocation, it passes a short-lived `ContinuationContext` in `ActionContext`. The extension can return an action response immediately, then use this handle to send `continuation-invoke` / `continuation-emit` to the daemon socket.

Continuations are limited to the original invocation's capability grant. The core validates handle identity, origin extension, expiry, and required capability. Invalid, expired, or under-capable continuation work fails closed. Continuation-backed invokes record continuation provenance in the `action.requested` event.

## stdio JSONL extension hosts

`spindle-extension-sdk` provides the typed contract between the kernel and extension hosts.

An extension host receives these requests over stdin/stdout:

- `register` — return `ExtensionRegistration`
- `invoke` — receive `ActionInvocation` and return `ActionOutput`
- `shutdown` — exit the host

Actions can return events through `ActionOutput`. Returnable event kinds must be declared in `produces` in the registration/manifest.

The source of returned events is not chosen by the extension; the kernel assigns the invoking extension ID. Returned events are stored in the event log and dispatched again to matching routes.

Invocations against the same stdio host session are serialized. Host sessions for different extensions run independently, so a slow extension does not block invocations for other extensions.

`ActionContext::extension()` contains the installed surface as seen by the daemon. Workflow extensions can use it at runtime to check whether required provider events or actions are available.

`ActionContext::continuation()` contains the short-lived continuation handle for deferred work.
