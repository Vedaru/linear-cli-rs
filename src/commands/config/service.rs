//! `linear config service`: the sections the bridge reads, in the file the CLI already has.
//!
//! One file holds both halves, which is the design rather than a coincidence: `linear webhook
//! serve` and `linear sync` read the same `linear.toml` the CLI does, with the same precedence,
//! so a deployment has one place to look and one thing to keep safe.
//!
//! Two things this deliberately does not do. It never writes a secret: a secret belongs in the
//! environment, and the file names the *variable* (`token_env = "FORGEJO_TOKEN"`) so a config can
//! be committed. And it does not call Linear to find out your team key - a generator that needs
//! the network to print a template is a generator that fails in a fresh checkout. What it cannot
//! know from its arguments or the working directory it leaves as an obvious placeholder.

use clap::Args;

use crate::config;
use crate::errors::Result;
use crate::output;
use crate::proc;

#[derive(Args, Debug)]
pub struct ServiceArgs {
    /// The Linear team key the mapping is scoped to (`VED`).
    #[arg(long)]
    pub team: Option<String>,
    /// The forge repository the mapping mirrors (`owner/name`). Defaults to the `origin` remote
    /// of the working directory, when there is one.
    #[arg(long)]
    pub repo: Option<String>,
    /// The forge's API root, without the `/api/v1` suffix.
    #[arg(long)]
    pub forge: Option<String>,
    /// Where the queue's database should live.
    #[arg(long, default_value = "~/.local/share/linear-bridge/bridge.db")]
    pub store: String,
}

pub fn run(args: ServiceArgs) -> Result<()> {
    let repo = args.repo.clone().or_else(repo_from_remote);
    let team = args.team.clone().unwrap_or_else(|| "<TEAM>".to_string());
    let repo = repo.unwrap_or_else(|| "<owner>/<repo>".to_string());
    let forge = args
        .forge
        .clone()
        .unwrap_or_else(|| "<your forge>".to_string());

    for line in sections(&team, &repo, &forge, &args.store) {
        output::line(&line);
    }
    output::blank();
    match config::service_config_text().map(|(path, _)| path) {
        Some(path) => output::line(&format!(
            "# Append the above to {}, or point `linear sync --config` at another file.",
            path.display()
        )),
        None => output::line(
            "# Append the above to your linear.toml - `linear config` writes one if you have none.",
        ),
    }
    Ok(())
}

/// The sections, as lines. A function rather than a `println!` block so the whole thing is a
/// value a test can parse, which is the only way to know the scaffold is a valid config.
fn sections(team: &str, repo: &str, forge: &str, store: &str) -> Vec<String> {
    vec![
        "# --- the bridge -----------------------------------------------------------".into(),
        "# Read by `linear webhook serve` and `linear sync`. Secrets are never written here:"
            .into(),
        "# the file names the environment variable, the environment holds the value.".into(),
        String::new(),
        "[bridge]".into(),
        "bind = \"127.0.0.1:8787\"".into(),
        format!("store = \"{store}\""),
        String::new(),
        "[platform.linear]".into(),
        "type = \"linear\"".into(),
        "# The CLI's own key, unless LINEAR_API_KEY is set, which wins either way.".into(),
        "token_env = \"LINEAR_API_KEY\"".into(),
        String::new(),
        "[platform.forgejo]".into(),
        "type = \"forgejo\"".into(),
        "token_env = \"FORGEJO_TOKEN\"".into(),
        format!("api_url = \"{forge}/api/v1\""),
        String::new(),
        "[[mapping]]".into(),
        format!(
            "name = \"{}\"",
            format!("{team}-{repo}").replace(['/', '<', '>'], "-")
        ),
        format!("source = \"linear:{team}\""),
        format!("sink = \"forgejo:{repo}\""),
    ]
}

/// The `owner/name` in the `origin` remote, if this is a git checkout with one.
fn repo_from_remote() -> Option<String> {
    repo_from_remote_url(proc::stdout_of("git", &["remote", "get-url", "origin"])?.trim())
}

/// The `owner/name` in a remote url.
///
/// Both shapes, because both are what people have, and they put the host in different places:
/// `https://host/owner/name.git` carries a scheme whose own `:` must not be mistaken for the
/// separator, while `git@host:owner/name.git` has nothing *but* that separator. Getting this
/// wrong is silent - it yields a plausible owner that is really part of the host name - which is
/// why it is a function with a test rather than three lines inside the caller.
fn repo_from_remote_url(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let path = if url.contains("://") {
        // scheme://host/owner/name -> owner/name
        url.splitn(4, '/').nth(3).unwrap_or_default()
    } else if let Some((_, after)) = url.rsplit_once(':') {
        after
    } else {
        url
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?;
    let name = parts.next()?.split('/').next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scaffold_is_a_config_the_bridge_can_read() {
        // The config resolves `token_env` when it loads, so the variables have to exist for
        // this to be the real check. A dummy value is enough: nothing here talks to a platform,
        // and the variable names - never values - are the point of the scaffold.
        std::env::set_var("LINEAR_API_KEY", "a-dummy-key-long-enough-to-be-one");
        std::env::set_var("FORGEJO_TOKEN", "a-dummy-token-long-enough-to-be-one");

        // The property that matters: what this prints, `webhook serve` can run. A scaffold that
        // only looks like a config is worse than none, because it fails at the first command
        // that tries it.
        let text = sections(
            "VED",
            "Vedaru/linear-cli-rs",
            "https://git.example.com",
            "/tmp/b.db",
        )
        .join("\n");
        let config = linear_bridge::config::BridgeConfig::from_toml(&text)
            .unwrap_or_else(|error| panic!("the scaffold should parse: {error}\n{text}"));
        assert_eq!(config.store_path, "/tmp/b.db");
        let mappings = config.reconcile_mappings().expect("and be runnable");
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].source.connector.as_str(), "linear");
        assert_eq!(mappings[0].source.scope, "VED");
        assert_eq!(mappings[0].sink.scope, "Vedaru/linear-cli-rs");
    }

    #[test]
    fn git_remotes_of_both_shapes_yield_a_repository() {
        // Read from the shape, not from a guess: ssh remotes have a `:` where https ones have
        // the host.
        for url in [
            "git@github.com:Vedaru/linear-cli-rs.git",
            "https://github.com/Vedaru/linear-cli-rs.git",
            "ssh://git@forge.example.com:2222/Vedaru/linear-cli-rs.git",
            "git@forge.example.com:Vedaru/linear-cli-rs",
        ] {
            assert_eq!(
                repo_from_remote_url(url).as_deref(),
                Some("Vedaru/linear-cli-rs"),
                "from {url}"
            );
        }
        // And nothing that is not a repository is answered with a guess.
        assert_eq!(repo_from_remote_url(""), None);
    }
}
