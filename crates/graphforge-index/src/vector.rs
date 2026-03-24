use crate::indexer::Index;
use crate::Result;

/// A vector search result.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub node_id: String,
    pub similarity: f64,
}

impl Index {
    /// Create the embedding storage tables.
    pub fn create_embedding_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS embedding_cache (
                node_id      TEXT PRIMARY KEY,
                content_hash TEXT NOT NULL,
                embedding    BLOB NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    /// Store an embedding for a node (keyed by content hash for cache invalidation).
    pub fn store_embedding(
        &self,
        node_id: &str,
        content_hash: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let blob = embedding_to_blob(embedding);
        self.conn.execute(
            "INSERT OR REPLACE INTO embedding_cache (node_id, content_hash, embedding) VALUES (?1, ?2, ?3)",
            rusqlite::params![node_id, content_hash, blob],
        )?;
        Ok(())
    }

    /// Get the cached embedding for a node, if the content hash matches.
    pub fn get_embedding(
        &self,
        node_id: &str,
        content_hash: &str,
    ) -> Result<Option<Vec<f32>>> {
        let result = self.conn.query_row(
            "SELECT embedding FROM embedding_cache WHERE node_id = ?1 AND content_hash = ?2",
            rusqlite::params![node_id, content_hash],
            |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            },
        );

        match result {
            Ok(blob) => Ok(Some(blob_to_embedding(&blob))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Check if a node has a cached embedding with the given content hash.
    pub fn has_valid_embedding(&self, node_id: &str, content_hash: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM embedding_cache WHERE node_id = ?1 AND content_hash = ?2",
            rusqlite::params![node_id, content_hash],
            |row| row.get(0),
        ).unwrap_or(0);
        Ok(count > 0)
    }

    /// KNN vector similarity search. Loads all embeddings and computes cosine similarity.
    /// For graphs under ~10k nodes this is fast enough (< 10ms).
    pub fn vector_search(
        &self,
        query_embedding: &[f32],
        max_results: usize,
        node_type_filter: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>> {
        let sql = if node_type_filter.is_some() {
            "SELECT ec.node_id, ec.embedding FROM embedding_cache ec \
             JOIN nodes n ON n.id = ec.node_id WHERE n.node_type = ?1"
        } else {
            "SELECT ec.node_id, ec.embedding FROM embedding_cache ec"
        };

        let mut stmt = self.conn.prepare(sql)?;

        let rows: Vec<(String, Vec<u8>)> = if let Some(nt) = node_type_filter {
            stmt.query_map([nt], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let mut results: Vec<VectorSearchResult> = rows
            .iter()
            .map(|(node_id, blob)| {
                let emb = blob_to_embedding(blob);
                let sim = cosine_similarity(query_embedding, &emb);
                VectorSearchResult {
                    node_id: node_id.clone(),
                    similarity: sim,
                }
            })
            .collect();

        results.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max_results);

        Ok(results)
    }

    /// Count the number of cached embeddings.
    pub fn embedding_count(&self) -> Result<usize> {
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM embedding_cache",
            [],
            |row| row.get(0),
        ).unwrap_or(0);
        Ok(count)
    }

    /// Remove embeddings for nodes no longer in the index.
    pub fn prune_embeddings(&self) -> Result<usize> {
        let deleted = self.conn.execute(
            "DELETE FROM embedding_cache WHERE node_id NOT IN (SELECT id FROM nodes)",
            [],
        )?;
        Ok(deleted)
    }
}

/// Convert f32 slice to raw byte blob.
fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Convert raw byte blob back to f32 vector.
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Index;

    fn make_index() -> Index {
        let index = Index::create_in_memory().unwrap();
        index.create_embedding_tables().unwrap();
        index
    }

    #[test]
    fn test_embedding_roundtrip() {
        let original = vec![0.1f32, 0.2, 0.3, 0.4];
        let blob = embedding_to_blob(&original);
        let recovered = blob_to_embedding(&blob);
        assert_eq!(original.len(), recovered.len());
        for (a, b) in original.iter().zip(recovered.iter()) {
            assert!((a - b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0f32, 0.0];
        let b = vec![-1.0f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_store_and_retrieve_embedding() {
        let index = make_index();

        // Need a node in the nodes table first
        index.conn.execute(
            "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('n1', 'feature', 'f.md', 'hash1', 'body', 'title')",
            [],
        ).unwrap();

        let emb = vec![0.1f32, 0.2, 0.3, 0.4];
        index.store_embedding("n1", "hash1", &emb).unwrap();

        let cached = index.get_embedding("n1", "hash1").unwrap();
        assert!(cached.is_some());
        let cached = cached.unwrap();
        assert_eq!(cached.len(), 4);
        assert!((cached[0] - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn test_embedding_cache_invalidation() {
        let index = make_index();

        index.conn.execute(
            "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('n1', 'feature', 'f.md', 'hash1', 'body', 'title')",
            [],
        ).unwrap();

        let emb = vec![0.1f32, 0.2, 0.3];
        index.store_embedding("n1", "hash1", &emb).unwrap();

        // Should not find with different hash
        assert!(index.get_embedding("n1", "hash2").unwrap().is_none());

        // has_valid_embedding checks
        assert!(index.has_valid_embedding("n1", "hash1").unwrap());
        assert!(!index.has_valid_embedding("n1", "hash_changed").unwrap());
    }

    #[test]
    fn test_vector_search_knn() {
        let index = make_index();

        // Insert some nodes
        for (id, emb) in &[
            ("n1", vec![1.0f32, 0.0, 0.0]),
            ("n2", vec![0.9, 0.1, 0.0]),
            ("n3", vec![0.0, 1.0, 0.0]),
            ("n4", vec![0.0, 0.0, 1.0]),
        ] {
            index.conn.execute(
                &format!("INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('{id}', 'feature', 'f.md', 'h', '', '')"),
                [],
            ).unwrap();
            index.store_embedding(id, "h", emb).unwrap();
        }

        let query = vec![1.0f32, 0.0, 0.0];
        let results = index.vector_search(&query, 2, None).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node_id, "n1"); // exact match
        assert_eq!(results[1].node_id, "n2"); // closest neighbor
        assert!(results[0].similarity > results[1].similarity);
    }

    #[test]
    fn test_vector_search_filtered() {
        let index = make_index();

        index.conn.execute(
            "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('f1', 'feature', 'f.md', 'h', '', '')",
            [],
        ).unwrap();
        index.conn.execute(
            "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('d1', 'decision', 'd.md', 'h', '', '')",
            [],
        ).unwrap();

        index.store_embedding("f1", "h", &[1.0, 0.0, 0.0]).unwrap();
        index.store_embedding("d1", "h", &[0.9, 0.1, 0.0]).unwrap();

        let query = vec![1.0f32, 0.0, 0.0];
        let results = index.vector_search(&query, 10, Some("decision")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "d1");
    }

    #[test]
    fn test_embedding_count() {
        let index = make_index();
        assert_eq!(index.embedding_count().unwrap(), 0);

        index.conn.execute(
            "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('n1', 'feature', 'f.md', 'h', '', '')",
            [],
        ).unwrap();
        index.store_embedding("n1", "h", &[1.0, 0.0]).unwrap();

        assert_eq!(index.embedding_count().unwrap(), 1);
    }

    #[test]
    fn test_prune_embeddings() {
        let index = make_index();

        index.conn.execute(
            "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('n1', 'feature', 'f.md', 'h', '', '')",
            [],
        ).unwrap();
        index.store_embedding("n1", "h", &[1.0]).unwrap();
        index.store_embedding("orphan", "h", &[0.5]).unwrap(); // no matching node

        let pruned = index.prune_embeddings().unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(index.embedding_count().unwrap(), 1);
    }
}
