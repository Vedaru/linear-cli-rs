//! `linear sync link`: say that two entities are the same, by hand.
//!
//! The engine pairs things by itself, but only when it has something to go on: a marker it wrote
//! into the body it created, or a link it recorded when it wrote across one. Two entities that
//! already exist, on two platforms, that nobody created from the other have neither - and until
//! something says they are the same, the engine can only ever treat them as strangers and make a
//! second copy.
//!
//! This is that statement, and it is deliberately only that: the link is recorded, and what the
//! engine makes of it is the sweep's business. That is why this command does not fetch either
//! side - `linear sync` does, and its dry run says what it found. A command that checked here
//! would check differently, and the sweep would still be the thing that decides.

use std::path::PathBuf;

use clap::Args;
use linear_bridge::config::BridgeConfig;
use linear_bridge::domain::parse_entity_address;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{Link, Store};
use serde_json::json;

use crate::commands::webhook::serve::config_source;
use crate::errors::{CliError, Result};
use crate::output;

#[derive(Args, Debug)]
pub struct LinkArgs {
    /// One entity, as `connector:scope#id` - e.g. `linear:VED#a-uuid`.
    pub left: String,
    /// The other, in the same form.
    pub right: String,
    /// The configuration to link in (the same file `webhook serve` reads).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: LinkArgs) -> Result<()> {
    let (path, text) = config_source(args.config.as_deref())?;
    let config = BridgeConfig::from_toml(&text)
        .map_err(|error| CliError::validation(format!("{}: {error}", path.display())))?;

    let left = parse_entity_address(&args.left).map_err(CliError::validation)?;
    let right = parse_entity_address(&args.right).map_err(CliError::validation)?;
    if left.same_entity(&right) {
        return Err(CliError::validation(format!(
            "{} is one entity, not a pair to link",
            left.describe()
        )));
    }

    // A pairing is only a pairing if a mapping would look at it: the sweep walks mappings, so a
    // link between two platforms none of them connects would sit there unread - which is worse
    // than refusing, because it would look like it had done something.
    let mappings = config
        .reconcile_mappings()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let mapping = mappings
        .iter()
        .find(|mapping| {
            let (source, sink) = (&mapping.source.connector, &mapping.sink.connector);
            (left.connector == *source && right.connector == *sink)
                || (left.connector == *sink && right.connector == *source)
        })
        .ok_or_else(|| {
            CliError::validation(format!(
                "no mapping connects `{}` and `{}`; this config has: {}",
                left.connector.as_str(),
                right.connector.as_str(),
                mappings
                    .iter()
                    .map(|mapping| format!(
                        "{} ({} -> {})",
                        mapping.name,
                        mapping.source.connector.as_str(),
                        mapping.sink.connector.as_str()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

    // No hash, on purpose: a link without a recorded revision is one the sweep treats as
    // *adopted*, which means the mapping's source wins where the two differ - "because that is
    // what source means". Recording a hash here would claim this command had written something.
    let link = Link::new(left.clone(), right.clone());
    let mut store = SqliteStore::open(&config.store_path)
        .map_err(|error| CliError::cli(format!("The store could not be opened: {error}")))?;
    store
        .upsert_link(&link)
        .map_err(|error| CliError::cli(format!("The pairing could not be recorded: {error}")))?;

    let source_side = &mapping.source.connector;
    let kept = if left.connector == *source_side {
        &left
    } else {
        &right
    };

    if args.json {
        output::print_json(&json!({
            "config": path.display().to_string(),
            "mapping": mapping.name,
            "left": left.describe(),
            "right": right.describe(),
            "linked": true,
            "kept": kept.describe(),
        }));
        return Ok(());
    }

    output::line(&format!(
        "linked {} to {} in `{}`",
        left.describe(),
        right.describe(),
        mapping.name
    ));
    output::line(&format!(
        "where the two differ, the next sweep keeps {} - the source side of this mapping",
        kept.describe()
    ));
    output::line("`linear sync` (a dry run) says what the sweep makes of the pair.");
    Ok(())
}
