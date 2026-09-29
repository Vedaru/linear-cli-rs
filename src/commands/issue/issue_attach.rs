//! `linear issue attach` — port of `src/commands/issue/issue-attach.ts`.
//!
//! Creates a sidebar link attachment on an issue. Images are uploaded to a
//! signed URL first; unlike `issue comment add --attach`, the resulting link is
//! a sidebar attachment and does not render inline, so image uploads print a
//! hint pointing at the inline-capable command.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::hyperlink;
use crate::linear;
use crate::{graphql, output, upload};

#[derive(Args, Debug)]
pub struct IssueAttachArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// File to upload
    #[arg(value_name = "filepath")]
    pub filepath: String,
    /// Custom title for the attachment
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Create a linked comment with this body; the file remains a sidebar
    /// attachment
    #[arg(short = 'c', long, value_name = "body")]
    pub comment: Option<String>,
    /// Upload images to a public, unauthenticated URL (default: private,
    /// workspace-members only)
    #[arg(long)]
    pub public: bool,
}

const ATTACHMENT_CREATE_MUTATION: &str = r#"
mutation AttachmentCreate($input: AttachmentCreateInput!) {
  attachmentCreate(input: $input) {
    success
    attachment {
      id
      url
      title
    }
  }
}
"#;

/// Quote a value for safe copy-paste into a shell command.
fn quote_for_shell(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./:@%+=-".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn run(args: IssueAttachArgs) -> Result<()> {
    attach(args).map_err(|error| error.with_context("Failed to attach file"))
}

fn attach(args: IssueAttachArgs) -> Result<()> {
    let Some(resolved_identifier) = linear::get_issue_identifier(Some(&args.issue_id))? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    upload::validate_file_path(&args.filepath)?;

    // attachmentCreate needs the issue UUID, not the identifier.
    let issue_uuid = match linear::get_issue_id(&resolved_identifier) {
        Ok(Some(id)) => id,
        Ok(None) => return Err(CliError::not_found("Issue", &resolved_identifier)),
        Err(error) if error.is_not_found() => {
            return Err(CliError::not_found("Issue", &resolved_identifier));
        }
        Err(error) => return Err(error),
    };

    let upload_result = upload::upload_file(
        &args.filepath,
        &upload::UploadOptions {
            make_public: Some(args.public),
        },
    )?;
    output::line(&format!("✓ Uploaded {}", upload_result.filename));
    if upload_result.public {
        eprintln!(
            "⚠ Uploaded to a public URL readable by anyone: {}",
            upload_result.asset_url
        );
    }

    let attachment_title = args
        .title
        .clone()
        .unwrap_or_else(|| basename(&args.filepath));

    let mut input = Map::new();
    input.insert("issueId".to_string(), Value::String(issue_uuid));
    input.insert("title".to_string(), Value::String(attachment_title));
    input.insert(
        "url".to_string(),
        Value::String(upload_result.asset_url.clone()),
    );
    if let Some(comment) = &args.comment {
        input.insert("commentBody".to_string(), Value::String(comment.clone()));
    }

    let client = graphql::client()?;
    let data = client.request(
        ATTACHMENT_CREATE_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;

    let created = data
        .get("attachmentCreate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !created {
        return Err(CliError::cli("Failed to create attachment"));
    }

    let attachment = data
        .get("attachmentCreate")
        .and_then(|value| value.get("attachment"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Failed to create attachment"))?;
    let title = attachment.get("title").and_then(Value::as_str).unwrap_or("");
    let url = attachment.get("url").and_then(Value::as_str).unwrap_or("");

    output::line(&format!("✓ Sidebar link attachment created: {title}"));
    output::line(url);

    if upload_result.content_type.starts_with("image/") {
        let mut suggested = vec![
            "linear issue comment add".to_string(),
            resolved_identifier.clone(),
            "--attach".to_string(),
            quote_for_shell(&args.filepath),
        ];
        if args.public {
            suggested.push("--public".to_string());
        }
        output::line(&format!(
            "Hint: Sidebar link attachments do not render images inline. For inline display, run: {}",
            suggested.join(" ")
        ));
    }

    // Upstream only elapses the spinner guard around the upload; keep the
    // import meaningful so the behaviour stays documented.
    let _ = hyperlink::should_show_spinner();

    Ok(())
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}
