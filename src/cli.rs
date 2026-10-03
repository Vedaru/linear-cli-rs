//! Command-line surface, mirroring the cliffy command tree in `src/cli.ts`.
//!
//! The global `--workspace <slug>` option and the top-level aliases are
//! reproduced exactly. Subcommand groups are added as they are ported; each
//! group lives in [`crate::commands`] and validates its own arguments.

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "linear",
    version,
    about = "Handy linear commands from the command line.",
    long_about = "Handy linear commands from the command line.\n\n\
                  Environment Variables:\n  \
                  LINEAR_DEBUG=1              Show full error details including stack traces\n  \
                  LINEAR_IGNORE_ENV_FILE=1    Skip loading .env files",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Target workspace (uses credentials)
    ///
    /// Not marked `global`: clap rejects a propagated global whose long name
    /// collides with a subcommand flag, and `label list` defines its own
    /// `--workspace` boolean (as upstream does, shadowing the global). Upstream
    /// documents the option ahead of the subcommand
    /// (`linear --workspace slug issue list`), which this still supports.
    #[arg(long, value_name = "slug")]
    pub workspace: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage Linear authentication
    Auth(AuthArgs),
    /// Manage Linear issues
    #[command(alias = "i")]
    Issue(crate::commands::issue::IssueArgs),
    /// Manage Linear projects
    #[command(alias = "p")]
    Project(crate::commands::project::ProjectArgs),
    /// Manage project status updates
    #[command(alias = "pu")]
    ProjectUpdate(crate::commands::project_update::ProjectUpdateArgs),
    /// Read roadmaps and the projects on them (reads only: Linear deprecated the writes)
    Roadmap(crate::commands::roadmap::RoadmapArgs),
    /// Manage Linear teams
    #[command(alias = "t")]
    Team(crate::commands::team::TeamArgs),
    /// Manage Linear users
    #[command(alias = "u")]
    User(crate::commands::user::UserArgs),
    /// Manage Linear team cycles
    #[command(alias = "cy")]
    Cycle(crate::commands::cycle::CycleArgs),
    /// Manage Linear project milestones
    #[command(alias = "m")]
    Milestone(crate::commands::milestone::MilestoneArgs),
    /// Manage Linear initiatives
    #[command(alias = "init")]
    Initiative(crate::commands::initiative::InitiativeArgs),
    /// Manage initiative status updates (timeline posts)
    #[command(alias = "iu", alias = "ls")]
    InitiativeUpdate(crate::commands::initiative_update::InitiativeUpdateArgs),
    /// Manage Linear issue labels
    #[command(alias = "l")]
    Label(crate::commands::label::LabelArgs),
    /// Browse Linear issue, project, and document templates
    Template(crate::commands::template::TemplateArgs),
    /// Manage Linear documents
    #[command(alias = "doc", alias = "docs")]
    Document(crate::commands::document::DocumentArgs),
    /// List and manage custom views (Linear's saved filters)
    View(crate::commands::view::ViewArgs),
    /// Notifications: what Linear told this user, and the read state that belongs to them
    Notification(crate::commands::notification::NotificationArgs),
    /// Interactively generate .linear.toml configuration
    #[command(alias = "configure")]
    Config(crate::commands::config::ConfigArgs),
    /// Print the GraphQL schema to stdout
    Schema(crate::commands::schema::SchemaArgs),
    /// Make a raw GraphQL API request
    Api(crate::commands::api::ApiArgs),
    /// Print the Linear markdown reference
    Markdown(crate::commands::markdown::MarkdownArgs),
    /// Generate shell completion scripts
    Completions(crate::commands::completions::CompletionsArgs),
    /// Run and inspect the webhook bridge service
    #[cfg(feature = "service")]
    Webhook(crate::commands::webhook::WebhookArgs),
    /// Bring a mapping's two platforms into agreement, once (dry run by default)
    #[cfg(feature = "service")]
    Sync(crate::commands::sync::SyncArgs),
}

/// `linear auth` — manage workspace credentials.
#[derive(Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Add a workspace credential
    Login {
        /// API key (prompted if not provided)
        #[arg(short = 'k', long, value_name = "key")]
        key: Option<String>,
        /// Store API key in credentials file instead of system keyring
        #[arg(long)]
        plaintext: bool,
    },
    /// Remove a workspace credential
    Logout {
        /// Workspace slug to remove
        workspace: Option<String>,
        /// Skip confirmation prompt
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// List configured workspaces
    List {
        /// Output the workspace list as JSON (an addition to upstream)
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Set the default workspace
    Default {
        /// Workspace slug to make default
        workspace: Option<String>,
    },
    /// Print the configured API token
    Token,
    /// Print information about the authenticated user
    Whoami {
        /// Output the raw viewer shape as JSON (an addition to upstream)
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Migrate plaintext credentials to system keyring
    Migrate,
}
