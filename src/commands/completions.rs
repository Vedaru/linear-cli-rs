//! `linear completions` — generate shell completion scripts.
//!
//! Replaces upstream's cliffy `CompletionsCommand` with `clap_complete`. The
//! accepted shell names match cliffy's (`bash`, `zsh`, `fish`, `powershell`).
//!
//! The script is rendered into memory and written out in one go, rather than
//! handing stdout to `clap_complete` directly. Its shell generators call
//! `.expect("failed to write completion file")` on any write error, so a reader
//! that stops early (`linear completions bash | head`) turned into a panic with
//! exit 101 - the one place in this port that bypassed the promise `output.rs`
//! makes, that a closed stdout is a normal end of output. Writing into a `Vec`
//! cannot fail, which leaves the final write as the only fallible step and the
//! only place that has to answer what a closed reader means.

use std::io::Write;

use clap::CommandFactory;

use crate::errors::{CliError, Result};

#[derive(clap::Args, Debug)]
pub struct CompletionsArgs {
    /// Shell to generate completions for (bash, zsh, fish, powershell)
    #[arg(value_name = "shell")]
    pub shell: Option<String>,
}

pub fn run(args: CompletionsArgs) -> Result<()> {
    let Some(shell_name) = args.shell else {
        return Err(CliError::validation("No shell provided").suggestion(
            "Pass a shell: linear completions <bash|zsh|fish|powershell>",
        ));
    };

    let shell = match shell_name.to_ascii_lowercase().as_str() {
        "bash" => clap_complete::Shell::Bash,
        "zsh" => clap_complete::Shell::Zsh,
        "fish" => clap_complete::Shell::Fish,
        "powershell" => clap_complete::Shell::PowerShell,
        _ => {
            return Err(CliError::validation(format!(
                "Unsupported shell: {shell_name}"
            ))
            .suggestion("Supported shells: bash, zsh, fish, powershell"));
        }
    };

    let mut command = crate::cli::Cli::command();
    let mut script: Vec<u8> = Vec::new();
    clap_complete::generate(shell, &mut command, "linear", &mut script);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    match lock.write_all(&script).and_then(|()| lock.flush()) {
        Ok(()) => Ok(()),
        // A reader that stopped early is not a failure: the same tolerance the
        // rest of the output path applies (`output.rs`, "Broken pipe
        // tolerance"), kept here so `linear completions bash | head` exits 0.
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(CliError::cli(format!(
            "Failed to write the completion script: {error}"
        ))),
    }
}
