//! `linear issue relation` — port of `src/commands/issue/issue-relation.ts`.
//!
//! Manages issue relations (dependencies): `add`, `delete`, and `list`. The
//! group itself has no action; with no subcommand it prints help, matching
//! upstream's `this.showHelp()`.

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::errors::{self, CliError, Result};
use crate::linear;
use crate::{graphql, output};

/// Manage issue relations (dependencies)
#[derive(Args, Debug)]
pub struct IssueRelationArgs {
    #[command(subcommand)]
    pub command: Option<RelationCommand>,
}

#[derive(Subcommand, Debug)]
pub enum RelationCommand {
    /// Add a relation between two issues
    Add(RelationAddArgs),
    /// Delete a relation between two issues
    Delete(RelationDeleteArgs),
    /// List relations for an issue
    List(RelationListArgs),
}

#[derive(Args, Debug)]
pub struct RelationAddArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Relation type: blocks, blocked-by, related, duplicate
    #[arg(value_name = "relationType")]
    pub relation_type: String,
    /// Related issue ID (e.g., ENG-456)
    #[arg(value_name = "relatedIssueId")]
    pub related_issue_id: String,
}

#[derive(Args, Debug)]
pub struct RelationDeleteArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Relation type: blocks, blocked-by, related, duplicate
    #[arg(value_name = "relationType")]
    pub relation_type: String,
    /// Related issue ID (e.g., ENG-456)
    #[arg(value_name = "relatedIssueId")]
    pub related_issue_id: String,
}

#[derive(Args, Debug)]
pub struct RelationListArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Output the relations as JSON (an addition to upstream)
    #[arg(short = 'j', long)]
    pub json: bool,
}

const RELATION_TYPES: [&str; 4] = ["blocks", "blocked-by", "related", "duplicate"];

const CREATE_ISSUE_RELATION_MUTATION: &str = r#"
mutation CreateIssueRelation($input: IssueRelationCreateInput!) {
  issueRelationCreate(input: $input) {
    success
    issueRelation {
      id
    }
  }
}
"#;

const FIND_ISSUE_RELATION_QUERY: &str = r#"
query FindIssueRelation($issueId: String!) {
  issue(id: $issueId) {
    relations {
      nodes {
        id
        type
        relatedIssue { id }
      }
    }
  }
}
"#;

const DELETE_ISSUE_RELATION_MUTATION: &str = r#"
mutation DeleteIssueRelation($id: String!) {
  issueRelationDelete(id: $id) {
    success
  }
}
"#;

const LIST_ISSUE_RELATIONS_QUERY: &str = r#"
query ListIssueRelations($issueId: String!) {
  issue(id: $issueId) {
    identifier
    title
    relations {
      nodes {
        id
        type
        relatedIssue {
          identifier
          title
        }
      }
    }
    inverseRelations {
      nodes {
        id
        type
        issue {
          identifier
          title
        }
      }
    }
  }
}
"#;

pub fn run(args: IssueRelationArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <IssueRelationArgs as clap::Args>::augment_args(clap::Command::new("relation"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        RelationCommand::Add(a) => {
            add_relation(a).map_err(|error| error.with_context("Failed to create relation"))
        }
        RelationCommand::Delete(a) => {
            delete_relation(a).map_err(|error| error.with_context("Failed to delete relation"))
        }
        RelationCommand::List(a) => {
            list_relations(a).map_err(|error| error.with_context("Failed to list relations"))
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn validate_relation_type(arg: &str) -> Result<String> {
    let relation_type = arg.to_lowercase();
    if !RELATION_TYPES.contains(&relation_type.as_str()) {
        return Err(
            CliError::validation(format!("Invalid relation type: {arg}"))
                .suggestion("Must be one of: blocks, blocked-by, related, duplicate"),
        );
    }
    Ok(relation_type)
}

/// Map CLI-friendly names to Linear API types. "blocked-by" is implemented by
/// reversing the issue order with "blocks".
fn api_relation_type(relation_type: &str) -> &'static str {
    match relation_type {
        "blocked-by" | "blocks" => "blocks",
        "related" => "related",
        "duplicate" => "duplicate",
        _ => "",
    }
}

/// Turn loose input into a canonical identifier, or the validation error
/// upstream uses in `add`/`delete`.
fn resolve_relation_identifier(arg: &str) -> Result<String> {
    match linear::get_issue_identifier(Some(arg))? {
        Some(identifier) => Ok(identifier),
        None => Err(CliError::validation(format!(
            "Could not resolve issue identifier: {arg}"
        ))),
    }
}

fn resolve_issue_uuid(identifier: &str) -> Result<String> {
    match linear::get_issue_id(identifier) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(CliError::not_found("Issue", identifier)),
        Err(error) if error.is_not_found() => Err(CliError::not_found("Issue", identifier)),
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------

fn add_relation(args: RelationAddArgs) -> Result<()> {
    let relation_type = validate_relation_type(&args.relation_type)?;

    let issue_identifier = resolve_relation_identifier(&args.issue_id)?;
    let related_identifier = resolve_relation_identifier(&args.related_issue_id)?;

    let issue_id = resolve_issue_uuid(&issue_identifier)?;
    let related_issue_id = resolve_issue_uuid(&related_identifier)?;

    // For "blocked-by", swap the issues so the relation is correct:
    // "A blocked-by B" means "B blocks A".
    let api_type = api_relation_type(&relation_type);
    let (from_id, to_id) = if relation_type == "blocked-by" {
        (related_issue_id, issue_id)
    } else {
        (issue_id, related_issue_id)
    };

    let client = graphql::client()?;
    let data = client.request(
        CREATE_ISSUE_RELATION_MUTATION,
        json!({
            "input": {
                "issueId": from_id,
                "relatedIssueId": to_id,
                "type": api_type,
            }
        }),
    )?;

    let created = data
        .get("issueRelationCreate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !created {
        return Err(CliError::cli("Failed to create relation"));
    }

    let has_relation = data
        .get("issueRelationCreate")
        .and_then(|value| value.get("issueRelation"))
        .map(|value| !value.is_null())
        .unwrap_or(false);
    if has_relation {
        output::line(&format!(
            "✓ Created relation: {issue_identifier} {relation_type} {related_identifier}"
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

fn delete_relation(args: RelationDeleteArgs) -> Result<()> {
    let relation_type = validate_relation_type(&args.relation_type)?;

    let issue_identifier = resolve_relation_identifier(&args.issue_id)?;
    let related_identifier = resolve_relation_identifier(&args.related_issue_id)?;

    let issue_id = resolve_issue_uuid(&issue_identifier)?;
    let related_issue_id = resolve_issue_uuid(&related_identifier)?;

    let api_type = api_relation_type(&relation_type);
    let (from_id, to_id) = if relation_type == "blocked-by" {
        (related_issue_id, issue_id)
    } else {
        (issue_id, related_issue_id)
    };

    let client = graphql::client()?;
    let find_data = client.request(FIND_ISSUE_RELATION_QUERY, json!({ "issueId": from_id }))?;

    let relation_id = find_data
        .get("issue")
        .and_then(|issue| issue.get("relations"))
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| {
            nodes.iter().find(|node| {
                node.get("type").and_then(Value::as_str) == Some(api_type)
                    && node
                        .get("relatedIssue")
                        .and_then(|related| related.get("id"))
                        .and_then(Value::as_str)
                        == Some(to_id.as_str())
            })
        })
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(relation_id) = relation_id else {
        return Err(CliError::not_found(
            "Relation",
            &format!("{relation_type} between {issue_identifier} and {related_identifier}"),
        ));
    };

    let delete_data =
        client.request(DELETE_ISSUE_RELATION_MUTATION, json!({ "id": relation_id }))?;
    let deleted = delete_data
        .get("issueRelationDelete")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !deleted {
        return Err(CliError::cli("Failed to delete relation"));
    }

    output::line(&format!(
        "✓ Deleted relation: {issue_identifier} {relation_type} {related_identifier}"
    ));

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list_relations(args: RelationListArgs) -> Result<()> {
    let resolved_identifier = linear::get_issue_identifier(args.issue_id.as_deref())?;
    let Some(identifier_input) = resolved_identifier else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    let client = graphql::client()?;
    let data = errors::translate_not_found("Issue", &identifier_input, || {
        client.request(
            LIST_ISSUE_RELATIONS_QUERY,
            json!({ "issueId": identifier_input }),
        )
    })?;

    let Some(issue) = data.get("issue").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Issue", &identifier_input));
    };

    if args.json {
        // The raw GraphQL shape: `identifier`, `relations` and `inverseRelations`
        // exactly as the query asked for them. Both directions are in one
        // document, which is the reason this is not a flat list a caller has to
        // interpret with a second rule.
        output::print_json(&data);
        return Ok(());
    }

    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");

    let outgoing: Vec<Value> = issue
        .get("relations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let incoming: Vec<Value> = issue
        .get("inverseRelations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    output::line(&format!("Relations for {identifier}: {title}"));
    output::blank();

    if outgoing.is_empty() && incoming.is_empty() {
        output::line("  No relations");
        return Ok(());
    }

    if !outgoing.is_empty() {
        output::line("Outgoing:");
        for rel in &outgoing {
            let rel_type = rel.get("type").and_then(Value::as_str).unwrap_or("");
            let related_identifier = rel
                .get("relatedIssue")
                .and_then(|related| related.get("identifier"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let related_title = rel
                .get("relatedIssue")
                .and_then(|related| related.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("");
            output::line(&format!(
                "  {identifier} {rel_type} {related_identifier}: {related_title}"
            ));
        }
    }

    if !incoming.is_empty() {
        if !outgoing.is_empty() {
            output::blank();
        }
        output::line("Incoming:");
        for rel in &incoming {
            // Show the inverse perspective.
            let rel_type = rel.get("type").and_then(Value::as_str).unwrap_or("");
            let display_type = if rel_type == "blocks" {
                "blocked-by"
            } else {
                rel_type
            };
            let related_identifier = rel
                .get("issue")
                .and_then(|related| related.get("identifier"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let related_title = rel
                .get("issue")
                .and_then(|related| related.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("");
            output::line(&format!(
                "  {identifier} {display_type} {related_identifier}: {related_title}"
            ));
        }
    }

    Ok(())
}
