//! Exact sqlite-vec mirrors keyed by the complete embedding profile (GH1105).
//! Source embeddings remain authoritative. Incomplete or absent mirrors use
//! the same-profile, scope-filtered exact scan, never a recency/ID sample.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::super::embedding::EmbeddingProfile;
#[cfg(test)]
use super::VectorSearchFilters;
use super::{vec_extension_loaded, VectorHit, VectorSearchScope};

pub(crate) const VEC_INDEX_BACKFILL_BATCH_SIZE: usize = 512;
const STATE_TABLE: &str = "memory_embedding_vec_state_v2";

fn ensure_state_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {STATE_TABLE} (
             model TEXT NOT NULL,
             dimensions INTEGER NOT NULL,
             last_memory_id INTEGER NOT NULL DEFAULT 0,
             done INTEGER NOT NULL DEFAULT 0,
             updated_at_epoch INTEGER NOT NULL,
             PRIMARY KEY(model, dimensions)
         )"
    ))?;
    Ok(())
}

pub(super) fn vec_table_name(profile: EmbeddingProfile<'_>) -> String {
    let mut digest = Sha256::new();
    digest.update((profile.model.len() as u64).to_le_bytes());
    digest.update(profile.model.as_bytes());
    digest.update((profile.dimensions as u64).to_le_bytes());
    format!(
        "memory_embedding_vec_v2_{}_{:x}",
        profile.dimensions,
        digest.finalize()
    )
}

fn vec_table_exists(conn: &Connection, profile: EmbeddingProfile<'_>) -> Result<bool> {
    super::table_exists(conn, &vec_table_name(profile))
}

fn create_vec_table(conn: &Connection, profile: EmbeddingProfile<'_>) -> Result<()> {
    anyhow::ensure!(
        !profile.model.trim().is_empty(),
        "embedding model must not be empty"
    );
    anyhow::ensure!(
        (1..=65_536).contains(&profile.dimensions),
        "embedding dimensions out of indexable range: {}",
        profile.dimensions
    );
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {} USING vec0(
             memory_id INTEGER PRIMARY KEY,
             embedding float[{}] distance_metric=cosine
         )",
        vec_table_name(profile),
        profile.dimensions
    ))?;
    Ok(())
}

/// Advance one bounded batch per source profile. Legacy mirrors never enter
/// v2 readiness; retiring them changes no authoritative embedding rows.
pub(crate) fn ensure_vec_index(conn: &Connection) -> Result<()> {
    if !vec_extension_loaded(conn) {
        return Ok(());
    }
    ensure_state_table(conn)?;
    retire_legacy_mirrors(conn)?;
    let profiles = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT model, dimensions FROM memory_embeddings ORDER BY model, dimensions",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        crate::db::query::collect_rows(rows)?
    };
    for (model, dimensions) in profiles {
        anyhow::ensure!(
            dimensions > 0,
            "memory_embeddings carries non-positive dimensions {dimensions}"
        );
        let profile = EmbeddingProfile {
            model: &model,
            dimensions: dimensions as usize,
        };
        super::with_embedding_savepoint(conn, "remem_vec_backfill", || {
            ensure_vec_index_profile(conn, profile)
        })?;
    }
    Ok(())
}

fn ensure_vec_index_profile(conn: &Connection, profile: EmbeddingProfile<'_>) -> Result<()> {
    let state: Option<(i64, i64)> = conn.query_row(
        &format!("SELECT last_memory_id, done FROM {STATE_TABLE} WHERE model = ?1 AND dimensions = ?2"),
        (profile.model, profile.dimensions as i64),
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let (mut cursor, done) = state.unwrap_or((0, 0));
    let exists = vec_table_exists(conn, profile)?;
    if exists && done == 1 {
        return Ok(());
    }
    if !exists {
        cursor = 0;
    }
    create_vec_table(conn, profile)?;
    let mut stmt = conn.prepare(
        "SELECT memory_id, embedding FROM memory_embeddings
         WHERE model = ?1 AND dimensions = ?2 AND memory_id > ?3
         ORDER BY memory_id LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            profile.model,
            profile.dimensions as i64,
            cursor,
            VEC_INDEX_BACKFILL_BATCH_SIZE as i64
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )?;
    let batch = crate::db::query::collect_rows(rows)?;
    let table = vec_table_name(profile);
    let mut delete = conn.prepare(&format!("DELETE FROM {table} WHERE memory_id = ?1"))?;
    let mut insert = conn.prepare(&format!(
        "INSERT INTO {table} (memory_id, embedding) VALUES (?1, ?2)"
    ))?;
    let mut advanced = cursor;
    for (memory_id, embedding) in &batch {
        delete.execute([memory_id])?;
        insert.execute(rusqlite::params![memory_id, embedding])?;
        advanced = *memory_id;
    }
    conn.execute(
        &format!(
            "INSERT INTO {STATE_TABLE} (model, dimensions, last_memory_id, done, updated_at_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(model, dimensions) DO UPDATE SET
                 last_memory_id = excluded.last_memory_id,
                 done = excluded.done,
                 updated_at_epoch = excluded.updated_at_epoch"
        ),
        rusqlite::params![
            profile.model,
            profile.dimensions as i64,
            advanced,
            (batch.len() < VEC_INDEX_BACKFILL_BATCH_SIZE) as i64,
            chrono::Utc::now().timestamp()
        ],
    )?;
    Ok(())
}

pub(crate) fn vec_index_ready(conn: &Connection, profile: EmbeddingProfile<'_>) -> Result<bool> {
    if !vec_extension_loaded(conn)
        || !vec_table_exists(conn, profile)?
        || !super::table_exists(conn, STATE_TABLE)?
    {
        return Ok(false);
    }
    let done: Option<i64> = conn
        .query_row(
            &format!("SELECT done FROM {STATE_TABLE} WHERE model = ?1 AND dimensions = ?2"),
            (profile.model, profile.dimensions as i64),
            |row| row.get(0),
        )
        .optional()?;
    Ok(done == Some(1))
}

/// Called inside the source-row transaction, including batch reindex writes.
pub(crate) fn sync_vec_upsert(
    conn: &Connection,
    memory_id: i64,
    model: &str,
    dimensions: usize,
) -> Result<()> {
    sync_vec_upsert_batch(conn, EmbeddingProfile { model, dimensions }, &[memory_id])
}

pub(crate) fn sync_vec_upsert_batch(
    conn: &Connection,
    profile: EmbeddingProfile<'_>,
    memory_ids: &[i64],
) -> Result<()> {
    if memory_ids.is_empty() || !vec_extension_loaded(conn) || !vec_table_exists(conn, profile)? {
        return Ok(());
    }
    let table = vec_table_name(profile);
    let ids_json = serde_json::to_string(memory_ids)?;
    conn.execute(
        &format!("DELETE FROM {table} WHERE memory_id IN (SELECT value FROM json_each(?1))"),
        [&ids_json],
    )
    .context("clear vector profile mirror batch")?;
    conn.execute(
        &format!(
            "INSERT INTO {table} (memory_id, embedding)
             SELECT memory_id, embedding FROM memory_embeddings
             WHERE model = ?1 AND dimensions = ?2
               AND memory_id IN (SELECT value FROM json_each(?3))"
        ),
        rusqlite::params![profile.model, profile.dimensions as i64, ids_json],
    )
    .context("sync vector profile mirror batch")?;
    Ok(())
}

pub(crate) fn sync_vec_keep_only_profile(
    conn: &Connection,
    model: &str,
    dimensions: usize,
) -> Result<()> {
    if !vec_extension_loaded(conn) {
        return Ok(());
    }
    let keep = vec_table_name(EmbeddingProfile { model, dimensions });
    for table in existing_vec_tables(conn)? {
        if table != keep {
            conn.execute_batch(&format!("DROP TABLE \"{table}\""))?;
        }
    }
    ensure_state_table(conn)?;
    conn.execute(
        &format!("DELETE FROM {STATE_TABLE} WHERE NOT (model = ?1 AND dimensions = ?2)"),
        (model, dimensions as i64),
    )?;
    conn.execute_batch("DROP TABLE IF EXISTS memory_embedding_vec_state")?;
    Ok(())
}

fn existing_vec_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'memory_embedding_vec_%'",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(crate::db::query::collect_rows(rows)?
        .into_iter()
        .filter(|name| {
            let Some(suffix) = name.strip_prefix("memory_embedding_vec_") else {
                return false;
            };
            if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
                return true;
            }
            suffix
                .strip_prefix("v2_")
                .and_then(|s| s.split_once('_'))
                .is_some_and(|(dims, digest)| {
                    !dims.is_empty()
                        && dims.bytes().all(|b| b.is_ascii_digit())
                        && digest.len() == 64
                        && digest.bytes().all(|b| b.is_ascii_hexdigit())
                })
        })
        .collect())
}

fn retire_legacy_mirrors(conn: &Connection) -> Result<()> {
    for table in existing_vec_tables(conn)? {
        if !table.starts_with("memory_embedding_vec_v2_") {
            conn.execute_batch(&format!("DROP TABLE \"{table}\""))?;
        }
    }
    conn.execute_batch("DROP TABLE IF EXISTS memory_embedding_vec_state")?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn knn_candidates(
    conn: &Connection,
    query_embedding: &[f32],
    profile: EmbeddingProfile<'_>,
    filters: VectorSearchFilters<'_>,
    candidate_limit: usize,
) -> Result<Option<Vec<VectorHit>>> {
    knn_candidates_scoped(
        conn,
        query_embedding,
        profile,
        &VectorSearchScope::from_filters(filters),
        candidate_limit,
    )
}

pub(super) fn knn_candidates_scoped(
    conn: &Connection,
    query_embedding: &[f32],
    profile: EmbeddingProfile<'_>,
    scope: &VectorSearchScope,
    candidate_limit: usize,
) -> Result<Option<Vec<VectorHit>>> {
    if !vec_index_ready(conn, profile)? {
        return Ok(None);
    }
    anyhow::ensure!(
        query_embedding.len() == profile.dimensions,
        "query embedding must be {} dimensions, got {}",
        profile.dimensions,
        query_embedding.len()
    );
    let count = scope.values.len();
    let blob = super::encode_embedding(query_embedding);
    let k = candidate_limit as i64;
    let dimensions = profile.dimensions as i64;
    let mut values = crate::db::to_sql_refs(&scope.values);
    values.extend([
        &blob as &dyn rusqlite::types::ToSql,
        &k,
        &profile.model,
        &dimensions,
    ]);
    // vec0 consumes rowid IN as a KNN prefilter. Outer JOIN/EXISTS predicates
    // would discard already-chosen neighbors and let foreign scopes starve it.
    let sql = format!(
        "SELECT memory_id, distance FROM {}
         WHERE embedding MATCH ?{} AND k = ?{}
           AND memory_id IN ({}) ORDER BY distance",
        vec_table_name(profile),
        count + 1,
        count + 2,
        scope.eligible_ids_sql(count + 3, count + 4)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(values.as_slice(), |row| {
        Ok(VectorHit {
            memory_id: row.get(0)?,
            distance: row.get(1)?,
        })
    })?;
    let mut hits = crate::db::query::collect_rows(rows)?;
    hits.sort_by(|a, b| {
        a.distance
            .total_cmp(&b.distance)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });
    Ok(Some(hits))
}
