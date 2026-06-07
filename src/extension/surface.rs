use std::collections::BTreeSet;

use super::RegisteredExtension;
use crate::SpindleError;

pub(super) fn ensure_surface_ownership(
    candidate: &RegisteredExtension,
    existing: &[RegisteredExtension],
) -> Result<(), SpindleError> {
    let candidate_events = event_surface(candidate);
    for extension in existing {
        for action in candidate.actions.keys() {
            if extension.actions.contains_key(action) {
                return Err(surface_conflict("action", action, extension, candidate));
            }
        }
        let existing_events = event_surface(extension);
        for event in &candidate_events {
            if existing_events.contains(event) {
                return Err(surface_conflict("event", event, extension, candidate));
            }
        }
        for capability in &candidate.capabilities {
            if extension
                .capabilities
                .iter()
                .any(|owned| owned == capability)
            {
                return Err(surface_conflict(
                    "capability",
                    capability,
                    extension,
                    candidate,
                ));
            }
        }
    }
    Ok(())
}

fn event_surface(extension: &RegisteredExtension) -> BTreeSet<&str> {
    extension
        .emits
        .iter()
        .chain(extension.produces.iter())
        .map(String::as_str)
        .collect()
}

fn surface_conflict(
    surface: &'static str,
    name: &str,
    existing: &RegisteredExtension,
    candidate: &RegisteredExtension,
) -> SpindleError {
    SpindleError::SurfaceConflict {
        surface,
        name: String::from(name),
        existing_extension: existing.id.clone(),
        new_extension: candidate.id.clone(),
    }
}
