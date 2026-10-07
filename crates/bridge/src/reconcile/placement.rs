//! Placement: which sink scope an entity's mirror belongs in.
//!
//! A mapping names one source container and one sink container, but one sink container
//! may hold many boards - so an issue's repository is a fact about its project, and the
//! operator states it in configuration:
//!
//! ```toml
//! [[mapping.project]]
//! project = "kuro"          # a project by id, slug or name
//! scope = "Vedaru/kuro"     # the repository its mirror lives in
//! ```
//!
//! Configuration is the only source. A project's *links* are written for people - a
//! reference implementation, a design doc, a GitHub mirror - and change for human
//! reasons, so they are not a sync contract and are never consulted here.
//!
//! The order an entity's scope is decided in, first match wins:
//!
//! 1. an existing pairing - a pair never moves, because moving it would delete the copy
//!    on the other side and with it the history that lives only there;
//! 2. the container's pairing (an issue follows its project's repository);
//! 3. a `[[mapping.project]]` entry, matched by id, then slug, then name;
//! 4. the mapping's own scope - for a *contained* entity, which still needs a home. A
//!    *project* that no entry names is not mirrored at all.
//!
//! Nothing here counts entries: several projects may name the same repository, and a
//! reference an entry does not name is simply not a target.

use std::collections::HashMap;

/// What a `[[mapping.project]]` entry may name a project by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    /// The platform's own id.
    pub id: &'a str,
    /// A human-facing alias, when the platform exposes one.
    pub slug: Option<&'a str>,
    /// The project's name.
    pub name: Option<&'a str>,
}

/// One `[[mapping.project]]`: a project, and the sink scope its mirror lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectScope {
    /// The project, by the id, slug or name a person wrote.
    pub project: String,
    /// The sink scope - a repository - its mirror lives in.
    pub scope: String,
}

/// The `[[mapping.project]]` entries a mapping carries.
///
/// Empty is meaningful: the deployment has said nothing about any project, so no project
/// is mirrored and its issues fall to the mapping's own scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectScopes {
    entries: Vec<ProjectScope>,
    /// The entry index by each key, so a lookup does not scan the list per candidate.
    index: HashMap<String, usize>,
}

impl ProjectScopes {
    pub fn new(entries: Vec<ProjectScope>) -> Self {
        let mut index: HashMap<String, usize> = HashMap::new();
        for (position, entry) in entries.iter().enumerate() {
            // First declaration wins: a key repeated later does not replace it.
            index.entry(entry.project.clone()).or_insert(position);
        }
        Self { entries, index }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The scope an entry names for a project, most specific key first.
    ///
    /// An id is what a platform issues and never changes; a slug is its human-facing
    /// alias; a name is what a person types. Within one tier the first declared entry
    /// wins.
    pub fn scope(&self, identity: &Identity<'_>) -> Option<&str> {
        for candidate in [Some(identity.id), identity.slug, identity.name]
            .into_iter()
            .flatten()
        {
            if let Some(position) = self.index.get(candidate) {
                return self.entries.get(*position).map(|entry| entry.scope.as_str());
            }
        }
        None
    }
}

/// What decided an entity's scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The entity's own pairing: a pair never moves repos.
    Pair,
    /// The pairing of the entity's container.
    ContainerPair,
    /// A `[[mapping.project]]` entry named it (its own, or its container's).
    Configured,
    /// Nothing said, and the entity names no container of its own, so the mapping's own
    /// sink scope applies. A project never lands here, and neither does an issue whose
    /// project the config does not name: those are not mirrored at all.
    Default,
}

/// Where an entity's mirror belongs, and what said so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The sink scope every read and write for this entity uses.
    pub scope: String,
    pub origin: Origin,
    /// The scope an entry names for this entity, when any entry matches. Equal to
    /// `scope` when an entry placed it; different when a pairing pinned the entity
    /// somewhere else (the "it would have moved" case).
    pub configured: Option<String>,
}

impl Placement {
    /// True when a pairing, rather than configuration, placed this entity.
    pub fn pinned(&self) -> bool {
        matches!(self.origin, Origin::Pair | Origin::ContainerPair)
    }

    /// True when configuration now names a different container than the pairing the
    /// entity already has. Reported, never acted on: the mirror stays put.
    pub fn would_move(&self) -> bool {
        self.pinned()
            && self
                .configured
                .as_deref()
                .is_some_and(|configured| configured != self.scope)
    }
}

/// The facts about one entity that decide where its mirror lives.
///
/// A container (a project) carries its own identity; a contained entity (an issue)
/// carries its container's.
#[derive(Clone, Copy, Debug)]
pub struct Entity<'a> {
    /// The sink scope the entity's own pairing sits in, if it is paired.
    pub paired: Option<&'a str>,
    /// The sink scope the entity's *container's* pairing sits in, if that is paired.
    pub container_paired: Option<&'a str>,
    /// The container the entity belongs to (an issue's project), for inheritance.
    pub container: Option<Identity<'a>>,
    /// The entity's own identity, when it is itself a container.
    pub own: Option<Identity<'a>>,
    /// Whether the entity is itself a container (a project). A container no entry names
    /// is not mirrored, and neither is an issue in it: only an issue that names no project
    /// at all has nothing to ask and falls back to the mapping's scope.
    pub is_container: bool,
}

/// The scope an entity's mirror belongs in, or `None` when nothing says where.
pub fn place(
    projects: &ProjectScopes,
    default_scope: &str,
    entity: Entity<'_>,
) -> Option<Placement> {
    let configured = entity
        .own
        .as_ref()
        .and_then(|own| projects.scope(own))
        .or_else(|| {
            entity
                .container
                .as_ref()
                .and_then(|container| projects.scope(container))
        })
        .map(str::to_string);

    if let Some(scope) = entity.paired {
        return Some(Placement {
            scope: scope.to_string(),
            origin: Origin::Pair,
            configured,
        });
    }
    if let Some(scope) = entity.container_paired {
        return Some(Placement {
            scope: scope.to_string(),
            origin: Origin::ContainerPair,
            configured,
        });
    }
    if let Some(scope) = configured {
        return Some(Placement {
            scope: scope.clone(),
            origin: Origin::Configured,
            configured: Some(scope),
        });
    }
    // Nothing said. A project is not mirrored anywhere, and neither is an issue whose
    // project the config does not name: projects are opt-in and an issue belongs to its
    // project. Only an issue that names no project at all has no container to ask, and
    // that one keeps the mapping's own scope - an issue that lives nowhere is worse than
    // one that lives in the team's repository.
    if entity.is_container || entity.container.is_some() {
        return None;
    }
    Some(Placement {
        scope: default_scope.to_string(),
        origin: Origin::Default,
        configured: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = "Vedaru/linear-cli-rs";

    fn projects() -> ProjectScopes {
        ProjectScopes::new(vec![
            ProjectScope {
                project: "project-kuro".into(),
                scope: "Vedaru/kuro".into(),
            },
            ProjectScope {
                project: "the-slug".into(),
                scope: "Vedaru/slug".into(),
            },
            ProjectScope {
                project: "The Name".into(),
                scope: "Vedaru/name".into(),
            },
        ])
    }

    fn identity<'a>(id: &'a str, slug: Option<&'a str>, name: Option<&'a str>) -> Identity<'a> {
        Identity { id, slug, name }
    }

    /// A project entity.
    fn project<'a>(paired: Option<&'a str>, own: Option<Identity<'a>>) -> Entity<'a> {
        Entity {
            paired,
            container_paired: None,
            container: None,
            own,
            is_container: true,
        }
    }

    /// An issue: it carries its project as its container, and no identity of its own.
    fn issue<'a>(
        paired: Option<&'a str>,
        container_paired: Option<&'a str>,
        container: Option<Identity<'a>>,
    ) -> Entity<'a> {
        Entity {
            paired,
            container_paired,
            container,
            own: None,
            is_container: false,
        }
    }

    #[test]
    fn an_entry_is_hit_by_id_then_slug_then_name() {
        let projects = projects();
        assert_eq!(
            projects.scope(&identity("project-kuro", None, None)),
            Some("Vedaru/kuro")
        );
        assert_eq!(
            projects.scope(&identity("a-uuid", None, Some("The Name"))),
            Some("Vedaru/name")
        );
        assert_eq!(
            projects.scope(&identity("a-uuid", Some("the-slug"), Some("The Name"))),
            Some("Vedaru/slug"),
            "a slug is more specific than a name"
        );
        assert_eq!(
            projects.scope(&identity("unknown", None, Some("Unknown"))),
            None
        );
    }

    #[test]
    fn a_project_takes_the_scope_its_entry_names() {
        let placement = place(
            &projects(),
            DEFAULT,
            project(None, Some(identity("project-kuro", None, Some("Kuro")))),
        )
        .expect("placed");
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::Configured);
        assert_eq!(placement.configured.as_deref(), Some("Vedaru/kuro"));
        assert!(!placement.would_move());
    }

    #[test]
    fn a_project_no_entry_names_is_not_placed() {
        let placement = place(
            &projects(),
            DEFAULT,
            project(None, Some(identity("project-plain", None, Some("Plain")))),
        );
        assert!(placement.is_none());
    }

    #[test]
    fn several_projects_may_name_the_same_repository() {
        let projects = ProjectScopes::new(vec![
            ProjectScope {
                project: "first".into(),
                scope: "Vedaru/shared".into(),
            },
            ProjectScope {
                project: "second".into(),
                scope: "Vedaru/shared".into(),
            },
        ]);
        let a = place(&projects, DEFAULT, project(None, Some(identity("first", None, None))))
            .expect("placed");
        let b = place(&projects, DEFAULT, project(None, Some(identity("second", None, None))))
            .expect("placed");
        assert_eq!(a.scope, b.scope);
        assert_eq!(a.scope, "Vedaru/shared");
    }

    #[test]
    fn an_issue_inherits_its_projects_entry() {
        let placement = place(
            &projects(),
            DEFAULT,
            issue(None, None, Some(identity("project-kuro", None, None))),
        )
        .expect("placed");
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::Configured);
    }

    #[test]
    fn a_project_less_issue_lands_in_the_mapping_scope() {
        // An issue that names no project at all has no container to ask, so it keeps the
        // mapping's own scope.
        let placement = place(&projects(), DEFAULT, issue(None, None, None)).expect("placed");
        assert_eq!(placement.scope, DEFAULT);
        assert_eq!(placement.origin, Origin::Default);
        assert!(!placement.would_move());
    }

    #[test]
    fn an_issue_whose_project_no_entry_names_is_not_placed() {
        // Projects are opt-in, and an issue belongs to its project.
        let placement = place(
            &projects(),
            DEFAULT,
            issue(
                None,
                None,
                Some(identity("project-plain", None, Some("Plain"))),
            ),
        );
        assert!(placement.is_none());
    }

    #[test]
    fn a_pairing_pins_an_entity_and_reports_the_entry_that_disagrees() {
        let placement = place(
            &projects(),
            DEFAULT,
            issue(
                Some(DEFAULT),
                None,
                Some(identity("project-kuro", None, None)),
            ),
        )
        .expect("placed");
        assert_eq!(placement.scope, DEFAULT);
        assert_eq!(placement.origin, Origin::Pair);
        assert!(placement.pinned());
        assert!(
            placement.would_move(),
            "the entry now names a different repository"
        );
    }

    #[test]
    fn a_container_pairing_pins_an_unpaired_issue() {
        let placement = place(
            &projects(),
            DEFAULT,
            issue(
                None,
                Some("Vedaru/kuro"),
                Some(identity("project-kuro", None, None)),
            ),
        )
        .expect("placed");
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::ContainerPair);
        assert!(!placement.would_move(), "the pair and the entry agree");
    }

    #[test]
    fn a_repeated_key_keeps_the_first_entry() {
        let projects = ProjectScopes::new(vec![
            ProjectScope {
                project: "kuro".into(),
                scope: "Vedaru/first".into(),
            },
            ProjectScope {
                project: "kuro".into(),
                scope: "Vedaru/second".into(),
            },
        ]);
        assert_eq!(
            projects.scope(&identity("kuro", None, None)),
            Some("Vedaru/first")
        );
        assert_eq!(projects.len(), 2, "nothing is dropped or refused");
    }
}
