//! Local fastembed wrapper for journal entry embeddings.
//!
//! Phase 3b2 uses BGE-small (`all-MiniLM-L6-v2`, 384-d) — the same
//! model the graph index already runs, so the ONNX runtime download
//! and warmup happen once per machine. Synchronous API; the caller
//! holds an `Embedder` and feeds it batches.
//!
//! **Failure model**: anything that goes wrong (model load failure,
//! ONNX runtime error, OOM during inference) bubbles up as an
//! `IndexError`. The indexer treats these as non-fatal — search
//! falls back to BM25-only, and the embed-pending pass retries on
//! the next refresh.

use std::sync::Mutex;

use crate::{IndexError, Result, schema};

/// One loaded model. Cheap to hold; lazy-loads its ONNX runtime on
/// the first `embed` call. Not `Sync` itself — the inner model is
/// behind a Mutex because fastembed's `embed` method is `&mut self`.
pub struct Embedder {
    inner: Mutex<fastembed::TextEmbedding>,
    dim: usize,
    model_name: &'static str,
}

impl Embedder {
    /// Load the default model (BGE-small / `all-MiniLM-L6-v2`,
    /// 384-d). First call on a new machine downloads the ONNX
    /// weights (~80 MB) — subsequent calls hit the on-disk cache.
    pub fn new() -> Result<Self> {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(false),
        )
        .map_err(|e| {
            IndexError::Embed(format!(
                "failed to load fastembed model {}: {e}",
                schema::EMBED_MODEL_NAME
            ))
        })?;
        Ok(Self {
            inner: Mutex::new(model),
            dim: schema::EMBED_DIM,
            model_name: schema::EMBED_MODEL_NAME,
        })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn model_name(&self) -> &'static str {
        self.model_name
    }

    /// Embed a batch of strings. Returns one f32 vector per input.
    /// fastembed handles batching/pooling internally; we just pass
    /// the slice through. An empty input yields an empty output —
    /// no model invocation.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        let mut model = self
            .inner
            .lock()
            .map_err(|_| IndexError::Embed("embedder mutex poisoned".to_string()))?;
        let vecs = model
            .embed(owned, None)
            .map_err(|e| IndexError::Embed(format!("fastembed inference failed: {e}")))?;
        // Sanity: every vector should match our expected dim.
        for v in &vecs {
            if v.len() != self.dim {
                return Err(IndexError::Embed(format!(
                    "embedding dimension mismatch: model returned {} but schema expects {}",
                    v.len(),
                    self.dim
                )));
            }
        }
        Ok(vecs)
    }

    /// Embed one string, returning the single vector. Convenience
    /// wrapper around `embed` for query-time use where the caller
    /// has exactly one text.
    pub fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut vecs = self.embed(&[text])?;
        vecs.pop()
            .ok_or_else(|| IndexError::Embed("embedder returned empty result".to_string()))
    }
}

/// Try to get a process-wide shared embedder. Lazy-initialized on
/// the first successful call; subsequent calls return the same
/// instance.
///
/// **Failure handling**: load errors do **not** poison the slot.
/// Earlier versions memoized `None` on the first failure, which
/// turned a transient error (network down for the first model
/// download, ONNX runtime momentarily unavailable, disk briefly
/// locked) into a permanent BM25-only mode for the rest of the
/// process. Now the OnceLock only stores successful values; a
/// failed call returns `None` *without* setting the slot, so the
/// next call retries. The "warning" stderr line is gated on a
/// separate one-shot flag so we don't spam.
///
/// How long to short-circuit `try_shared_embedder` after a failed
/// load attempt. Without this backoff, a hard "no model" environment
/// makes every `Embedder::new()` call hit the slow timeout path
/// behind the INIT mutex; the backoff coalesces bursts of search
/// requests in such an environment. 5 seconds matches the rerank
/// module's value.
const EMBED_RETRY_BACKOFF_MS: u64 = 5_000;

/// **Cold-start serialization + retry backoff**: under concurrent
/// first calls (two MCP `journal_search` requests racing for the
/// first embedder), the `OnceLock` alone would let both threads kick
/// off independent `Embedder::new()` invocations — each pulling
/// ~80 MB and warming its own ONNX runtime, of which only one's
/// value would actually be stored via `OnceLock::set`. We gate the
/// load behind a separate `Mutex` and re-check `EMB.get()` after
/// acquiring it (double-checked locking) so at most one model load
/// is in flight at a time. After a *failed* load, an atomic
/// last-failure timestamp short-circuits subsequent attempts within
/// [`EMBED_RETRY_BACKOFF_MS`] so a hard-failing environment doesn't
/// pay the slow timeout per search.
///
/// The cost of the first `Some(_)` is the model download (~80 MB
/// on first machine encounter) + ONNX runtime warmup (~1-2s). All
/// subsequent successful calls are O(1).
pub fn try_shared_embedder() -> Option<&'static Embedder> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    static EMB: OnceLock<Embedder> = OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());
    static WARNED: AtomicBool = AtomicBool::new(false);
    /// Wall-clock millis (since UNIX epoch) of the last failed load.
    /// 0 = no recorded failure. Relaxed ordering — stale reads at
    /// most cause one extra retry past the backoff window.
    static LAST_FAIL_MS: AtomicU64 = AtomicU64::new(0);

    if let Some(e) = EMB.get() {
        return Some(e);
    }
    // Skip the full load if a recent attempt failed and we're still
    // inside the backoff window.
    let now_ms = unix_epoch_ms();
    let last_fail = LAST_FAIL_MS.load(Ordering::Relaxed);
    if last_fail != 0 && now_ms.saturating_sub(last_fail) < EMBED_RETRY_BACKOFF_MS {
        return None;
    }
    // Serialize the cold-start path so racing callers don't each kick
    // off their own ~80 MB download + warmup. A poisoned mutex from a
    // panicked prior loader is recovered by destructuring the guard —
    // the lock only acts as a barrier, it doesn't protect data.
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = EMB.get() {
        return Some(e);
    }
    // Re-check the backoff window after acquiring the lock — another
    // thread may have just failed while we were waiting.
    let last_fail = LAST_FAIL_MS.load(Ordering::Relaxed);
    if last_fail != 0 && unix_epoch_ms().saturating_sub(last_fail) < EMBED_RETRY_BACKOFF_MS {
        return None;
    }
    match Embedder::new() {
        Ok(e) => {
            // `set` only returns Err if another thread snuck a value
            // in past us, which the INIT lock makes impossible — but
            // ignore the error result anyway so future contributors
            // don't have to reason about it.
            let _ = EMB.set(e);
            // Clear any stale failure stamp on success.
            LAST_FAIL_MS.store(0, Ordering::Relaxed);
            EMB.get()
        }
        Err(err) => {
            LAST_FAIL_MS.store(unix_epoch_ms(), Ordering::Relaxed);
            // Log on the first failure only — subsequent retries
            // stay quiet so a hard "no model" environment doesn't
            // emit a warning per search call.
            if !WARNED.swap(true, Ordering::Relaxed) {
                eprintln!("warning: tempyr journal embedder unavailable, using BM25 only: {err}");
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

/// Emit a one-shot warning to stderr that the query-embedding path
/// failed. Both the CLI (`tempyr journal search`) and the MCP
/// `journal_search` tool fall back to BM25-only when `embed_one`
/// errors, but if they each warn on every call the user gets one
/// log line per search — noisy and not useful. Warn once per
/// process, then stay quiet; subsequent retries proceed silently.
///
/// Mirrors the warn-once pattern used by [`try_shared_embedder`]
/// for model-load failures, so the user sees at most two warnings
/// total in a process: one for the model load and one for query
/// embedding.
pub fn warn_query_embed_failure_once(err: &IndexError) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("warning: journal query embedding failed, falling back to BM25 only: {err}");
    }
}

/// Convert a `Vec<f32>` to the little-endian bytes that sqlite-vec
/// expects when binding a `vec_f32(...)` parameter. We bind the bytes
/// as a SQLite BLOB and let sqlite-vec parse it.
pub fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Reverse of [`vec_to_bytes`]: parse a little-endian f32 BLOB back
/// to `Vec<f32>`. Returns an error if the byte length isn't a
/// multiple of 4.
pub fn bytes_to_vec(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(IndexError::Embed(format!(
            "embedding blob length {} is not a multiple of 4",
            bytes.len()
        )));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! ## Test taxonomy
    //!
    //! Tests in this module split into two groups:
    //!
    //! - **Pure data-shape** (`vec_bytes_roundtrip`,
    //!   `bytes_to_vec_rejects_misaligned_input`): no fastembed
    //!   dependency, run on every `cargo test`.
    //! - **Model-backed** (`embedder_returns_correct_dim`,
    //!   `embed_batch_roundtrips`, `embed_one_returns_single_vec`,
    //!   `empty_input_yields_empty_output`): construct an `Embedder`
    //!   which downloads the BGE-small ONNX weights (~80 MB) on the
    //!   first machine encounter and loads the runtime. **These
    //!   tests are `#[ignore]` by default** — the standard `cargo
    //!   test --workspace` stays hermetic and offline. Run them
    //!   explicitly with `cargo test -- --ignored` (or scope to
    //!   this module: `cargo test -p tempyr-journal-index embed::
    //!   -- --ignored`) when validating embedding-related changes.
    //!
    //! The model-backed tests in `search` use the same convention.
    use super::*;

    /// Lazy-init shared embedder for the model-backed tests in this
    /// module. Only invoked from `#[ignore]`-marked tests so the
    /// default test run never triggers the model download/load.
    fn shared_embedder() -> &'static Embedder {
        use std::sync::OnceLock;
        static EMB: OnceLock<Embedder> = OnceLock::new();
        EMB.get_or_init(|| Embedder::new().expect("fastembed model should load"))
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn embedder_returns_correct_dim() {
        let e = shared_embedder();
        assert_eq!(e.dim(), 384);
        assert_eq!(e.model_name(), "all-MiniLM-L6-v2");
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn embed_batch_roundtrips() {
        let e = shared_embedder();
        let texts = vec!["the quick brown fox", "another sentence here"];
        let vecs = e.embed(&texts).unwrap();
        assert_eq!(vecs.len(), 2);
        assert!(vecs.iter().all(|v| v.len() == 384));
        // Vectors aren't all-zero (sanity).
        let nonzero = vecs[0].iter().filter(|f| f.abs() > 1e-9).count();
        assert!(nonzero > 100);
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn embed_one_returns_single_vec() {
        let e = shared_embedder();
        let v = e.embed_one("a single query").unwrap();
        assert_eq!(v.len(), 384);
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn empty_input_yields_empty_output() {
        // Even though `embed(&[])` short-circuits before invoking
        // the model, `shared_embedder()` still triggers the load —
        // hence #[ignore]. (Once the model is cached from a prior
        // run, this becomes a fast in-memory check.)
        let e = shared_embedder();
        let texts: Vec<&str> = Vec::new();
        let vecs = e.embed(&texts).unwrap();
        assert!(vecs.is_empty());
    }

    #[test]
    fn vec_bytes_roundtrip() {
        let v = vec![0.0, 1.0, -1.0, 2.5_f32, f32::MIN, f32::MAX];
        let bytes = vec_to_bytes(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        let back = bytes_to_vec(&bytes).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn bytes_to_vec_rejects_misaligned_input() {
        let bad = vec![1u8, 2, 3]; // 3 bytes — not a multiple of 4
        assert!(bytes_to_vec(&bad).is_err());
    }
}
