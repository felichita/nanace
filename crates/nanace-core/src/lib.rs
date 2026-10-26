//! Domain types and pure logic shared by the nanace backend crates.

pub mod chunk;
pub mod role;
pub mod validate;

pub use chunk::chunk_markdown;
pub use role::{OrgRole, TeamRole};

use sha2::{Digest, Sha256};

/// Hash of everything that feeds a context's embeddings. A changed hash means
/// the stored vectors are stale and must be regenerated.
pub fn embedding_input_hash(title: &str, content: &str, model_id: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [model_id, title, content] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_length_prefixed() {
        // Without length prefixes these two inputs would collide.
        assert_ne!(
            embedding_input_hash("ab", "c", "m"),
            embedding_input_hash("a", "bc", "m")
        );
        assert_ne!(
            embedding_input_hash("t", "c", "m1"),
            embedding_input_hash("t", "c", "m2")
        );
        assert_eq!(embedding_input_hash("t", "c", "m"), embedding_input_hash("t", "c", "m"));
    }
}
