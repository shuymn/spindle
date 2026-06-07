use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use sha2::Digest;
use spindle_extension_sdk::{
    ActionInvocation, ExtensionRegistration, RegistrationAction, RegistrationRoute,
};
use spindle_test_host::TestHostConfig;

use super::*;
use crate::{EventLog, ExtensionRuntimeHost, SpindleError, serve, validate_installed_registry};

mod manifest;
mod registry;
mod runtime_trust;
mod stage;
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
    let package = dir.join(id);
    fs::create_dir_all(package.join("bin"))?;
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
    let runtime = if surface.action_names.is_empty() {
        ExtensionRuntime::Recipe
    } else {
        crate::store::tests_support::write_executable(
            &package.join("bin").join(id),
            "#!/bin/sh\nexit 0\n",
        )?;
        ExtensionRuntime::StdioJsonl
    };
    let manifest = ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        runtime,
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
    fs::write(
        package.join("extension.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(package)
}

fn write_clock_bootstrap_package(dir: &Path) -> Result<PathBuf, SpindleError> {
    let package = dir.join("clock");
    fs::create_dir_all(&package)?;
    fs::write(
        package.join("extension.json"),
        r#"{
          "id": "clock",
          "version": "0.1.0",
          "runtime": "recipe",
          "emits": ["clock.tick"],
          "routes": [
            {
              "event": "clock.tick",
              "action": "sketchybar.message.send"
            }
          ]
        }"#,
    )?;
    Ok(package)
}

fn write_sketchybar_bootstrap_package(dir: &Path) -> Result<PathBuf, SpindleError> {
    write_static_manifest_with_surface(
        dir,
        "sketchybar",
        StaticManifestSurface {
            action_names: &["sketchybar.message.send"],
            emits: &[],
            produces: &[],
            capabilities: &[],
        },
    )
}

fn registered_stdio_extension(
    dir: &Path,
    id: &str,
    host: &Path,
) -> Result<RegisteredExtension, SpindleError> {
    let package = dir.join(id);
    fs::create_dir_all(package.join("bin"))?;
    let staged_host = package.join("bin").join(id);
    fs::copy(host, &staged_host)?;
    let host_config = host.with_extension("json");
    if host_config.is_file() {
        fs::copy(&host_config, staged_host.with_extension("json"))?;
    }
    let mut permissions = fs::metadata(&staged_host)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&staged_host, permissions)?;
    let entrypoint_path = fs::canonicalize(&staged_host)?;
    let entrypoint_sha256 = sha256_file(&entrypoint_path)?;
    Ok(RegisteredExtension {
        id: String::from(id),
        version: String::from("0.1.0"),
        package_root: package,
        runtime: ExtensionRuntime::StdioJsonl,
        capabilities: Vec::new(),
        emits: Vec::new(),
        produces: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
        runtime_trust: Some(RegisteredRuntimeTrust {
            entrypoint_path,
            entrypoint_sha256,
            registered_at_unix_ms: 0,
        }),
    })
}

fn write_marker_stdio_host(dir: &Path, name: &str, marker: &str) -> Result<PathBuf, SpindleError> {
    let registration =
        ExtensionRegistration::new().action("test.render", RegistrationAction::new());
    let config = TestHostConfig::with_marker_invoke(registration, marker);
    crate::store::tests_support::install_test_host(dir, name, &config)
}

fn write_marker_shell_host(dir: &Path, name: &str, marker: &str) -> Result<PathBuf, SpindleError> {
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

fn write_stdio_package(dir: &Path, id: &str, host: &Path) -> Result<PathBuf, SpindleError> {
    let package = dir.join(id);
    fs::create_dir_all(package.join("bin"))?;
    let staged_host = package.join("bin").join(id);
    fs::copy(host, &staged_host)?;
    let host_config = host.with_extension("json");
    if host_config.is_file() {
        fs::copy(&host_config, staged_host.with_extension("json"))?;
    }
    let mut permissions = fs::metadata(&staged_host)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&staged_host, permissions)?;
    let manifest = ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    };
    fs::write(
        package.join("extension.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(package)
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
