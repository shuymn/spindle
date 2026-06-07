# spindle-sketchybar-extension

SketchyBar capability extension for spindle.

This package owns SketchyBar Mach IPC and UI write actions. It does not read
AeroSpace state and does not know workspace-indicator rendering rules.

`sketchybar.message.send` accepts generic `SketchyBar` command arguments from
routed events.
