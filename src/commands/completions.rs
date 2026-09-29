//! `linear completions` — generate shell completion scripts.
//!
//! Replaces upstream's cliffy `CompletionsCommand` with `clap_complete`. The
//! accepted shell names match cliffy's (`bash`, `zsh`, `fish`, `powershell`).

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
        return Err(CliError::validation("No shell provided")
            .suggestion("Pass a shell: linear completions <bash|zsh|fish|powershell>"));
    };

    let shell = match shell_name.to_ascii_lowercase().as_str() {
        "bash" => clap_complete::Shell::Bash,
        "zsh" => clap_complete::Shell::Zsh,
        "fish" => clap_complete::Shell::Fish,
        "powershell" => clap_complete::Shell::PowerShell,
        _ => {
            return Err(
                CliError::validation(format!("Unsupported shell: {shell_name}"))
                    .suggestion("Supported shells: bash, zsh, fish, powershell"),
            );
        }
    };

    let mut command = crate::cli::Cli::command();
    clap_complete::generate(shell, &mut command, "linear", &mut std::io::stdout());
    Ok(())
}
