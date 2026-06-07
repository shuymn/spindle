//! Config-driven stdio JSONL host used by spindle integration tests.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

mod config;
mod run;

pub use config::{
    FailResponse, InvokeEffect, InvokeRule, RegisterResponse, ResponseTemplate, ShutdownResponse,
    StartupAction, TestHostConfig,
};
pub use run::{config_path_for_executable, load_config, run_stdio_host};
