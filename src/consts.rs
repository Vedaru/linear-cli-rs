//! Shared constants. Mirrors `src/const.ts` in the TypeScript CLI.

pub const LINEAR_WEB_BASE_URL: &str = "https://linear.app";
pub const LINEAR_API_ENDPOINT: &str = "https://api.linear.app/graphql";

/// Requires auth to access.
pub const LINEAR_PRIVATE_UPLOAD_HOST: &str = "uploads.linear.app";
pub const LINEAR_PUBLIC_UPLOAD_HOST: &str = "public.linear.app";

pub const LINEAR_UPLOAD_HOSTNAMES: [&str; 2] =
    [LINEAR_PRIVATE_UPLOAD_HOST, LINEAR_PUBLIC_UPLOAD_HOST];

/// Default User-Agent prefix. The TypeScript CLI sends
/// `schpet-linear-cli/<version>`; we keep the same string so server-side logs
/// match and any API-side allowlisting keeps working.
pub const USER_AGENT_PREFIX: &str = "schpet-linear-cli";

/// Version reported by `--version`. Kept in sync with `Cargo.toml` and with the
/// upstream TypeScript CLI's `deno.json`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
