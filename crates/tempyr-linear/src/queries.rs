use chrono::{DateTime, Utc};
use serde::Deserialize;

// ─── Pagination ────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection<T> {
    pub nodes: Vec<T>,
    pub page_info: PageInfo,
}

// ─── Viewer ────────────────────────────────────────────

pub const VIEWER_QUERY: &str = r#"
    query {
        viewer {
            id
            name
            email
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
pub struct ViewerData {
    pub viewer: Viewer,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Viewer {
    pub id: String,
    pub name: String,
    pub email: String,
}

// ─── Teams ─────────────────────────────────────────────

pub const TEAMS_QUERY: &str = r#"
    query {
        teams {
            nodes {
                id
                name
                key
            }
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
pub struct TeamsData {
    pub teams: Connection<Team>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub key: String,
}

// ─── Workflow States ───────────────────────────────────

pub const WORKFLOW_STATES_QUERY: &str = r#"
    query WorkflowStates($teamId: String!) {
        workflowStates(filter: { team: { id: { eq: $teamId } } }) {
            nodes {
                id
                name
                type
            }
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStatesData {
    pub workflow_states: Connection<WorkflowState>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub state_type: String,
}

// ─── Projects ──────────────────────────────────────────

pub const PROJECT_CREATE_MUTATION: &str = r#"
    mutation ProjectCreate($input: ProjectCreateInput!) {
        projectCreate(input: $input) {
            success
            project {
                id
                name
                url
                state
                updatedAt
            }
        }
    }
"#;

pub const PROJECT_UPDATE_MUTATION: &str = r#"
    mutation ProjectUpdate($id: String!, $input: ProjectUpdateInput!) {
        projectUpdate(id: $id, input: $input) {
            success
            project {
                id
                name
                url
                state
                updatedAt
            }
        }
    }
"#;

pub const PROJECTS_QUERY: &str = r#"
    query Projects($first: Int, $after: String) {
        projects(first: $first, after: $after) {
            nodes {
                id
                name
                description
                state
                url
                updatedAt
            }
            pageInfo {
                hasNextPage
                endCursor
            }
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateData {
    pub project_create: ProjectPayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectPayload {
    pub success: bool,
    pub project: Option<Project>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdateData {
    pub project_update: ProjectPayload,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsData {
    pub projects: Connection<Project>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    #[serde(default)]
    pub url: Option<String>,
    pub updated_at: DateTime<Utc>,
}

// ─── Issues ────────────────────────────────────────────

pub const ISSUE_CREATE_MUTATION: &str = r#"
    mutation IssueCreate($input: IssueCreateInput!) {
        issueCreate(input: $input) {
            success
            issue {
                id
                identifier
                title
                url
                state {
                    id
                    name
                    type
                }
                updatedAt
            }
        }
    }
"#;

pub const ISSUE_UPDATE_MUTATION: &str = r#"
    mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
        issueUpdate(id: $id, input: $input) {
            success
            issue {
                id
                identifier
                title
                url
                state {
                    id
                    name
                    type
                }
                updatedAt
            }
        }
    }
"#;

pub const ISSUES_QUERY: &str = r#"
    query Issues($teamId: String!, $after: String, $updatedAfter: DateTime) {
        issues(
            filter: {
                team: { id: { eq: $teamId } }
                updatedAt: { gte: $updatedAfter }
            }
            first: 50
            after: $after
            orderBy: updatedAt
        ) {
            nodes {
                id
                identifier
                title
                description
                state {
                    id
                    name
                    type
                }
                parent {
                    id
                    identifier
                }
                project {
                    id
                    name
                }
                url
                updatedAt
                archivedAt
            }
            pageInfo {
                hasNextPage
                endCursor
            }
        }
    }
"#;

pub const ISSUE_QUERY: &str = r#"
    query Issue($id: String!) {
        issue(id: $id) {
            id
            identifier
            title
            description
            state {
                id
                name
                type
            }
            parent {
                id
                identifier
            }
            project {
                id
                name
            }
            url
            updatedAt
            archivedAt
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCreateData {
    pub issue_create: IssuePayload,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueUpdateData {
    pub issue_update: IssuePayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssuePayload {
    pub success: bool,
    pub issue: Option<Issue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuesData {
    pub issues: Connection<Issue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueData {
    pub issue: Issue,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: IssueState,
    #[serde(default)]
    pub parent: Option<IssueRef>,
    #[serde(default)]
    pub project: Option<ProjectRef>,
    #[serde(default)]
    pub url: Option<String>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub state_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueRef {
    pub id: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
}

// ─── Attachments ───────────────────────────────────────

pub const ATTACHMENT_CREATE_MUTATION: &str = r#"
    mutation AttachmentCreate($input: AttachmentCreateInput!) {
        attachmentCreate(input: $input) {
            success
            attachment {
                id
            }
        }
    }
"#;

pub const ATTACHMENT_DELETE_MUTATION: &str = r#"
    mutation AttachmentDelete($id: String!) {
        attachmentDelete(id: $id) {
            success
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCreateData {
    pub attachment_create: AttachmentPayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentPayload {
    pub success: bool,
    pub attachment: Option<AttachmentRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentRef {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDeleteData {
    pub attachment_delete: SuccessPayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SuccessPayload {
    pub success: bool,
}

// ─── Issue Relations ───────────────────────────────────

pub const ISSUE_RELATION_CREATE_MUTATION: &str = r#"
    mutation IssueRelationCreate($input: IssueRelationCreateInput!) {
        issueRelationCreate(input: $input) {
            success
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRelationCreateData {
    pub issue_relation_create: SuccessPayload,
}

// ─── Labels ────────────────────────────────────────────

pub const LABEL_CREATE_MUTATION: &str = r#"
    mutation IssueLabelCreate($input: IssueLabelCreateInput!) {
        issueLabelCreate(input: $input) {
            success
            issueLabel {
                id
                name
            }
        }
    }
"#;

pub const LABELS_QUERY: &str = r#"
    query IssueLabels($teamId: String) {
        issueLabels(
            filter: { team: { id: { eq: $teamId } } }
        ) {
            nodes {
                id
                name
            }
        }
    }
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCreateData {
    pub issue_label_create: LabelPayload,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelPayload {
    pub success: bool,
    pub issue_label: Option<Label>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelsData {
    pub issue_labels: Connection<Label>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub id: String,
    pub name: String,
}
