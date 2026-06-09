mod manifest;
mod registry;
mod stage;
mod surface;

pub use manifest::{ExtensionAction, ExtensionManifest, ExtensionRoute, ExtensionRuntime};
pub use registry::{ExtensionRegistry, RegisteredExtension, RegisteredRuntimeTrust, sha256_file};
pub use stage::{MANIFEST_FILE, StagedPackage, materialize_package, resolve_source_package};

#[cfg(test)]
mod tests;
