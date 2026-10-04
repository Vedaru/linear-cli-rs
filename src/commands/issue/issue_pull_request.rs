//! `linear issue pull-request` — port of `src/commands/issue/issue-pull-request.ts`.
//!
//! Creates a GitHub pull request via `gh pr create`. The body is the Linear
//! issue URL, optionally prefixed with a template read from `--template`, the
//! `pr_template` config option, or `LINEAR_PR_TEMPLATE`.
//!
//! `gh` is never handed `--template`: `gh` rejects it alongside `--body`, and
//! only consults a template interactively, so a non-TTY caller that dropped
//! `--body` would get no pull request at all. The template is folded into the
//! body here instead, with the issue URL last because that is what Linear
//! matches on to attach the pull request to the issue.

use clap::Args;

use crate::config::{self, PrTemplateArg};
use crate::errors::{CliError, Result};
use crate::hyperlink;
use crate::linear;
use crate::proc;

#[derive(Args, Debug)]
pub struct IssuePullRequestArgs {
    /// The branch into which you want your code merged
    #[arg(long, value_name = "branch")]
    pub base: Option<String>,
    /// Create the pull request as a draft
    #[arg(long)]
    pub draft: bool,
    /// Optional title for the pull request (Linear issue ID will be prefixed)
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Open the pull request in the browser after creating it
    #[arg(long)]
    pub web: bool,
    /// The branch that contains commits for your pull request
    #[arg(long, value_name = "branch")]
    pub head: Option<String>,
    /// Start the pull request body from this template file (the Linear issue
    /// URL is appended)
    #[arg(short = 'T', long, value_name = "file")]
    pub template: Option<String>,
    /// Ignore the pr_template config option for this pull request
    #[arg(long)]
    pub no_template: bool,
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
}

/// Compose the pull request body from a template and the Linear issue URL.
pub fn compose_pull_request_body(template_contents: &str, issue_url: &str) -> String {
    let template = template_contents.trim_end();
    if template.is_empty() {
        issue_url.to_string()
    } else {
        format!("{template}\n\n{issue_url}")
    }
}

/// Read a pull request template, rejecting anything that would not produce a
/// usable body.
///
/// An explicitly requested template that cannot be used is an error, never a
/// silent fallback to the plain URL body: the caller asked for it, so failing
/// quietly would ship a pull request missing the content they expected.
pub fn read_pull_request_template(path: &str) -> Result<String> {
    let unusable = |reason: String| {
        CliError::validation(format!("Cannot read pull request template: {reason}")).suggestion(
            "Pass a readable file to --template, fix the pr_template config option, or use --no-template to skip the template.",
        )
    };

    if path.trim().is_empty() {
        return Err(unusable("the path is empty".to_string()));
    }

    let info = match std::fs::metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(unusable(format!("\"{path}\" does not exist")));
        }
        Err(error) => {
            return Err(unusable(format!("\"{path}\" could not be read: {error}")));
        }
    };
    if info.is_dir() {
        return Err(unusable(format!("\"{path}\" is a directory, not a file")));
    }
    if !info.is_file() {
        return Err(unusable(format!("\"{path}\" is not a regular file")));
    }

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(unusable(format!("\"{path}\" could not be read: {error}")));
        }
    };

    // Deno.readTextFile does not reject binary input -- it substitutes U+FFFD
    // and keeps any NUL bytes, which Deno.Command then rejects with a bare
    // "nul byte found in provided data" TypeError. Catch it here with a message
    // that names the file.
    if bytes.contains(&0) {
        return Err(unusable(format!("\"{path}\" is not a text file")));
    }

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn run(args: IssuePullRequestArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        // `--no-template` opts out even when the config option is set;
        // otherwise an explicit path wins over the default. A path from a
        // config file resolves against that file, so a project-wide default
        // keeps working from a subdirectory.
        let template_arg = if args.no_template {
            PrTemplateArg::Disabled
        } else {
            match args.template.as_deref() {
                Some(value) => PrTemplateArg::Value(value),
                None => PrTemplateArg::Unset,
            }
        };
        let template_path = config::resolve_pr_template(template_arg)?;
        let template_contents = match &template_path {
            Some(path) => Some(read_pull_request_template(path)?),
            None => None,
        };

        let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID")
                .suggestion("Please provide an issue ID like 'ENG-123'."));
        };

        let details = linear::fetch_issue_details(&resolved_id, hyperlink::should_show_spinner())?;
        let title = details
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let url = details
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        let pr_title = format!("{resolved_id} {}", args.title.as_deref().unwrap_or(title));
        let body = match &template_contents {
            Some(contents) => compose_pull_request_body(contents, url),
            None => url.to_string(),
        };

        let mut gh_args: Vec<String> = vec![
            "pr".to_string(),
            "create".to_string(),
            "--title".to_string(),
            pr_title,
            "--body".to_string(),
            body,
        ];
        if let Some(base) = &args.base {
            gh_args.push("--base".to_string());
            gh_args.push(base.clone());
        }
        if let Some(head) = &args.head {
            gh_args.push("--head".to_string());
            gh_args.push(head.clone());
        }
        if args.draft {
            gh_args.push("--draft".to_string());
        }
        if args.web {
            gh_args.push("--web".to_string());
        }

        let arg_refs: Vec<&str> = gh_args.iter().map(String::as_str).collect();
        // `gh` missing is the common failure and the one the user can fix, and the
        // bare message names nothing (`LINEAR_DEBUG` adds nothing here either), so
        // say which dependency is absent before handing over the terminal.
        if !proc::exists("gh") {
            return Err(CliError::cli("gh is not installed").suggestion(
                "Install the GitHub CLI (https://cli.github.com) and authenticate with `gh auth login`, or create the pull request in the browser.",
            ));
        }
        // `gh` inherits our terminal; give it the editor-length deadline rather
        // than the short default so a `--web`/credential prompt is not killed.
        let status = proc::run_inherit("gh", &arg_refs, None, proc::EDITOR_TIMEOUT);
        if status != Some(true) {
            return Err(CliError::cli("Failed to create pull request"));
        }
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to create pull request"))
}
