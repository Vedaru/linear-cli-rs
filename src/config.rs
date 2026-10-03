//! Configuration and environment resolution. Port of `src/config.ts`.
//!
//! Precedence, highest first:
//!
//! 1. CLI flag
//! 2. process environment (`LINEAR_*`, or set from a project `.env`)
//! 3. project config file (`linear.toml`, `.linear.toml`, `.config/linear.toml`
//!    in the working directory or at the git root)
//! 4. global config file (`$XDG_CONFIG_HOME/linear/linear.toml`)
//!
//! `.env` handling is deliberately narrow: only `LINEAR_`, `GH_` and `GITHUB_`
//! assignments are read, from either the working directory or the git root, and
//! values are never shell-expanded. See [`select_relevant_assignments`] for why
//! that hardening is load-bearing.

use crate::errors::{CliError, Result};
use crate::output;
use crate::proc;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Issue ordering options, mirroring `ISSUE_SORT_VALUES`.
pub const ISSUE_SORT_VALUES: [&str; 2] = ["manual", "priority"];
pub const DEFAULT_ISSUE_SORT: IssueSort = IssueSort::Priority;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSort {
    Manual,
    Priority,
}

impl IssueSort {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueSort::Manual => "manual",
            IssueSort::Priority => "priority",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(IssueSort::Manual),
            "priority" => Some(IssueSort::Priority),
            _ => None,
        }
    }
}

/// `issue_create_assign_self` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignSelf {
    Always,
    Auto,
    Never,
}

impl AssignSelf {
    pub fn as_str(self) -> &'static str {
        match self {
            AssignSelf::Always => "always",
            AssignSelf::Auto => "auto",
            AssignSelf::Never => "never",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "always" => Some(AssignSelf::Always),
            "auto" => Some(AssignSelf::Auto),
            "never" => Some(AssignSelf::Never),
            _ => None,
        }
    }
}

/// `vcs` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vcs {
    Git,
    Jj,
}

impl Vcs {
    pub fn as_str(self) -> &'static str {
        match self {
            Vcs::Git => "git",
            Vcs::Jj => "jj",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "git" => Some(Vcs::Git),
            "jj" => Some(Vcs::Jj),
            _ => None,
        }
    }
}

/// Where a resolved option value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionSource {
    Cli,
    /// Pre-existing process environment variable.
    Env,
    /// `LINEAR_*` applied from a project `.env` file.
    ProjectEnv,
    /// `linear.toml` / `.linear.toml` in the working directory or git root.
    ProjectConfig,
    /// XDG / `~/.config` / APPDATA `linear.toml`.
    GlobalConfig,
}

#[derive(Debug, Clone)]
pub struct Resolved<T> {
    pub value: T,
    pub source: OptionSource,
}

/// A resolved `pr_template` input, distinguishing "not given" from
/// `--no-template`, which both mean different things.
#[derive(Debug, Clone, Copy)]
pub enum PrTemplateArg<'a> {
    Unset,
    /// `--no-template`: the template is explicitly suppressed.
    Disabled,
    Value(&'a str),
}

struct ConfigState {
    global: Map<String, Value>,
    project: Map<String, Value>,
    global_path: Option<PathBuf>,
    project_path: Option<PathBuf>,
    /// Env keys that `load_env_files()` wrote, as opposed to values already
    /// present in the process environment.
    dotenv_applied_keys: HashSet<String>,
    cli_workspace: Option<String>,
}

impl ConfigState {
    fn empty() -> Self {
        ConfigState {
            global: Map::new(),
            project: Map::new(),
            global_path: None,
            project_path: None,
            dotenv_applied_keys: HashSet::new(),
            cli_workspace: None,
        }
    }
}

fn state() -> &'static Mutex<ConfigState> {
    static STATE: OnceLock<Mutex<ConfigState>> = OnceLock::new();
    STATE.get_or_init(|| {
        let mut state = ConfigState::empty();
        load_into(&mut state);
        Mutex::new(state)
    })
}

/// Force configuration and `.env` loading to happen now.
///
/// Called once from `main` so the ordering of warnings and network work is
/// predictable. Every getter would trigger the same load on first use.
pub fn init() {
    let _ = state();
}

// --- Config files ---

fn load_config_from_path(path: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// Global config path: `$XDG_CONFIG_HOME/linear/linear.toml` on Unix,
/// `%APPDATA%\linear\linear.toml` on Windows.
pub fn global_config_path() -> Option<PathBuf> {
    crate::paths::config_file("linear.toml")
}

/// Text of the config file the *service* half should read: the project file when
/// one exists, otherwise the global one - the same order every other lookup here
/// uses.
///
/// The bridge parses its own sections (`[bridge]`, `[platform.*]`,
/// `[[mapping]]`) out of this text rather than being handed this module's parsed
/// schema, so the two halves share one file and one precedence rule without
/// sharing a key list that would have to grow in lockstep.
pub fn service_config_text() -> Option<(PathBuf, String)> {
    init();
    let state = state().lock().ok()?;
    let path = state
        .project_path
        .clone()
        .or_else(|| state.global_path.clone())?;
    let text = std::fs::read_to_string(&path).ok()?;
    Some((path, text))
}

/// Project config candidates, in precedence order.
fn project_config_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("linear.toml"), PathBuf::from(".linear.toml")];
    if let Some(root) = proc::stdout_of("git", &["rev-parse", "--show-toplevel"]) {
        if !root.is_empty() {
            let root = PathBuf::from(root);
            paths.push(root.join("linear.toml"));
            paths.push(root.join(".linear.toml"));
            paths.push(root.join(".config").join("linear.toml"));
        }
    }
    paths
}

fn load_config(state: &mut ConfigState) {
    if let Some(path) = global_config_path() {
        if let Some(loaded) = load_config_from_path(&path) {
            state.global = loaded;
            state.global_path = Some(path);
        }
    }
    for path in project_config_paths() {
        if let Some(loaded) = load_config_from_path(&path) {
            state.project = loaded;
            state.project_path = Some(path);
            break;
        }
    }
}

// --- .env loading ---

/// Env keys this CLI reads from a `.env` file. Everything else is ignored and
/// never parsed.
const ALLOWED_ENV_VAR_PREFIXES: [&str; 3] = ["LINEAR_", "GH_", "GITHUB_"];

/// Opt out of `.env` loading entirely, for repos whose `.env` is not
/// dotenv-shaped.
fn env_file_loading_disabled() -> bool {
    matches!(
        std::env::var("LINEAR_IGNORE_ENV_FILE").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// True when `value` references a shell variable, mirroring the expansion
/// syntax `@std/dotenv` acts on: `${NAME}`, or an unescaped `$NAME`. A `$` that
/// is not a reference (`ENG # $note`, `a$`) is left alone.
///
/// The lookbehind in the upstream regular expression is written out as a scan
/// because Rust's `regex` crate has no lookaround.
fn has_shell_reference(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }
        // An escaped `$` is literal.
        if index > 0 && bytes[index - 1] == b'\\' {
            index += 1;
            continue;
        }
        let rest = &bytes[index + 1..];
        if rest.first() == Some(&b'{') {
            // `${}` has no name to expand; upstream's `.+?` requires at least
            // one character between the braces.
            if rest[2..].contains(&b'}') {
                return true;
            }
        } else if rest
            .first()
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            return true;
        }
        index += 1;
    }
    false
}

#[derive(Debug)]
struct ParsedValue {
    text: String,
    /// Whether `@std/dotenv` would expand `$` references here. Only unquoted
    /// values are expanded.
    expands: bool,
}

/// The value the dotenv parser would see for an assignment, or `None` when a
/// quote is opened and never closed on the same line.
fn effective_value(raw_value: &str) -> Option<ParsedValue> {
    let value = raw_value.trim_start();
    let mut chars = value.chars();
    match chars.next() {
        Some(quote @ ('"' | '\'')) => {
            let mut text = String::new();
            let mut escaped = false;
            for ch in chars {
                if quote == '"' && escaped {
                    text.push(ch);
                    escaped = false;
                    continue;
                }
                if quote == '"' && ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == quote {
                    return Some(ParsedValue {
                        text,
                        expands: false,
                    });
                }
                text.push(ch);
            }
            None
        }
        _ => {
            let comment = value.find('#');
            let text = match comment {
                Some(index) => value[..index].trim_end(),
                None => value.trim_end(),
            };
            Some(ParsedValue {
                text: text.to_string(),
                expands: true,
            })
        }
    }
}

#[derive(Debug, Default)]
struct SelectedAssignments {
    /// A dotenv document containing only the assignments this CLI consumes.
    text: String,
    /// Our keys skipped because the value references a shell variable.
    skipped_expansion_keys: Vec<String>,
    /// Our keys skipped because the value opens a quote it never closes.
    skipped_unterminated_keys: Vec<String>,
}

/// Reduce a `.env` file to just the assignments this CLI consumes, before
/// handing it to the dotenv parser.
///
/// This is load-bearing, not an optimization. `@std/dotenv` expands `$VAR`
/// references in a `while` loop that never terminates when a value refers to
/// itself, so one ordinary line like `export PATH=$PATH:/opt/bin` hangs the CLI
/// forever at startup — no error, no exit. Since the only keys we ever apply
/// are `LINEAR_`/`GH_`/`GITHUB_`, dropping every other line first removes that
/// whole class of failure. The Rust parser below has no such loop, but the
/// filter is kept for the same user-visible behavior: unknown keys are ignored
/// silently and unexpandable values are reported instead of guessed.
fn select_relevant_assignments(text: &str) -> SelectedAssignments {
    let mut selected = SelectedAssignments::default();
    let mut kept: Vec<String> = Vec::new();

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim_start_matches([' ', '\t']);
        let trimmed = trimmed.strip_prefix("export").map_or(trimmed, |rest| {
            if rest.starts_with([' ', '\t']) {
                rest.trim_start_matches([' ', '\t'])
            } else {
                trimmed
            }
        });
        let Some(equals) = trimmed.find('=') else {
            continue;
        };
        // Upstream's regex allows `[ \t]*=` before the `=`, so a key with
        // trailing blanks (`LINEAR_KEY =value`) is still recognized.
        let key = trimmed[..equals].trim_end();
        if key.is_empty() || !is_env_key(key) {
            continue;
        }
        if !ALLOWED_ENV_VAR_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            continue;
        }
        let raw_value = &trimmed[equals + 1..];
        let Some(value) = effective_value(raw_value) else {
            selected.skipped_unterminated_keys.push(key.to_string());
            continue;
        };
        if value.expands && has_shell_reference(&value.text) {
            selected.skipped_expansion_keys.push(key.to_string());
            continue;
        }
        // Keep the original text so the dotenv parser, not this filter, stays
        // responsible for unquoting and escape handling.
        kept.push(format!("{key}={raw_value}"));
    }

    selected.text = kept.join("\n");
    selected
}

fn is_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

enum EnvFileOutcome {
    Absent,
    Unusable(String),
    Loaded {
        vars: HashMap<String, String>,
        skipped_expansion_keys: Vec<String>,
        skipped_unterminated_keys: Vec<String>,
    },
}

/// Read one `.env` candidate. Never fails: a broken `.env` is optional input
/// and must not take down a command that did not need it.
fn read_env_file(path: &Path) -> EnvFileOutcome {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return EnvFileOutcome::Absent
        }
        Err(error) => return EnvFileOutcome::Unusable(error.to_string()),
    };
    if metadata.is_dir() {
        return EnvFileOutcome::Unusable("it is a directory, not a file".to_string());
    }
    // Also covers FIFOs, sockets and devices, which would otherwise block the
    // read forever rather than fail.
    if !metadata.is_file() {
        return EnvFileOutcome::Unusable("it is not a regular file".to_string());
    }

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return EnvFileOutcome::Absent
        }
        Err(error) => return EnvFileOutcome::Unusable(error.to_string()),
    };

    let selected = select_relevant_assignments(&text);
    EnvFileOutcome::Loaded {
        vars: parse_dotenv(&selected.text),
        skipped_expansion_keys: selected.skipped_expansion_keys,
        skipped_unterminated_keys: selected.skipped_unterminated_keys,
    }
}

/// Parse the filtered dotenv document.
///
/// Handles the three value forms `@std/dotenv` recognises: single-quoted
/// (literal), double-quoted (with `\n`, `\r`, `\t`, `\\`, `\"`, `\$` escapes)
/// and unquoted (running to the first `#`).
fn parse_dotenv(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(equals) = line.find('=') else {
            continue;
        };
        let key = line[..equals].trim();
        if key.is_empty() {
            continue;
        }
        let raw_value = line[equals + 1..].trim_start();
        let value = match raw_value.chars().next() {
            Some('\'') => raw_value
                .trim_start_matches('\'')
                .split('\'')
                .next()
                .unwrap_or("")
                .to_string(),
            Some('"') => {
                let inner = raw_value[1..].to_string();
                let mut out = String::new();
                let mut chars = inner.chars();
                while let Some(ch) = chars.next() {
                    if ch == '"' {
                        break;
                    }
                    if ch == '\\' {
                        match chars.next() {
                            Some('n') => out.push('\n'),
                            Some('r') => out.push('\r'),
                            Some('t') => out.push('\t'),
                            Some(other) => out.push(other),
                            None => break,
                        }
                        continue;
                    }
                    out.push(ch);
                }
                out
            }
            _ => {
                let comment = raw_value.find('#');
                match comment {
                    Some(index) => raw_value[..index].trim_end().to_string(),
                    None => raw_value.trim_end().to_string(),
                }
            }
        };
        vars.insert(key.to_string(), value);
    }
    vars
}

fn load_env_files(state: &mut ConfigState) {
    if env_file_loading_disabled() {
        return;
    }

    let cwd_env_path = std::env::current_dir()
        .map(|dir| dir.join(".env"))
        .unwrap_or_else(|_| PathBuf::from(".env"));
    let mut loaded_path = cwd_env_path.clone();
    let mut outcome = read_env_file(&cwd_env_path);

    if let EnvFileOutcome::Unusable(reason) = &outcome {
        output::warn_with_suggestion(
            &format!(
                "Ignoring {}: {reason}. No variables were loaded from it.",
                cwd_env_path.display()
            ),
            "Set LINEAR_IGNORE_ENV_FILE=1 to skip .env loading entirely.",
        );
    }

    // Fall back to the repository root only when the working directory did not
    // provide a usable file.
    if !matches!(outcome, EnvFileOutcome::Loaded { .. }) {
        let git_root_env = proc::stdout_of("git", &["rev-parse", "--show-toplevel"])
            .filter(|root| !root.is_empty())
            .map(|root| PathBuf::from(root).join(".env"));
        if let Some(git_root_env) = git_root_env {
            if git_root_env != cwd_env_path {
                let root_outcome = read_env_file(&git_root_env);
                if let EnvFileOutcome::Unusable(reason) = &root_outcome {
                    output::warn_with_suggestion(
                        &format!(
                            "Ignoring {}: {reason}. No variables were loaded from it.",
                            git_root_env.display()
                        ),
                        "Set LINEAR_IGNORE_ENV_FILE=1 to skip .env loading entirely.",
                    );
                }
                loaded_path = git_root_env;
                outcome = root_outcome;
            }
        }
    }

    let EnvFileOutcome::Loaded {
        vars,
        skipped_expansion_keys,
        skipped_unterminated_keys,
    } = outcome
    else {
        return;
    };

    for (key, value) in &vars {
        // Match dotenv precedence: the process environment wins.
        if std::env::var_os(key).is_some() {
            continue;
        }
        std::env::set_var(key, value);
        state.dotenv_applied_keys.insert(key.clone());
    }

    // Only report values we would otherwise have applied, so a file full of
    // shell syntax we never consume stays quiet.
    let would_have_applied = |keys: &[String]| -> Vec<String> {
        keys.iter()
            .filter(|key| std::env::var_os(key.as_str()).is_none())
            .cloned()
            .collect()
    };

    let skipped_expansion = would_have_applied(&skipped_expansion_keys);
    if !skipped_expansion.is_empty() {
        output::warn_with_suggestion(
            &format!(
                "Ignoring {} in {}: the value references a shell variable, which linear does not expand.",
                skipped_expansion.join(", "),
                loaded_path.display()
            ),
            "Write the literal value, or set the variable in your environment instead.",
        );
    }
    let skipped_unterminated = would_have_applied(&skipped_unterminated_keys);
    if !skipped_unterminated.is_empty() {
        output::warn_with_suggestion(
            &format!(
                "Ignoring {} in {}: the value opens a quote it never closes on the same line.",
                skipped_unterminated.join(", "),
                loaded_path.display()
            ),
            "linear does not support values that span multiple lines.",
        );
    }
}

fn load_into(state: &mut ConfigState) {
    load_env_files(state);
    load_config(state);
}

// --- Option resolution ---

fn coerce_bool(value: &Value) -> Option<bool> {
    const TRUTHY: [&str; 6] = ["true", "yes", "y", "on", "1", "t"];
    const FALSY: [&str; 6] = ["false", "no", "n", "off", "0", "f"];
    match value {
        Value::Bool(value) => Some(*value),
        Value::String(text) => {
            let lower = text.to_lowercase();
            if TRUTHY.contains(&lower.as_str()) {
                Some(true)
            } else if FALSY.contains(&lower.as_str()) {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve an option to its raw value and the source it came from.
///
/// Presence is checked rather than nullishness, so a present-but-invalid
/// higher-precedence value still shadows lower-precedence values.
pub fn resolve_raw(option_name: &str, cli_value: Option<&str>) -> Option<(Value, OptionSource)> {
    if let Some(value) = cli_value {
        return Some((Value::String(value.to_string()), OptionSource::Cli));
    }
    let env_key = format!("LINEAR_{}", option_name.to_ascii_uppercase());
    if let Some(value) = std::env::var_os(&env_key) {
        let value = value.to_string_lossy().to_string();
        let from_dotenv = state()
            .lock()
            .map(|state| state.dotenv_applied_keys.contains(&env_key))
            .unwrap_or(false);
        let source = if from_dotenv {
            OptionSource::ProjectEnv
        } else {
            OptionSource::Env
        };
        return Some((Value::String(value), source));
    }
    let guard = state().lock().ok()?;
    if let Some(value) = guard.project.get(option_name) {
        return Some((value.clone(), OptionSource::ProjectConfig));
    }
    if let Some(value) = guard.global.get(option_name) {
        return Some((value.clone(), OptionSource::GlobalConfig));
    }
    None
}

/// The directory a relative path from `source` should resolve against, or
/// `None` to use the working directory.
///
/// A path written in a config file is relative to that file; resolving it
/// against the working directory would make a project-wide setting such as
/// `pr_template = ".github/pull_request_template.md"` work at the repository
/// root and fail in every subdirectory.
pub fn option_base_dir(source: OptionSource) -> Option<PathBuf> {
    let guard = state().lock().ok()?;
    match source {
        OptionSource::ProjectConfig => guard
            .project_path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf)),
        OptionSource::GlobalConfig => guard
            .global_path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf)),
        OptionSource::Cli | OptionSource::Env | OptionSource::ProjectEnv => None,
    }
}

fn resolved_string(option_name: &str, cli_value: Option<&str>) -> Option<Resolved<String>> {
    let (raw, source) = resolve_raw(option_name, cli_value)?;
    let value = raw.as_str()?.to_string();
    Some(Resolved { value, source })
}

fn resolved_bool(option_name: &str, cli_value: Option<&str>) -> Option<Resolved<bool>> {
    let (raw, source) = resolve_raw(option_name, cli_value)?;
    let value = coerce_bool(&raw)?;
    Some(Resolved { value, source })
}

fn resolved_picklist<T>(
    option_name: &str,
    cli_value: Option<&str>,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<Resolved<T>> {
    let (raw, source) = resolve_raw(option_name, cli_value)?;
    let value = parse(raw.as_str()?)?;
    Some(Resolved { value, source })
}

/// `team_id`, with its source so callers can report where a bad value came
/// from.
pub fn team_id_resolved(cli_value: Option<&str>) -> Option<Resolved<String>> {
    resolved_string("team_id", cli_value)
}

pub fn team_id(cli_value: Option<&str>) -> Option<String> {
    team_id_resolved(cli_value).map(|resolved| resolved.value)
}

/// `api_key` from the project or global config. Environment and flag handling
/// for keys lives in `graphql::get_resolved_api_key`, which owns the full
/// precedence chain.
pub fn api_key() -> Option<String> {
    resolved_string("api_key", None).map(|resolved| resolved.value)
}

pub fn workspace() -> Option<String> {
    resolved_string("workspace", None).map(|resolved| resolved.value)
}

pub fn issue_create_ask_project() -> Option<bool> {
    resolved_bool("issue_create_ask_project", None).map(|resolved| resolved.value)
}

pub fn issue_create_assign_self() -> Option<AssignSelf> {
    resolved_picklist("issue_create_assign_self", None, AssignSelf::parse)
        .map(|resolved| resolved.value)
}

pub fn vcs() -> Option<Vcs> {
    resolved_picklist("vcs", None, Vcs::parse).map(|resolved| resolved.value)
}

pub fn download_images() -> Option<bool> {
    resolved_bool("download_images", None).map(|resolved| resolved.value)
}

pub fn hyperlink_format() -> Option<String> {
    resolved_string("hyperlink_format", None).map(|resolved| resolved.value)
}

pub fn attachment_dir() -> Option<String> {
    resolved_string("attachment_dir", None).map(|resolved| resolved.value)
}

pub fn auto_download_attachments() -> Option<bool> {
    resolved_bool("auto_download_attachments", None).map(|resolved| resolved.value)
}

/// Resolve the issue sort order from a CLI flag, `LINEAR_ISSUE_SORT`, or the
/// `issue_sort` config, defaulting to `priority`.
///
/// Unlike the getters above, an explicitly configured but invalid value errors
/// instead of silently falling back to the default.
pub fn resolve_issue_sort(cli_value: Option<&str>) -> Result<IssueSort> {
    let Some((raw, _source)) = resolve_raw("issue_sort", cli_value) else {
        return Ok(DEFAULT_ISSUE_SORT);
    };
    if raw.is_null() {
        return Ok(DEFAULT_ISSUE_SORT);
    }
    let Some(text) = raw.as_str() else {
        return Err(invalid_issue_sort(&raw));
    };
    IssueSort::parse(text).ok_or_else(|| invalid_issue_sort(&raw))
}

fn invalid_issue_sort(raw: &Value) -> CliError {
    CliError::validation(format!(
        "Invalid issue sort: {}",
        serde_json::to_string(raw).unwrap_or_default()
    ))
    .suggestion(format!(
        "Use one of: {} (via --sort, the issue_sort config option, or the LINEAR_ISSUE_SORT environment variable)",
        ISSUE_SORT_VALUES.join(", ")
    ))
}

/// Resolve the pull request template path from `--template`,
/// `LINEAR_PR_TEMPLATE`, or the `pr_template` config option, with
/// [`PrTemplateArg::Disabled`] meaning `--no-template`.
///
/// Follows [`resolve_issue_sort`] rather than a lenient getter: silently
/// returning `None` for a value that fails to parse would create a pull request
/// quietly missing the template the user configured. A path from a config file
/// is resolved against that file's directory.
pub fn resolve_pr_template(cli_value: PrTemplateArg<'_>) -> Result<Option<String>> {
    let cli = match cli_value {
        PrTemplateArg::Disabled => return Ok(None),
        PrTemplateArg::Unset => None,
        PrTemplateArg::Value(value) => Some(value),
    };
    let Some((raw, source)) = resolve_raw("pr_template", cli) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(text) = raw.as_str() else {
        return Err(invalid_pr_template(&raw));
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(invalid_pr_template(&raw));
    }
    match option_base_dir(source) {
        // An absolute value is honoured as written.
        Some(base) => Ok(Some(base.join(trimmed).to_string_lossy().to_string())),
        None => Ok(Some(trimmed.to_string())),
    }
}

fn invalid_pr_template(raw: &Value) -> CliError {
    CliError::validation(format!(
        "Invalid pull request template: {}",
        serde_json::to_string(raw).unwrap_or_default()
    ))
    .suggestion(
        "Set a non-empty file path via --template, the pr_template config option, or LINEAR_PR_TEMPLATE; use --no-template to skip the template.",
    )
}

// --- CLI workspace ---

/// Record the `--workspace` flag value.
pub fn set_cli_workspace(workspace: Option<String>) {
    if let Ok(mut guard) = state().lock() {
        guard.cli_workspace = workspace;
    }
}

pub fn cli_workspace() -> Option<String> {
    state()
        .lock()
        .ok()
        .and_then(|guard| guard.cli_workspace.clone())
}

#[cfg(test)]
mod tests;
