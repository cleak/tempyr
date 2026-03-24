use chrono::NaiveDate;

use crate::edge::EdgeEntry;
use crate::node::Node;

/// Filter configuration for temporal queries.
#[derive(Debug, Clone, Default)]
pub struct TemporalFilter {
    /// If set, only include edges valid at this date.
    pub as_of: Option<NaiveDate>,
    /// If true, include superseded/historical edges.
    pub include_history: bool,
}

impl TemporalFilter {
    pub fn current() -> Self {
        Self::default()
    }

    pub fn as_of(date: NaiveDate) -> Self {
        Self {
            as_of: Some(date),
            include_history: false,
        }
    }

    pub fn with_history() -> Self {
        Self {
            as_of: None,
            include_history: true,
        }
    }
}

/// Filter edges based on temporal validity.
///
/// When `as_of` is set: include only edges where
///   `valid_from <= as_of` AND (`valid_until IS NULL` OR `valid_until > as_of`)
///
/// When `include_history` is true: include all edges regardless of temporal fields.
///
/// Default (no as_of, no include_history): include edges where `valid_until` is None
/// (i.e., currently active edges).
pub fn filter_edges<'a>(
    edges: &'a [EdgeEntry],
    filter: &TemporalFilter,
) -> Vec<&'a EdgeEntry> {
    if filter.include_history {
        return edges.iter().collect();
    }

    edges
        .iter()
        .filter(|edge| is_edge_visible(edge, filter))
        .collect()
}

/// Check if a single edge is visible under the given temporal filter.
fn is_edge_visible(edge: &EdgeEntry, filter: &TemporalFilter) -> bool {
    match filter.as_of {
        Some(as_of) => {
            // valid_from must be <= as_of (if set)
            if let Some(from) = edge.valid_from
                && from > as_of {
                    return false;
                }
            // valid_until must be None or > as_of
            if let Some(until) = edge.valid_until
                && until <= as_of {
                    return false;
                }
            true
        }
        None => {
            // Default: only show currently active edges (valid_until is None)
            edge.valid_until.is_none()
        }
    }
}

/// Check if a node is visible under the given temporal filter.
///
/// Superseded nodes (status = "superseded") are excluded from default renders
/// but included with `include_history`.
pub fn is_node_visible(node: &Node, filter: &TemporalFilter) -> bool {
    if filter.include_history {
        return true;
    }

    // Superseded nodes are hidden by default
    if node.status() == Some("superseded") {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::EdgeEntry;
    use crate::node::parse_node;
    use std::path::PathBuf;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn make_edge(target: &str, edge_type: &str, from: Option<NaiveDate>, until: Option<NaiveDate>) -> EdgeEntry {
        EdgeEntry {
            target: target.to_string(),
            edge_type: edge_type.to_string(),
            valid_from: from,
            valid_until: until,
            annotation: None,
        }
    }

    #[test]
    fn test_filter_default_hides_expired() {
        let edges = vec![
            make_edge("a", "depends_on", None, None),                          // active
            make_edge("b", "depends_on", Some(date(2026, 1, 1)), None),        // active (has from, no until)
            make_edge("c", "depends_on", Some(date(2026, 1, 1)), Some(date(2026, 3, 1))), // expired
        ];

        let filter = TemporalFilter::current();
        let visible = filter_edges(&edges, &filter);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].target, "a");
        assert_eq!(visible[1].target, "b");
    }

    #[test]
    fn test_filter_as_of_date() {
        let edges = vec![
            make_edge("a", "x", Some(date(2026, 1, 15)), Some(date(2026, 3, 1))), // Jan-Mar
            make_edge("b", "x", Some(date(2026, 4, 1)), None),                      // Apr onward
            make_edge("c", "x", None, None),                                         // always
        ];

        // February: a is visible, b is not yet, c is always
        let filter = TemporalFilter::as_of(date(2026, 2, 15));
        let visible = filter_edges(&edges, &filter);
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|e| e.target == "a"));
        assert!(visible.iter().any(|e| e.target == "c"));

        // April: a expired, b started, c always
        let filter = TemporalFilter::as_of(date(2026, 4, 15));
        let visible = filter_edges(&edges, &filter);
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|e| e.target == "b"));
        assert!(visible.iter().any(|e| e.target == "c"));
    }

    #[test]
    fn test_filter_include_history() {
        let edges = vec![
            make_edge("a", "x", None, Some(date(2020, 1, 1))), // expired long ago
            make_edge("b", "x", None, None),                    // active
        ];

        let filter = TemporalFilter::with_history();
        let visible = filter_edges(&edges, &filter);
        assert_eq!(visible.len(), 2); // both visible
    }

    #[test]
    fn test_node_visibility_superseded() {
        let content = "---\nid: old-decision\ntype: decision\nstatus: superseded\n---\n# Old\n";
        let node = parse_node(content, PathBuf::from("test.md")).unwrap();

        assert!(!is_node_visible(&node, &TemporalFilter::current()));
        assert!(is_node_visible(&node, &TemporalFilter::with_history()));
    }

    #[test]
    fn test_node_visibility_active() {
        let content = "---\nid: decision-a\ntype: decision\nstatus: decided\n---\n# A\n";
        let node = parse_node(content, PathBuf::from("test.md")).unwrap();

        assert!(is_node_visible(&node, &TemporalFilter::current()));
    }
}
