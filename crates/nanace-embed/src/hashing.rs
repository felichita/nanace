use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{EmbedError, Embedder};

/// Deterministic feature-hashing embedder: each lowercase alphanumeric token
/// adds ±1 to one dimension, then the vector is L2-normalized. Texts sharing
/// words get a positive cosine similarity, which is enough for tests.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dimensions: usize,
}

impl HashingEmbedder {
    pub const MODEL_ID: &'static str = "hashing-v1";

    pub fn new(dimensions: usize) -> Self {
        assert!(dimensions > 0, "dimensions must be positive");
        Self { dimensions }
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; self.dimensions];
        for token in text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()) {
            let digest = Sha256::digest(token.to_lowercase().as_bytes());
            let mut index_bytes = [0u8; 8];
            index_bytes.copy_from_slice(&digest[..8]);
            // `dimensions` fits in u64 on every supported target, and the
            // remainder is below `dimensions`, so both conversions are lossless.
            let index = (u64::from_le_bytes(index_bytes) % self.dimensions as u64) as usize;
            let sign = if digest[8] & 1 == 0 { 1.0 } else { -1.0 };
            vector[index] += sign;
        }
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            vector.iter_mut().for_each(|v| *v /= norm);
        }
        vector
    }
}

#[async_trait]
impl Embedder for HashingEmbedder {
    fn model_id(&self) -> &str {
        Self::MODEL_ID
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|t| self.embed(t)).collect())
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(self.embed(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[tokio::test]
    async fn deterministic_normalized_and_lexically_similar() {
        let embedder = HashingEmbedder::new(384);
        let passages = embedder
            .embed_passages(vec![
                "How to authenticate users with JWT".to_owned(),
                "Kubernetes node autoscaling".to_owned(),
            ])
            .await
            .unwrap();
        let query = embedder.embed_query("authenticate users").await.unwrap();

        assert_eq!(query, embedder.embed_query("Authenticate USERS").await.unwrap());
        assert_eq!(query.len(), 384);
        assert!((cosine(&query, &query) - 1.0).abs() < 1e-5);
        assert!(cosine(&query, &passages[0]) > cosine(&query, &passages[1]));
    }

    #[tokio::test]
    async fn empty_text_is_zero_vector() {
        let embedder = HashingEmbedder::new(8);
        assert_eq!(embedder.embed_query("  ").await.unwrap(), vec![0.0; 8]);
    }
}
