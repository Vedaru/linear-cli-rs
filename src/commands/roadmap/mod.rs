//! `linear roadmap` — the plan-level view of a workspace.
//!
//! Reads only, on purpose: Linear deprecated roadmaps and the API refuses every roadmap write by
//! name ("Roadmaps are deprecated, use initiatives instead"), so `create`, `update` and `delete`
//! here would be commands that can only fail. `linear initiative` is the successor, and its write
//! half - including `add-project` - is wrapped. `src/linear/roadmaps.rs` carries the full reason
//! and the measured evidence.

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

mod roadmap_list;
mod roadmap_view;

#[derive(Args, Debug)]
pub struct RoadmapArgs {
    #[command(subcommand)]
    pub command: Option<RoadmapCommand>,
}

#[derive(Subcommand, Debug)]
pub enum RoadmapCommand {
    /// List roadmaps
    List(roadmap_list::RoadmapListArgs),
    /// Show one roadmap and the projects on it
    View(roadmap_view::RoadmapViewArgs),
}

pub fn run(args: RoadmapArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <RoadmapArgs as clap::Args>::augment_args(clap::Command::new("roadmap"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        RoadmapCommand::List(args) => {
            roadmap_list::run(args).map_err(|error| error.with_context("Failed to fetch roadmaps"))
        }
        RoadmapCommand::View(args) => {
            roadmap_view::run(args).map_err(|error| error.with_context("Failed to fetch roadmap"))
        }
    }
}
