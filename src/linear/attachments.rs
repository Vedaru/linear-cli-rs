//! Issue attachments: the sidebar links on an issue.
//!
//! `issue attach` and `issue link` create them; the API's `attachment(id:)`, `attachmentUpdate`
//! and `attachmentDelete` are the other three quarters, so a link created with the wrong title -
//! or an integration's PR link that went stale - could only be corrected in the app.
//!
//! Three shape facts decide how the commands are written, all read from the schema:
//!
//! * `attachment(id: String!): Attachment!` is **non-null**, so an id that does not exist is an
//!   `Entity not found: Attachment` error rather than an empty node, and `get` has no "not found"
//!   branch of its own to invent.
//! * `AttachmentUpdateInput.title` is **required**. There is no partial update that leaves the
//!   title alone, so an update that does not name one must send the title the attachment already
//!   has - which is why `update_attachment`'s caller reads before it writes.
//! * the URL has no place in the update input at all: it *is* the attachment's identity within an
//!   issue (`attachmentCreate` answers "creates a new attachment, or updates existing if the same
//!   `url` and `issueId` is used"). Changing it is therefore a re-link - create at the new URL,
//!   then delete the old - which is what `issue attachment update --url` does, deliberately in
//!   that order so a failure to create never leaves the link gone.

use super::prelude::*;
use super::*;

const LIST_ATTACHMENTS_QUERY: &str = r#"
query IssueAttachments($id: String!, $first: Int, $after: String) {
  issue(id: $id) {
    id
    identifier
    attachments(first: $first, after: $after) {
      nodes {
        id
        title
        subtitle
        url
        sourceType
        metadata
        createdAt
        updatedAt
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const GET_ATTACHMENT_QUERY: &str = r#"
query GetAttachment($id: String!) {
  attachment(id: $id) {
    id
    title
    subtitle
    url
    sourceType
    metadata
    createdAt
    updatedAt
    issue {
      id
      identifier
    }
  }
}
"#;

const CREATE_ATTACHMENT_MUTATION: &str = r#"
mutation AttachmentCreateForRelink($input: AttachmentCreateInput!) {
  attachmentCreate(input: $input) {
    success
    attachment {
      id
      title
      subtitle
      url
      issue {
        identifier
      }
    }
  }
}
"#;

const UPDATE_ATTACHMENT_MUTATION: &str = r#"
mutation AttachmentUpdate($id: String!, $input: AttachmentUpdateInput!) {
  attachmentUpdate(id: $id, input: $input) {
    success
    attachment {
      id
      title
      subtitle
      url
      issue {
        identifier
      }
    }
  }
}
"#;

const DELETE_ATTACHMENT_MUTATION: &str = r#"
mutation AttachmentDelete($id: String!) {
  attachmentDelete(id: $id) {
    success
  }
}
"#;

/// Every attachment on an issue, following pages, with the last page's `pageInfo`.
///
/// The issue is addressed the way the API accepts it - a UUID *or* an identifier like `ENG-123` -
/// so the caller passes what the user typed rather than resolving twice. `attachmentList`'s
/// callers want the identifier back for their own sentences, and `get_issue_identifier` is the
/// resolver every other command already funnels loose input through.
pub fn list_issue_attachments(issue: &str) -> Result<(String, Vec<Value>, Value)> {
    let client = graphql::client()?;
    // Resolve before the request: the query declares `$id: String!` and Linear ignores an
    // undeclared/unset variable by refusing the whole operation, so the resolved identifier has to
    // be in `variables` - not merely computed afterwards for the human sentence.
    let identifier = get_issue_identifier(Some(issue))?.unwrap_or_else(|| issue.to_string());
    let mut variables = Map::new();
    variables.insert("id".to_string(), json!(identifier));
    let (nodes, page_info) = client.paginate_connection_page(
        LIST_ATTACHMENTS_QUERY,
        variables,
        &["issue", "attachments"],
    )?;
    Ok((identifier, nodes, page_info))
}

/// One attachment, by its id.
///
/// `attachment(id:)` is non-null in the schema, so an unknown id arrives as Linear's own
/// `Entity not found: Attachment` refusal rather than a null node - the error is passed through
/// rather than restated as a not-found we invented.
pub fn get_attachment(id: &str) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(GET_ATTACHMENT_QUERY, json!({ "id": id }))?;
    data.get("attachment")
        .filter(|node| !node.is_null())
        .cloned()
        .ok_or_else(|| CliError::not_found("Attachment", id))
}

/// Create an attachment (the write half of `--url`; `issue attach` owns the file-upload path).
///
/// Returns the API's own response document, the way `issue create --json` does, so `--json` can
/// print the server's shape verbatim and the text path digs the node out of the same value.
pub fn create_attachment(input: Value) -> Result<Value> {
    let client = graphql::client()?;
    client.request(CREATE_ATTACHMENT_MUTATION, json!({ "input": input }))
}

/// The API's `attachmentUpdate`. The input must carry a title: see the module note.
pub fn update_attachment(id: &str, input: Value) -> Result<Value> {
    let client = graphql::client()?;
    client.request(
        UPDATE_ATTACHMENT_MUTATION,
        json!({ "id": id, "input": input }),
    )
}

/// The API's `attachmentDelete`, which is permanent: the schema offers no restore for an
/// attachment, so the command that calls this one confirms first.
///
/// Returns the whole response document, the same shape `create` and `update` answer, so `--json`
/// prints one kind of thing across the group. `entityId` is the API's own confirmation of what it
/// removed, and the caller compares it against the id it sent rather than trusting the input.
pub fn delete_attachment(id: &str) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(DELETE_ATTACHMENT_MUTATION, json!({ "id": id }))?;
    let result = data
        .get("attachmentDelete")
        .ok_or_else(|| CliError::cli("Linear API response did not contain attachmentDelete"))?;
    if result.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli(format!(
            "Linear refused to delete attachment {id}"
        )));
    }
    Ok(data)
}

/// The attachment a create/update response carries, refusing a `success: false`.
pub fn attachment_from(document: &Value, mutation: &str) -> Result<Value> {
    let result = document
        .get(mutation)
        .ok_or_else(|| CliError::cli(format!("Linear API response did not contain {mutation}")))?;
    if result.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli(format!(
            "Linear refused the {mutation} request"
        )));
    }
    result
        .get("attachment")
        .filter(|node| !node.is_null())
        .cloned()
        .ok_or_else(|| CliError::cli(format!("{mutation} returned no attachment")))
}
