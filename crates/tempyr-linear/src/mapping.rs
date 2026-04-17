use std::collections::HashMap;

use tempyr_core::node::Node;

use crate::queries::WorkflowState;

/// Maps between Tempyr statuses and Linear workflow states.
pub struct StatusMapper {
    /// state_name (lowercased) -> state_id
    name_to_id: HashMap<String, String>,
    /// state_id -> state_name
    id_to_name: HashMap<String, String>,
}

impl StatusMapper {
    pub fn new(states: Vec<WorkflowState>) -> Self {
        let mut name_to_id = HashMap::new();
        let mut id_to_name = HashMap::new();
        for state in states {
            name_to_id.insert(state.name.to_lowercase(), state.id.clone());
            id_to_name.insert(state.id, state.name);
        }
        Self {
            name_to_id,
            id_to_name,
        }
    }

    /// Get Linear workflow state ID for a Tempyr status.
    pub fn to_linear_state_id(
        &self,
        node_type: &str,
        gf_status: &str,
        overrides: &HashMap<String, HashMap<String, String>>,
    ) -> Option<String> {
        // Check user overrides first
        if let Some(type_overrides) = overrides.get(node_type)
            && let Some(linear_name) = type_overrides.get(gf_status)
        {
            return self.name_to_id.get(&linear_name.to_lowercase()).cloned();
        }

        // Default mapping
        let linear_name = default_status_to_linear(node_type, gf_status)?;
        self.name_to_id.get(&linear_name.to_lowercase()).cloned()
    }

    /// Get Tempyr status from a Linear workflow state name.
    pub fn from_linear_state(&self, node_type: &str, linear_state_name: &str) -> Option<String> {
        default_linear_to_status(node_type, linear_state_name)
    }

    /// Get state name by ID.
    pub fn state_name(&self, state_id: &str) -> Option<&str> {
        self.id_to_name.get(state_id).map(|s| s.as_str())
    }
}

/// Default Tempyr status -> Linear state name mapping.
fn default_status_to_linear(node_type: &str, gf_status: &str) -> Option<String> {
    let name = match node_type {
        "task" => match gf_status {
            "backlog" => "Backlog",
            "in_progress" => "In Progress",
            "done" => "Done",
            "blocked" => "Blocked",
            "cut" => "Canceled",
            _ => return None,
        },
        "feature" => match gf_status {
            "draft" => "Backlog",
            "active" => "In Progress",
            "completed" => "Done",
            "superseded" => "Canceled",
            "archived" => "Canceled",
            _ => return None,
        },
        "epic" => match gf_status {
            "draft" => "planned",
            "active" => "started",
            "completed" => "completed",
            "archived" => "canceled",
            _ => return None,
        },
        _ => return None,
    };
    Some(name.to_string())
}

/// Default Linear state name -> Tempyr status mapping.
fn default_linear_to_status(node_type: &str, linear_state: &str) -> Option<String> {
    let lower = linear_state.to_lowercase();
    let status = match node_type {
        "task" => match lower.as_str() {
            "backlog" | "triage" => "backlog",
            "in progress" | "started" => "in_progress",
            "done" | "completed" => "done",
            "blocked" => "blocked",
            "canceled" | "cancelled" | "duplicate" => "cut",
            _ => return None,
        },
        "feature" => match lower.as_str() {
            "backlog" | "triage" => "draft",
            "in progress" | "started" => "active",
            "done" | "completed" => "completed",
            "canceled" | "cancelled" | "duplicate" => "superseded",
            _ => return None,
        },
        "epic" => match lower.as_str() {
            "planned" | "backlog" => "draft",
            "started" | "in progress" => "active",
            "completed" | "done" => "completed",
            "paused" => "active",
            "canceled" | "cancelled" => "archived",
            _ => return None,
        },
        _ => return None,
    };
    Some(status.to_string())
}

/// Generate a kebab-case node ID from a title and type prefix.
pub fn slugify(title: &str, node_type: &str) -> String {
    let prefix = match node_type {
        "epic" => "epic",
        "feature" => "feat",
        "task" => "task",
        _ => node_type,
    };

    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive dashes and trim
    let mut result = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash && !result.is_empty() {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }

    // Trim trailing dash and limit length
    let trimmed = result.trim_end_matches('-');
    let max_slug_len = 50;
    let truncated = if trimmed.len() > max_slug_len {
        // Don't cut in the middle of a word
        match trimmed[..max_slug_len].rfind('-') {
            Some(pos) => &trimmed[..pos],
            None => &trimmed[..max_slug_len],
        }
    } else {
        trimmed
    };

    format!("{prefix}-{truncated}")
}

/// Extract the first paragraph from a node body as a summary.
pub fn body_summary(body: &str, max_len: usize) -> String {
    // Skip the H1 heading line if present
    let text = body
        .lines()
        .skip_while(|l| l.starts_with('#') || l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if text.len() <= max_len {
        text
    } else {
        match text[..max_len].rfind(' ') {
            Some(pos) => format!("{}...", &text[..pos]),
            None => format!("{}...", &text[..max_len]),
        }
    }
}

/// Extract the title from a node (first H1 heading or fallback to ID).
pub fn node_title(node: &Node) -> String {
    for line in node.body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            return heading.trim().to_string();
        }
    }
    node.id().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_basic() {
        assert_eq!(
            slugify("Build Auth System", "task"),
            "task-build-auth-system"
        );
        assert_eq!(slugify("Session Replay", "feature"), "feat-session-replay");
        assert_eq!(slugify("Platform V2", "epic"), "epic-platform-v2");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(
            slugify("Add SSO (SAML + OIDC)", "task"),
            "task-add-sso-saml-oidc"
        );
        assert_eq!(
            slugify("Fix bug #123: auth fails", "task"),
            "task-fix-bug-123-auth-fails"
        );
    }

    #[test]
    fn test_slugify_truncates_long_titles() {
        let long = "a]".repeat(100);
        let slug = slugify(&long, "task");
        assert!(slug.len() <= 60); // prefix + dash + max_slug_len
    }

    #[test]
    fn test_status_mapping_task() {
        let states = vec![
            WorkflowState {
                id: "s1".into(),
                name: "Backlog".into(),
                state_type: "backlog".into(),
            },
            WorkflowState {
                id: "s2".into(),
                name: "In Progress".into(),
                state_type: "started".into(),
            },
            WorkflowState {
                id: "s3".into(),
                name: "Done".into(),
                state_type: "completed".into(),
            },
            WorkflowState {
                id: "s4".into(),
                name: "Canceled".into(),
                state_type: "canceled".into(),
            },
        ];
        let mapper = StatusMapper::new(states);
        let no_overrides = HashMap::new();

        assert_eq!(
            mapper.to_linear_state_id("task", "backlog", &no_overrides),
            Some("s1".into())
        );
        assert_eq!(
            mapper.to_linear_state_id("task", "in_progress", &no_overrides),
            Some("s2".into())
        );
        assert_eq!(
            mapper.to_linear_state_id("task", "done", &no_overrides),
            Some("s3".into())
        );
        assert_eq!(
            mapper.to_linear_state_id("task", "cut", &no_overrides),
            Some("s4".into())
        );
    }

    #[test]
    fn test_status_mapping_reverse() {
        let states = vec![WorkflowState {
            id: "s1".into(),
            name: "Backlog".into(),
            state_type: "backlog".into(),
        }];
        let mapper = StatusMapper::new(states);

        assert_eq!(
            mapper.from_linear_state("task", "Backlog"),
            Some("backlog".into())
        );
        assert_eq!(
            mapper.from_linear_state("feature", "Backlog"),
            Some("draft".into())
        );
        assert_eq!(mapper.from_linear_state("task", "Unknown"), None);
    }

    #[test]
    fn test_body_summary() {
        let body =
            "# My Feature\n\nThis is the first paragraph of the description.\n\nSecond paragraph.";
        assert_eq!(
            body_summary(body, 200),
            "This is the first paragraph of the description."
        );
    }

    #[test]
    fn test_body_summary_truncation() {
        let body = "# Title\n\nThis is a very long paragraph that should be truncated at a reasonable boundary.";
        let summary = body_summary(body, 30);
        assert!(summary.ends_with("..."));
        assert!(summary.len() <= 35);
    }
}
