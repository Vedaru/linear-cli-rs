//! Routing: which container on the sink each entity's mirror belongs in.
//!
//! A mapping names one source container and one sink container, but one sink
//! container may hold many *boards*, and a board points at exactly one repository.
//! So an issue's repository is usually a fact about its project - but an issue with
//! no project still needs somewhere to live. Routes are the declarative answer: a
//! `[[mapping.route]]` matches an entity and names the sink scope it is mirrored
//! into.
//!
//! A route may match on any of three keys, and the precedence is by key, not by
//! declaration order:
//!
//! 1. `project` - matches the project entity itself *and* every issue that belongs
//!    to that project (inheritance);
//! 2. `issue` - an assignment for one issue, by its identifier;
//! 3. `label` - an assignment for any issue carrying that label.
//!
//! The precedence is `project`, then `issue`, then `label`, then the mapping's own
//! sink scope - first match wins. (An explicit `issue` rule overriding its project
//! would be a one-line reordering of the tiers in [`routed_scope`].)
//!
//! Over that sits the rule that keeps the mirror honest: an existing pair never
//! moves repos. Relocating a paired issue would mean deleting the copy on the other
//! side, and with it the history that lives only there. When a route disagrees with a
//! pairing the resolver keeps the pairing and reports the disagreement
//! ([`Placement::would_move`]) so the operator can decide.

/// One configured route: what it matches, and the sink scope its mirror lives in.
///
/// Each match key is optional; at least one must be set (checked at load time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// A project, by its id, slug or name on the source platform. Matches the
    /// project entity and every issue that belongs to it.
    pub project: Option<String>,
    /// One issue, by its source identifier (e.g. `VED-119`).
    pub issue: Option<String>,
    /// Any issue carrying this label.
    pub label: Option<String>,
    /// The sink scope - the container this route's entities are mirrored into.
    pub scope: String,
}

impl Route {
    /// Whether this route names anything to match on.
    pub fn has_key(&self) -> bool {
        self.project.is_some() || self.issue.is_some() || self.label.is_some()
    }
}

/// The routes a mapping carries, matched in a fixed order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Routes {
    entries: Vec<Route>,
}

impl Routes {
    pub fn new(entries: Vec<Route>) -> Self {
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every scope the routes name, distinct and in declaration order.
    pub fn scopes(&self) -> Vec<&str> {
        let mut scopes: Vec<&str> = Vec::new();
        for entry in &self.entries {
            if !scopes.contains(&entry.scope.as_str()) {
                scopes.push(entry.scope.as_str());
            }
        }
        scopes
    }

    /// The `project` rule a container's identity matches, most specific first.
    ///
    /// `id` beats `slug` beats `name`: an id is what a platform issues and never
    /// changes, a slug is its human-facing alias, and a name is what a person types
    /// and may be ambiguous. Within one tier the first declared route wins.
    pub fn project_scope(&self, identity: &Identity<'_>) -> Option<&str> {
        for candidate in [Some(identity.id), identity.slug, identity.name]
            .into_iter()
            .flatten()
        {
            if let Some(route) = self
                .entries
                .iter()
                .find(|route| route.project.as_deref() == Some(candidate))
            {
                return Some(route.scope.as_str());
            }
        }
        None
    }

    /// The `issue` rule naming this identifier, if any.
    pub fn issue_scope(&self, issue: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|route| {
                route
                    .issue
                    .as_deref()
                    .is_some_and(|named| named.eq_ignore_ascii_case(issue))
            })
            .map(|route| route.scope.as_str())
    }

    /// The `label` rule matching one of these labels, if any.
    pub fn label_scope(&self, labels: &[String]) -> Option<&str> {
        self.entries
            .iter()
            .find(|route| {
                route.label.as_deref().is_some_and(|named| {
                    labels.iter().any(|label| label.eq_ignore_ascii_case(named))
                })
            })
            .map(|route| route.scope.as_str())
    }
}

/// A URL on a platform that names a scope there, as a template with a `{scope}`
/// capture (e.g. `https://git.example.com/{scope}`).
///
/// The pattern lives in a preset, not here: the engine only asks "does any of this
/// entity's declared locations name a scope on that platform?", and a platform
/// answers how its own URLs are shaped. A URL that does not match is simply not this
/// platform's - which is how a link to another host is ignored rather than guessed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    prefix: String,
    suffix: String,
}

impl Location {
    /// Compile a pattern. `None` when it has no `{scope}` capture.
    pub fn parse(pattern: &str) -> Option<Self> {
        let (prefix, suffix) = pattern.split_once("{scope}")?;
        Some(Self {
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
        })
    }

    /// The scope a URL names, when it is this platform's and carries one.
    ///
    /// An empty capture is not a scope: a link to the host root names no container.
    pub fn scope<'a>(&self, url: &'a str) -> Option<&'a str> {
        let rest = url.strip_prefix(&self.prefix)?;
        let scope = rest.strip_suffix(&self.suffix)?;
        (!scope.is_empty() && !scope.chars().any(char::is_whitespace)).then_some(scope)
    }
}

/// What a `project` rule may name an entity by, most specific first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    /// The platform's own id.
    pub id: &'a str,
    /// A human-facing alias, when the platform exposes one.
    pub slug: Option<&'a str>,
    /// The entity's name.
    pub name: Option<&'a str>,
}

impl<'a> Identity<'a> {
    /// The identity of an entity only its id is known for.
    pub fn id(id: &'a str) -> Self {
        Self {
            id,
            slug: None,
            name: None,
        }
    }
}

/// What decided an entity's scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The entity's own pairing: a pair never moves repos.
    Pair,
    /// The pairing of the entity's container.
    ContainerPair,
    /// A route matched the entity (project inheritance, an issue rule, a label rule).
    Route,
    /// A location the entity declares (a project's link) points at the sink platform.
    Link,
    /// Nothing said, so the mapping's default sink scope applies.
    Default,
}

/// Where an entity's mirror belongs, and what said so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The sink scope every read and write for this entity uses.
    pub scope: String,
    pub origin: Origin,
    /// What the routes would have chosen, when any route matches this entity. Equal
    /// to `scope` when a route placed it; different when a pairing pinned the entity
    /// somewhere else (the "it would have moved" case).
    pub routed: Option<String>,
}

impl Placement {
    /// True when a pairing, rather than a route or the default, placed this entity.
    pub fn pinned(&self) -> bool {
        matches!(self.origin, Origin::Pair | Origin::ContainerPair)
    }

    /// True when a route would put this entity in a different container than the
    /// pairing it already has. Reported, never acted on: the mirror stays put.
    pub fn would_move(&self) -> bool {
        self.pinned()
            && self
                .routed
                .as_deref()
                .is_some_and(|routed| routed != self.scope)
    }
}

/// The facts about one entity that decide where its mirror lives.
///
/// A container (a project) carries its own identity; a contained entity (an issue)
/// carries its container's identity, its own identifier and its labels.
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
    /// The contained entity's own key (an issue's identifier), for an `issue` rule.
    pub issue: Option<&'a str>,
    /// The contained entity's labels, for a `label` rule.
    pub labels: &'a [String],
    /// Locations the entity declares (a project's links), for the sink-platform step.
    /// For a contained entity these are its container's.
    pub links: &'a [String],
}

/// The scope an entity's mirror belongs in, and why.
///
/// `location` is the sink platform's way of recognising its own URLs (a preset's
/// pattern); `None` means the sink declares no such shape and links are not consulted.
pub fn place(
    routes: &Routes,
    location: Option<&Location>,
    default_scope: &str,
    entity: Entity<'_>,
) -> Placement {
    // What the routes and the entity's own declared locations say, computed first so
    // that a pairing overriding either can still be reported.
    let routed = routed_scope(routes, location, &entity);

    if let Some(scope) = entity.paired {
        return Placement {
            scope: scope.to_string(),
            origin: Origin::Pair,
            routed: routed.map(|(scope, _)| scope),
        };
    }
    if let Some(scope) = entity.container_paired {
        return Placement {
            scope: scope.to_string(),
            origin: Origin::ContainerPair,
            routed: routed.map(|(scope, _)| scope),
        };
    }
    if let Some((scope, origin)) = routed {
        return Placement {
            scope: scope.clone(),
            origin,
            routed: Some(scope),
        };
    }
    Placement {
        scope: default_scope.to_string(),
        origin: Origin::Default,
        routed: None,
    }
}

/// What the routes and the entity's declared locations say about it, by precedence.
///
/// A container resolves by `project` then its own declared links; a contained entity
/// inherits its container's `project` scope, then falls to its own `issue` rule, then
/// a `label` rule, then its container's links. Moving the `container` block below the
/// `issue` block would let an explicit `issue` rule override its project - the
/// one-line change the operator asked to leave available.
fn routed_scope(
    routes: &Routes,
    location: Option<&Location>,
    entity: &Entity<'_>,
) -> Option<(String, Origin)> {
    if let Some(own) = &entity.own {
        if let Some(scope) = routes.project_scope(own) {
            return Some((scope.to_string(), Origin::Route));
        }
        return link_scope(location, entity.links).map(|scope| (scope.to_string(), Origin::Link));
    }
    if let Some(container) = &entity.container {
        if let Some(scope) = routes.project_scope(container) {
            return Some((scope.to_string(), Origin::Route));
        }
    }
    if let Some(issue) = entity.issue {
        if let Some(scope) = routes.issue_scope(issue) {
            return Some((scope.to_string(), Origin::Route));
        }
    }
    if let Some(scope) = routes.label_scope(entity.labels) {
        return Some((scope.to_string(), Origin::Route));
    }
    link_scope(location, entity.links).map(|scope| (scope.to_string(), Origin::Link))
}

/// The scope one of an entity's declared locations names on the sink platform, if any.
///
/// A location that does not match the sink's pattern - a link to another host - is
/// simply not a candidate, never an error.
fn link_scope<'a>(location: Option<&Location>, links: &'a [String]) -> Option<&'a str> {
    let location = location?;
    links.iter().find_map(|url| location.scope(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_route(project: &str, scope: &str) -> Route {
        Route {
            project: Some(project.into()),
            issue: None,
            label: None,
            scope: scope.into(),
        }
    }

    fn issue_route(issue: &str, scope: &str) -> Route {
        Route {
            project: None,
            issue: Some(issue.into()),
            label: None,
            scope: scope.into(),
        }
    }

    fn label_route(label: &str, scope: &str) -> Route {
        Route {
            project: None,
            issue: None,
            label: Some(label.into()),
            scope: scope.into(),
        }
    }

    fn routes() -> Routes {
        Routes::new(vec![
            project_route("project-kuro", "Vedaru/kuro"),
            project_route("Mirror the widget", "Vedaru/widget"),
            issue_route("VED-200", "Vedaru/one-off"),
            label_route("urgent", "Vedaru/urgent"),
        ])
    }

    fn identity<'a>(id: &'a str, slug: Option<&'a str>, name: Option<&'a str>) -> Identity<'a> {
        Identity { id, slug, name }
    }

    /// The sink platform's URL shape, as a preset would declare it.
    fn location() -> Location {
        Location::parse("https://git.vedaru.cn/{scope}").expect("a pattern with a capture")
    }

    /// An entity with nothing but its container (or its own id for a project).
    fn entity<'a>(container: Option<Identity<'a>>, own: Option<Identity<'a>>) -> Entity<'a> {
        Entity {
            paired: None,
            container_paired: None,
            container,
            own,
            issue: None,
            labels: &[],
            links: &[],
        }
    }

    /// A placement with no sink URL pattern, for the cases that are not about links.
    fn place_only(routes: &Routes, default_scope: &str, entity: Entity<'_>) -> Placement {
        place(routes, None, default_scope, entity)
    }

    #[test]
    fn a_project_rule_is_hit_by_id_then_slug_then_name() {
        let routes = routes();
        assert_eq!(
            routes.project_scope(&identity("project-kuro", None, None)),
            Some("Vedaru/kuro")
        );
        assert_eq!(
            routes.project_scope(&identity("a-uuid", None, Some("Mirror the widget"))),
            Some("Vedaru/widget")
        );
        let with_slug = Routes::new(vec![
            project_route("the-slug", "Vedaru/slug"),
            project_route("The Name", "Vedaru/name"),
        ]);
        assert_eq!(
            with_slug.project_scope(&identity("a-uuid", Some("the-slug"), Some("The Name"))),
            Some("Vedaru/slug"),
            "a slug is more specific than a name"
        );
        assert_eq!(
            routes.project_scope(&identity("unknown", None, Some("Unknown"))),
            None
        );
    }

    #[test]
    fn an_unpaired_project_takes_its_project_route() {
        let placement = place_only(
            &routes(),
            "Vedaru/linear-cli-rs",
            entity(None, Some(identity("project-kuro", None, Some("Kuro")))),
        );
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::Route);
        assert_eq!(placement.routed.as_deref(), Some("Vedaru/kuro"));
        assert!(!placement.would_move());
    }

    #[test]
    fn an_unpaired_project_without_a_route_takes_the_default() {
        let placement = place_only(
            &routes(),
            "Vedaru/linear-cli-rs",
            entity(None, Some(identity("project-plain", None, Some("Plain")))),
        );
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::Default);
        assert_eq!(placement.routed, None);
    }

    #[test]
    fn an_issue_inherits_its_projects_route() {
        let placement = place_only(
            &routes(),
            "Vedaru/linear-cli-rs",
            entity(Some(identity("project-kuro", None, None)), None),
        );
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::Route);
    }

    #[test]
    fn an_issue_whose_container_identity_is_id_only_misses_a_name_rule() {
        // The container's slug and name could not be resolved - a project the sweep
        // never listed, or a platform that exposes neither. The route names the
        // project by name, so it must not match: the issue falls to the default,
        // rather than erroring or being routed on a guess.
        let routes = Routes::new(vec![project_route("Mirror the widget", "Vedaru/widget")]);
        let placement = place_only(
            &routes,
            "Vedaru/linear-cli-rs",
            entity(Some(identity("a-uuid", None, None)), None),
        );
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::Default);
        assert_eq!(placement.routed, None);
    }

    #[test]
    fn an_issue_inherits_a_container_routed_by_slug_or_name() {
        // The container's identity carries the slug and name the handler resolved, so
        // a `project` rule naming either reaches the issue, not only the project.
        let routes = Routes::new(vec![
            project_route("the-slug", "Vedaru/slug"),
            project_route("The Name", "Vedaru/name"),
        ]);
        let by_slug = place_only(
            &routes,
            "Vedaru/linear-cli-rs",
            entity(
                Some(identity("a-uuid", Some("the-slug"), Some("The Name"))),
                None,
            ),
        );
        assert_eq!(by_slug.scope, "Vedaru/slug", "a slug is more specific");
        assert_eq!(by_slug.origin, Origin::Route);
        let by_name = place_only(
            &routes,
            "Vedaru/linear-cli-rs",
            entity(Some(identity("a-uuid", None, Some("The Name"))), None),
        );
        assert_eq!(by_name.scope, "Vedaru/name");
        assert_eq!(by_name.origin, Origin::Route);
    }

    #[test]
    fn an_issue_with_no_project_is_assigned_by_an_issue_rule() {
        let mut facts = entity(None, None);
        facts.issue = Some("VED-200");
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/one-off");
        assert_eq!(placement.origin, Origin::Route);
    }

    #[test]
    fn an_issue_with_no_project_is_assigned_by_a_label_rule() {
        let labels = vec!["Bug".to_string(), "Urgent".to_string()];
        let mut facts = entity(None, None);
        facts.labels = &labels;
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/urgent");
        assert_eq!(placement.origin, Origin::Route);
    }

    #[test]
    fn an_issue_with_no_project_and_no_rule_takes_the_default() {
        let labels = vec!["Bug".to_string()];
        let mut facts = entity(None, None);
        facts.issue = Some("VED-9");
        facts.labels = &labels;
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::Default);
    }

    #[test]
    fn a_projects_route_beats_an_issues_label_rule() {
        // The issue is in a routed project *and* carries a label that routes
        // elsewhere: inheritance wins, because the board is where the issue lives.
        let labels = vec!["urgent".to_string()];
        let mut facts = entity(Some(identity("project-kuro", None, None)), None);
        facts.issue = Some("VED-200");
        facts.labels = &labels;
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::Route);
    }

    #[test]
    fn a_projects_route_beats_an_issues_own_issue_rule_too() {
        let mut facts = entity(Some(identity("project-kuro", None, None)), None);
        facts.issue = Some("VED-200");
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/kuro");
    }

    #[test]
    fn a_paired_issue_stays_where_its_pair_is() {
        let mut facts = entity(Some(identity("project-kuro", None, None)), None);
        facts.paired = Some("Vedaru/linear-cli-rs");
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        // The project routes to kuro, but the pair lives elsewhere: a pair never moves.
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::Pair);
        assert_eq!(placement.routed.as_deref(), Some("Vedaru/kuro"));
        assert!(placement.would_move(), "the move is reported, not made");
    }

    #[test]
    fn an_issue_follows_its_projects_pairing() {
        let mut facts = entity(Some(identity("project-kuro", None, None)), None);
        facts.container_paired = Some("Vedaru/kuro");
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/kuro");
        assert_eq!(placement.origin, Origin::ContainerPair);
        assert!(
            !placement.would_move(),
            "the project's pair and route agree"
        );
    }

    #[test]
    fn a_container_pairing_that_disagrees_with_a_route_is_reported_too() {
        let mut facts = entity(Some(identity("project-kuro", None, None)), None);
        facts.container_paired = Some("Vedaru/linear-cli-rs");
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::ContainerPair);
        assert!(placement.would_move());
    }

    #[test]
    fn a_paired_project_keeps_its_pairing() {
        let mut facts = entity(None, Some(identity("project-kuro", None, Some("Kuro"))));
        facts.paired = Some("Vedaru/other");
        let placement = place_only(&routes(), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/other");
        assert_eq!(placement.origin, Origin::Pair);
        assert!(placement.would_move());
    }

    // --- declared locations (a project's links) -----------------------------

    #[test]
    fn a_projects_link_to_the_sink_platform_routes_its_issues() {
        let links = vec!["https://git.vedaru.cn/Vedaru/linked".to_string()];
        let mut facts = entity(Some(identity("project-unrouted", None, None)), None);
        facts.links = &links;
        let placement = place(&routes(), Some(&location()), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/linked");
        assert_eq!(placement.origin, Origin::Link);
        assert_eq!(placement.routed.as_deref(), Some("Vedaru/linked"));
    }

    #[test]
    fn a_projects_own_link_routes_the_project_too() {
        let links = vec!["https://git.vedaru.cn/Vedaru/linked".to_string()];
        let mut facts = entity(
            None,
            Some(identity("project-unrouted", None, Some("Unrouted"))),
        );
        facts.links = &links;
        let placement = place(&routes(), Some(&location()), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/linked");
        assert_eq!(placement.origin, Origin::Link);
    }

    #[test]
    fn a_link_to_another_host_is_not_a_candidate() {
        let links = vec!["https://github.com/h-paetzold/linforge".to_string()];
        let mut facts = entity(Some(identity("project-ghost", None, None)), None);
        facts.links = &links;
        let placement = place(&routes(), Some(&location()), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::Default);
        assert_eq!(placement.routed, None);
    }

    #[test]
    fn a_link_the_connector_cannot_parse_is_ignored_rather_than_fatal() {
        // It matches the host but names no scope: the host root, and a URL with a
        // trailing fragment the pattern still captures is not a scope either.
        let links = vec![
            "https://git.vedaru.cn/".to_string(),
            "not a url".to_string(),
        ];
        let mut facts = entity(Some(identity("project-odd", None, None)), None);
        facts.links = &links;
        let placement = place(&routes(), Some(&location()), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/linear-cli-rs");
        assert_eq!(placement.origin, Origin::Default);
    }

    #[test]
    fn an_explicit_route_beats_a_link() {
        // project-kuro has an explicit route; the link points elsewhere.
        let links = vec!["https://git.vedaru.cn/Vedaru/linked".to_string()];
        let mut facts = entity(Some(identity("project-kuro", None, None)), None);
        facts.issue = Some("VED-200");
        facts.links = &links;
        let placement = place(&routes(), Some(&location()), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/kuro", "the explicit route wins");
        assert_eq!(placement.origin, Origin::Route);
    }

    #[test]
    fn an_issue_rule_beats_a_link_too() {
        let links = vec!["https://git.vedaru.cn/Vedaru/linked".to_string()];
        let mut facts = entity(None, None);
        facts.issue = Some("VED-200");
        facts.links = &links;
        let placement = place(&routes(), Some(&location()), "Vedaru/linear-cli-rs", facts);
        assert_eq!(placement.scope, "Vedaru/one-off");
    }

    #[test]
    fn a_link_pattern_needs_a_capture_and_an_empty_one_is_not_a_scope() {
        assert_eq!(Location::parse("https://git.vedaru.cn/"), None);
        let location = location();
        assert_eq!(
            location.scope("https://git.vedaru.cn/Vedaru/kuro"),
            Some("Vedaru/kuro")
        );
        assert_eq!(location.scope("https://git.vedaru.cn/"), None);
        assert_eq!(location.scope("https://github.com/a/b"), None);
    }

    #[test]
    fn routes_name_their_scopes_distinctly() {
        let routes = Routes::new(vec![
            project_route("a", "Vedaru/one"),
            issue_route("VED-1", "Vedaru/one"),
            label_route("x", "Vedaru/two"),
        ]);
        assert_eq!(routes.scopes(), vec!["Vedaru/one", "Vedaru/two"]);
    }

    #[test]
    fn a_route_names_at_least_one_key() {
        assert!(project_route("p", "s").has_key());
        assert!(issue_route("VED-1", "s").has_key());
        assert!(label_route("l", "s").has_key());
        assert!(!Route {
            project: None,
            issue: None,
            label: None,
            scope: "s".into(),
        }
        .has_key());
    }
}
