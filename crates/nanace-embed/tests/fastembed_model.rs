//! Exercises the real bge-small model. Downloads ~130 MB on first run:
//! `cargo test -p nanace-embed -- --ignored`.
#![cfg(feature = "fastembed")]

use nanace_embed::{Embedder, FastEmbedder};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

#[tokio::test]
#[ignore = "downloads the bge-small-en-v1.5 model"]
async fn bge_small_ranks_semantic_match_first() {
    let cache = std::env::var("FASTEMBED_CACHE_DIR").unwrap_or_else(|_| ".fastembed_cache".to_owned());
    let embedder = tokio::task::spawn_blocking(move || FastEmbedder::load(cache.into()))
        .await
        .unwrap()
        .unwrap();

    let passages = embedder
        .embed_passages(vec![
            "## API Authentication\nPOST /auth/login returns a JWT token".to_owned(),
            "## Deploying\nRun the Helm chart against the staging cluster".to_owned(),
        ])
        .await
        .unwrap();
    let query = embedder.embed_query("How do I log users in?").await.unwrap();

    assert_eq!(query.len(), embedder.dimensions());
    assert!(cosine(&query, &passages[0]) > cosine(&query, &passages[1]));
}
