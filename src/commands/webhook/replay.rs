//! `linear webhook replay <id>`: run a stored delivery again.
//!
//! The store keeps every delivery's raw body, and says why in its own schema comment: "so a
//! handler can be re-run against exactly what the provider sent, which is the only way to
//! diagnose a sync bug after deploying a fix". Nothing ran it, so the capability was a comment.
//!
//! This is that, and it is the *same* engine: the delivery is handed to the handler the service
//! builds, which re-parses the stored body rather than trusting the row's summary of it. What
//! this decides is only what to record afterwards - and it records exactly what the worker
//! would, so `sync status` stays truthful about a delivery that was re-run.

use std::path::PathBuf;

use clap::Args;
use linear_bridge::config::BridgeConfig;
use linear_bridge::queue::Handler;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::Store;
use serde_json::json;

use crate::commands::webhook::serve::{config_source, reconcile_factory};
use crate::errors::{CliError, Result};
use crate::output;

#[derive(Args, Debug)]
pub struct ReplayArgs {
    /// The id `linear sync status` prints for the delivery.
    pub id: i64,
    /// The configuration to replay against (the same file `webhook serve` reads).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: ReplayArgs) -> Result<()> {
    linear_bridge::logging::init_default();

    let (path, text) = config_source(args.config.as_deref())?;
    let config = BridgeConfig::from_toml(&text)
        .map_err(|error| CliError::validation(format!("{}: {error}", path.display())))?;

    let mut store = SqliteStore::open(&config.store_path)
        .map_err(|error| CliError::cli(format!("The store could not be opened: {error}")))?;
    let delivery = store
        .find_delivery(args.id)
        .map_err(|error| CliError::cli(format!("The delivery cannot be read: {error}")))?
        .ok_or_else(|| {
            // Naming the store matters here: an id from a log line names a delivery in *this*
            // database, and an operator pointing at another one should hear that.
            CliError::validation(format!(
                "{} holds no delivery #{}",
                config.store_path, args.id
            ))
        })?;

    // The same platforms and mappings the service would use, and the same builder: a replay
    // that took a different path to the engine could reach a different conclusion, which would
    // make it useless for the one thing it is for.
    let mappings = config
        .reconcile_mappings()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let sources = config.sources();
    let sinks = config.sinks();
    let mut handler = reconcile_factory(&sources, &sinks, &mappings, &config.store_path)
        .map_err(|error| CliError::validation(format!("the reconciler cannot start: {error}")))?;

    // Recorded the way the worker records it, so what an operator sees afterwards matches what
    // happened: a delivery that works is done, and one that fails stays dead with the *new*
    // reason rather than the reason it failed the first time.
    match handler.handle(&delivery) {
        Ok(()) => {
            store.complete(delivery.id).map_err(|error| {
                CliError::cli(format!("The delivery could not be filed: {error}"))
            })?;
            if args.json {
                output::print_json(&json!({
                    "config": path.display().to_string(),
                    "id": delivery.id,
                    "delivery": delivery.delivery_id,
                    "connector": delivery.connector.as_str(),
                    "event": delivery.event,
                    "replayed": true,
                }));
                return Ok(());
            }
            output::line(&format!(
                "replayed #{} ({} {}) - it no longer stops the queue",
                delivery.id,
                delivery.connector.as_str(),
                delivery.event
            ));
            Ok(())
        }
        Err(error) => {
            let reason = error.to_string();
            store.fail(delivery.id, &reason, None).map_err(|error| {
                CliError::cli(format!("The failure could not be recorded: {error}"))
            })?;
            // Loud: a replay that fails is the answer the operator was looking for, and a
            // command that swallowed it would leave the delivery looking like it was handled.
            Err(CliError::cli(format!(
                "replaying #{} failed: {reason}",
                delivery.id
            )))
        }
    }
}
