//! `linear issue view` — port of `src/commands/issue/issue-view.ts`.
//!
//! Ports upstream's non-TTY branch: the issue is rendered as raw Linear-flavored
//! Markdown (see AGENTS.md — there is no Rust equivalent of `@littletof/charmd`).
//! The TTY branch's renderer, hyperlink extension, and pager wiring are not
//! emitted; `--no-pager` is still accepted so agent prompts keep parsing.
//!
//! `--web` / `--app` open the issue in the browser or desktop app and return
//! before the rest of the command runs, mirroring upstream (that branch sits
//! outside the `try`/`handleError`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config;
use crate::display;
use crate::errors::{CliError, Result};
use crate::linear;
use crate::markdown;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueViewArgs {
    /// Issue ID, URL, or omit for the current branch's issue
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Open in web browser
    #[arg(short = 'w', long)]
    pub web: bool,
    /// Open in Linear.app
    #[arg(short = 'a', long)]
    pub app: bool,
    /// Exclude comments from the output
    #[arg(long = "no-comments", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub comments: bool,
    /// Include resolved comment threads in the output
    #[arg(long = "show-resolved-threads")]
    pub show_resolved_threads: bool,
    /// Disable automatic paging for long output
    #[arg(long = "no-pager", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub pager: bool,
    /// Output issue data as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Keep remote URLs instead of downloading files
    #[arg(long = "no-download", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub download: bool,
}

pub fn run(args: IssueViewArgs) -> Result<()> {
    if args.web || args.app {
        return crate::actions::open_issue_page(args.issue_id.as_deref(), args.app);
    }

    let result = view(&args);
    result.map_err(|error| error.with_context("Failed to view issue"))
}

fn view(args: &IssueViewArgs) -> Result<()> {
    let show_comments = args.comments;
    let show_resolved_threads = args.show_resolved_threads;

    let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    if args.json {
        let issue_data = linear::fetch_issue_details_raw(&resolved_id, show_comments)?;
        output::print_json(&issue_data.unwrap_or(Value::Null));
        return Ok(());
    }

    let issue_data = linear::fetch_issue_details(&resolved_id, show_comments)?;

    let mut description = issue_data
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut comment_values: Option<Vec<Value>> = issue_data
        .get("comments")
        .and_then(Value::as_array)
        .cloned();

    // Download images embedded in the description and comment bodies, then
    // rewrite the bodies to point at the local copies.
    let should_download = args.download && config::download_images() != Some(false);
    if should_download {
        let mut sources: Vec<Option<&str>> = vec![description.as_deref()];
        if let Some(comments) = &comment_values {
            for comment in comments {
                sources.push(comment.get("body").and_then(Value::as_str));
            }
        }
        let url_to_path = markdown::download_markdown_images(&sources);
        if !url_to_path.is_empty() {
            if let Some(text) = &description {
                description = Some(markdown::replace_urls(text, &url_to_path));
            }
            if let Some(comments) = &mut comment_values {
                for comment in comments.iter_mut() {
                    if let Some(body) = comment.get("body").and_then(Value::as_str) {
                        let replaced = markdown::replace_urls(body, &url_to_path);
                        comment["body"] = Value::String(replaced);
                    }
                }
            }
        }
    }

    // Download file attachments when enabled.
    let attachments: Vec<Value> = issue_data
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let should_download_attachments =
        should_download && config::auto_download_attachments() != Some(false);
    let attachment_paths: HashMap<String, String> =
        if should_download_attachments && !attachments.is_empty() {
            let identifier = issue_data
                .get("identifier")
                .and_then(Value::as_str)
                .unwrap_or("");
            download_attachments(identifier, &attachments)?
        } else {
            HashMap::new()
        };

    let comments: Option<Vec<Comment>> =
        comment_values.map(|values| values.iter().map(parse_comment).collect());
    let derived = comments
        .as_ref()
        .map(|list| derive_comment_view(list, show_resolved_threads));

    let identifier = issue_data
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = issue_data
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("");

    let mut meta_parts: Vec<String> = Vec::new();
    if let Some(state) = issue_data.pointer("/state/name").and_then(Value::as_str) {
        meta_parts.push(format!("**State:** {state}"));
    }
    let priority = issue_data
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    meta_parts.push(format!(
        "**Priority:** {}",
        display::get_priority_display(priority)
    ));
    let assignee_display = match issue_data
        .pointer("/assignee/displayName")
        .and_then(Value::as_str)
    {
        Some(name) => format!("@{name}"),
        None => "Unassigned".to_string(),
    };
    meta_parts.push(format!("**Assignee:** {assignee_display}"));
    if let Some(project) = issue_data.pointer("/project/name").and_then(Value::as_str) {
        meta_parts.push(format!("**Project:** {project}"));
    }
    if let Some(milestone) = issue_data
        .pointer("/projectMilestone/name")
        .and_then(Value::as_str)
    {
        meta_parts.push(format!("**Milestone:** {milestone}"));
    }
    if let Some(cycle) = issue_data.get("cycle").filter(|value| !value.is_null()) {
        let active_cycle_number = issue_data
            .pointer("/team/activeCycle/number")
            .and_then(Value::as_i64);
        let cycle_short = display::format_cycle_short(Some(cycle_info(cycle)), active_cycle_number);
        let number = cycle.get("number").and_then(Value::as_i64).unwrap_or(0);
        let cycle_label = match cycle.get("name").and_then(Value::as_str) {
            Some(name) => format!("#{number} {name}"),
            None => format!("#{number}"),
        };
        let cycle_display = if cycle_short.text.starts_with('#') {
            cycle_label
        } else {
            format!("{cycle_label} ({})", cycle_short.text)
        };
        meta_parts.push(format!("**Cycle:** {cycle_display}"));
    }
    let meta_line = if meta_parts.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", meta_parts.join(" | "))
    };

    let description_part = match &description {
        Some(text) if !text.is_empty() => format!("\n\n{text}"),
        _ => String::new(),
    };
    let mut markdown_body = format!("# {identifier}: {title}{meta_line}{description_part}");

    let parent = issue_data.get("parent").filter(|value| !value.is_null());
    let children: Vec<Value> = issue_data
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    markdown_body += &format_issue_hierarchy_as_markdown(parent, &children);

    if !attachments.is_empty() {
        markdown_body += &format_attachments_as_markdown(&attachments, &attachment_paths);
    }

    let documents: Vec<Value> = issue_data
        .get("documents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !documents.is_empty() {
        markdown_body += &format_documents_as_markdown(&documents);
    }

    if let Some(derived) = &derived {
        if show_comments && !derived.visible_root_comments.is_empty() {
            markdown_body += "\n\n## Comments\n\n";
            markdown_body += &format_comments_as_markdown(
                &derived.visible_root_comments,
                &derived.replies_by_root,
            );
        }
        if show_comments && derived.hidden_resolved_thread_count > 0 {
            markdown_body += "\n\n";
            markdown_body += &format_resolved_threads_summary(derived.hidden_resolved_thread_count);
        }
    }

    output::line(&markdown_body);
    Ok(())
}

/// A parsed comment, with the author already resolved via [`comment_author`].
#[derive(Debug, Clone)]
struct Comment {
    id: String,
    body: String,
    created_at: String,
    url: String,
    resolved_at: Option<String>,
    parent_id: Option<String>,
    author: String,
}

fn parse_comment(value: &Value) -> Comment {
    let string_at = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    Comment {
        id: string_at("id").unwrap_or_default(),
        body: string_at("body").unwrap_or_default(),
        created_at: string_at("createdAt").unwrap_or_default(),
        url: string_at("url").unwrap_or_default(),
        resolved_at: string_at("resolvedAt"),
        parent_id: value
            .pointer("/parent/id")
            .and_then(Value::as_str)
            .map(str::to_string),
        author: comment_author(value),
    }
}

/// `getCommentAuthor`: the first non-empty of the four author fields.
fn comment_author(comment: &Value) -> String {
    const PATHS: [&str; 4] = [
        "/user/displayName",
        "/user/name",
        "/externalUser/displayName",
        "/externalUser/name",
    ];
    for path in PATHS {
        if let Some(name) = comment.pointer(path).and_then(Value::as_str) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    "Unknown".to_string()
}

struct CommentView {
    visible_root_comments: Vec<Comment>,
    replies_by_root: HashMap<String, Vec<Comment>>,
    hidden_resolved_thread_count: usize,
}

/// `deriveCommentView`: split comments into chronological root threads plus
/// their replies, hiding resolved roots unless asked.
fn derive_comment_view(comments: &[Comment], show_resolved_threads: bool) -> CommentView {
    let mut root_comments: Vec<Comment> = comments
        .iter()
        .filter(|comment| comment.parent_id.is_none())
        .cloned()
        .collect();
    sort_by_created(&mut root_comments);

    let by_id: HashMap<&str, &Comment> = comments
        .iter()
        .map(|comment| (comment.id.as_str(), comment))
        .collect();
    let mut root_cache: HashMap<String, String> = HashMap::new();
    let mut replies_by_root: HashMap<String, Vec<Comment>> = HashMap::new();
    for comment in comments {
        if comment.parent_id.is_none() {
            continue;
        }
        let root_id = resolve_root_id(&comment.id, &by_id, &mut root_cache);
        replies_by_root
            .entry(root_id)
            .or_default()
            .push(comment.clone());
    }
    for replies in replies_by_root.values_mut() {
        sort_by_created(replies);
    }

    let visible_root_comments: Vec<Comment> = if show_resolved_threads {
        root_comments.clone()
    } else {
        root_comments
            .iter()
            .filter(|comment| comment.resolved_at.is_none())
            .cloned()
            .collect()
    };
    let hidden_resolved_thread_count = root_comments.len() - visible_root_comments.len();

    CommentView {
        visible_root_comments,
        replies_by_root,
        hidden_resolved_thread_count,
    }
}

/// Walk `parent_id` up to a root comment, memoizing each hop. A missing comment
/// is treated as its own root, matching upstream's `comment?.parent == null`.
fn resolve_root_id(
    comment_id: &str,
    by_id: &HashMap<&str, &Comment>,
    cache: &mut HashMap<String, String>,
) -> String {
    if let Some(cached) = cache.get(comment_id) {
        return cached.clone();
    }
    let Some(comment) = by_id.get(comment_id) else {
        return comment_id.to_string();
    };
    let root_id = match &comment.parent_id {
        None => comment_id.to_string(),
        Some(parent_id) => resolve_root_id(parent_id, by_id, cache),
    };
    cache.insert(comment_id.to_string(), root_id.clone());
    root_id
}

fn sort_by_created(comments: &mut [Comment]) {
    comments.sort_by_key(|comment| time_ms(&comment.created_at));
}

fn time_ms(timestamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|date| date.timestamp_millis())
        .unwrap_or(0)
}

fn cycle_info(cycle: &Value) -> display::CycleDisplayInfo {
    let flag = |key: &str| cycle.get(key).and_then(Value::as_bool).unwrap_or(false);
    display::CycleDisplayInfo {
        number: cycle.get("number").and_then(Value::as_i64).unwrap_or(0),
        is_active: flag("isActive"),
        is_next: flag("isNext"),
        is_previous: flag("isPrevious"),
        is_past: flag("isPast"),
    }
}

/// `formatIssueHierarchyAsMarkdown`.
fn format_issue_hierarchy_as_markdown(parent: Option<&Value>, children: &[Value]) -> String {
    let mut result = String::new();

    if let Some(parent) = parent {
        result += "\n\n## Parent\n\n";
        result += &format_issue_ref_line(parent);
    }

    if !children.is_empty() {
        result += "\n\n## Sub-issues\n\n";
        for child in children {
            result += &format_issue_ref_line(child);
        }
    }

    result
}

fn format_issue_ref_line(issue: &Value) -> String {
    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
    let state = issue
        .pointer("/state/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("- **{identifier}**: {title} _[{state}]_\n")
}

/// `formatAttachmentsAsMarkdown`; `local_paths` is empty when downloads are off.
fn format_attachments_as_markdown(
    attachments: &[Value],
    local_paths: &HashMap<String, String>,
) -> String {
    if attachments.is_empty() {
        return String::new();
    }

    let mut result = String::from("\n\n## Attachments\n\n");
    for attachment in attachments {
        let url = attachment.get("url").and_then(Value::as_str).unwrap_or("");
        let title = attachment
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("");
        let source_label = attachment
            .get("sourceType")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|source| format!(" _[{source}]_"))
            .unwrap_or_default();
        let target = local_paths.get(url).map(String::as_str).unwrap_or(url);

        result += &format!("- **{title}**: {target}{source_label}\n");

        if let Some(subtitle) = attachment
            .get("subtitle")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            result += &format!("  _{subtitle}_\n");
        }
    }
    result
}

/// `formatDocumentsAsMarkdown`.
fn format_documents_as_markdown(documents: &[Value]) -> String {
    if documents.is_empty() {
        return String::new();
    }

    let mut result = String::from("\n\n## Documents\n\n");
    for document in documents {
        let title = document.get("title").and_then(Value::as_str).unwrap_or("");
        let url = document.get("url").and_then(Value::as_str).unwrap_or("");
        result += &format!("- **{title}**: {url}\n");
    }
    result
}

/// `formatCommentsAsMarkdown` (non-TTY path: no hyperlinks, plain headers).
fn format_comments_as_markdown(
    root_comments: &[Comment],
    replies_by_root: &HashMap<String, Vec<Comment>>,
) -> String {
    let mut result = String::new();

    for root in root_comments {
        let replies = replies_by_root.get(&root.id).cloned().unwrap_or_default();
        let root_date = display::format_relative_time(&root.created_at);
        let suffix = thread_header_suffix(root);

        result += &format!("- **@{}** - *{root_date}* {suffix}\n\n", root.author);
        result += &format!("  {}\n\n", root.body.replace('\n', "\n  "));

        for reply in &replies {
            let reply_date = display::format_relative_time(&reply.created_at);
            result += &format!("  - **@{}** - *{reply_date}*\n\n", reply.author);
            result += &format!("    {}\n\n", reply.body.replace('\n', "\n    "));
        }
    }

    result
}

/// `getThreadHeaderSuffix` with hyperlinks disabled (the port has no renderer).
fn thread_header_suffix(root: &Comment) -> String {
    let mut parts = vec![format!("[thread: {}]", root.id)];
    if root.resolved_at.is_some() {
        parts.push("[resolved]".to_string());
    }
    parts.join(" ")
}

/// `formatResolvedThreadsSummary`.
fn format_resolved_threads_summary(hidden_count: usize) -> String {
    let noun = if hidden_count == 1 {
        "thread"
    } else {
        "threads"
    };
    format!("Resolved {noun} hidden: {hidden_count}. Use --show-resolved-threads to show them.")
}

/// `getAttachmentCacheDir`: configured dir, else `$TMPDIR|$TMP|$TEMP|/tmp` +
/// `linear-cli-attachments`.
fn attachment_cache_dir() -> PathBuf {
    if let Some(dir) = config::attachment_dir() {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = std::env::var("TMPDIR")
        .or_else(|_| std::env::var("TMP"))
        .or_else(|_| std::env::var("TEMP"))
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/tmp".to_string());
    Path::new(&base).join("linear-cli-attachments")
}

/// `downloadAttachments`: cache each upload-host attachment under
/// `<dir>/<identifier>/<sanitized title>`. Non-upload URLs are skipped; a single
/// failure is reported on stderr but does not abort the rest.
fn download_attachments(
    identifier: &str,
    attachments: &[Value],
) -> Result<HashMap<String, String>> {
    let issue_dir = attachment_cache_dir().join(identifier);
    std::fs::create_dir_all(&issue_dir).map_err(|error| {
        CliError::cli(format!("Failed to create attachment cache dir: {error}"))
    })?;

    let mut url_to_path = HashMap::new();
    for attachment in attachments {
        let url = attachment.get("url").and_then(Value::as_str).unwrap_or("");
        let title = attachment
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("");

        if markdown::get_linear_upload_host(url).is_none() {
            continue;
        }

        let filepath = issue_dir.join(markdown::sanitize_filename(title));
        if filepath.exists() {
            url_to_path.insert(url.to_string(), filepath.to_string_lossy().into_owned());
            continue;
        }

        match markdown::download_linear_file(url, &filepath, "attachment") {
            Ok(()) => {
                url_to_path.insert(url.to_string(), filepath.to_string_lossy().into_owned());
            }
            Err(error) => {
                eprintln!(
                    "Failed to download attachment \"{title}\": {}",
                    error.user_message
                );
            }
        }
    }
    Ok(url_to_path)
}
