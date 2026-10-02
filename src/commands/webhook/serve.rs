//! `linear webhook serve` — the durable intake service.
//!
//! Reads the service sections (`[bridge]`, `[platform.*]`, `[[mapping]]`) out of
//! the same `linear.toml` the rest of the CLI uses, so an operator configures one
//! file and one precedence chain. Everything substantive lives in
//! `linear-bridge`; this module is wiring plus the two things only the CLI can
//! know: where the config file is, and how to print an error a human can act on.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Args;
use linear_bridge::config::BridgeConfig;
use linear_bridge::http::intake::Intake;
use linear_bridge::http::{Bridge, HandlerFactory, ServeDeps, StoreFactory};
use linear_bridge::logging;
use linear_bridge::queue::{Handler, LoggingHandler};
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::Store;
use serde_json::json;

use crate::config;
use crate::errors::{CliError, Result};
use crate::output;

/// Default file mode for the service's database: owner read/write only. The
/// delivery log holds issue and comment bodies, which are not secrets but are
/// certainly not world-readable either.
const DATABASE_MODE_HINT: &str = "0o600";

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Address to bind, overriding `[bridge] bind`
    #[arg(long, value_name = "host:port")]
    pub bind: Option<String>,

    /// Config file to read, instead of the project/global `linear.toml` lookup
    #[arg(long, value_name = "path")]
    pub config: Option<PathBuf>,

    /// Resolve and print the configuration, then exit without binding
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: ServeArgs) -> Result<()> {
    logging::init_default();

    let (path, text) = config_source(args.config.as_deref())?;
    let mut service = BridgeConfig::from_toml(&text)
        .map_err(|error| CliError::validation(format!("{}: {error}", path.display())))?;

    if let Some(bind) = &args.bind {
        let addr: SocketAddr = bind.parse().map_err(|error| {
            CliError::validation(format!("`--bind {bind}` is not host:port ({error})"))
        })?;
        service = service.with_bind(addr);
    }

    if args.check {
        return print_resolved(&path, &service);
    }

    let sources = service
        .sources()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let intake = Arc::new(
        Intake::new(sources, service.body_limit)
            .map_err(|error| CliError::validation(error.to_string()))?,
    );

    let store_path = service.store_path.clone();
    let store: StoreFactory = {
        let store_path = store_path.clone();
        Arc::new(move || -> linear_bridge::Result<Box<dyn Store>> {
            Ok(Box::new(SqliteStore::open(&store_path)?))
        })
    };
    let handler: HandlerFactory = Arc::new(|| Box::new(LoggingHandler) as Box<dyn Handler>);

    let deps = ServeDeps {
        addr: service.bind,
        intake: intake.clone(),
        store,
        handler,
        http_threads: service.http_threads,
        worker_threads: service.worker_threads,
        worker: service.worker,
    };
    let bridge = Bridge::bind(deps).map_err(|error| CliError::cli(error.to_string()))?;
    let addr = bridge
        .local_addr()
        .map_err(|error| CliError::cli(error.to_string()))?;

    output::line(&format!(
        "Accepting webhooks on http://{addr} (config {}, database {store_path}, {DATABASE_MODE_HINT})",
        path.display()
    ));
    for (name, capabilities) in intake.connectors() {
        output::line(&format!(
            "  POST http://{addr}/webhooks/{name}   supports: {}",
            capabilities.join(", ")
        ));
    }
    output::line("Press Ctrl-C to stop.");

    // The process is supervised: SIGINT/SIGTERM terminate it, and the queue is
    // durable, so a hard stop loses nothing that was already acknowledged. A
    // signal handler that set this flag would only make shutdown tidier.
    let shutdown = Arc::new(AtomicBool::new(false));
    bridge
        .run(shutdown)
        .map_err(|error| CliError::cli(format!("The webhook service stopped: {error}")))
}

/// Where the service config comes from: an explicit `--config`, else the same
/// project-over-global lookup the CLI uses for everything else.
fn config_source(explicit: Option<&std::path::Path>) -> Result<(PathBuf, String)> {
    if let Some(path) = explicit {
        let text = std::fs::read_to_string(path).map_err(|error| {
            CliError::validation(format!("cannot read {}: {error}", path.display()))
        })?;
        return Ok((path.to_path_buf(), text));
    }
    config::service_config_text().ok_or_else(|| {
        CliError::validation(
            "no linear.toml with a [bridge] section was found; pass --config <path>".to_string(),
        )
    })
}

/// `--check`: print what the service would do, with secrets redacted (the
/// `Secret` type's `Debug` impl makes that automatic).
fn print_resolved(path: &std::path::Path, service: &BridgeConfig) -> Result<()> {
    let platforms: Vec<_> = service
        .platforms
        .iter()
        .map(|platform| {
            json!({
                "name": platform.name.as_str(),
                "type": platform.declared_type,
                "secret": format!("{:?}", platform.secret),
                "spec": platform.spec.describe(),
                "capabilities": linear_bridge::domain::Capabilities::from(platform.spec.capabilities).describe(),
            })
        })
        .collect();
    let mappings: Vec<_> = service
        .mappings
        .iter()
        .map(|mapping| {
            json!({
                "name": mapping.name,
                "source": mapping.source,
                "sink": mapping.sink,
                "sync_issues": mapping.sync_issues,
                "git_automation": mapping.git_automation,
                "delete_sync": mapping.delete_sync,
            })
        })
        .collect();

    output::print_json(&json!({
        "config": path.display().to_string(),
        "bind": service.bind.to_string(),
        "database": service.store_path,
        "body_limit": service.body_limit,
        "http_threads": service.http_threads,
        "worker_threads": service.worker_threads,
        "worker": {
            "max_attempts": service.worker.max_attempts,
            "backoff_base_ms": service.worker.backoff_base.as_millis() as u64,
            "backoff_max_ms": service.worker.backoff_max.as_millis() as u64,
            "poll_interval_ms": service.worker.poll_interval.as_millis() as u64,
            "lease_secs": service.worker.lease.as_secs(),
        },
        "platforms": platforms,
        "mappings": mappings,
        "endpoints": service.platforms.iter()
            .map(|platform| format!("POST http://{}/webhooks/{}", service.bind, platform.name))
            .collect::<Vec<_>>(),
    }));
    Ok(())
}
