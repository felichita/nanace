use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::{EmbedError, Embedder};

/// bge v1.5 recommends this instruction for short retrieval queries; passages
/// are embedded without it.
const BGE_QUERY_INSTRUCTION: &str = "Represent this sentence for searching relevant passages: ";
const MODEL_ID: &str = "BAAI/bge-small-en-v1.5";
const DIMENSIONS: usize = 384;
const BATCH_SIZE: usize = 32;

/// Local `bge-small-en-v1.5` inference via ONNX Runtime. Inference is
/// CPU-bound and runs on Tokio's blocking pool, one batch at a time.
#[derive(Clone)]
pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
}

impl FastEmbedder {
    /// Loads the model, downloading it into `cache_dir` on first use.
    /// Blocking: call from a blocking context.
    pub fn load(cache_dir: PathBuf) -> Result<Self, EmbedError> {
        let options = TextInitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let model = TextEmbedding::try_new(options).map_err(|e| EmbedError::Model(e.to_string()))?;
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
        })
    }

    async fn run(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        let expected = texts.len();
        let model = Arc::clone(&self.model);
        let vectors = tokio::task::spawn_blocking(move || {
            let mut model = model
                .lock()
                .map_err(|_| EmbedError::Worker("embedding model mutex poisoned".to_owned()))?;
            model
                .embed(&texts, Some(BATCH_SIZE))
                .map_err(|e| EmbedError::Model(e.to_string()))
        })
        .await
        .map_err(|e| EmbedError::Worker(e.to_string()))??;
        if vectors.len() != expected {
            return Err(EmbedError::CountMismatch {
                expected,
                got: vectors.len(),
            });
        }
        Ok(vectors)
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn dimensions(&self) -> usize {
        DIMENSIONS
    }

    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.run(texts).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut vectors = self.run(vec![format!("{BGE_QUERY_INSTRUCTION}{text}")]).await?;
        vectors.pop().ok_or(EmbedError::CountMismatch { expected: 1, got: 0 })
    }
}
