# workspace-indicator

Workspace indicator workflow package for spindle.

This package has a stdio JSONL host entrypoint, but it does not talk to `AeroSpace` or
`SketchyBar` directly. It owns the projection that used to live in local
SketchyBar scripts: workspace labels, colors, status labels, and cache keys.

It connects provider extensions through routes and produces generic provider
requests from action output:

- `aerospace` emits domain events and exposes state/control actions.
- `workspace-indicator` registers event handlers from its host.
- `workspace-indicator` projects `AeroSpace` snapshots into
  produced `workspace-indicator.sketchybar.message.requested` events.
- `sketchybar` owns the actual UI write action.

Routes in this package grant only the capabilities needed by the target action,
for example `aerospace.state.read`, `aerospace.window.control`, or
`sketchybar.ui.write`.

Before producing a request, the host reads `ActionContext::extension()` and
checks that `workspace-indicator.sketchybar.message.requested` and `sketchybar.message.send` are
available in the daemon-provided surface.

The important path is:

```text
aerospace.workspace.changed -> aerospace.workspace.snapshot -> workspace-indicator.workspaces.render -> workspace-indicator.sketchybar.message.requested -> sketchybar.message.send
sketchybar.workspace.clicked -> aerospace.workspace.focus
```
