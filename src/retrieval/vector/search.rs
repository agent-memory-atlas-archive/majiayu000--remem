use std::cmp::Ordering;
use std::collections::BinaryHeap;

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::super::embedding::TextEmbedding;
use super::{VectorHit, VectorSearchOutcome, VectorSearchScope};

pub(crate) fn vector_search_embedding_with_scope(
    conn: &Connection,
    query: &TextEmbedding,
    scope: &VectorSearchScope,
    limit: usize,
) -> Result<VectorSearchOutcome> {
    if limit == 0 {
        return Ok(VectorSearchOutcome::ready(vec![]));
    }
    crate::memory::retrieval_enrichment::ensure_retrieval_open(conn)?;
    if super::super::embedding::provider_disabled_or_error()? {
        return Ok(VectorSearchOutcome::disabled("embedding provider is off"));
    }
    if !super::table_exists(conn, "memory_embeddings")? {
        return Ok(VectorSearchOutcome::disabled(
            "memory_embeddings table is missing; run migrations/backfill",
        ));
    }
    let mut timings = Vec::new();
    let indexed = crate::perf::time_result(&mut timings, "vector_knn_index", || {
        super::vec_index::knn_candidates_scoped(
            conn,
            query.values(),
            query.profile(),
            scope,
            super::super::vector_candidates::vector_candidate_limit(limit),
        )
    })?;
    if let Some(mut hits) = indexed {
        if !hits.is_empty() {
            let scanned = hits.len();
            hits.truncate(limit);
            return Ok(VectorSearchOutcome::ready_with_scan_count_and_timings(
                hits, scanned, timings,
            ));
        }
    }
    let (hits, scanned) = crate::perf::time_result(&mut timings, "vector_exact_scan", || {
        exact_scoped_scan(conn, query, scope, limit)
    })?;
    if scanned == 0 {
        let values = crate::db::to_sql_refs(&scope.values);
        let count: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM memories m WHERE {}",
                scope.predicate()
            ),
            values.as_slice(),
            |row| row.get(0),
        )?;
        if count > 0 {
            let reason = if super::embedding_count(conn)? == 0 {
                "memory_embeddings table is empty; run `remem reindex-embeddings --limit 1000`"
                    .to_string()
            } else {
                format!("memory_embeddings has no rows for model={} dimensions={}; run `remem reindex-embeddings --limit 1000`",
                    query.model(), query.dimensions())
            };
            return Ok(VectorSearchOutcome::disabled_with_timings(reason, timings));
        }
    }
    Ok(VectorSearchOutcome::ready_with_scan_count_and_timings(
        hits, scanned, timings,
    ))
}

fn exact_scoped_scan(
    conn: &Connection,
    query: &TextEmbedding,
    scope: &VectorSearchScope,
    limit: usize,
) -> Result<(Vec<VectorHit>, usize)> {
    let count = scope.values.len();
    let dimensions = query.dimensions() as i64;
    let model = query.model();
    let mut values = crate::db::to_sql_refs(&scope.values);
    values.extend([&model as &dyn rusqlite::types::ToSql, &dimensions]);
    let sql = format!(
        "SELECT e.memory_id, e.embedding, e.dimensions FROM memory_embeddings e
         JOIN memories m ON m.id = e.memory_id
         WHERE e.model = ?{} AND e.dimensions = ?{} AND {}",
        count + 1,
        count + 2,
        scope.predicate()
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(values.as_slice())?;
    let mut nearest = BinaryHeap::<NearestHit>::new();
    let mut scanned = 0;
    while let Some(row) = rows.next()? {
        let memory_id = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        let embedding = super::decode_embedding(&blob, row.get(2)?)
            .with_context(|| format!("invalid embedding blob for memory id={memory_id}"))?;
        let distance = super::cosine_distance(query.values(), &embedding)?;
        anyhow::ensure!(
            distance.is_finite(),
            "non-finite vector distance for memory id={memory_id}"
        );
        let hit = NearestHit(VectorHit {
            memory_id,
            distance,
        });
        scanned += 1;
        if nearest.len() < limit {
            nearest.push(hit);
        } else if nearest.peek().is_some_and(|worst| hit < *worst) {
            nearest.pop();
            nearest.push(hit);
        }
    }
    Ok((
        nearest
            .into_sorted_vec()
            .into_iter()
            .map(|hit| hit.0)
            .collect(),
        scanned,
    ))
}

struct NearestHit(VectorHit);

impl PartialEq for NearestHit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for NearestHit {}
impl PartialOrd for NearestHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for NearestHit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .distance
            .total_cmp(&other.0.distance)
            .then_with(|| self.0.memory_id.cmp(&other.0.memory_id))
    }
}
