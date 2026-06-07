use super::*;

#[test]
fn manifest_allows_action_capabilities_without_providing_them() -> Result<(), SpindleError> {
    let mut actions = BTreeMap::new();
    actions.insert(
        String::from("sketchybar.render"),
        ExtensionAction {
            capabilities: vec![String::from("sketchybar.ui.write")],
        },
    );

    let manifest = ExtensionManifest {
        id: String::from("sketchybar-agent-status"),
        version: String::from("0.1.0"),
        entrypoint: Some(String::from("./bin/extension")),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions,
        routes: Vec::new(),
    };

    manifest.validate()?;
    Ok(())
}

#[test]
fn manifest_rejects_duplicate_action_capabilities() {
    let mut actions = BTreeMap::new();
    actions.insert(
        String::from("test.render"),
        ExtensionAction {
            capabilities: vec![String::from("test.write"), String::from("test.write")],
        },
    );
    let manifest = ExtensionManifest {
        id: String::from("duplicate-action-capability"),
        version: String::from("0.1.0"),
        entrypoint: Some(String::from("./bin/extension")),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: vec![String::from("test.write")],
        actions,
        routes: Vec::new(),
    };

    assert!(matches!(
        manifest.validate(),
        Err(SpindleError::InvalidField {
            field: "action.capabilities",
            ..
        })
    ));
}

#[test]
fn manifest_rejects_recipe_actions() {
    let mut actions = BTreeMap::new();
    actions.insert(
        String::from("workflow.render"),
        ExtensionAction {
            capabilities: Vec::new(),
        },
    );
    let manifest = ExtensionManifest {
        id: String::from("recipe-with-actions"),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions,
        routes: Vec::new(),
    };

    assert!(matches!(
        manifest.validate(),
        Err(SpindleError::InvalidField {
            field: "actions",
            reason: "recipe extensions cannot declare actions",
        })
    ));
}

#[test]
fn manifest_rejects_duplicate_route_capabilities() {
    let manifest = ExtensionManifest {
        id: String::from("duplicate-route-capability"),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: vec![ExtensionRoute {
            event: String::from("test.changed"),
            source: Some(String::from("test")),
            action: String::from("test.render"),
            capabilities: vec![String::from("test.write"), String::from("test.write")],
            args: serde_json::json!({}),
        }],
    };

    assert!(matches!(
        manifest.validate(),
        Err(SpindleError::InvalidField {
            field: "route.capabilities",
            ..
        })
    ));
}

#[test]
fn manifest_rejects_legacy_action_command_metadata() {
    let result = serde_json::from_str::<ExtensionManifest>(
        r#"{
          "id": "legacy-host",
          "version": "0.1.0",
          "entrypoint": "./bin/extension",
          "actions": {
            "legacy.render": {
              "capabilities": [],
              "command": ["render"]
            }
          }
        }"#,
    );

    assert!(result.is_err());
}

#[test]
fn manifest_rejects_emit_produce_overlap() {
    let manifest = ExtensionManifest {
        id: String::from("overlap"),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: vec![String::from("shared.event")],
        produces: vec![String::from("shared.event")],
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    };

    assert!(matches!(
        manifest.validate(),
        Err(SpindleError::InvalidField {
            field: "produces",
            ..
        })
    ));
}
