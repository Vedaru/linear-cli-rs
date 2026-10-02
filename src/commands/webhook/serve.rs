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
use linear_bridge::connector::Source;
use linear_bridge::http::intake::Intake;
use linear_bridge::http::{Bridge, HandlerFactory, ServeDeps, StoreFactory};
use linear_bridge::logging;
use linear_bridge::queue::{FailingHandler, Handler, LoggingHandler};
use linear_bridge::reconcile::handler::{Mapping, ReconcileHandler};
use linear_bridge::reconcile::Direction;
use linear_bridge::sink::Sink;
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

    // Shared, not moved: intake parses the request, and a worker re-parses the
    // stored body (one delivery can carry several events), so both need the same
    // connectors.
    let sources: Arc<Vec<Arc<dyn Source>>> = Arc::new(
        service
            .receiving_sources()
            .map_err(|error| CliError::validation(error.to_string()))?,
    );
    let intake = Arc::new(
        Intake::new(sources.as_ref().clone(), service.body_limit)
            .map_err(|error| CliError::validation(error.to_string()))?,
    );

    // The sync half, when the config has mappings. A deployment with none still
    // accepts and logs deliveries - the service is useful as an intake alone.
    let mappings: Arc<Vec<Mapping>> = Arc::new(
        service
            .reconcile_mappings()
            .map_err(|error| CliError::validation(error.to_string()))?,
    );
    let sinks = Arc::new(service.sinks());

    let store_path = service.store_path.clone();
    let store: StoreFactory = {
        let store_path = store_path.clone();
        Arc::new(move || -> linear_bridge::Result<Box<dyn Store>> {
            Ok(Box::new(SqliteStore::open(&store_path)?))
        })
    };

    let handler: HandlerFactory = if mappings.is_empty() {
        Arc::new(|| Box::new(LoggingHandler) as Box<dyn Handler>)
    } else {
        // Prove the reconciler can be built *before* binding: an operator should
        // hear about a mapping that cannot run at startup, not on the first
        // webhook, and not after the port is already accepting traffic.
        reconcile_factory(&sources, &sinks, &mappings, &store_path).map_err(|error| {
            CliError::validation(format!("the reconciler cannot start: {error}"))
        })?;

        let sources = Arc::clone(&sources);
        let mappings = Arc::clone(&mappings);
        let sinks = Arc::clone(&sinks);
        let store_path = store_path.clone();
        Arc::new(move || -> Box<dyn Handler> {
            match reconcile_factory(&sources, &sinks, &mappings, &store_path) {
                Ok(handler) => handler,
                // Unreachable in practice, since the same construction succeeded
                // above. It stays because the honest behaviour has to be here rather
                // than assumed: a worker that cannot reconcile must not acknowledge
                // deliveries it did not sync. Deliveries it refuses are retried and
                // then parked as dead, where the queue's own logging shows them.
                Err(error) => Box::new(FailingHandler::new(error)),
            }
        })
    };

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
    if mappings.is_empty() {
        output::line("No [[mapping]] is configured: deliveries are logged, not mirrored.");
    } else {
        for mapping in mappings.iter() {
            output::line(&format!(
                "  syncing {} <-> {} ({})",
                mapping.source.describe(),
                mapping.sink.describe(),
                describe_direction(mapping.policy.direction),
            ));
        }
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

/// Build one reconciler, with its own store connection.
///
/// One per worker thread, and each with its own connection: the queue owns the
/// connection it claims deliveries on, and a handler that shared it would couple
/// its transactions to the queue's.
pub(crate) fn reconcile_factory(
    sources: &[Arc<dyn Source>],
    sinks: &[Arc<dyn Sink>],
    mappings: &[Mapping],
    store_path: &str,
) -> std::result::Result<Box<dyn Handler>, String> {
    let store: Box<dyn Store> =
        Box::new(SqliteStore::open(store_path).map_err(|error| error.to_string())?);
    let handler = ReconcileHandler::new(sources.to_vec(), sinks.to_vec(), mappings.to_vec(), store)
        .map_err(|error| error.to_string())?;
    Ok(Box::new(handler) as Box<dyn Handler>)
}

fn describe_direction(direction: Direction) -> &'static str {
    match direction {
        Direction::Both => "both ways",
        Direction::SourceToSink => "one way, source to sink",
        Direction::SinkToSource => "one way, sink to source",
    }
}

/// Where the service config comes from: an explicit `--config`, else the same
/// project-over-global lookup the CLI uses for everything else.
pub(crate) fn config_source(explicit: Option<&std::path::Path>) -> Result<(PathBuf, String)> {
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
    // Proof that this config could serve, not just that it parsed: a `--check` that
    // passes on a config the service then refuses is worse than no check at all. This
    // is where "you asked me to receive here but configured no webhook secret" belongs.
    service
        .receiving_sources()
        .map_err(|error| CliError::validation(error.to_string()))?;

    let platforms: Vec<_> = service
        .platforms
        .iter()
        .map(|platform| {
            json!({
                "name": platform.name.as_str(),
                "type": platform.declared_type,
                // Redacted by the `Secret` type, never printed. "Absent" is not a
                // redaction: it means this platform receives nothing, which is fine
                // for `sync` and the reason `serve` will refuse the file.
                "secret": platform
                    .secret
                    .as_ref()
                    .map(|_| "redacted".to_string())
                    .unwrap_or_else(|| "absent (CLI-only)".to_string()),
                "token": platform.token.as_ref().map(|_| "set").unwrap_or("none"),
                "api_url": platform.sink_spec().map(|spec| spec.base_url),
                "states": {
                    "closed": platform.states.closed,
                    "open": platform.states.open,
                    "initial": platform.states.initial,
                },
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
                "direction": describe_direction(mapping.direction),
                "sync_issues": mapping.sync_issues,
                "git_automation": mapping.git_automation,
                "delete_sync": mapping.delete_sync,
            })
        })
        .collect();

    // Resolving the mappings here is the point of `--check`: it proves the join
    // between the config and the reconciler works, without binding a port.
    let resolved: Vec<_> = service
        .reconcile_mappings()
        .map_err(|error| CliError::validation(error.to_string()))?
        .iter()
        .map(|mapping| {
            json!({
                "name": mapping.name,
                "source": mapping.source.describe(),
                "sink": mapping.sink.describe(),
                "direction": describe_direction(mapping.policy.direction),
                "states": {
                    "source_closed": mapping.policy.names.source.closed,
                    "sink_closed": mapping.policy.names.sink.closed,
                    "initial_on_sink": mapping.policy.names.sink.initial,
                },
                // How a person is known on each side, so `--check` answers "will the
                // assignee come across?" without a delivery.
                "identities": mapping.users.describe(),
            })
        })
        .collect();

    output::print_json(&json!({
        "config": path.display().to_string(),
        "writable": service.sinks().len(),
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
        "resolved_mappings": resolved,
        "endpoints": service.platforms.iter()
            .map(|platform| format!("POST http://{}/webhooks/{}", service.bind, platform.name))
            .collect::<Vec<_>>(),
    }));
    Ok(())
}
