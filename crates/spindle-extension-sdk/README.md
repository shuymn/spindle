# spindle-extension-sdk

Typed stdio-host contract shared by the spindle kernel (`crates/spindle`) and
extension hosts.

During install, extension hosts return an `ExtensionRegistration` from a
`register` request to declare emitted events, produced action-output events,
actions, capabilities, and event handlers. This runtime registration only runs
when the user passes `--trust-runtime`; static validation does not execute the
entrypoint.

During dispatch, the daemon keeps the extension process alive and sends an
`invoke` request containing an `ActionInvocation`. Extensions read the same
contract as an `ActionContext`.

`ActionContext::extension()` exposes the daemon-provided surface visible to the
extension: event types, installed actions, and capabilities. This lets workflow
extensions adapt to registered providers instead of linking directly to provider
implementations.

Action and route capabilities are enforced by spindle during dispatch. Direct
invocations must grant the target action's required capabilities, and routes can
grant capabilities with `RegistrationRoute::capability(...)`. A route that
grants capabilities must also set `RegistrationRoute::source(...)`.

Routes are the event handler contract. The SDK intentionally has no
`subscriptions` list: an extension handles an event by registering a handler
action and a route to that action.

`ExtensionRegistration::on(...)` and `on_with_args(...)` are for capless
handler routes and do not copy action capabilities onto the generated route.
Use `on_from(...)` / `on_with_args_from(...)` for source-aware handler routes
that should inherit the handler action's capabilities. Use
`RegistrationRoute::capability(...)` when wiring a route to an action registered
elsewhere.

Action output events do not carry a public source field. The core stamps emitted
events with the invoking extension id and rejects event kinds not declared with
`ExtensionRegistration::produce(...)` or static manifest `produces`.
