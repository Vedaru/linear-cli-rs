//! `linear import issues` — read an export back, and write only what differs.
//!
//! Three decisions shape this command:
//!
//! * **It is a dry run unless `--apply`.** The failure mode to avoid is a bulk write nobody could
//!   preview, so the default prints the plan - create, update with each field's before and after,
//!   or unchanged - and touches nothing.
//! * **Matching is by `identifier`, then by (team, title).** An exported row carries its
//!   identifier, so re-importing an export updates the same issues instead of creating twins; a
//!   row without one is matched by title within its team.
//! * **Only changed fields are sent.** The comparison is row-against-row through
//!   [`crate::transfer`], so an unchanged export produces no differences at all and therefore no
//!   requests - which is what makes the round trip a no-op rather than a rewrite.
//!
//! The columns it does **not** write: `identifier`/`id`/`url` (identity), `cycle` and `milestone`
//! (the API's `issueUpdate` covers both, but this command does not yet), and the timestamps.

use std::collections::BTreeMap;
use std::io::Read;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::csv::Table;
use crate::errors::{CliError, Result};
use crate::transfer::{self, ISSUE_COLUMNS};
use crate::{graphql, linear, output};

const IMPORT_CREATE_MUTATION: &str = r#"
mutation ImportIssueCreate($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue {
      id
      identifier
    }
  }
}
"#;

const IMPORT_UPDATE_MUTATION: &str = r#"
mutation ImportIssueUpdate($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
    issue {
      id
      identifier
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct ImportIssuesArgs {
    /// File to read (`-` for stdin)
    #[arg(value_name = "file", default_value = "-")]
    pub file: String,
    /// csv or json (default: the file's extension, else the first character of its content)
    #[arg(long, value_name = "format")]
    pub format: Option<String>,
    /// Write the differences; without it the run is a dry run
    #[arg(long)]
    pub apply: bool,
    /// Team key, name, or ID for rows that name no team
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Output the plan as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

/// One row's fate.
struct PlanRow {
    action: &'static str,
    identifier: String,
    title: String,
    team_key: String,
    issue_id: Option<String>,
    changes: Vec<(String, String, String)>,
    fields: Map<String, Value>,
    line: usize,
}

pub fn run(args: ImportIssuesArgs) -> Result<()> {
    let text = read_source(&args.file)?;
    let format = Format::resolve(args.format.as_deref(), &args.file, &text)?;
    let rows = parse_rows(format, &text)?;

    let default_team = match args.team.as_deref() {
        Some(team) => Some(linear::resolve_team(team)?),
        None => None,
    };

    // Every team the file mentions, plus the default one: the "what does Linear already have"
    // read has to cover each team an issue could land in.
    let mut team_keys: Vec<String> = Vec::new();
    for row in &rows {
        if let Some(key) = row.fields.get("team").and_then(Value::as_str) {
            if !key.is_empty() && !team_keys.iter().any(|existing| existing == key) {
                team_keys.push(key.to_string());
            }
        }
    }
    if let Some(team) = &default_team {
        if !team_keys.iter().any(|key| key == &team.key) {
            team_keys.push(team.key.clone());
        }
    }
    if team_keys.is_empty() {
        let key = linear::get_team_key()?;
        if let Some(key) = key {
            team_keys.push(key);
        }
    }

    let existing = existing_issues(&team_keys)?;

    let mut plan: Vec<PlanRow> = Vec::new();
    for row in &rows {
        plan.push(plan_row(row, &existing, default_team.as_ref())?);
    }

    let writes: Vec<&PlanRow> = plan
        .iter()
        .filter(|row| row.action != "unchanged")
        .collect();
    let would_write = writes.len();

    if !args.apply {
        report(&plan, &args)?;
        if !args.json {
            output::line(&format!(
                "Dry run: {would_write} issue(s) would change. Pass --apply to write them."
            ));
        }
        return Ok(());
    }

    for row in &writes {
        apply(row)?;
    }

    if args.json {
        output::print_json(&plan_json(&plan, true));
        return Ok(());
    }

    report(&plan, &args)?;
    output::line(&format!("Applied: {would_write} issue(s) changed."));
    Ok(())
}

/// One parsed row: where it came from, the fields it wants, and how it is recognised.
struct Row {
    line: usize,
    fields: Map<String, Value>,
    node: Value,
    identifier: String,
    team_key: String,
    title: String,
}

impl Row {
    /// The fields compared and written, read the way the row's own format reads them.
    fn row_fields(&self) -> &Map<String, Value> {
        &self.fields
    }
}

fn read_source(file: &str) -> Result<String> {
    if file == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| CliError::cli(format!("Failed to read stdin: {error}")))?;
        return Ok(text);
    }
    std::fs::read_to_string(file).map_err(|error| {
        CliError::cli(format!("Failed to read {file}: {error}"))
            .suggestion("Check the path, or pipe the export in with `-`.")
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Csv,
    Json,
}

impl Format {
    fn resolve(explicit: Option<&str>, file: &str, text: &str) -> Result<Format> {
        if let Some(value) = explicit {
            return match value.to_ascii_lowercase().as_str() {
                "csv" => Ok(Format::Csv),
                "json" => Ok(Format::Json),
                other => Err(CliError::validation(format!("Unknown format: {other}"))
                    .suggestion("Use csv or json.")),
            };
        }
        if file.ends_with(".csv") {
            return Ok(Format::Csv);
        }
        if file.ends_with(".json") || file.ends_with(".ndjson") {
            return Ok(Format::Json);
        }
        // Sniffing beats guessing from a name: an export's document is the shape that starts with
        // a brace, and everything else is a table.
        Ok(match text.trim_start().chars().next() {
            Some('{') | Some('[') => Format::Json,
            _ => Format::Csv,
        })
    }
}

/// The rows of the file, in the shape an import compares.
fn parse_rows(format: Format, text: &str) -> Result<Vec<Row>> {
    match format {
        Format::Csv => {
            let table = Table::parse(text)?;
            let mut rows = Vec::new();
            for (index, cells) in table.rows.iter().enumerate() {
                let fields = transfer::writable_fields(&table.header, cells);
                rows.push(row_from_parts(
                    index + 2,
                    fields,
                    Value::Null,
                    &table,
                    cells,
                ));
            }
            Ok(rows)
        }
        Format::Json => {
            let nodes = json_nodes(text)?;
            let mut rows = Vec::new();
            for (index, node) in nodes.iter().enumerate() {
                let fields = transfer::writable_fields_from_node(node);
                rows.push(row_from_parts(
                    index + 1,
                    fields,
                    node.clone(),
                    &Table {
                        header: ISSUE_COLUMNS.map(str::to_string).to_vec(),
                        rows: Vec::new(),
                    },
                    &[],
                ));
            }
            Ok(rows)
        }
    }
}

/// The nodes of a JSON document, an array of them, or an NDJSON stream.
///
/// `export --format ndjson` writes one node per line, so the same reader takes that too: a whole
/// document parses first, and a file that does not is read line by line rather than being reported
/// as malformed JSON.
fn json_nodes(text: &str) -> Result<Vec<Value>> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return match value {
            Value::Array(nodes) => Ok(nodes),
            Value::Object(object) => object
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| {
                    CliError::validation(
                        "The JSON is not an export: expected an object with a `nodes` array",
                    )
                }),
            _ => Err(CliError::validation(
                "The JSON is not an export: expected an object or an array",
            )),
        };
    }

    let mut nodes = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let node: Value = serde_json::from_str(line).map_err(|error| {
            CliError::validation(format!("Line {} is not JSON: {error}", index + 1))
        })?;
        nodes.push(node);
    }
    if nodes.is_empty() {
        return Err(CliError::validation(
            "The file has no JSON nodes: neither a document nor an NDJSON stream",
        ));
    }
    Ok(nodes)
}

/// Fill in the parts every format shares: identity, team, title.
fn row_from_parts(
    line: usize,
    fields: Map<String, Value>,
    node: Value,
    table: &Table,
    cells: &[String],
) -> Row {
    let identifier = match &node {
        Value::Null => table.cell(cells, "identifier").unwrap_or("").to_string(),
        node => node
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    };
    let title = match &node {
        Value::Null => table.cell(cells, "title").unwrap_or("").to_string(),
        node => node
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    };
    let team_key = match &node {
        Value::Null => table.cell(cells, "team").unwrap_or("").to_string(),
        node => node
            .pointer("/team/key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    };

    Row {
        line,
        fields,
        node,
        identifier,
        team_key,
        title,
    }
}

/// What Linear already has, by identifier and by (team, title).
fn existing_issues(team_keys: &[String]) -> Result<Existing> {
    let mut by_identifier: BTreeMap<String, Value> = BTreeMap::new();
    let mut by_title: BTreeMap<String, Value> = BTreeMap::new();

    for team_key in team_keys {
        let options = linear::FetchIssuesForQueryOptions {
            team_keys: Some(vec![team_key.clone()]),
            all_teams: false,
            state: None,
            assignee: None,
            unassigned: false,
            sort: None,
            limit: Some(0),
            project_id: None,
            project_label: None,
            cycle_id: None,
            milestone_id: None,
            label_names: None,
            created_after: None,
            updated_after: None,
            include_archived: Some(false),
            raw_filter: None,
        };
        let document = linear::fetch_export_issues(&options)?;
        for node in document
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(identifier) = node.get("identifier").and_then(Value::as_str) {
                by_identifier.insert(identifier.to_uppercase(), node.clone());
            }
            let title = node.get("title").and_then(Value::as_str).unwrap_or("");
            if !title.is_empty() {
                by_title
                    .entry(format!(
                        "{}::{}",
                        team_key.to_uppercase(),
                        title.to_lowercase()
                    ))
                    .or_insert_with(|| node.clone());
            }
        }
    }

    Ok(Existing {
        by_identifier,
        by_title,
    })
}

struct Existing {
    by_identifier: BTreeMap<String, Value>,
    by_title: BTreeMap<String, Value>,
}

/// Decide what one row does, and what it would change.
fn plan_row(
    row: &Row,
    existing: &Existing,
    default_team: Option<&linear::ResolvedTeam>,
) -> Result<PlanRow> {
    let team_key = if !row.team_key.is_empty() {
        linear::resolve_team(&row.team_key)?.key
    } else {
        match default_team {
            Some(team) => team.key.clone(),
            None => linear::get_team_key()?.ok_or_else(|| {
                CliError::validation(format!(
                    "Row {} names no team and no default team is configured",
                    row.line
                ))
                .suggestion("Pass --team <key, name, or ID>.")
            })?,
        }
    };

    let matched = if row.identifier.is_empty() {
        None
    } else {
        existing.by_identifier.get(&row.identifier.to_uppercase())
    };
    let matched = matched.or_else(|| {
        if row.title.is_empty() {
            return None;
        }
        existing.by_title.get(&format!(
            "{}::{}",
            team_key.to_uppercase(),
            row.title.to_lowercase()
        ))
    });

    let Some(node) = matched else {
        if row.title.is_empty() {
            return Err(CliError::validation(format!(
                "Row {} has no title, so it cannot be created",
                row.line
            )));
        }
        return Ok(PlanRow {
            action: "create",
            identifier: row.identifier.clone(),
            title: row.title.clone(),
            team_key,
            issue_id: None,
            changes: row
                .fields
                .iter()
                .map(|(field, value)| {
                    (
                        field.clone(),
                        String::new(),
                        value.as_str().unwrap_or("").to_string(),
                    )
                })
                .collect(),
            fields: row.fields.clone(),
            line: row.line,
        });
    };

    // The existing issue is read the way the *row* was read, so a column the file does not carry
    // is not compared - and a change to it is not invented.
    let existing_fields = match &row.node {
        Value::Null => transfer::writable_fields(
            &ISSUE_COLUMNS.map(str::to_string),
            &transfer::issue_row(node),
        ),
        _ => transfer::writable_fields_from_node(node),
    };
    let changes = transfer::differences(&existing_fields, row.row_fields());

    Ok(PlanRow {
        action: if changes.is_empty() {
            "unchanged"
        } else {
            "update"
        },
        identifier: node
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or(&row.identifier)
            .to_string(),
        title: row.title.clone(),
        team_key,
        issue_id: node.get("id").and_then(Value::as_str).map(str::to_string),
        changes,
        fields: row.fields.clone(),
        line: row.line,
    })
}

/// Write one planned row.
fn apply(row: &PlanRow) -> Result<()> {
    let client = graphql::client()?;
    let changed: Vec<String> = row
        .changes
        .iter()
        .map(|(field, _, _)| field.clone())
        .collect();
    let input = write_input(row, &changed)?;

    match row.action {
        "create" => {
            let document = client.request(
                IMPORT_CREATE_MUTATION,
                json!({ "input": Value::Object(input) }),
            )?;
            let created = document
                .get("issueCreate")
                .ok_or_else(|| CliError::cli("Linear API response did not contain issueCreate"))?;
            if created.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(CliError::cli(format!(
                    "Failed to create the issue for row {}",
                    row.line
                )));
            }
            let identifier = created
                .pointer("/issue/identifier")
                .and_then(Value::as_str)
                .unwrap_or(&row.title);
            output::line(&format!("✓ Created {identifier}: {}", row.title));
        }
        "update" => {
            let Some(id) = row.issue_id.as_deref() else {
                return Err(CliError::cli(format!(
                    "Row {} matched an issue with no id",
                    row.line
                )));
            };
            let document = client.request(
                IMPORT_UPDATE_MUTATION,
                json!({ "id": id, "input": Value::Object(input) }),
            )?;
            let updated = document
                .get("issueUpdate")
                .ok_or_else(|| CliError::cli("Linear API response did not contain issueUpdate"))?;
            if updated.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(CliError::cli(format!(
                    "Failed to update {} (row {})",
                    row.identifier, row.line
                )));
            }
            output::line(&format!(
                "✓ Updated {}: {}",
                row.identifier,
                changed.join(", ")
            ));
        }
        _ => {}
    }

    Ok(())
}

/// The `IssueCreateInput`/`IssueUpdateInput` for the fields that changed.
fn write_input(row: &PlanRow, changed: &[String]) -> Result<Map<String, Value>> {
    let mut input = Map::new();

    for field in changed {
        let raw = row
            .fields
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match field.as_str() {
            "title" => {
                input.insert("title".to_string(), json!(raw));
            }
            "description" => {
                input.insert("description".to_string(), json!(raw));
            }
            "priority" => match raw.parse::<i64>() {
                Ok(priority) => {
                    input.insert("priority".to_string(), json!(priority));
                }
                Err(_) => {
                    return Err(CliError::validation(format!(
                        "Row {}: priority '{raw}' is not a number",
                        row.line
                    )))
                }
            },
            "estimate" => {
                let estimate: Value = match raw.parse::<i64>() {
                    Ok(estimate) => json!(estimate),
                    Err(_) => Value::Null,
                };
                input.insert("estimate".to_string(), estimate);
            }
            "dueDate" => {
                let due: Value = if raw.is_empty() {
                    Value::Null
                } else {
                    json!(raw)
                };
                input.insert("dueDate".to_string(), due);
            }
            "assignee" => {
                let assignee: Value = if raw.is_empty() {
                    Value::Null
                } else {
                    match linear::lookup_user_id(&raw)? {
                        Some(id) => json!(id),
                        None => return Err(CliError::not_found("User", &raw)),
                    }
                };
                input.insert("assigneeId".to_string(), assignee);
            }
            "project" => {
                let project: Value = if raw.is_empty() {
                    Value::Null
                } else {
                    json!(linear::resolve_project_id(&raw)?)
                };
                input.insert("projectId".to_string(), project);
            }
            "state" => {
                let states = linear::get_workflow_states(&row.team_key)?;
                let state = linear::resolve_workflow_state(&states, &raw)?.ok_or_else(|| {
                    linear::workflow_state_not_found_error(&row.team_key, &raw, &states)
                })?;
                input.insert("stateId".to_string(), json!(state.id));
            }
            "labels" => {
                let mut ids: Vec<String> = Vec::new();
                for name in transfer::split_labels(&raw) {
                    let id = linear::get_issue_label_id_by_name_for_team(&name, &row.team_key)?
                        .ok_or_else(|| CliError::not_found("Label", &name))?;
                    ids.push(id);
                }
                input.insert("labelIds".to_string(), json!(ids));
            }
            "team" => {
                let team = linear::resolve_team(&raw)?;
                input.insert("teamId".to_string(), json!(team.id));
            }
            other => {
                return Err(CliError::cli(format!(
                    "Row {}: '{other}' is not a field this import writes",
                    row.line
                )))
            }
        }
    }

    // A create needs a team; an update that moved the issue already carries one.
    if row.action == "create" && !input.contains_key("teamId") {
        let team = linear::resolve_team(&row.team_key)?;
        input.insert("teamId".to_string(), json!(team.id));
    }

    Ok(input)
}

/// The human-readable plan (or the JSON one with `--json`).
fn report(plan: &[PlanRow], args: &ImportIssuesArgs) -> Result<()> {
    if args.json {
        output::print_json(&plan_json(plan, args.apply));
        return Ok(());
    }

    for row in plan {
        match row.action {
            "create" => output::line(&format!(
                "would create  {}  {} [{}]",
                if row.identifier.is_empty() {
                    "(new)"
                } else {
                    row.identifier.as_str()
                },
                row.title,
                row.team_key
            )),
            "update" => {
                output::line(&format!("would update  {}  {}", row.identifier, row.title));
                for (field, before, after) in &row.changes {
                    output::line(&format!("    {field}: \"{before}\" → \"{after}\""));
                }
            }
            _ => output::line(&format!("unchanged     {}  {}", row.identifier, row.title)),
        }
    }
    Ok(())
}

fn plan_json(plan: &[PlanRow], applied: bool) -> Value {
    let rows: Vec<Value> = plan
        .iter()
        .map(|row| {
            json!({
                "action": row.action,
                "identifier": row.identifier,
                "title": row.title,
                "team": row.team_key,
                "line": row.line,
                "changes": row
                    .changes
                    .iter()
                    .map(|(field, before, after)| json!({
                        "field": field,
                        "from": before,
                        "to": after
                    }))
                    .collect::<Vec<Value>>(),
            })
        })
        .collect();

    json!({
        "applied": applied,
        "create": plan.iter().filter(|row| row.action == "create").count(),
        "update": plan.iter().filter(|row| row.action == "update").count(),
        "unchanged": plan.iter().filter(|row| row.action == "unchanged").count(),
        "plan": rows,
    })
}
