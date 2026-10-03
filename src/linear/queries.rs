// ---------------------------------------------------------------------------
// GraphQL documents
// ---------------------------------------------------------------------------

pub(crate) const GET_ISSUE_ID_QUERY: &str = r#"
query GetIssueId($id: String!) {
  issue(id: $id) {
    id
  }
}
"#;

pub(crate) const GET_WORKFLOW_STATES_QUERY: &str = r#"
query GetWorkflowStates($teamKey: String!) {
  team(id: $teamKey) {
    states {
      nodes {
        id
        name
        type
        position
      }
    }
  }
}
"#;

pub(crate) const GET_WORKFLOW_STATES_WITH_TEAMS_QUERY: &str = r#"
query GetWorkflowStatesWithTeams($filter: WorkflowStateFilter, $after: String) {
  workflowStates(filter: $filter, first: 250, after: $after) {
    nodes {
      id
      name
      type
      team {
        key
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

pub(crate) const GET_ISSUE_DETAILS_QUERY: &str = r#"
query GetIssueDetails($id: String!) {
  issue(id: $id) {
    identifier
    title
    description
    url
    branchName
    state {
      name
      color
    }
    assignee {
      name
      displayName
    }
    priority
    project {
      name
    }
    projectMilestone {
      name
    }
    cycle {
      id
      number
      name
      isActive
      isNext
      isPrevious
      isFuture
      isPast
    }
    team {
      activeCycle {
        number
      }
    }
    labels(first: 50) {
      nodes {
        id
        name
        color
      }
    }
    parent {
      identifier
      title
      state {
        name
        color
      }
    }
    children(first: 250) {
      nodes {
        identifier
        title
        state {
          name
          color
        }
      }
    }
    attachments(first: 50) {
      nodes {
        id
        title
        url
        subtitle
        sourceType
        metadata
        createdAt
      }
    }
    documents(first: 50) {
      nodes {
        id
        title
        slugId
        url
        createdAt
        updatedAt
      }
    }
  }
}
"#;

pub(crate) const GET_ISSUE_DETAILS_WITH_COMMENTS_QUERY: &str = r#"
query GetIssueDetailsWithComments($id: String!) {
  issue(id: $id) {
    identifier
    title
    description
    url
    branchName
    state {
      name
      color
    }
    assignee {
      name
      displayName
    }
    priority
    project {
      name
    }
    projectMilestone {
      name
    }
    cycle {
      id
      number
      name
      isActive
      isNext
      isPrevious
      isFuture
      isPast
    }
    team {
      activeCycle {
        number
      }
    }
    labels(first: 50) {
      nodes {
        id
        name
        color
      }
    }
    parent {
      identifier
      title
      state {
        name
        color
      }
    }
    children(first: 250) {
      nodes {
        identifier
        title
        state {
          name
          color
        }
      }
    }
    comments(first: 50, orderBy: createdAt) {
      nodes {
        id
        body
        quotedText
        createdAt
        url
        resolvedAt
        resolvingCommentId
        resolvingUser {
          name
          displayName
        }
        user {
          name
          displayName
        }
        externalUser {
          name
          displayName
        }
        parent {
          id
        }
      }
    }
    attachments(first: 50) {
      nodes {
        id
        title
        url
        subtitle
        sourceType
        metadata
        createdAt
      }
    }
    documents(first: 50) {
      nodes {
        id
        title
        slugId
        url
        createdAt
        updatedAt
      }
    }
  }
}
"#;

pub(crate) const FETCH_PARENT_ISSUE_TITLE_QUERY: &str = r#"
query FetchParentIssueTitle($id: String!) {
  issue(id: $id) {
    identifier
    title
  }
}
"#;

pub(crate) const FETCH_PARENT_ISSUE_DATA_QUERY: &str = r#"
query FetchParentIssueData($id: String!) {
  issue(id: $id) {
    identifier
    title
    project {
      id
    }
  }
}
"#;

pub(crate) const FETCH_ISSUES_QUERY: &str = r#"
query FetchIssues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String, $includeArchived: Boolean) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after, includeArchived: $includeArchived) {
    nodes {
      id
      identifier
      title
      priority
      priorityLabel
      estimate
      url
      createdAt
      updatedAt
      state {
        id
        name
        type
        color
        position
      }
      assignee {
        id
        name
        displayName
        initials
        avatarUrl
      }
      team {
        id
        key
        name
        cyclesEnabled
        activeCycle {
          number
        }
      }
      project {
        id
        name
        url
      }
      projectMilestone {
        id
        name
      }
      cycle {
        id
        number
        name
        isActive
        isNext
        isPrevious
        isFuture
        isPast
      }
      labels {
        nodes {
          id
          name
          color
        }
      }
      inverseRelations(first: 100) {
        nodes {
          type
          issue {
            state {
              type
            }
          }
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

pub(crate) const SEARCH_ISSUES_QUERY: &str = r#"
query SearchIssues($term: String!, $filter: IssueFilter, $includeArchived: Boolean, $first: Int, $after: String, $includeComments: Boolean, $orderBy: PaginationOrderBy) {
  searchIssues(term: $term, filter: $filter, includeArchived: $includeArchived, first: $first, after: $after, includeComments: $includeComments, orderBy: $orderBy) {
    nodes {
      id
      identifier
      title
      priority
      priorityLabel
      estimate
      url
      createdAt
      updatedAt
      state {
        id
        name
        type
        color
        position
      }
      assignee {
        id
        name
        displayName
        initials
        avatarUrl
      }
      team {
        id
        key
        name
        cyclesEnabled
        activeCycle {
          number
        }
      }
      project {
        id
        name
        url
      }
      projectMilestone {
        id
        name
      }
      cycle {
        id
        number
        name
        isActive
        isNext
        isPrevious
        isFuture
        isPast
      }
      labels {
        nodes {
          id
          name
          color
        }
      }
      inverseRelations(first: 100) {
        nodes {
          type
          issue {
            state {
              type
            }
          }
        }
      }
      metadata
    }
    pageInfo {
      hasNextPage
      endCursor
    }
    totalCount
  }
}
"#;

pub(crate) const SEARCH_TEAMS_QUERY: &str = r#"
query SearchTeams($key: String!) {
  teams(filter: { key: { containsIgnoreCase: $key } }) {
    nodes {
      id
      key
      name
    }
  }
}
"#;

pub(crate) const GET_ALL_TEAMS_QUERY: &str = r#"
query GetAllTeams {
  teams {
    nodes {
      id
      key
      name
    }
  }
}
"#;

/// The count Linear will *state*: a team's issue total, with no nodes to walk.
///
/// `IssueConnection` has no count field - asking for `totalCount` is a validation error -
/// so this is the only number the API hands over, and it takes no filter arguments. It
/// answers the unfiltered case exactly; anything filtered counts ids instead.
pub(crate) const TEAM_ISSUE_COUNTS_QUERY: &str = r#"
query TeamIssueCounts($keys: [String!]) {
  teams(filter: { key: { in: $keys } }) {
    nodes {
      key
      issueCount
    }
  }
}
"#;

/// The same stated number, for every team the token can see (`--all-teams`).
///
/// A separate query rather than an empty key filter: `in: []` matches no team, which would
/// answer 0 to a question about the whole workspace.
pub(crate) const ALL_TEAM_ISSUE_COUNTS_QUERY: &str = r#"
query AllTeamIssueCounts {
  teams {
    nodes {
      key
      issueCount
    }
  }
}
"#;

/// Counting a *filtered* set: the smallest thing an issue can be, and the cursor to walk.
///
/// One page when the answer fits in one, more when it does not - but never a title, a state
/// or a body, which is the difference between answering "how many?" and fetching the issues
/// to find out.
pub(crate) const COUNT_ISSUES_QUERY: &str = r#"
query CountIssues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String, $includeArchived: Boolean) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after, includeArchived: $includeArchived) {
    nodes {
      id
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

pub(crate) const GET_LABELS_QUERY: &str = r#"
query GetTeamLabels($teamId: String!) {
  team(id: $teamId) {
    labels {
      nodes {
        id
        name
        color
      }
    }
  }
}
"#;

pub(crate) const GET_TEAM_MEMBERS_QUERY: &str = r#"
query GetTeamMembers($teamId: String!, $first: Int, $after: String) {
  team(id: $teamId) {
    members(first: $first, after: $after) {
      nodes {
        id
        name
        displayName
        email
        avatarUrl
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

pub(crate) const GET_ORGANIZATION_MEMBERS_QUERY: &str = r#"
query GetOrganizationMembers($first: Int, $after: String) {
  users(first: $first, after: $after) {
    nodes {
      id
      name
      displayName
      email
      avatarUrl
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

pub(crate) const GET_VIEWER_ID_QUERY: &str = r#"
query GetViewerId {
  viewer {
    id
  }
}
"#;

pub(crate) const LOOKUP_USER_QUERY: &str = r#"
query LookupUser($input: String!) {
  users(filter: {
    or: [
      { email: { eqIgnoreCase: $input } }
      { displayName: { eqIgnoreCase: $input } }
    ]
  }, first: 10) {
    nodes {
      id
      name
      displayName
      email
    }
  }
}
"#;

pub(crate) const GET_PROJECT_BY_NAME_QUERY: &str = r#"
query GetProjectByName($name: String!) {
  projects(filter: { name: { eqIgnoreCase: $name } }) {
    nodes {
      id
      name
    }
  }
}
"#;

pub(crate) const GET_PROJECTS_BY_NAME_QUERY: &str = r#"
query GetProjectsByName($name: String!) {
  projects(filter: { name: { containsIgnoreCase: $name } }) {
    nodes {
      id
      name
    }
  }
}
"#;

pub(crate) const FIND_PROJECT_BY_SLUG_QUERY: &str = r#"
query FindProjectBySlug($slugId: String!) {
  projects(filter: { slugId: { eq: $slugId } }) {
    nodes {
      id
    }
  }
}
"#;

pub(crate) const GET_PROJECTS_FOR_TEAM_QUERY: &str = r#"
query GetProjectsForTeam($teamId: String!) {
  team(id: $teamId) {
    projects {
      nodes {
        id
        name
        url
      }
    }
  }
}
"#;

pub(crate) const GET_ISSUE_LABEL_BY_NAME_QUERY: &str = r#"
query GetIssueLabelByName($name: String!, $teamKey: String!) {
  issueLabels(filter: {
    name: { eqIgnoreCase: $name }
    or: [
      { team: { key: { eq: $teamKey } } }
      { team: { null: true } }
    ]
  }) {
    nodes {
      id
      name
    }
  }
}
"#;

pub(crate) const GET_ISSUE_LABELS_BY_NAME_QUERY: &str = r#"
query GetIssueLabelsByName($name: String!, $teamKey: String!) {
  issueLabels(filter: {
    name: { containsIgnoreCase: $name }
    or: [
      { team: { key: { eq: $teamKey } } }
      { team: { null: true } }
    ]
  }) {
    nodes {
      id
      name
    }
  }
}
"#;

pub(crate) const GET_PROJECT_LABEL_BY_NAME_QUERY: &str = r#"
query GetProjectLabelByName($name: String!) {
  projectLabels(filter: { name: { eqIgnoreCase: $name } }) {
    nodes {
      id
      name
    }
  }
}
"#;

pub(crate) const GET_MILESTONE_BY_NAME_QUERY: &str = r#"
query GetMilestoneByName($projectId: String!, $name: String!) {
  project(id: $projectId) {
    projectMilestones(filter: { name: { eqIgnoreCase: $name } }) {
      nodes {
        id
        name
      }
    }
  }
}
"#;

pub(crate) const GET_PROJECT_STATUSES_QUERY: &str = r#"
query GetProjectStatuses {
  projectStatuses {
    nodes {
      id
      name
      type
    }
  }
}
"#;

pub(crate) const GET_TEAM_CYCLES_QUERY: &str = r#"
query GetTeamCycles($teamId: String!, $after: String) {
  team(id: $teamId) {
    id
    key
    name
    cyclesEnabled
    activeCycle {
      id
      number
      name
    }
    cycles(first: 250, after: $after) {
      nodes {
        id
        number
        name
        startsAt
        isNext
        isPrevious
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

pub(crate) const RESOLVE_INITIATIVE_BY_SLUG_QUERY: &str = r#"
query ResolveInitiativeBySlug($slugId: String!, $includeArchived: Boolean) {
  initiatives(
    filter: { slugId: { eq: $slugId } }
    includeArchived: $includeArchived
  ) {
    nodes {
      id
    }
  }
}
"#;

pub(crate) const RESOLVE_INITIATIVE_BY_NAME_QUERY: &str = r#"
query ResolveInitiativeByName($name: String!) {
  initiatives(filter: { name: { eqIgnoreCase: $name } }) {
    nodes {
      id
      name
      slugId
    }
  }
}
"#;

/// The archived-aware name lookup, used only by the two commands whose subject
/// is an archived initiative: `initiative unarchive` and `initiative delete`.
///
/// Upstream declares this document inline in each command's own local
/// `resolveInitiativeId` — `GetInitiativeByNameIncludeArchived` in
/// `initiative-unarchive.ts`, `GetInitiativeByNameForDelete` in
/// `initiative-delete.ts`; same body, different operation name. The port keeps
/// one shared document, named after the convention above, and both commands
/// reach it through `resolve_initiative_id_including_archived`.
///
/// `includeArchived: true` is a hardcoded argument, not a variable, exactly as
/// upstream writes it. The `slugId` selection is the port's own addition (the
/// shared resolver's ambiguity error lists name/slugId/id); the filter and the
/// flag are upstream's.
pub(crate) const RESOLVE_INITIATIVE_BY_NAME_INCLUDE_ARCHIVED_QUERY: &str = r#"
query ResolveInitiativeByNameIncludeArchived($name: String!) {
  initiatives(filter: { name: { eqIgnoreCase: $name } }, includeArchived: true) {
    nodes {
      id
      name
      slugId
    }
  }
}
"#;

pub(crate) const RESOLVE_RELEASES_QUERY: &str = r#"
query ResolveReleases($input: String!, $after: String) {
  releases(
    filter: {
      or: [
        { name: { eqIgnoreCase: $input } }
        { version: { eq: $input } }
      ]
    }
    first: 100
    after: $after
  ) {
    nodes {
      id
      name
      version
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;
