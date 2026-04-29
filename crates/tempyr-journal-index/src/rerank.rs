//! Cross-encoder reranking for journal search.
//!
//! Wraps `fastembed::TextRerank` to score `(query, document)` pairs
//! directly, producing a more accurate relevance ordering than the
//! bi-encoder (BGE-small) used for first-stage vector retrieval. The
//! ranking pipeline calls into [`Reranker::rerank`] after the BM25 +
//! vec0 RRF fusion has narrowed the candidate set down to a small
//! pool (target 50 entries) — the cross-encoder's per-pair cost makes
//! it impractical to run over the full index.
//!
//! Default model is [`fastembed::RerankerModel::BGERerankerBase`]
//! (~280 MB on first download, after which the on-disk cache is hit).
//! That's bigger than the embedder's BGE-small, so reranking is
//! opt-in via `SearchOptions::rerank` rather than always-on.
//!
//! **Failure model**: load and inference errors bubble up as
//! [`crate::IndexError::Embed`] (same variant as the bi-encoder
//! failures — both fall under "fastembed/ONNX runtime had a
//! problem"). The search pipeline downgrades that to a single
//! warn-once line and falls back to the unranked RRF order, matching
//! the bi-encoder fallback contract.

use std::sync::Mutex;

use crate::{IndexError, Result};

/// Default reranking model. `BGERerankerBase` is the smaller of the
/// two BGE rerankers fastembed ships and works well across general
/// English text. Code that wants to swap in a different model should
/// gain a method on this struct rather than hard-coding here.
pub const RERANK_MODEL_NAME: &str = "BAAI/bge-reranker-base";

/// One loaded cross-encoder. Mirrors [`crate::Embedder`]'s shape:
/// not `Sync` itself, the inner `TextRerank` sits behind a `Mutex`
/// because `rerank` takes `&mut self`.
pub struct Reranker {
    inner: Mutex<fastembed::TextRerank>,
    model_name: &'static str,
}

impl Reranker {
    /// Load the default model. First call on a new machine pulls
    /// the ONNX weights from Hugging Face (~280 MB) and warms the
    /// runtime. Subsequent calls hit the on-disk cache and complete
    /// in ~1-2 seconds.
    pub fn new() -> Result<Self> {
        use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
        let model = TextRerank::try_new(
            RerankInitOptions::new(RerankerModel::BGERerankerBase)
                .with_show_download_progress(false),
        )
        .map_err(|e| {
            IndexError::Embed(format!(
                "failed to load fastembed reranker {RERANK_MODEL_NAME}: {e}"
            ))
        })?;
        Ok(Self {
            inner: Mutex::new(model),
            model_name: RERANK_MODEL_NAME,
        })
    }

    pub fn model_name(&self) -> &'static str {
        self.model_name
    }

    /// Score each document against the query. Returns a vector of
    /// scores aligned 1:1 with `documents` (i.e. `out[i]` is the
    /// score for `documents[i]`). Higher = more relevant.
    ///
    /// fastembed's `rerank` returns results sorted by descending
    /// score with an `index` back-pointer; this method reorders them
    /// into the input order so callers can pair each score with its
    /// original candidate without an extra lookup. An empty input
    /// short-circuits to an empty output (no model invocation).
    pub fn rerank(&self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        // fastembed's `rerank` constrains `query` and each document
        // to the same `AsRef<str>` type, so we feed both as `&str`
        // through a fresh `Vec<&str>` borrow over the input slice.
        let docs: Vec<&str> = documents.to_vec();
        let mut model = self
            .inner
            .lock()
            .map_err(|_| IndexError::Embed("reranker mutex poisoned".to_string()))?;
        let results = model
            .rerank(query, docs, /* return_documents */ false, None)
            .map_err(|e| IndexError::Embed(format!("fastembed rerank failed: {e}")))?;
        // Re-align by `index` so output[i] is the score for input[i].
        let mut scores = vec![f32::NEG_INFINITY; documents.len()];
        for r in results {
            if r.index < scores.len() {
                scores[r.index] = r.score;
            }
        }
        Ok(scores)
    }
}

/// How long to short-circuit `try_shared_reranker` after a failed
/// load attempt. A hard "no model" environment (no network, ONNX
/// runtime missing) makes every `Reranker::new()` call hit the
/// slow timeout path; without this backoff, a flurry of search
/// requests would each pay that cost in series behind the INIT
/// mutex. 5 seconds is short enough that a transient network glitch
/// resolves into a retry quickly, long enough to coalesce a burst
/// of searches issued back-to-back.
const RERANK_RETRY_BACKOFF_MS: u64 = 5_000;

/// Process-wide shared reranker, lazy-initialized on the first
/// successful call. Same retry-on-failure pattern as
/// [`crate::try_shared_embedder`]: a failed load does **not** poison
/// the slot, so a transient error (network, ONNX runtime hiccup) is
/// retried on the next search instead of locking the process into
/// "RRF-only mode" until restart. The "warning" stderr line is gated
/// on a one-shot flag so a hard "no model" environment doesn't spam.
///
/// **Cold-start serialization + retry backoff**: under concurrent
/// first calls (two MCP `journal_search` requests racing for the
/// first reranker), the `OnceLock` alone would let both threads kick
/// off independent `Reranker::new()` invocations — each pulling
/// ~280 MB and warming its own ONNX runtime, of which only one's
/// value would actually be stored via `OnceLock::set`. We gate the
/// load behind a separate `Mutex` and re-check `RR.get()` after
/// acquiring it (double-checked locking) so at most one model load
/// is in flight at a time. After a *failed* load, an atomic
/// last-failure timestamp short-circuits subsequent attempts within
/// [`RERANK_RETRY_BACKOFF_MS`] so a hard-failing environment doesn't
/// pay the slow timeout per search.
///
/// First successful call costs the model download + warmup; all
/// subsequent successful calls are O(1).
pub fn try_shared_reranker() -> Option<&'static Reranker> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    static RR: OnceLock<Reranker> = OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());
    static WARNED: AtomicBool = AtomicBool::new(false);
    /// Wall-clock millis (since UNIX epoch) of the last failed load.
    /// 0 = no recorded failure. Reads/writes are `Relaxed`: stale
    /// reads at most cause one extra retry past the backoff window,
    /// which is harmless.
    static LAST_FAIL_MS: AtomicU64 = AtomicU64::new(0);

    if let Some(r) = RR.get() {
        return Some(r);
    }
    // Skip the full load if a recent attempt failed and we're still
    // inside the backoff window. Cheap atomic read on the hot path
    // for repeat-failure environments.
    let now_ms = unix_epoch_ms();
    let last_fail = LAST_FAIL_MS.load(Ordering::Relaxed);
    if last_fail != 0 && now_ms.saturating_sub(last_fail) < RERANK_RETRY_BACKOFF_MS {
        return None;
    }
    // Serialize the cold-start path so racing callers don't each kick
    // off their own ~280 MB download + warmup. A poisoned mutex from
    // a panicked prior loader is recovered by destructuring the guard
    // — we only use the lock as a barrier, not to protect data.
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = RR.get() {
        return Some(r);
    }
    // Re-check the backoff window after acquiring the lock — another
    // thread may have just failed and stamped LAST_FAIL_MS while we
    // were waiting.
    let last_fail = LAST_FAIL_MS.load(Ordering::Relaxed);
    if last_fail != 0 && unix_epoch_ms().saturating_sub(last_fail) < RERANK_RETRY_BACKOFF_MS {
        return None;
    }
    match Reranker::new() {
        Ok(r) => {
            // `set` only returns Err if another thread snuck a value
            // in past us, which the INIT lock makes impossible — but
            // ignore the error result anyway so future contributors
            // don't have to reason about it.
            let _ = RR.set(r);
            // Clear any stale failure stamp on success.
            LAST_FAIL_MS.store(0, Ordering::Relaxed);
            RR.get()
        }
        Err(err) => {
            LAST_FAIL_MS.store(unix_epoch_ms(), Ordering::Relaxed);
            if !WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "warning: tempyr journal reranker unavailable, falling back to RRF only: {err}"
                );
            }
            None
        }
    }
}

/// Wall-clock millis since the UNIX epoch, saturating to 0 on the
/// (effectively impossible) `SystemTime::now() < UNIX_EPOCH`.
fn unix_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Emit a one-shot warning when query-time reranking inference fails
/// after the model loaded successfully. Mirrors
/// [`crate::warn_query_embed_failure_once`] for the bi-encoder side
/// — keeps stderr quiet across many `journal search` calls in a
/// process where reranking is broken in some persistent way.
pub fn warn_query_rerank_failure_once(err: &IndexError) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("warning: journal query rerank failed, falling back to RRF only: {err}");
    }
}

#[cfg(test)]
mod tests {
    //! Tests are `#[ignore]` by default — the standard `cargo test`
    //! run stays hermetic and offline. Run with
    //! `cargo test -p tempyr-journal-index rerank:: -- --ignored`
    //! when validating reranker changes.

    use super::*;

    #[test]
    #[ignore = "downloads/loads the BGE-Reranker-base ONNX model; run with --ignored"]
    fn reranker_model_loads() {
        let r = Reranker::new().expect("reranker should load");
        assert_eq!(r.model_name(), RERANK_MODEL_NAME);
    }

    #[test]
    #[ignore = "downloads/loads the BGE-Reranker-base ONNX model; run with --ignored"]
    fn rerank_orders_relevance_correctly() {
        // The whole point of the cross-encoder: a topically relevant
        // document with no shared keywords should still outscore a
        // semantically irrelevant one.
        let r = Reranker::new().unwrap();
        let query = "how do I serialize a struct to JSON in Rust";
        let docs = [
            "use serde_json::to_string with #[derive(Serialize)] on the struct",
            "the cat sat on the mat near the windowsill",
            "JSON is a text-based data interchange format",
        ];
        let scores = r.rerank(query, &docs).unwrap();
        assert_eq!(scores.len(), 3);
        // The serde_json snippet should outscore the cat sentence
        // (which is irrelevant) by a wide margin.
        assert!(
            scores[0] > scores[1],
            "relevant doc {} should outscore irrelevant {}",
            scores[0],
            scores[1]
        );
    }

    #[test]
    #[ignore = "downloads/loads the BGE-Reranker-base ONNX model; run with --ignored"]
    fn rerank_empty_input_yields_empty_output() {
        let r = Reranker::new().unwrap();
        let docs: Vec<&str> = Vec::new();
        let out = r.rerank("any query", &docs).unwrap();
        assert!(out.is_empty());
    }
}
