use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Ordering helpers
// ---------------------------------------------------------------------------

/// Order issues by status the way the Linear app groups them.
///
/// `position` is only meaningful within one team, so single-team results sort
/// by type group then position descending, while multi-team results sort by
/// type group only (ranking one team's position against another's compares
/// unrelated numbers). Ties keep the server's priority/manual order, which a
/// stable sort preserves.
pub fn sort_issues_by_workflow_state(issues: &mut [Value]) {
    let first_team = issues
        .first()
        .and_then(|issue| issue.get("team"))
        .and_then(|team| team.get("key"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let multi_team = issues.iter().any(|issue| {
        issue
            .get("team")
            .and_then(|team| team.get("key"))
            .and_then(Value::as_str)
            != first_team.as_deref()
    });

    issues.sort_by(|a, b| {
        let a_type = a
            .get("state")
            .and_then(|state| state.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let b_type = b
            .get("state")
            .and_then(|state| state.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if multi_team {
            return compare_workflow_state_types(a_type, b_type);
        }
        let a_position = a
            .get("state")
            .and_then(|state| state.get("position"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let b_position = b
            .get("state")
            .and_then(|state| state.get("position"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        compare_workflow_state_types(a_type, b_type).then_with(|| {
            b_position
                .partial_cmp(&a_position)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
}

pub(crate) fn name_lowercase(value: &Value) -> String {
    value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase()
}

/// Read a JSON number as an integer, tolerating a float encoding.
pub(crate) fn integer_field(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float as i64)),
        _ => None,
    }
}
