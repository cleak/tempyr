use crate::config::ProjectContext;
use tempyr_core::graph::Graph;

pub fn run(ctx: &ProjectContext, threshold: f64, json: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;

    let nodes: Vec<_> = graph.nodes.values().collect();
    let mut candidates: Vec<DuplicateCandidate> = Vec::new();

    // Compare all pairs of nodes of the same type
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            let a = nodes[i];
            let b = nodes[j];

            if a.node_type() != b.node_type() {
                continue;
            }

            let similarity = title_similarity(a.title(), b.title());
            if similarity >= threshold {
                candidates.push(DuplicateCandidate {
                    node_a: a.id().to_string(),
                    node_b: b.id().to_string(),
                    node_type: a.node_type().to_string(),
                    title_a: a.title().to_string(),
                    title_b: b.title().to_string(),
                    similarity,
                });
            }
        }
    }

    candidates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if json {
        let json_candidates: Vec<_> = candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "node_a": c.node_a,
                    "node_b": c.node_b,
                    "type": c.node_type,
                    "title_a": c.title_a,
                    "title_b": c.title_b,
                    "similarity": c.similarity,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_candidates)?);
        return Ok(());
    }

    if candidates.is_empty() {
        println!("No potential duplicates found (threshold: {threshold:.0}%).");
        return Ok(());
    }

    println!("Potential duplicates ({} found):\n", candidates.len());
    for c in &candidates {
        println!(
            "  {:.0}% similar: {} vs {} ({})",
            c.similarity * 100.0,
            c.node_a,
            c.node_b,
            c.node_type,
        );
        println!("    \"{}\" vs \"{}\"", c.title_a, c.title_b);
        println!();
    }
    println!("Use `tempyr rename` to merge duplicates by renaming one and updating references.");

    Ok(())
}

struct DuplicateCandidate {
    node_a: String,
    node_b: String,
    node_type: String,
    title_a: String,
    title_b: String,
    similarity: f64,
}

/// Simple title similarity using normalized longest common subsequence.
fn title_similarity(a: &str, b: &str) -> f64 {
    let a = a.to_lowercase();
    let b = b.to_lowercase();

    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_words: Vec<&str> = a.split_whitespace().collect();
    let b_words: Vec<&str> = b.split_whitespace().collect();

    let common = a_words.iter().filter(|w| b_words.contains(w)).count();
    let total = a_words.len().max(b_words.len());

    if total == 0 {
        0.0
    } else {
        common as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_title_similarity_identical() {
        assert!((title_similarity("Session Replay", "Session Replay") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_title_similarity_partial() {
        let sim = title_similarity("Session Replay Feature", "Session Replay Component");
        assert!(sim > 0.5);
    }

    #[test]
    fn test_title_similarity_different() {
        let sim = title_similarity("Authentication System", "Billing Engine");
        assert!(sim < 0.3);
    }

    #[test]
    fn test_title_similarity_case_insensitive() {
        assert!((title_similarity("Session replay", "SESSION REPLAY") - 1.0).abs() < f64::EPSILON);
    }
}
