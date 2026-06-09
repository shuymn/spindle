# Concepts

`spindle` is organized around a small kernel plus external extensions. The kernel stores events, validates extension registrations, and dispatches routes from events to actions. App integrations, external tool integrations, and agent hooks stay outside the kernel as extensions.

## Event/action flow

```text
receive an event
  -> store it in an append-only JSONL log
  -> find installed routes
  -> invoke extension actions
  -> dispatch events produced by those actions
```

Example flows:

```text
provider.event.changed
  -> workflow.prepare
  -> workflow.render
  -> notifier.message.send

ui.item.clicked
  -> provider.item.focus
```

Provider extensions handle external protocols. Workflow extensions own projection logic, debounce policies, and branching. The kernel validates registered surface, then dispatches events to actions.

## Kernel responsibilities

- Append-only JSONL event log with UUID v7-style IDs
- JSONL requests over a Unix domain socket
- Event append and route dispatch via `emit`
- Direct action execution and `action.requested` logging via `invoke`
- Extension manifest validation
- Registration, startup, and reuse of stdio JSONL extension hosts
- Ownership checks for extension-declared event/action surfaces and provided capabilities
- Direct invoke without capability minting
- Installed route capability grants from trusted extension declarations
- Capability-scoped continuation handles for deferred extension work
- Recursive dispatch of action output events, with a depth limit

## What stays out of the kernel

The following features are intentionally excluded from the core. Use this table to decide whether a new feature belongs in the kernel or in an extension.

| Excluded feature | Why it is not in core | Extension path |
|------------------|-----------------------|----------------|
| Event filtering | Filter conditions vary by workflow. Putting them in core would require rebuilding the kernel for each change. Route `source` matching is enough. | Use route `source` fields. |
| Template engine | Message formats and display logic are domain-specific. A core template language would limit extension expressiveness. | Let each extension own its rendering logic. |
| Retry / replay | Retry count, interval, and backoff depend on action semantics. Fixed core logic is not enough. | Implement in extensions, or declare later through an instruction-surface retry policy. |
| Conditional branching | If the core owns branching logic, extensions lose freedom to shape workflows. Branching belongs to workflow extensions. | Workflow extensions branch through action output. |
| Workflow-specific state | Keeping progress counters or intermediate state in core creates implicit coupling between extensions and makes testing/reuse harder. | Let each extension own its state or query the event log. |
| Scheduling / timers | Periodic execution is the responsibility of external tools such as launchd or cron. Pulling it into core complicates process management. | Trigger `emit` or `invoke` from an external scheduler. |
| Notifications / alerts | Notification targets differ by environment. Supporting every path in core would bloat it. | Add notification extensions or route action output to notification targets. |
| Event retention policy | Retention and rotation are operational concerns, not kernel responsibilities. | Use external log rotation or a log-reading extension. |
| Third-party protocols | Embedding app-specific protocols leaves unused dependencies in core and requires core changes for every new protocol. | Implement each protocol as an extension. |
| Dynamic plugin loading (`dlopen`) | In-process dynamic loading blurs safety boundaries and lets extension crashes affect the whole kernel. | Use process isolation through stdio JSONL, with one child process per extension. |

### Decision rule

Add a feature to the kernel only when it satisfies **all** of these conditions:

1. **Universal** — every session needs it even with no extensions installed.
2. **Safety-critical** — delegating it to third-party extension authors would make unsafe or invalid behavior unavoidable.
3. **Stable** — its interface does not vary by user or team and can remain fixed long-term.

If any condition is not met, design it as extension surface instead.

## Workspace layout

```text
spindle/
  crates/spindle/                    daemon kernel, CLI, socket server
  crates/spindle-extension-sdk/      typed SDK for stdio JSONL extension hosts
  crates/spindle-extension-example/    minimal stdio JSONL extension example
  crates/spindle-test-host/          config-driven test host (integration tests)
```

- `crates/spindle/` — event log, dispatch, manifest validation, daemon
- `crates/spindle-extension-sdk/` — contract types shared by the kernel and extension hosts
- `crates/spindle-extension-example/` — reference extension built with the workspace

`spindle-extension-sdk` is a library for extension authors. `crates/spindle` is the daemon itself; it depends on the SDK to validate and execute the extension contract.
