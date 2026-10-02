//! `linear sync status`: what the store holds, before deciding anything.
//!
//! The first question when a sync goes quiet is whether the queue drained or filled. A service
//! that is up but backed up and a service that is doing nothing look identical from outside -
//! both answer a webhook with `202` and neither writes anything - so this is the command that
//! tells them apart.
//!
//! It prints the dead deliveries with their errors rather than only counting them, because a
//! count of failures that does not say what failed is a number, not a lead.

use std::path::PathBuf;

use clap::Args;
use linear_bridge::config::BridgeConfig;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::Store;
use serde_json::json;

use crate::commands::webhook::serve::config_source;
use crate::errors::{CliError, Result};
use crate::output;

/// How many dead deliveries to list. The rest are counted.
const DEAD_SHOWN: usize = 20;

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// The configuration to report on (the same file `webhook serve` reads).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: StatusArgs) -> Result<()> {
    let (path, text) = config_source(args.config.as_deref())?;
    let config = BridgeConfig::from_toml(&text)
        .map_err(|error| CliError::validation(format!("{}: {error}", path.display())))?;

    // Reading the mappings is half of what this reports: a config that cannot run is worth
    // failing on, rather than printing a healthy queue beside it and letting the operator
    // discover the real problem when the service refuses to start.
    let mappings = config
        .reconcile_mappings()
        .map_err(|error| CliError::validation(error.to_string()))?;

    // Opened rather than created-and-assumed: the same store `webhook serve` and `linear sync`
    // use, and a store that cannot be read is the answer to the question being asked.
    let mut store = SqliteStore::open(&config.store_path)
        .map_err(|error| CliError::cli(format!("The store could not be opened: {error}")))?;
    store
        .health()
        .map_err(|error| CliError::cli(format!("The store cannot be read: {error}")))?;

    let counts = store
        .counts()
        .map_err(|error| CliError::cli(format!("The store cannot be counted: {error}")))?;
    let dead = store
        .dead_deliveries(DEAD_SHOWN)
        .map_err(|error| CliError::cli(format!("The dead deliveries cannot be read: {error}")))?;

    if args.json {
        output::print_json(&json!({
            "config": path.display().to_string(),
            "store": config.store_path,
            "mappings": mappings.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
            "queue": {
                "pending": counts.pending,
                "active": counts.active,
                "done": counts.done,
                "dead": counts.dead,
            },
            "dead": dead.iter().map(|delivery| json!({
                "id": delivery.id,
                "connector": delivery.connector.as_str(),
                "event": delivery.event,
                "scope": delivery.scope,
                "native_id": delivery.native_id,
                "attempts": delivery.attempts,
                "error": delivery.last_error,
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    output::line(&format!("config:   {}", path.display()));
    output::line(&format!("store:    {}", config.store_path));
    output::line(&format!(
        "mappings: {}",
        mappings
            .iter()
            .map(|mapping| mapping.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    output::line(&format!(
        "queue:    {} pending, {} active, {} done, {} dead",
        counts.pending, counts.active, counts.done, counts.dead
    ));

    if !dead.is_empty() {
        output::line("");
        output::line(&format!("dead (showing {DEAD_SHOWN} at most):"));
        for delivery in &dead {
            // The scope is part of the address: a forge issue number means nothing without the
            // repository it is a number in.
            let at = delivery
                .scope
                .as_deref()
                .map_or_else(String::new, |scope| format!("{scope}#"));
            output::line(&format!(
                "  #{} {} {} {}{}  ({} attempt(s)): {}",
                delivery.id,
                delivery.connector.as_str(),
                delivery.event,
                at,
                delivery.native_id,
                delivery.attempts,
                delivery
                    .last_error
                    .as_deref()
                    .unwrap_or("no error recorded"),
            ));
        }
        output::line("");
        output::line("Re-run one with `linear webhook replay <id>` once the cause is fixed.");
    }

    Ok(())
}
