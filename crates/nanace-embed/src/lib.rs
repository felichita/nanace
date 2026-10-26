//! Text embedding behind a provider-neutral trait.
//!
//! - [`FastEmbedder`] (feature `fastembed`): local ONNX inference with
//!   `bge-small-en-v1.5` (384 dimensions). Production default.
//! - [`HashingEmbedder`]: deterministic bag-of-words hashing. Lexical only; for
//!   tests and offline development, never for production search quality.

#[cfg(feature = "fastembed")]
mod fast;
mod hashing;

#[cfg(feature = "fastembed")]
pub use fast::FastEmbedder;
pub use hashing::HashingEmbedder;

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("embedding model failed: {0}")]
    Model(String),
    #[error("embedding worker failed: {0}")]
    Worker(String),
    #[error("model returned {got} vectors for {expected} inputs")]
    CountMismatch { expected: usize, got: usize },
}

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Stable identifier stored next to vectors; changing models requires a
    /// re-index because vectors from different models are not comparable.
    fn model_id(&self) -> &str;

    fn dimensions(&self) -> usize;

    /// Embeds stored passages (context chunks).
    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Embeds a search query. Some models use a different prompt for queries
    /// than for passages.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
}
