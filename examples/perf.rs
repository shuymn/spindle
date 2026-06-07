use std::{
    collections::BTreeMap,
    env, fs,
    hint::black_box,
    path::{Path, PathBuf},
    process,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::json;
use spindle::{
    Dispatcher, Event, EventFilter, EventLog, ExtensionAction, ExtensionRegistry, ExtensionRoute,
    ExtensionRuntime, ExtensionRuntimeHost, RegisteredExtension,
};

const EVENT_BUILD_ITERS: usize = 100_000;
const APPEND_ITERS: usize = 200;
const READ_ITERS: usize = 100;
const CONCURRENT_THREADS: usize = 4;
const CONCURRENT_APPENDS_PER_THREAD: usize = 50;
const REGISTRY_INSTALLS: usize = 50;
const REGISTRY_LIST_ITERS: usize = 500;
const STDIO_DISPATCH_ITERS: usize = 1_000;
const WORKSPACE_INDICATOR_BIN_ENV: &str = "SPINDLE_WORKSPACE_INDICATOR_BIN";

fn main() -> Result<()> {
    let root = temp_dir("spindle-perf")?;
    println!("state_dir={}", root.display());
    println!(
        "{:<34} {:>12} {:>14} {:>14}",
        "benchmark", "iterations", "elapsed_ms", "avg_us"
    );

    measure_event_build()?;
    measure_event_log_append(&root)?;
    measure_event_log_read(&root)?;
    measure_event_log_concurrent_append(&root)?;
    measure_registry_install_and_list(&root)?;
    measure_stdio_dispatch(&root)?;

    fs::remove_dir_all(&root).context("remove benchmark state dir")?;
    Ok(())
}

fn measure_event_build() -> Result<()> {
    let elapsed = measure(|| {
        for index in 0..EVENT_BUILD_ITERS {
            let event = Event::builder(String::from("bench.changed"), String::from("perf"))
                .subject(Some(index.to_string()))
                .build()?;
            black_box(event.id.len());
        }
        Ok(())
    })?;
    print_measurement("event build", EVENT_BUILD_ITERS, elapsed);
    Ok(())
}

fn measure_event_log_append(root: &Path) -> Result<()> {
    let dir = root.join("append");
    let log = EventLog::in_dir(&dir);
    let events = build_events(APPEND_ITERS)?;

    let elapsed = measure(|| {
        for event in &events {
            log.append(event)?;
        }
        Ok(())
    })?;

    print_measurement("event log append fsync", APPEND_ITERS, elapsed);
    Ok(())
}

fn measure_event_log_read(root: &Path) -> Result<()> {
    let dir = root.join("read");
    let log = EventLog::in_dir(&dir);
    for event in build_events(APPEND_ITERS)? {
        log.append(&event)?;
    }

    let elapsed = measure(|| {
        for _ in 0..READ_ITERS {
            let events = log.read(&EventFilter::default())?;
            black_box(events.len());
        }
        Ok(())
    })?;

    print_measurement("event log read 200 events", READ_ITERS, elapsed);
    Ok(())
}

fn measure_event_log_concurrent_append(root: &Path) -> Result<()> {
    let dir = root.join("concurrent-append");
    let log = Arc::new(EventLog::in_dir(&dir));
    let barrier = Arc::new(Barrier::new(CONCURRENT_THREADS + 1));
    let mut handles = Vec::with_capacity(CONCURRENT_THREADS);

    for thread_index in 0..CONCURRENT_THREADS {
        let log = Arc::clone(&log);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> Result<()> {
            let events = build_thread_events(thread_index, CONCURRENT_APPENDS_PER_THREAD)?;
            barrier.wait();
            for event in events {
                log.append(&event)?;
            }
            Ok(())
        }));
    }

    let started = Instant::now();
    barrier.wait();
    for handle in handles {
        handle
            .join()
            .map_err(|_payload| anyhow::anyhow!("benchmark thread panicked"))??;
    }
    let elapsed = started.elapsed();

    print_measurement(
        "event log append concurrent",
        CONCURRENT_THREADS * CONCURRENT_APPENDS_PER_THREAD,
        elapsed,
    );
    Ok(())
}

fn measure_registry_install_and_list(root: &Path) -> Result<()> {
    let dir = root.join("registry");
    fs::create_dir_all(&dir).context("create registry benchmark dir")?;
    let registry = ExtensionRegistry::in_dir(&dir);
    let manifest_dir = dir.join("manifests");
    fs::create_dir_all(&manifest_dir).context("create manifest benchmark dir")?;
    let manifests = write_recipe_manifests(&manifest_dir, REGISTRY_INSTALLS)?;

    let install_elapsed = measure(|| {
        for manifest in &manifests {
            registry.register_manifest(manifest)?;
        }
        Ok(())
    })?;
    print_measurement(
        "registry install recipe",
        REGISTRY_INSTALLS,
        install_elapsed,
    );

    let list_elapsed = measure(|| {
        for _ in 0..REGISTRY_LIST_ITERS {
            let entries = registry.list()?;
            black_box(entries.len());
        }
        Ok(())
    })?;
    print_measurement(
        "registry list 50 entries",
        REGISTRY_LIST_ITERS,
        list_elapsed,
    );
    Ok(())
}

fn measure_stdio_dispatch(root: &Path) -> Result<()> {
    let dir = root.join("stdio-dispatch");
    fs::create_dir_all(&dir).context("create dispatch benchmark dir")?;
    let registry = ExtensionRegistry::in_dir(&dir);
    write_dispatch_registry(&registry, &dir)?;
    let runtime = ExtensionRuntimeHost::new();
    let args = json!({
        "active": "1",
        "occupied": ["1", "3", "8"],
        "workspaces": "1,2,3,4,5,6,7,8,9,10"
    });

    let cold_elapsed = measure(|| {
        let dispatcher = Dispatcher::new(&registry, &runtime);
        let reports =
            dispatcher.dispatch_action("workspace-indicator.workspaces.render", &args, &[])?;
        black_box(reports.len());
        Ok(())
    })?;
    print_measurement("stdio dispatch cold", 1, cold_elapsed);

    let hot_elapsed = measure(|| {
        for _ in 0..STDIO_DISPATCH_ITERS {
            let dispatcher = Dispatcher::new(&registry, &runtime);
            let reports =
                dispatcher.dispatch_action("workspace-indicator.workspaces.render", &args, &[])?;
            black_box(reports.len());
        }
        Ok(())
    })?;
    print_measurement("stdio dispatch hot", STDIO_DISPATCH_ITERS, hot_elapsed);
    runtime.shutdown()?;
    Ok(())
}

fn build_events(count: usize) -> Result<Vec<Event>> {
    (0..count)
        .map(|index| {
            Event::builder(String::from("bench.changed"), String::from("perf"))
                .subject(Some(index.to_string()))
                .build()
                .context("build benchmark event")
        })
        .collect()
}

fn build_thread_events(thread_index: usize, count: usize) -> Result<Vec<Event>> {
    (0..count)
        .map(|index| {
            Event::builder(String::from("bench.changed"), String::from("perf"))
                .subject(Some(format!("{thread_index}-{index}")))
                .build()
                .context("build threaded benchmark event")
        })
        .collect()
}

fn write_recipe_manifests(dir: &Path, count: usize) -> Result<Vec<PathBuf>> {
    (0..count)
        .map(|index| {
            let path = dir.join(format!("recipe-{index}.json"));
            fs::write(
                &path,
                format!(
                    r#"{{
  "id": "recipe-{index}",
  "version": "0.1.0",
  "runtime": "recipe",
  "emits": ["bench.event.{index}"],
  "capabilities": ["bench.capability.{index}"],
  "routes": [
    {{
      "event": "bench.trigger.{index}",
      "action": "bench.action.{index}"
    }}
  ]
}}"#
                ),
            )
            .with_context(|| format!("write {}", path.display()))?;
            Ok(path)
        })
        .collect()
}

fn write_dispatch_registry(registry: &ExtensionRegistry, dir: &Path) -> Result<()> {
    let workspace_entrypoint = workspace_indicator_entrypoint()?;
    let entries = vec![
        RegisteredExtension {
            id: String::from("sketchybar"),
            version: String::from("0.1.0"),
            manifest_path: dir.join("sketchybar.json"),
            runtime: ExtensionRuntime::StdioJsonl,
            entrypoint: Some(String::from("/usr/bin/true")),
            capabilities: vec![String::from("sketchybar.ui.write")],
            emits: Vec::new(),
            produces: Vec::new(),
            actions: [(
                String::from("sketchybar.message.send"),
                ExtensionAction {
                    capabilities: vec![String::from("sketchybar.ui.write")],
                },
            )]
            .into(),
            routes: Vec::new(),
            runtime_trust: None,
        },
        RegisteredExtension {
            id: String::from("workspace-indicator"),
            version: String::from("0.1.0"),
            manifest_path: dir.join("workspace-indicator.json"),
            runtime: ExtensionRuntime::StdioJsonl,
            entrypoint: Some(workspace_entrypoint.to_string_lossy().into_owned()),
            capabilities: Vec::new(),
            emits: Vec::new(),
            produces: vec![String::from(
                "workspace-indicator.sketchybar.message.requested",
            )],
            actions: BTreeMap::from([
                (
                    String::from("workspace-indicator.status.render"),
                    ExtensionAction {
                        capabilities: Vec::new(),
                    },
                ),
                (
                    String::from("workspace-indicator.workspaces.render"),
                    ExtensionAction {
                        capabilities: Vec::new(),
                    },
                ),
            ]),
            routes: Vec::<ExtensionRoute>::new(),
            runtime_trust: None,
        },
    ];
    fs::write(registry.path(), serde_json::to_vec_pretty(&entries)?)
        .context("write dispatch benchmark registry")?;
    Ok(())
}

fn workspace_indicator_entrypoint() -> Result<PathBuf> {
    let path = env::var_os(WORKSPACE_INDICATOR_BIN_ENV).with_context(|| {
        format!(
            "{WORKSPACE_INDICATOR_BIN_ENV} is required for stdio dispatch benchmark. Build spindle-workspace-indicator first and pass its path via {WORKSPACE_INDICATOR_BIN_ENV}."
        )
    })?;
    Ok(PathBuf::from(path))
}

fn measure(run: impl FnOnce() -> Result<()>) -> Result<Duration> {
    let started = Instant::now();
    run()?;
    Ok(started.elapsed())
}

fn print_measurement(name: &str, iterations: usize, elapsed: Duration) {
    let elapsed_us = elapsed.as_micros();
    let avg_us = if iterations == 0 {
        0
    } else {
        elapsed_us / u128::try_from(iterations).unwrap_or(u128::MAX)
    };
    println!(
        "{name:<34} {iterations:>12} {:>14} {avg_us:>14}",
        elapsed.as_millis()
    );
}

fn temp_dir(prefix: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before unix epoch")?
        .as_nanos();
    let path = env::temp_dir().join(format!("{prefix}-{}-{nanos}", process::id()));
    fs::create_dir_all(&path).context("create benchmark state dir")?;
    Ok(path)
}
