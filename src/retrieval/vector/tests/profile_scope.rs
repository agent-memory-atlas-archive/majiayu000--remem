//! Regression fixtures use native sqlite-vec and fabricated vectors only.
use super::super::vec_index::{
    ensure_vec_index, sync_vec_keep_only_profile, sync_vec_upsert_batch, vec_index_ready,
    vec_table_name,
};
use super::*;
use crate::retrieval::embedding::EmbeddingProfile;

fn memory(conn: &Connection, id: i64, project: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO memories (id, project, title, content, memory_type, status, created_at_epoch, updated_at_epoch)
         VALUES (?1, ?2, 'Independent note', 'Synthetic vector evidence', 'decision', 'active', ?1, ?1)",
        params![id, project],
    )?;
    Ok(())
}

fn put(conn: &Connection, id: i64, model: &str, values: &[f32]) -> Result<()> {
    upsert_embedding_with_metadata(conn, id, model, "fixture", values, 1)
}

fn query(conn: &Connection, model: &str, project: &str) -> Result<Vec<i64>> {
    let vector = TextEmbedding::new(model, vec![1.0, 0.0])?;
    Ok(vector_search_embedding_filtered(
        conn,
        &vector,
        VectorSearchFilters {
            project: Some(project),
            ..VectorSearchFilters::default()
        },
        10,
    )?
    .hits
    .into_iter()
    .map(|hit| hit.memory_id)
    .collect())
}

#[test]
fn same_dimension_artifact_profiles_coexist_through_sync_and_prune() -> Result<()> {
    let conn = setup_vector_conn()?;
    load_vec_extension(&conn)?;
    let model_a = "local-preset@sha256:aaaaaaaa";
    let model_b = "local-preset@sha256:bbbbbbbb";
    let a = EmbeddingProfile {
        model: model_a,
        dimensions: 2,
    };
    let b = EmbeddingProfile {
        model: model_b,
        dimensions: 2,
    };
    for id in 1..=2 {
        memory(&conn, id, "/repo")?;
    }
    put(&conn, 1, model_a, &[1.0, 0.0])?;
    put(&conn, 2, model_a, &[0.0, 1.0])?;
    ensure_vec_index(&conn)?;
    assert!(vec_index_ready(&conn, a)?);
    assert!(!vec_index_ready(&conn, b)?);
    put(&conn, 1, model_b, &[0.0, 1.0])?;
    put(&conn, 2, model_b, &[1.0, 0.0])?;
    ensure_vec_index(&conn)?;
    assert!(vec_index_ready(&conn, b)?);
    assert_ne!(vec_table_name(a), vec_table_name(b));
    assert_eq!(query(&conn, model_a, "/repo")?, vec![1, 2]);
    assert_eq!(query(&conn, model_b, "/repo")?, vec![2, 1]);
    // Both models remain in the source table during each batch synchronization.
    sync_vec_upsert_batch(&conn, a, &[1, 2])?;
    sync_vec_upsert_batch(&conn, b, &[1, 2])?;
    assert_eq!(query(&conn, model_a, "/repo")?, vec![1, 2]);
    assert_eq!(query(&conn, model_b, "/repo")?, vec![2, 1]);
    assert_eq!(embedding_count(&conn)?, 4);
    // Exercise mirror pruning only; authoritative prune policy is covered by
    // the existing coverage/pinning tests.
    sync_vec_keep_only_profile(&conn, model_b, 2)?;
    assert!(!vec_index_ready(&conn, a)?);
    assert!(vec_index_ready(&conn, b)?);
    assert_eq!(query(&conn, model_a, "/repo")?, vec![1, 2]); // exact fallback
    assert_eq!(query(&conn, model_b, "/repo")?, vec![2, 1]);
    Ok(())
}

#[test]
fn profile_backfill_readiness_is_independent_and_atomic() -> Result<()> {
    let conn = setup_vector_conn()?;
    load_vec_extension(&conn)?;
    let a = EmbeddingProfile {
        model: "A",
        dimensions: 2,
    };
    let b = EmbeddingProfile {
        model: "B",
        dimensions: 2,
    };
    for id in 1..=513 {
        memory(&conn, id, "/repo")?;
        put(
            &conn,
            id,
            "A",
            if id == 1 { &[1.0, 0.0] } else { &[0.0, 1.0] },
        )?;
    }
    put(&conn, 1, "B", &[0.0, 1.0])?;
    ensure_vec_index(&conn)?;
    assert!(!vec_index_ready(&conn, a)?);
    assert!(vec_index_ready(&conn, b)?);
    let vector = TextEmbedding::new("A", vec![1.0, 0.0])?;
    let during =
        vector_search_embedding_filtered(&conn, &vector, VectorSearchFilters::default(), 10)?;
    assert_eq!(during.candidates_scanned, 513);
    assert!(during
        .timings
        .iter()
        .any(|t| t.phase == "vector_exact_scan"));
    ensure_vec_index(&conn)?;
    assert!(vec_index_ready(&conn, a)?);
    // A failed source+mirror update must leave both prior snapshots intact.
    conn.execute_batch(
        "CREATE TRIGGER fail_a BEFORE UPDATE ON memory_embeddings
        WHEN NEW.model = 'A' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
    )?;
    assert!(put(&conn, 1, "A", &[0.0, 1.0]).is_err());
    conn.execute_batch("DROP TRIGGER fail_a")?;
    assert_eq!(query(&conn, "A", "/repo")?.first(), Some(&1));
    conn.execute("INSERT INTO memory_embeddings(memory_id,embedding,dimensions,model,content_hash,updated_at_epoch)
        VALUES(1,x'00000000',2,'bad-profile','fixture',1)", [])?;
    assert!(ensure_vec_index(&conn).is_err());
    let bad = EmbeddingProfile {
        model: "bad-profile",
        dimensions: 2,
    };
    assert!(!vec_index_ready(&conn, bad)?);
    assert!(
        !table_exists(&conn, &vec_table_name(bad))?,
        "failed mirror creation and cursor commit must roll back together"
    );
    assert!(vec_index_ready(&conn, a)?);
    Ok(())
}

#[test]
fn knn_prefilters_scope_before_foreign_neighbors_consume_k() -> Result<()> {
    let conn = setup_vector_conn()?;
    load_vec_extension(&conn)?;
    conn.execute_batch("BEGIN")?;
    for id in 1..=514 {
        memory(
            &conn,
            id,
            if id == 1 || id == 514 {
                "/wanted"
            } else {
                "/other"
            },
        )?;
        let cosine: f32 = if id == 1 {
            1.0
        } else if id == 514 {
            0.7
        } else {
            0.9
        };
        put(&conn, id, "A", &[cosine, (1.0 - cosine * cosine).sqrt()])?;
    }
    conn.execute_batch("COMMIT")?;
    let exact = query(&conn, "A", "/wanted")?;
    ensure_vec_index(&conn)?;
    ensure_vec_index(&conn)?;
    assert_eq!(query(&conn, "A", "/wanted")?, vec![1, 514]);
    assert_eq!(query(&conn, "A", "/wanted")?, exact);
    // The same prefilter handles branch/type/lifecycle competition.
    conn.execute(
        "UPDATE memories SET project='/wanted', branch='other' WHERE id BETWEEN 2 AND 513",
        [],
    )?;
    let vector = TextEmbedding::new("A", vec![1.0, 0.0])?;
    for field in ["branch", "memory_type", "status"] {
        conn.execute("UPDATE memories SET branch=NULL, memory_type='decision', status='active' WHERE id BETWEEN 2 AND 513", [])?;
        let value = match field {
            "branch" => "other",
            "memory_type" => "discovery",
            _ => "stale",
        };
        conn.execute(
            &format!("UPDATE memories SET {field}=?1 WHERE id BETWEEN 2 AND 513"),
            [value],
        )?;
        // Canonical field changes invalidate embeddings through memories_au.
        // Restore real near-neighbor competition for every scope predicate.
        for id in 2..=513 {
            put(&conn, id, "A", &[0.9, (1.0_f32 - 0.9_f32.powi(2)).sqrt()])?;
        }
        assert_eq!(embedding_count(&conn)?, 514, "{field}");
        let outcome = vector_search_embedding_filtered(
            &conn,
            &vector,
            VectorSearchFilters {
                project: Some("/wanted"),
                branch: Some("main"),
                memory_type: Some("decision"),
                include_stale: false,
            },
            10,
        )?;
        assert_eq!(
            outcome.hits.iter().map(|h| h.memory_id).collect::<Vec<_>>(),
            vec![1, 514],
            "{field}"
        );
    }
    conn.execute("UPDATE memories SET branch=NULL, memory_type='decision', status='active', title='scope-fixture-suppressed' WHERE id BETWEEN 2 AND 513", [])?;
    for id in 2..=513 {
        put(&conn, id, "A", &[0.9, (1.0_f32 - 0.9_f32.powi(2)).sqrt()])?;
    }
    assert_eq!(embedding_count(&conn)?, 514, "suppression");
    conn.execute("INSERT INTO memory_suppressions(target_kind,target_value,reason,actor,status,created_at_epoch,updated_at_epoch)
        VALUES('pattern','scope-fixture-suppressed','fixture','test','active',1,1)", [])?;
    let filters = VectorSearchFilters {
        project: Some("/wanted"),
        ..VectorSearchFilters::default()
    };
    let allowed = vector_search_embedding_filtered_with_suppression_policy(
        &conn, &vector, filters, false, 10,
    )?;
    assert_eq!(
        allowed
            .hits
            .iter()
            .map(|hit| hit.memory_id)
            .collect::<Vec<_>>(),
        vec![1, 514]
    );
    let inspection = vector_search_embedding_filtered_with_suppression_policy(
        &conn, &vector, filters, true, 10,
    )?;
    assert!(inspection
        .hits
        .iter()
        .any(|hit| (2..=513).contains(&hit.memory_id)));
    conn.execute(
        "DELETE FROM memory_embeddings WHERE memory_id=1 AND model='A'",
        [],
    )?;
    let after_delete = vector_search_embedding_filtered_with_suppression_policy(
        &conn, &vector, filters, false, 10,
    )?;
    assert_eq!(
        after_delete
            .hits
            .iter()
            .map(|hit| hit.memory_id)
            .collect::<Vec<_>>(),
        vec![514]
    );
    Ok(())
}

#[test]
fn absent_index_exact_scan_does_not_sample_away_old_matches() -> Result<()> {
    let conn = setup_vector_conn()?;
    conn.execute_batch("BEGIN")?;
    for id in 1..=4_097 {
        memory(&conn, id, "/repo")?;
        put(
            &conn,
            id,
            "A",
            if id == 1 { &[1.0, 0.0] } else { &[0.0, 1.0] },
        )?;
    }
    conn.execute_batch("COMMIT")?;
    let vector = TextEmbedding::new("A", vec![1.0, 0.0])?;
    let outcome =
        vector_search_embedding_filtered(&conn, &vector, VectorSearchFilters::default(), 1)?;
    assert_eq!(outcome.candidates_scanned, 4_097);
    assert_eq!(outcome.hits[0].memory_id, 1);
    assert_eq!(outcome.hits[0].distance, 0.0);
    Ok(())
}

#[test]
fn legacy_dimension_only_mirror_never_makes_profile_ready() -> Result<()> {
    let conn = setup_vector_conn()?;
    load_vec_extension(&conn)?;
    memory(&conn, 1, "/repo")?;
    put(&conn, 1, "A", &[1.0, 0.0])?;
    conn.execute_batch(
        "CREATE VIRTUAL TABLE memory_embedding_vec_2 USING vec0(
        memory_id INTEGER PRIMARY KEY, embedding float[2] distance_metric=cosine, +model TEXT);
        CREATE TABLE memory_embedding_vec_state(dimensions INTEGER PRIMARY KEY, done INTEGER);
        INSERT INTO memory_embedding_vec_state VALUES(2, 1);",
    )?;
    let a = EmbeddingProfile {
        model: "A",
        dimensions: 2,
    };
    assert!(!vec_index_ready(&conn, a)?);
    assert_eq!(query(&conn, "A", "/repo")?, vec![1]);
    ensure_vec_index(&conn)?;
    assert!(vec_index_ready(&conn, a)?);
    assert!(!table_exists(&conn, "memory_embedding_vec_2")?);
    assert_eq!(embedding_count(&conn)?, 1);
    Ok(())
}
