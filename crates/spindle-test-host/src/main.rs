#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::{env, process};

use spindle_test_host::{load_config, run_stdio_host};

fn main() {
    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("spindle-test-host: failed to resolve executable path: {error}");
            process::exit(1);
        }
    };
    let config = match load_config(&executable) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("spindle-test-host: {error}");
            process::exit(1);
        }
    };
    if let Err(error) = run_stdio_host(&config, &executable) {
        eprintln!("spindle-test-host: {error}");
        process::exit(1);
    }
}
