use std::collections::BTreeSet;

use serde_json::json;
use spindle_workspace_indicator_extension::{
    WorkspaceSnapshot, build_layout_status, build_mode_status, build_status_message,
    build_workspace_message,
};

#[test]
fn workspace_message_matches_existing_sketchybar_items() -> anyhow::Result<()> {
    let workspaces = ["1", "2", "10"];
    let occupied = BTreeSet::from([String::from("2"), String::from("10")]);
    let snapshot = WorkspaceSnapshot::new(Some(String::from("10")), occupied);

    let request = build_workspace_message(&workspaces, &snapshot)?;

    assert_eq!(request.cache_key(), "workspace-indicator.workspaces");
    assert_eq!(request.cache_value(), "10\n 2  10 \n");
    assert_eq!(
        request.args(),
        &[
            "--set",
            "aerospace.workspace.1",
            "label=1",
            "label.color=0xff8b949e",
            "label.font=SF Pro Text:Medium:13.0",
            "label.width=24",
            "label.align=center",
            "label.padding_left=0",
            "label.padding_right=0",
            "background.drawing=off",
            "background.color=0xff1f2328",
            "--set",
            "aerospace.workspace.2",
            "label=2",
            "label.color=0xffc9d1d9",
            "label.font=SF Pro Text:Medium:13.0",
            "label.width=24",
            "label.align=center",
            "label.padding_left=0",
            "label.padding_right=0",
            "background.drawing=on",
            "background.color=0xff3a4048",
            "--set",
            "aerospace.workspace.10",
            "label=0",
            "label.color=0xff1f2328",
            "label.font=SF Pro Text:Semibold:13.0",
            "label.width=24",
            "label.align=center",
            "label.padding_left=0",
            "label.padding_right=0",
            "background.drawing=on",
            "background.color=0xffd2a8ff",
        ]
    );
    Ok(())
}

#[test]
fn message_request_becomes_generic_sketchybar_event() -> anyhow::Result<()> {
    let workspaces = ["1"];
    let snapshot = WorkspaceSnapshot::new(Some(String::from("1")), BTreeSet::new());

    let event = build_workspace_message(&workspaces, &snapshot)?.into_event();

    assert_eq!(event.kind, "sketchybar.message.requested");
    assert_eq!(event.source, "workspace-indicator");
    assert_eq!(
        event.data,
        json!({
            "args": [
                "--set",
                "aerospace.workspace.1",
                "label=1",
                "label.color=0xff1f2328",
                "label.font=SF Pro Text:Semibold:13.0",
                "label.width=24",
                "label.align=center",
                "label.padding_left=0",
                "label.padding_right=0",
                "background.drawing=on",
                "background.color=0xffd2a8ff"
            ],
            "cache_key": "workspace-indicator.workspaces",
            "cache_value": "1\n\n"
        })
    );
    Ok(())
}

#[test]
fn mode_status_matches_existing_mode_colors() {
    let request = build_mode_status("aerospace.mode", "service");

    assert_eq!(
        request.cache_key(),
        "workspace-indicator.status.aerospace.mode"
    );
    assert_eq!(request.cache_value(), "S|0xff1f2328|0xffff7b72");
    assert_eq!(
        request.args(),
        &[
            "--set",
            "aerospace.mode",
            "label=S",
            "label.color=0xff1f2328",
            "background.color=0xffff7b72",
        ]
    );
}

#[test]
fn layout_status_matches_existing_window_layout_labels() {
    let tiled = build_layout_status("aerospace.layout", Some("false|h_tiles"));
    let fullscreen = build_layout_status("aerospace.layout", Some("true|h_tiles"));
    let none = build_layout_status("aerospace.layout", None);

    assert_eq!(tiled.cache_value(), "TILE|0xff3fb950");
    assert_eq!(
        tiled.args(),
        &[
            "--set",
            "aerospace.layout",
            "label=TILE",
            "label.color=0xff3fb950",
        ]
    );
    assert_eq!(fullscreen.cache_value(), "FULL|0xffd29922");
    assert_eq!(none.cache_value(), "NONE|0xff8b949e");
}

#[test]
fn unknown_status_item_does_not_emit_message() {
    assert!(build_status_message("clock", Some("main"), None).is_none());
}
