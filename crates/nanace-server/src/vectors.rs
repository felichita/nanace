//! Qdrant storage for context chunk embeddings.
//!
//! One collection per organization (`contexts_<org uuid>`), so orgs never share
//! an index. Every point carries `org_id` and `team_id`; searches always filter
//! on both, using team ids derived from PostgreSQL memberships. Results are
//! re-checked against PostgreSQL (with RLS) before they are returned, so stale
//! points can never leak content.

use std::collections::HashMap;

use anyhow::Context;
use qdrant_client::qdrant::value::Kind;
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, DeletePointsBuilder, Distance, FieldType,
    Filter, PointStruct, QueryPointsBuilder, Range, UpsertPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use uuid::Uuid;

/// Namespace for deterministic point ids: `uuid_v5(NS, "<context>:<chunk>")`.
/// Re-indexing a context overwrites its points in place.
const POINT_NAMESPACE: Uuid = Uuid::from_u128(0x6e61_6e61_6365_4000_8000_0000_6374_7873);

pub struct Chunk {
    pub text: String,
    pub vector: Vec<f32>,
}

pub struct ChunkHit {
    pub context_id: Uuid,
    pub score: f32,
    pub text: String,
}

pub struct VectorStore {
    client: Qdrant,
    dimensions: u64,
}

pub fn collection_name(org_id: Uuid) -> String {
    format!("contexts_{}", org_id.simple())
}

fn point_id(context_id: Uuid, chunk_index: usize) -> String {
    Uuid::new_v5(&POINT_NAMESPACE, format!("{context_id}:{chunk_index}").as_bytes()).to_string()
}

impl VectorStore {
    pub fn connect(url: &str, dimensions: usize) -> anyhow::Result<Self> {
        let client = Qdrant::from_url(url)
            .skip_compatibility_check()
            .build()
            .context("failed to build Qdrant client")?;
        Ok(Self {
            client,
            dimensions: u64::try_from(dimensions).context("embedding dimensions overflow u64")?,
        })
    }

    pub async fn health(&self) -> anyhow::Result<()> {
        self.client.health_check().await.context("Qdrant health check failed")?;
        Ok(())
    }

    /// Creates the org collection and its payload indexes if missing.
    pub async fn ensure_collection(&self, org_id: Uuid) -> anyhow::Result<()> {
        let name = collection_name(org_id);
        if self.client.collection_exists(&name).await? {
            return Ok(());
        }
        let created = self
            .client
            .create_collection(
                CreateCollectionBuilder::new(&name)
                    .vectors_config(VectorParamsBuilder::new(self.dimensions, Distance::Cosine)),
            )
            .await;
        if let Err(err) = created {
            // A concurrent request may have created it first.
            if !self.client.collection_exists(&name).await? {
                return Err(err).context("failed to create Qdrant collection");
            }
        }
        for (field, kind) in [
            ("team_id", FieldType::Keyword),
            ("context_id", FieldType::Keyword),
            ("chunk_index", FieldType::Integer),
        ] {
            self.client
                .create_field_index(CreateFieldIndexCollectionBuilder::new(&name, field, kind).wait(true))
                .await
                .with_context(|| format!("failed to index payload field {field}"))?;
        }
        Ok(())
    }

    /// Writes `chunks` as the context's complete point set. New points are
    /// upserted before stale ones are removed, so the context stays searchable
    /// throughout.
    pub async fn replace_context(
        &self,
        org_id: Uuid,
        team_id: Uuid,
        context_id: Uuid,
        chunks: Vec<Chunk>,
    ) -> anyhow::Result<()> {
        self.ensure_collection(org_id).await?;
        let name = collection_name(org_id);
        let chunk_count = chunks.len();
        let points = chunks
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| {
                let payload = Payload::try_from(serde_json::json!({
                    "org_id": org_id.to_string(),
                    "team_id": team_id.to_string(),
                    "context_id": context_id.to_string(),
                    "chunk_index": index,
                    "text": chunk.text,
                }))
                .context("chunk payload is not a JSON object")?;
                Ok(PointStruct::new(point_id(context_id, index), chunk.vector, payload))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if !points.is_empty() {
            self.client
                .upsert_points(UpsertPointsBuilder::new(&name, points).wait(true))
                .await
                .context("failed to upsert context points")?;
        }
        let stale_from = i64::try_from(chunk_count).context("chunk count overflows i64")?;
        self.client
            .delete_points(
                DeletePointsBuilder::new(&name)
                    .points(Filter::must([
                        Condition::matches("context_id", context_id.to_string()),
                        Condition::range(
                            "chunk_index",
                            Range {
                                gte: Some(stale_from as f64),
                                ..Default::default()
                            },
                        ),
                    ]))
                    .wait(true),
            )
            .await
            .context("failed to delete stale context points")?;
        Ok(())
    }

    pub async fn delete_context(&self, org_id: Uuid, context_id: Uuid) -> anyhow::Result<()> {
        let name = collection_name(org_id);
        if !self.client.collection_exists(&name).await? {
            return Ok(());
        }
        self.client
            .delete_points(
                DeletePointsBuilder::new(&name)
                    .points(Filter::must([Condition::matches("context_id", context_id.to_string())]))
                    .wait(true),
            )
            .await
            .context("failed to delete context points")?;
        Ok(())
    }

    /// Nearest chunks restricted to `team_ids`. `team_ids` must be non-empty.
    pub async fn search(
        &self,
        org_id: Uuid,
        team_ids: &[Uuid],
        vector: Vec<f32>,
        limit: u64,
    ) -> anyhow::Result<Vec<ChunkHit>> {
        anyhow::ensure!(!team_ids.is_empty(), "search requires at least one team");
        let name = collection_name(org_id);
        if !self.client.collection_exists(&name).await? {
            return Ok(Vec::new());
        }
        let teams: Vec<String> = team_ids.iter().map(Uuid::to_string).collect();
        let response = self
            .client
            .query(
                QueryPointsBuilder::new(&name)
                    .query(vector)
                    .filter(Filter::must([
                        Condition::matches("org_id", org_id.to_string()),
                        Condition::matches("team_id", teams),
                    ]))
                    .limit(limit)
                    .with_payload(true),
            )
            .await
            .context("Qdrant query failed")?;

        Ok(response
            .result
            .into_iter()
            .filter_map(|point| {
                let payload: HashMap<_, _> = point.payload;
                let text_of = |key: &str| match payload.get(key).and_then(|v| v.kind.as_ref()) {
                    Some(Kind::StringValue(s)) => Some(s.clone()),
                    _ => None,
                };
                Some(ChunkHit {
                    context_id: text_of("context_id")?.parse().ok()?,
                    score: point.score,
                    text: text_of("text")?,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_ids_are_stable_and_distinct() {
        let ctx = Uuid::from_u128(1);
        assert_eq!(point_id(ctx, 0), point_id(ctx, 0));
        assert_ne!(point_id(ctx, 0), point_id(ctx, 1));
        assert_ne!(point_id(ctx, 0), point_id(Uuid::from_u128(2), 0));
    }

    #[test]
    fn collection_name_is_per_org() {
        let org = Uuid::parse_str("0f8fad5b-d9cb-469f-a165-70867728950e").unwrap();
        assert_eq!(collection_name(org), "contexts_0f8fad5bd9cb469fa16570867728950e");
    }
}
