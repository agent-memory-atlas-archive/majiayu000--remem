use super::*;

#[test]
fn raw_search_starts_with_fts_before_project_ordering() -> Result<()> {
    let conn = setup_conn();
    insert_at_epoch(
        &conn,
        "s1",
        "/proj",
        ROLE_USER,
        "rare diagnostic phrase",
        100,
    );
    insert_at_epoch(
        &conn,
        "s2",
        "/other",
        ROLE_USER,
        "rare diagnostic phrase",
        200,
    );
    insert_at_epoch(
        &conn,
        "s3",
        "/proj",
        ROLE_USER,
        "rare diagnostic phrase",
        300,
    );
    let sql = format!(
        "EXPLAIN QUERY PLAN {RAW_SEARCH_SELECT} AND r.project = ?2 ORDER BY r.created_at_epoch DESC LIMIT 1"
    );
    let plan = conn
        .prepare(&sql)?
        .query_map(
            params![fts_query("rare diagnostic phrase"), "/proj"],
            |row| row.get::<_, String>(3),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // SQLite 3.45 otherwise chooses the project index first and repeatedly
    // executes MATCH for each row. Real archive queries then exceed 30 seconds.
    assert!(plan[0].contains("SCAN f VIRTUAL TABLE"), "{plan:?}");
    assert!(
        !plan
            .iter()
            .any(|step| step.contains("idx_raw_messages_project_created")),
        "{plan:?}"
    );
    let hits = search_raw_messages(
        &conn,
        &RawSearchRequest {
            query: "rare diagnostic phrase".to_string(),
            project: Some("/proj".to_string()),
            branch: None,
            role: Some(ROLE_USER.to_string()),
            limit: 1,
            offset: 0,
            since_epoch: Some(150),
            until_epoch: Some(350),
        },
    )?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "s3");
    Ok(())
}
