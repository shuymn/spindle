#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]
// Cargo-level lint only: current transitive graph contains duplicate versions
// from upstream crates and uuid/getrandom target support outside this crate's control.
#![allow(clippy::multiple_crate_versions)]

fn main() -> anyhow::Result<()> {
    spindle::cli::run()
}
