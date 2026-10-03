//! Notifications: what Linear told this user, and the read state that belongs to that user alone.
//!
//! The read state is the whole reason these commands are worded the way they are: `readAt` lives on
//! the notification, per user, so marking one read changes *this token's user's* view of Linear and
//! nobody else's. Every mutation here returns the viewer alongside its payload so the caller can say
//! whose state it changed - at the cost of one extra request, because `viewer` cannot be selected in
//! the same document as a mutation (`VIEWER_QUERY` records the measured reason).
//!
//! Two facts about the API that shape the surface, both measured rather than assumed:
//!
//! * `NotificationFilter` has `createdAt`, `type`, `subscriptionType` and the date comparators - it
//!   has **no** `readAt`. So "unread" cannot be a server-side filter and `--unread` is applied
//!   here, to the page that came back. `notificationsUnreadCount` is the API's own answer for the
//!   *count*, which is why the listing prints that and filters its rows locally.
//! * The node is an interface. Interface fields (`id`, `type`, `readAt`, `createdAt`, `archivedAt`)
//!   are selected directly; the issue link needs an inline fragment on `IssueNotification`, so a
//!   notification that is not about an issue simply has no `issue` key - absent, not null.

use super::prelude::*;
use super::*;

const LIST_NOTIFICATIONS_QUERY: &str = r#"
query ListNotifications($filter: NotificationFilter, $first: Int, $after: String, $includeArchived: Boolean) {
  notifications(filter: $filter, first: $first, after: $after, includeArchived: $includeArchived) {
    nodes {
      id
      type
      readAt
      createdAt
      archivedAt
      ... on IssueNotification {
        issue {
          identifier
          title
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
  viewer {
    name
    email
  }
}
"#;

const UNREAD_COUNT_QUERY: &str = r#"
query NotificationsUnreadCount {
  notificationsUnreadCount
}
"#;

const ARCHIVE_MUTATION: &str = r#"
mutation ArchiveNotification($id: String!) {
  notificationArchive(id: $id) {
    success
    entity {
      id
      archivedAt
    }
  }
}
"#;

/// The viewer, as its own document.
///
/// This is a second call and it was not supposed to be: the first version of this module asked for
/// `viewer` *inside* the mutation documents, because GraphQL answers every selected field in one
/// round trip. It does not - and the schema says so plainly once you look at the root types. A
/// mutation document's root is `Mutation`, which has no `viewer` field; `viewer` lives on `Query`.
/// Selecting it next to a mutation is a validation error, which the document checker caught before
/// any of this ran live ("Cannot query field 'viewer' on type 'Mutation'").
///
/// So saying *whose* state changed costs one extra request on a write path. That is the honest
/// price, and it is paid only by `read`/`archive` - the listing gets its viewer for free because a
/// query document's root really is `Query`.
const VIEWER_QUERY: &str = r#"
query NotificationViewer {
  viewer {
    name
    email
  }
}
"#;

/// The user this token belongs to, which is whose read state the writes change.
pub fn viewer() -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(VIEWER_QUERY, json!({}))?;
    Ok(data.get("viewer").cloned().unwrap_or(Value::Null))
}

const MARK_READ_MUTATION: &str = r#"
mutation MarkNotificationRead($id: String!, $readAt: DateTime!) {
  notificationUpdate(id: $id, input: { readAt: $readAt }) {
    success
    notification {
      id
      readAt
    }
  }
}
"#;

/// How many notifications are unread, by the API's own count.
///
/// Not `nodes.filter(...).len()`: the listing is paged and this is exact, so the two can disagree
/// and the one that is right is this one.
pub fn unread_count() -> Result<i64> {
    let client = graphql::client()?;
    let data = client.request(UNREAD_COUNT_QUERY, json!({}))?;
    data.get("notificationsUnreadCount")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            CliError::cli("Linear API response did not contain notificationsUnreadCount")
        })
}

/// Notifications this token's user can see, following pages, with the last page's `pageInfo` and
/// the viewer whose read state these are.
///
/// The viewer rides along in the same document rather than costing a second call, because every
/// listing here is about *someone's* notifications and a listing that does not say whose invites the
/// reader to assume it is theirs.
///
/// `since` is already an ISO timestamp by the time it arrives (the caller parses `7d`/`2024-01-15`
/// with the same helper the rest of the CLI uses); it becomes `createdAt: { gt: ... }`, which is the
/// one comparator `NotificationFilter` offers for a window. `unread_only` is applied by the caller
/// to the page, because the filter has no read state - see the module comment.
pub fn list_notifications(
    limit: Option<u32>,
    since: Option<String>,
    include_archived: bool,
) -> Result<(Vec<Value>, Value, Value)> {
    let client = graphql::client()?;
    let fetch_all = limit.is_none() || limit == Some(0);
    let limit = limit.unwrap_or(50);
    let page_size: u32 = if fetch_all { 50 } else { limit.min(50) };

    let mut filter = Map::new();
    if let Some(since) = &since {
        filter.insert("createdAt".to_string(), json!({ "gt": since }));
    }

    let mut notifications: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut has_next = true;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut viewer = Value::Null;

    while has_next {
        let mut variables = Map::new();
        variables.insert("first".to_string(), json!(page_size));
        variables.insert("includeArchived".to_string(), json!(include_archived));
        if !filter.is_empty() {
            variables.insert("filter".to_string(), Value::Object(filter.clone()));
        }
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(LIST_NOTIFICATIONS_QUERY, Value::Object(variables))?;
        if let Some(found) = data.get("viewer") {
            viewer = found.clone();
        }
        let connection = data
            .get("notifications")
            .ok_or_else(|| CliError::cli("Linear API response did not contain notifications"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            notifications.extend(nodes.iter().cloned());
        }

        if let Some(page_info) = connection.get("pageInfo") {
            last_page_info = page_info.clone();
            has_next = page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            after = page_info
                .get("endCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
        } else {
            has_next = false;
        }

        if !fetch_all && notifications.len() >= limit as usize {
            break;
        }
    }

    notifications.truncate(limit as usize);
    Ok((notifications, last_page_info, viewer))
}

/// Mark one notification read, returning `(payload, viewer)`.
///
/// `readAt` is now: the API takes the timestamp rather than a flag, and "read" is the recorded
/// moment. The viewer is a second request because `viewer` cannot be selected next to a mutation -
/// see `VIEWER_QUERY` for the measured reason.
pub fn mark_read(id: &str) -> Result<(Value, Value)> {
    let client = graphql::client()?;
    let read_at = Utc::now().to_rfc3339();
    let data = client.request(MARK_READ_MUTATION, json!({ "id": id, "readAt": read_at }))?;
    let payload = payload_of(&data, "notificationUpdate")?;
    Ok((payload, viewer()?))
}

/// Archive one notification, returning `(payload, viewer)`.
pub fn archive(id: &str) -> Result<(Value, Value)> {
    let client = graphql::client()?;
    let data = client.request(ARCHIVE_MUTATION, json!({ "id": id }))?;
    let payload = payload_of(&data, "notificationArchive")?;
    Ok((payload, viewer()?))
}

/// The payload under `field`, refusing a `success: false` rather than reporting one as done.
fn payload_of(data: &Value, field: &str) -> Result<Value> {
    let payload = data
        .get(field)
        .ok_or_else(|| CliError::cli(format!("Linear API response did not contain {field}")))?;
    match payload.get("success").and_then(Value::as_bool) {
        Some(false) => Err(CliError::cli(format!(
            "Linear refused the {field} mutation (success: false)"
        ))),
        _ => Ok(payload.clone()),
    }
}

/// Find one notification by id or by the issue identifier it refers to.
///
/// Reads the pages it needs rather than asking for one id: the listing is the only way to reach a
/// notification's issue, and an identifier like `VED-42` is what a person has in hand. A reference
/// that matches nothing is refused by name, not by index.
pub fn resolve_notification(reference: &str) -> Result<Value> {
    let (nodes, _, _) = list_notifications(Some(250), None, true)?;

    // An id first: exact, and it is what `list --json` hands back.
    if let Some(found) = nodes
        .iter()
        .find(|node| node.get("id").and_then(Value::as_str) == Some(reference))
    {
        return Ok(found.clone());
    }

    let wanted = normalize_issue_identifier(reference);
    let matches: Vec<&Value> = nodes
        .iter()
        .filter(|node| {
            let identifier = node
                .get("issue")
                .and_then(|issue| issue.get("identifier"))
                .and_then(Value::as_str);
            match (identifier, wanted.as_deref()) {
                (Some(identifier), Some(wanted)) => identifier.eq_ignore_ascii_case(wanted),
                _ => false,
            }
        })
        .collect();

    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(CliError::not_found("notification", reference)),
        many => Err(CliError::validation(format!(
            "'{reference}' matches {} notifications; pass the id instead - the newest is {}",
            many.len(),
            many[0].get("id").and_then(Value::as_str).unwrap_or("")
        ))),
    }
}
