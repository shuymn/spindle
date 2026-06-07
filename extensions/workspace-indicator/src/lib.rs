//! Workspace indicator projection logic.
//!
//! This extension does not talk to `AeroSpace` or `SketchyBar` directly. It
//! turns provider events into generic `SketchyBar` message requests.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::collections::BTreeSet;

use serde_json::json;
use spindle_extension_sdk::ActionOutputEvent;
use thiserror::Error;

const EMPTY_LABEL_COLOR: &str = "0xff8b949e";
const NORMAL_LABEL_COLOR: &str = "0xffc9d1d9";
const ACTIVE_LABEL_COLOR: &str = "0xff1f2328";
const EMPTY_BACKGROUND_COLOR: &str = "0xff1f2328";
const OCCUPIED_BACKGROUND_COLOR: &str = "0xff3a4048";
const ACTIVE_BACKGROUND_COLOR: &str = "0xffd2a8ff";
const MEDIUM_FONT: &str = "SF Pro Text:Medium:13.0";
const SEMIBOLD_FONT: &str = "SF Pro Text:Semibold:13.0";
const MODE_SERVICE_COLOR: &str = "0xffff7b72";
const MODE_RESIZE_COLOR: &str = "0xff56d4dd";
const LAYOUT_FULLSCREEN_COLOR: &str = "0xffd29922";
const LAYOUT_FLOATING_COLOR: &str = "0xffa371f7";
const LAYOUT_ACCORDION_COLOR: &str = "0xff58a6ff";
const LAYOUT_TILED_COLOR: &str = "0xff3fb950";

/// Event emitted when a generic `SketchyBar` message should be sent.
pub const OUTPUT_EVENT: &str = "sketchybar.message.requested";
/// Source used for workspace-indicator output events.
pub const OUTPUT_SOURCE: &str = "workspace-indicator";

/// Error returned by the workspace indicator extension.
#[derive(Debug, Error)]
pub enum WorkspaceIndicatorError {
    /// A workspace value cannot be rendered safely.
    #[error("invalid workspace: {reason}")]
    InvalidWorkspace {
        /// Validation failure reason.
        reason: &'static str,
    },

    /// A configured workspace list does not contain any usable workspace.
    #[error("invalid workspaces: must contain at least one workspace")]
    EmptyWorkspaces,
}

/// Snapshot of workspace state supplied by the `AeroSpace` provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    active: Option<String>,
    occupied: BTreeSet<String>,
}

impl WorkspaceSnapshot {
    /// Create a workspace snapshot.
    #[must_use]
    pub const fn new(active: Option<String>, occupied: BTreeSet<String>) -> Self {
        Self { active, occupied }
    }
}

/// Generic `SketchyBar` message request produced by this workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchybarMessageRequest {
    args: Vec<String>,
    cache_key: String,
    cache_value: String,
}

impl SketchybarMessageRequest {
    /// Return command-line style `SketchyBar` arguments.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Return a provider-local cache key for this message.
    #[must_use]
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    /// Return the cache value represented by this message.
    #[must_use]
    pub fn cache_value(&self) -> &str {
        &self.cache_value
    }

    /// Convert this message request into a spindle event.
    #[must_use]
    pub fn into_event(self) -> ActionOutputEvent {
        ActionOutputEvent::new(OUTPUT_EVENT, OUTPUT_SOURCE).with_data(json!({
            "args": self.args,
            "cache_key": self.cache_key,
            "cache_value": self.cache_value,
        }))
    }
}

/// Build a `SketchyBar` message request for all workspace items.
///
/// # Errors
///
/// Returns an error when the configured workspace list is empty or contains
/// invalid workspace names.
pub fn build_workspace_message<S>(
    workspaces: &[S],
    snapshot: &WorkspaceSnapshot,
) -> Result<SketchybarMessageRequest, WorkspaceIndicatorError>
where
    S: AsRef<str>,
{
    validate_workspaces(workspaces)?;

    let mut args = Vec::with_capacity(workspaces.len() * 11);
    for workspace in workspaces {
        let workspace = workspace.as_ref();
        let role = WorkspaceRole::for_workspace(workspace, snapshot);
        append_workspace_args(&mut args, workspace, role);
    }

    Ok(SketchybarMessageRequest {
        args,
        cache_key: String::from("workspace-indicator.workspaces"),
        cache_value: workspace_cache_value(workspaces, snapshot),
    })
}

/// Build a `SketchyBar` message request for an `AeroSpace` mode item.
#[must_use]
pub fn build_mode_status(item: &str, mode: &str) -> SketchybarMessageRequest {
    let (label, label_color, background_color) = match mode {
        "main" => ("N", NORMAL_LABEL_COLOR, OCCUPIED_BACKGROUND_COLOR),
        "service" => ("S", ACTIVE_LABEL_COLOR, MODE_SERVICE_COLOR),
        "resize" => ("R", ACTIVE_LABEL_COLOR, MODE_RESIZE_COLOR),
        _ => {
            return build_unknown_mode_status(item, mode);
        }
    };

    status_update(
        item,
        &[
            property("label", label),
            property("label.color", label_color),
            property("background.color", background_color),
        ],
        &[label, label_color, background_color],
    )
}

/// Build a `SketchyBar` message request for an `AeroSpace` window layout item.
#[must_use]
pub fn build_layout_status(item: &str, window_info: Option<&str>) -> SketchybarMessageRequest {
    let (label, color) = layout_status(window_info);
    status_update(
        item,
        &[property("label", label), property("label.color", color)],
        &[label, color],
    )
}

/// Build the status message request for a managed item.
#[must_use]
pub fn build_status_message(
    item: &str,
    mode: Option<&str>,
    window_info: Option<&str>,
) -> Option<SketchybarMessageRequest> {
    match status_kind(item) {
        Some(StatusKind::Mode) => Some(build_mode_status(item, mode.unwrap_or("?"))),
        Some(StatusKind::Layout) => Some(build_layout_status(item, window_info)),
        None => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceRole {
    Active,
    Occupied,
    Empty,
}

impl WorkspaceRole {
    fn for_workspace(workspace: &str, snapshot: &WorkspaceSnapshot) -> Self {
        if snapshot
            .active
            .as_ref()
            .is_some_and(|active| active == workspace)
        {
            Self::Active
        } else if snapshot.occupied.contains(workspace) {
            Self::Occupied
        } else {
            Self::Empty
        }
    }

    const fn label_color(self) -> &'static str {
        match self {
            Self::Active => ACTIVE_LABEL_COLOR,
            Self::Occupied => NORMAL_LABEL_COLOR,
            Self::Empty => EMPTY_LABEL_COLOR,
        }
    }

    const fn label_font(self) -> &'static str {
        match self {
            Self::Active => SEMIBOLD_FONT,
            Self::Occupied | Self::Empty => MEDIUM_FONT,
        }
    }

    const fn background_drawing(self) -> &'static str {
        match self {
            Self::Active | Self::Occupied => "on",
            Self::Empty => "off",
        }
    }

    const fn background_color(self) -> &'static str {
        match self {
            Self::Active => ACTIVE_BACKGROUND_COLOR,
            Self::Occupied => OCCUPIED_BACKGROUND_COLOR,
            Self::Empty => EMPTY_BACKGROUND_COLOR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Mode,
    Layout,
}

fn status_kind(item: &str) -> Option<StatusKind> {
    match item.rsplit_once('.').map(|(_prefix, kind)| kind) {
        Some("mode") => Some(StatusKind::Mode),
        Some("layout") => Some(StatusKind::Layout),
        _ => None,
    }
}

fn append_workspace_args(args: &mut Vec<String>, workspace: &str, role: WorkspaceRole) {
    args.push(String::from("--set"));
    args.push(workspace_item_name(workspace));
    args.push(property("label", workspace_label(workspace)));
    args.push(property("label.color", role.label_color()));
    args.push(property("label.font", role.label_font()));
    args.push(String::from("label.width=24"));
    args.push(String::from("label.align=center"));
    args.push(String::from("label.padding_left=0"));
    args.push(String::from("label.padding_right=0"));
    args.push(property("background.drawing", role.background_drawing()));
    args.push(property("background.color", role.background_color()));
}

fn workspace_cache_value<S>(workspaces: &[S], snapshot: &WorkspaceSnapshot) -> String
where
    S: AsRef<str>,
{
    let mut value = String::new();
    if let Some(active) = &snapshot.active {
        value.push_str(active);
    }
    value.push('\n');
    for workspace in workspaces {
        let workspace = workspace.as_ref();
        if snapshot.occupied.contains(workspace) {
            value.push(' ');
            value.push_str(workspace);
            value.push(' ');
        }
    }
    value.push('\n');
    value
}

fn workspace_item_name(workspace: &str) -> String {
    let mut item = String::from("aerospace.workspace.");
    item.push_str(workspace);
    item
}

fn workspace_label(workspace: &str) -> &str {
    if workspace == "10" { "0" } else { workspace }
}

fn property(key: &str, value: &str) -> String {
    let mut property = String::with_capacity(key.len() + value.len() + 1);
    property.push_str(key);
    property.push('=');
    property.push_str(value);
    property
}

fn status_update(
    item: &str,
    properties: &[String],
    cache_parts: &[&str],
) -> SketchybarMessageRequest {
    let mut args = Vec::with_capacity(properties.len() + 2);
    args.push(String::from("--set"));
    args.push(String::from(item));
    args.extend_from_slice(properties);

    SketchybarMessageRequest {
        args,
        cache_key: status_cache_key(item),
        cache_value: cache_parts.join("|"),
    }
}

fn status_cache_key(item: &str) -> String {
    let mut key = String::from("workspace-indicator.status.");
    key.push_str(item);
    key
}

fn build_unknown_mode_status(item: &str, mode: &str) -> SketchybarMessageRequest {
    let label = mode
        .chars()
        .next()
        .map_or_else(|| String::from("?"), uppercase_char);
    status_update(
        item,
        &[
            property("label", &label),
            property("label.color", ACTIVE_LABEL_COLOR),
            property("background.color", ACTIVE_BACKGROUND_COLOR),
        ],
        &[&label, ACTIVE_LABEL_COLOR, ACTIVE_BACKGROUND_COLOR],
    )
}

fn uppercase_char(character: char) -> String {
    character.to_uppercase().collect()
}

fn layout_status(window_info: Option<&str>) -> (&'static str, &'static str) {
    let Some(window_info) = window_info.filter(|value| !value.trim().is_empty()) else {
        return ("NONE", EMPTY_LABEL_COLOR);
    };

    let (is_fullscreen, layout) = window_info.split_once('|').unwrap_or((window_info, ""));
    if is_fullscreen == "true" {
        return ("FULL", LAYOUT_FULLSCREEN_COLOR);
    }

    if layout.contains("floating") {
        ("FLOAT", LAYOUT_FLOATING_COLOR)
    } else if layout.contains("accordion") {
        ("ACCORD", LAYOUT_ACCORDION_COLOR)
    } else {
        ("TILE", LAYOUT_TILED_COLOR)
    }
}

fn validate_workspace(workspace: &str) -> Result<(), WorkspaceIndicatorError> {
    if workspace.trim().is_empty() {
        return Err(WorkspaceIndicatorError::InvalidWorkspace {
            reason: "must not be empty",
        });
    }

    if workspace.chars().any(char::is_control) {
        return Err(WorkspaceIndicatorError::InvalidWorkspace {
            reason: "must not contain control characters",
        });
    }

    Ok(())
}

fn validate_workspaces<S>(workspaces: &[S]) -> Result<(), WorkspaceIndicatorError>
where
    S: AsRef<str>,
{
    if workspaces.is_empty() {
        return Err(WorkspaceIndicatorError::EmptyWorkspaces);
    }

    for workspace in workspaces {
        validate_workspace(workspace.as_ref())?;
    }

    Ok(())
}
