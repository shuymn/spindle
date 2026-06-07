# spindle-extension-sdk

Typed stdio-host contract shared by the spindle core and official extensions.

During install, extension hosts return an `ExtensionRegistration` from a
`register` request to declare events, actions, capabilities, and event handlers.
This is the spindle equivalent of using a host-provided registration API from
extension code.

During dispatch, the daemon keeps the extension process alive and sends an
`invoke` request containing an `ActionInvocation`. Extensions read the same
contract as an `ActionContext`.

`ActionContext::extension()` exposes the daemon-provided surface visible to the
extension: event types, installed actions, and capabilities. This lets workflow
extensions adapt to registered providers instead of linking directly to provider
implementations.

Action and route capabilities are enforced by spindle during dispatch. Direct
invocations must grant the target action's required capabilities, and routes can
grant capabilities with `RegistrationRoute::capability(...)`.

Routes are the event handler contract. The SDK intentionally has no
`subscriptions` list: an extension handles an event by registering a handler
action and a route to that action.

`ExtensionRegistration::on(...)` and `on_with_args(...)` copy the handler
action's required capabilities onto the generated route. Use
`RegistrationRoute::capability(...)` when wiring a route to an action registered
elsewhere.
