use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use sha2::Digest;
use spindle_extension_sdk::{ActionInvocation, RegistrationAction, RegistrationRoute};

use super::*;
use crate::{ExtensionRuntimeHost, SpindleError};

mod manifest;
mod registry;
mod runtime_trust;
mod surface;

fn write_static_manifest(
    dir: &Path,
    id: &str,
    action_names: &[&str],
    emits: &[&str],
    capabilities: &[&str],
) -> Result<PathBuf, SpindleError> {
    write_static_manifest_with_surface(
        dir,
        id,
        StaticManifestSurface {
            action_names,
            emits,
            produces: &[],
            capabilities,
        },
    )
}

#[derive(Clone, Copy)]
struct StaticManifestSurface<'a> {
    action_names: &'a [&'a str],
    emits: &'a [&'a str],
    produces: &'a [&'a str],
    capabilities: &'a [&'a str],
}

fn write_static_manifest_with_surface(
    dir: &Path,
    id: &str,
    surface: StaticManifestSurface<'_>,
) -> Result<PathBuf, SpindleError> {
    let actions = surface
        .action_names
        .iter()
        .map(|name| {
            (
                String::from(*name),
                ExtensionAction {
                    capabilities: Vec::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let manifest = ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: surface
            .emits
            .iter()
            .map(|event| String::from(*event))
            .collect(),
        produces: surface
            .produces
            .iter()
            .map(|event| String::from(*event))
            .collect(),
        capabilities: surface
            .capabilities
            .iter()
            .map(|capability| String::from(*capability))
            .collect(),
        actions,
        routes: Vec::new(),
    };
    let path = dir.join(format!("{id}.json"));
    fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(path)
}

fn registered_stdio_extension(dir: &Path, id: &str, host: &Path) -> RegisteredExtension {
    RegisteredExtension {
        id: String::from(id),
        version: String::from("0.1.0"),
        manifest_path: dir.join("extension.json"),
        runtime: ExtensionRuntime::StdioJsonl,
        entrypoint: Some(host.to_string_lossy().into_owned()),
        capabilities: Vec::new(),
        emits: Vec::new(),
        produces: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
        runtime_trust: None,
    }
}

fn write_marker_stdio_host(dir: &Path, name: &str, marker: &str) -> Result<PathBuf, SpindleError> {
    let host = dir.join(format!("{name}.sh"));
    let script = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{{"type":"registration","registration":{{"actions":{{"test.render":{{}}}}}}}}'
      ;;
    *'"type":"invoke"'*)
      printf '%s\n' '{{"type":"action-output","output":{{"events":[{{"type":"test.rendered","source":"replace-me","data":{{"marker":"{marker}"}}}}]}}}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
  esac
done
"#
    );
    crate::store::tests_support::write_executable(&host, &script)?;
    Ok(host)
}

fn write_stdio_manifest(
    dir: &Path,
    file_name: &str,
    id: &str,
    host: &Path,
) -> Result<PathBuf, SpindleError> {
    let manifest = ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        entrypoint: Some(host.to_string_lossy().into_owned()),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    };
    let path = dir.join(file_name);
    fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(path)
}

fn assert_surface_conflict(
    error: SpindleError,
    expected_surface: &'static str,
    expected_name: &str,
    expected_existing_extension: &str,
    expected_new_extension: &str,
) -> Result<(), SpindleError> {
    let SpindleError::SurfaceConflict {
        surface,
        name,
        existing_extension,
        new_extension,
    } = error
    else {
        return Err(error);
    };

    assert_eq!(surface, expected_surface);
    assert_eq!(name, expected_name);
    assert_eq!(existing_extension, expected_existing_extension);
    assert_eq!(new_extension, expected_new_extension);
    Ok(())
}
