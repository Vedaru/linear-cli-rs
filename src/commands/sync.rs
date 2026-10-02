//! `linear sync`: bring a mapping's two ends into agreement, now, once.
//!
//! The webhook service decides *when* the engine runs; this decides it on demand, from
//! a terminal, using the same config, the same engine and the same store. That is the
//! whole difference - a sweep and a delivery cannot disagree about what to write,
//! because they run the same decision code and compare against the same records.
//!
//! Dry run by default. A sweep can *create*, and the plan it prints is the plan that
//! would run, so looking first costs one command and never a surprise.

use std::path::PathBuf;

use clap::Args;
use linear_bridge::config::BridgeConfig;
use linear_bridge::reconcile::handler::ReconcileHandler;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::Store;
use serde_json::json;

use crate::commands::webhook::serve::config_source;
use crate::errors::{CliError, Result};
use crate::output;

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// The configuration to sync (the same file `webhook serve` reads).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Only this mapping, by name.
    #[arg(long)]
    pub mapping: Option<String>,
    /// Write what the plan says. Without this nothing is written.
    #[arg(long)]
    pub apply: bool,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: SyncArgs) -> Result<()> {
    linear_bridge::logging::init_default();

    let (path, text) = config_source(args.config.as_deref())?;
    let config = BridgeConfig::from_toml(&text)
        .map_err(|error| CliError::validation(format!("{}: {error}", path.display())))?;

    let mappings = config
        .reconcile_mappings()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let selected: Vec<(usize, String)> = mappings
        .iter()
        .enumerate()
        .filter(|(_, mapping)| {
            // `map_or`, not `is_none_or`: this crate's floor is Rust 1.80.
            args.mapping
                .as_deref()
                .map_or(true, |wanted| mapping.name == wanted)
        })
        .map(|(index, mapping)| (index, mapping.name.clone()))
        .collect();

    if selected.is_empty() {
        // An empty sweep is never what someone meant: either the config has no mapping
        // to sync, or the name is wrong. Saying which beats printing "nothing to do".
        return Err(CliError::validation(match &args.mapping {
            Some(name) => format!("{} has no mapping named `{name}`", path.display()),
            None => format!("{} has no [[mapping]] to sync", path.display()),
        }));
    }

    // The store is what makes a sweep idempotent: it holds the revision last written
    // across each pair, which is both the loop guard and how a sweep tells which side
    // moved. Same file the service uses, so the two can be mixed freely.
    let store: Box<dyn Store> = Box::new(
        SqliteStore::open(&config.store_path)
            .map_err(|error| CliError::cli(format!("The store could not be opened: {error}")))?,
    );
    let mut handler = ReconcileHandler::new(
        // Not `receiving_sources`: a sweep translates, it never verifies a delivery,
        // so it has no business demanding a webhook secret.
        config.sources(),
        config.sinks(),
        mappings,
        store,
    )
    .map_err(|error| CliError::validation(error.to_string()))?;

    let mut planned = 0;
    let mut written = 0;
    let mut surveys = Vec::new();
    for (index, name) in &selected {
        let survey = handler
            .survey(*index)
            .map_err(|error| CliError::cli(format!("Reading `{name}` failed: {error}")))?;
        planned += survey.writes();

        if args.json {
            surveys.push(json!({
                "mapping": survey.mapping,
                "source": survey.source,
                "sink": survey.sink,
                "writes": survey.writes(),
                "applied": args.apply && !survey.is_quiet(),
                "entries": survey.entries.iter().map(|entry| json!({
                    "subject": format!("{}:{}", entry.subject.connector, entry.subject.native_id),
                    "counterpart": entry.counterpart.as_ref()
                        .map(|other| format!("{}:{}", other.connector, other.native_id)),
                    "action": entry.action.describe(),
                })).collect::<Vec<_>>(),
            }));
        } else {
            for line in survey.report() {
                output::line(&line);
            }
        }

        if args.apply && !survey.is_quiet() {
            written += handler
                .apply_survey(*index, &survey)
                .map_err(|error| CliError::cli(format!("Writing `{name}` failed: {error}")))?;
        }
    }

    if args.json {
        output::print_json(&json!({
            "config": path.display().to_string(),
            "dry_run": !args.apply,
            "planned": planned,
            "written": written,
            "mappings": surveys,
        }));
        return Ok(());
    }

    if args.apply {
        output::line(&format!(
            "{written} change(s) written; {} mapping(s) synced",
            selected.len()
        ));
    } else if planned > 0 {
        output::line(&format!(
            "{planned} change(s) would be made - nothing was written (add --apply to write)"
        ));
    } else {
        output::line("Nothing to do: both ends already agree.");
    }
    Ok(())
}
