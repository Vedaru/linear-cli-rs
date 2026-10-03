//! The row model shared by `linear export` and `linear import`.
//!
//! One definition of "what an issue looks like as a row", used in both directions, is what makes
//! an export re-import to a no-op: the comparison on import is row-against-row, and neither side
//! invents its own formatting. The JSON path needs no mapping at all - it *is* the document the
//! API layer already produces (`{nodes, pageInfo}` of issue nodes) - which is why only CSV needs
//! a table here.
//!
//! Only [`WRITABLE_ISSUE_FIELDS`] are compared and written. The rest (`identifier`, `id`, `url`,
//! `cycle`, `milestone`, `updatedAt`) travel with the export because a row without them is hard to
//! read and impossible to match, but an import that silently rewrote a cycle from a column nobody
//! asked it to write would be worse than one that says which columns it leaves alone.

use serde_json::{Map, Value};

use crate::csv::{encode_row, Table};

/// The CSV columns of an issue export, in the order they are written.
pub const ISSUE_COLUMNS: [&str; 16] = [
    "identifier",
    "title",
    "state",
    "assignee",
    "priority",
    "estimate",
    "project",
    "labels",
    "dueDate",
    "cycle",
    "milestone",
    "team",
    "description",
    "url",
    "id",
    "updatedAt",
];

/// The columns `linear import issues` will write back, and the only ones it compares.
pub const WRITABLE_ISSUE_FIELDS: [&str; 10] = [
    "title",
    "description",
    "state",
    "assignee",
    "priority",
    "estimate",
    "project",
    "labels",
    "dueDate",
    "team",
];

/// The CSV columns of a project export.
pub const PROJECT_COLUMNS: [&str; 12] = [
    "name",
    "status",
    "health",
    "priority",
    "lead",
    "teams",
    "startDate",
    "targetDate",
    "url",
    "id",
    "createdAt",
    "updatedAt",
];

/// The separator inside the `labels` cell.
///
/// A comma would be legal CSV (the codec would quote it) but it would also make the cell ambiguous
/// with a label whose name contains one, so the export picks a character Linear does not use in
/// labels and the reader splits on that.
pub const LABEL_SEPARATOR: char = '|';

/// The header line for an issue export.
pub fn issue_header() -> String {
    encode_row(&ISSUE_COLUMNS.map(str::to_string))
}

/// The header line for a project export.
pub fn project_header() -> String {
    encode_row(&PROJECT_COLUMNS.map(str::to_string))
}

/// One issue node as a row, in [`ISSUE_COLUMNS`] order.
pub fn issue_row(node: &Value) -> Vec<String> {
    vec![
        text(node, "identifier"),
        text(node, "title"),
        pointer(node, "/state/name"),
        pointer(node, "/assignee/displayName"),
        number(node, "priority"),
        optional_number(node, "estimate"),
        pointer(node, "/project/name"),
        label_names(node).join(&LABEL_SEPARATOR.to_string()),
        text(node, "dueDate"),
        cycle_label(node),
        pointer(node, "/projectMilestone/name"),
        pointer(node, "/team/key"),
        text(node, "description"),
        text(node, "url"),
        text(node, "id"),
        text(node, "updatedAt"),
    ]
}

/// One project node as a row, in [`PROJECT_COLUMNS`] order.
pub fn project_row(node: &Value) -> Vec<String> {
    vec![
        text(node, "name"),
        pointer(node, "/status/name"),
        text(node, "health"),
        number(node, "priority"),
        pointer(node, "/lead/displayName"),
        team_keys(node).join(&LABEL_SEPARATOR.to_string()),
        text(node, "startDate"),
        text(node, "targetDate"),
        text(node, "url"),
        text(node, "id"),
        text(node, "createdAt"),
        text(node, "updatedAt"),
    ]
}

/// The desired value of every writable column the row carries, normalised.
///
/// The same function reads the existing issue (through [`issue_row`]) and the incoming row, so the
/// two sides cannot disagree about what "the same" means.
pub fn writable_fields(header: &[String], row: &[String]) -> Map<String, Value> {
    let table = Table {
        header: header.to_vec(),
        rows: Vec::new(),
    };

    let mut fields = Map::new();
    for column in WRITABLE_ISSUE_FIELDS {
        let Some(cell) = table.cell(row, column) else {
            continue;
        };
        fields.insert(column.to_string(), Value::String(normalize(column, cell)));
    }
    fields
}

/// The same fields read straight off a node, for a JSON import.
///
/// The difference from [`writable_fields`] is absence: a CSV column that is present but empty is
/// an explicit empty (that is how an assignee is cleared), while a JSON key that is *missing* says
/// nothing at all and must not be read as "set this to empty".
pub fn writable_fields_from_node(node: &Value) -> Map<String, Value> {
    let row = issue_row(node);
    let header = ISSUE_COLUMNS.map(str::to_string);
    let table = Table {
        header: header.to_vec(),
        rows: Vec::new(),
    };

    let mut fields = Map::new();
    for column in WRITABLE_ISSUE_FIELDS {
        let raw = match column {
            "title" | "description" | "dueDate" => node
                .get(column)
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "state" => node
                .get("state")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "assignee" => node
                .get("assignee")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "project" => node
                .get("project")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "labels" => node
                .pointer("/labels/nodes")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "priority" => node
                .get("priority")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "estimate" => node
                .get("estimate")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            "team" => node
                .get("team")
                .map(|_| table.cell(&row, column).unwrap_or("")),
            _ => None,
        };
        if let Some(raw) = raw {
            fields.insert(column.to_string(), Value::String(normalize(column, raw)));
        }
    }
    fields
}

/// How one column's cell is compared, in both directions.
fn normalize(column: &str, cell: &str) -> String {
    match column {
        // Numbers are compared as numbers: a hand-edited "03" is the same estimate as "3".
        "priority" | "estimate" => match cell.trim().parse::<i64>() {
            Ok(number) => number.to_string(),
            Err(_) => cell.trim().to_string(),
        },
        // Labels are a set: reordering them in a spreadsheet is not an edit, so both sides are
        // compared in one sorted order.
        "labels" => {
            let mut names = split_labels(cell);
            names.sort();
            names.join(&LABEL_SEPARATOR.to_string())
        }
        _ => cell.trim().to_string(),
    }
}

/// The writable fields whose value differs, as `(field, existing, desired)`.
pub fn differences(
    existing: &Map<String, Value>,
    desired: &Map<String, Value>,
) -> Vec<(String, String, String)> {
    let mut changed: Vec<(String, String, String)> = Vec::new();
    for (field, desired_value) in desired {
        let before = existing
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let after = desired_value.as_str().unwrap_or("").to_string();
        if before != after {
            changed.push((field.clone(), before, after));
        }
    }
    changed
}

/// The label names in a `labels` cell, trimmed and without the empties.
pub fn split_labels(cell: &str) -> Vec<String> {
    cell.split(LABEL_SEPARATOR)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn label_names(node: &Value) -> Vec<String> {
    let mut names: Vec<String> = node
        .pointer("/labels/nodes")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn team_keys(node: &Value) -> Vec<String> {
    let mut keys: Vec<String> = node
        .pointer("/teams/nodes")
        .and_then(Value::as_array)
        .map(|teams| {
            teams
                .iter()
                .filter_map(|team| team.get("key").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

/// `name`, else `#number`, else empty - the same label `cycle list` prints.
fn cycle_label(node: &Value) -> String {
    let Some(cycle) = node.get("cycle").filter(|cycle| !cycle.is_null()) else {
        return String::new();
    };
    match cycle.get("name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => cycle
            .get("number")
            .and_then(Value::as_i64)
            .map(|number| format!("#{number}"))
            .unwrap_or_default(),
    }
}

fn text(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn pointer(node: &Value, path: &str) -> String {
    node.pointer(path)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn number(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_i64)
        .map(|number| number.to_string())
        .unwrap_or_default()
}

fn optional_number(node: &Value, name: &str) -> String {
    node.get(name)
        .filter(|value| !value.is_null())
        .and_then(Value::as_i64)
        .map(|number| number.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "id": "issue-1",
            "identifier": "ENG-1",
            "title": "Ship it",
            "description": "line one\nline two",
            "priority": 2,
            "estimate": 3,
            "dueDate": "2026-10-10",
            "url": "https://linear.app/example/issue/ENG-1",
            "updatedAt": "2026-10-01T00:00:00.000Z",
            "state": { "id": "s-1", "name": "In Progress", "type": "started" },
            "assignee": { "id": "u-1", "displayName": "Ada" },
            "team": { "id": "t-1", "key": "ENG" },
            "project": { "id": "p-1", "name": "Board" },
            "projectMilestone": { "id": "m-1", "name": "M7" },
            "cycle": { "id": "c-1", "number": 7, "name": null },
            "labels": { "nodes": [
                { "id": "l-2", "name": "Improvement" },
                { "id": "l-1", "name": "Bug" }
            ] }
        })
    }

    #[test]
    fn a_row_carries_every_column() {
        let row = issue_row(&sample());
        assert_eq!(row.len(), ISSUE_COLUMNS.len());
        let fields = writable_fields(&ISSUE_COLUMNS.map(str::to_string), &row);
        // Read-only columns are not part of the writable set at all.
        assert!(fields.get("identifier").is_none());
        assert_eq!(fields["title"], json!("Ship it"));
        assert_eq!(fields["state"], json!("In Progress"));
        assert_eq!(fields["assignee"], json!("Ada"));
        assert_eq!(fields["priority"], json!("2"));
        assert_eq!(fields["labels"], json!("Bug|Improvement"));
        assert_eq!(fields["team"], json!("ENG"));
    }

    /// The point of the whole module: reading a row it wrote gives back the same fields, so an
    /// unchanged export has no differences to apply.
    #[test]
    fn a_row_read_back_is_identical_to_the_node_it_came_from() {
        let header = ISSUE_COLUMNS.map(str::to_string);
        let node = sample();
        let first = writable_fields(&header, &issue_row(&node));
        let second = writable_fields(&header, &issue_row(&node));
        assert!(differences(&first, &second).is_empty());
    }

    #[test]
    fn a_hand_edit_shows_up_as_a_difference() {
        let header = ISSUE_COLUMNS.map(str::to_string);
        let node = sample();
        let mut row = issue_row(&node);
        let title = ISSUE_COLUMNS.iter().position(|c| *c == "title").unwrap();
        row[title] = "Ship it today".to_string();

        let existing = writable_fields(&header, &issue_row(&node));
        let desired = writable_fields(&header, &row);
        let changed = differences(&existing, &desired);
        assert_eq!(
            changed,
            vec![(
                "title".to_string(),
                "Ship it".to_string(),
                "Ship it today".to_string()
            )]
        );
    }

    #[test]
    fn reordered_labels_and_padded_numbers_are_not_edits() {
        let header = ISSUE_COLUMNS.map(str::to_string);
        let node = sample();
        let mut row = issue_row(&node);
        let labels = ISSUE_COLUMNS.iter().position(|c| *c == "labels").unwrap();
        let priority = ISSUE_COLUMNS.iter().position(|c| *c == "priority").unwrap();
        row[labels] = "Improvement|Bug".to_string();
        row[priority] = "02".to_string();

        let existing = writable_fields(&header, &issue_row(&node));
        assert!(differences(&existing, &writable_fields(&header, &row)).is_empty());
    }

    #[test]
    fn a_project_row_carries_its_teams() {
        let node = json!({
            "id": "p-1",
            "name": "Board",
            "status": { "name": "In Progress" },
            "health": "onTrack",
            "priority": 2,
            "lead": { "displayName": "Ada" },
            "teams": { "nodes": [{ "key": "ENG" }, { "key": "OPS" }] },
            "startDate": "2026-10-01",
            "targetDate": "2026-12-01",
            "url": "https://linear.app/example/project/board-1",
            "createdAt": "2026-09-01T00:00:00.000Z",
            "updatedAt": "2026-10-01T00:00:00.000Z"
        });
        let row = project_row(&node);
        assert_eq!(row.len(), PROJECT_COLUMNS.len());
        assert_eq!(row[0], "Board");
        assert_eq!(row[5], "ENG|OPS");
    }
}
