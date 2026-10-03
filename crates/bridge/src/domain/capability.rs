//! What a platform can do. Capabilities are *data*, queried at runtime, never
//! assumed from the platform's name: a platform that has no due dates must be
//! refused at configuration time when a mapping asks for them, rather than
//! silently dropping the value at sync time.

/// A platform's state model. Linear has named workflow states; a forge has
/// open/closed. The reconciler maps between the two, so the
/// difference has to be visible rather than encoded in an `if platform == ..`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateModel {
    /// Arbitrary, named workflow states.
    Named,
    /// Open or closed.
    OpenClosed,
}

/// A field that may or may not survive a round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Title,
    Body,
    Labels,
    DueDate,
    Priority,
    Assignees,
    /// References from commit messages and pull-request text (`Fixes VED-123`).
    References,
    Deletion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub states: StateModel,
    /// Whether the platform can be *enumerated* - the difference between a mirror
    /// that reacts to deliveries and one that can sweep a scope. Derived from the
    /// preset's `[sink.issue.list]` rather than declared separately: two places to
    /// say it is two places to disagree.
    pub list: bool,
    pub labels: bool,
    pub due_dates: bool,
    /// Whether milestones travel here. Both sides can hold one, so both presets say
    /// true - the write only happens where the preset declares the directive.
    pub milestones: bool,

    pub priorities: bool,
    pub multiple_assignees: bool,
    pub native_pull_requests: bool,
    /// Whether the platform both emits and accepts deletions. Forgejo emits no
    /// webhook when an issue is deleted, so a deletion can only ever be
    /// propagated *to* it, never from it.
    pub deletion: bool,
}

impl Capabilities {
    pub fn supports(&self, field: Field) -> bool {
        match field {
            Field::Title | Field::Body => true,
            Field::Labels => self.labels,
            Field::DueDate => self.due_dates,
            Field::Priority => self.priorities,
            Field::Assignees => true,
            Field::References => self.native_pull_requests,
            Field::Deletion => self.deletion,
        }
    }

    /// Human-readable list of what is supported, for logs and `--json` output.
    pub fn describe(&self) -> Vec<&'static str> {
        let mut out = vec!["title", "body", "assignees"];
        if self.list {
            out.push("list");
        }
        if self.labels {
            out.push("labels");
        }
        if self.due_dates {
            out.push("due_dates");
        }
        if self.priorities {
            out.push("priorities");
        }
        if self.native_pull_requests {
            out.push("references");
        }
        if self.deletion {
            out.push("deletion");
        }
        out.push(match self.states {
            StateModel::Named => "states:named",
            StateModel::OpenClosed => "states:open/closed",
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forgejo_like() -> Capabilities {
        Capabilities {
            states: StateModel::OpenClosed,
            list: true,
            labels: true,
            due_dates: true,
            milestones: true,
            priorities: false,
            multiple_assignees: false,
            native_pull_requests: true,
            deletion: false,
        }
    }

    #[test]
    fn unsupported_fields_are_visible() {
        let caps = forgejo_like();
        assert!(caps.supports(Field::Labels));
        assert!(!caps.supports(Field::Priority));
        assert!(!caps.supports(Field::Deletion));
        assert!(caps.describe().contains(&"states:open/closed"));
    }
}
