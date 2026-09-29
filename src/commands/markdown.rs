//! `linear markdown` — print the Linear-flavored Markdown reference.
//!
//! Port of `src/commands/markdown.ts`. The reference is both the command
//! description upstream and what the bare command prints; this port keeps the
//! printed form (the `crate::cli` doc comment supplies the one-line summary)
//! and emits it unindented so the `+++` syntax can be copied verbatim.

use crate::errors::Result;
use crate::markdown::LINEAR_MARKDOWN_REFERENCE;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct MarkdownArgs {}

pub fn run(_args: MarkdownArgs) -> Result<()> {
    output::line(LINEAR_MARKDOWN_REFERENCE);
    Ok(())
}
