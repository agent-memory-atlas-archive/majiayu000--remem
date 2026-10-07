//! One read-only composition of existing memory evidence, without a second ledger.
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Params};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{db, memory, truth};

const LIMIT: usize = 20;

pub(crate) struct MemoryDiagnosticOptions {
    pub project: String,
    pub query: Option<String>,
    pub host: Option<String>,
    pub session_id: Option<String>,
    pub source_root: Option<String>,
    pub injection_run_id: Option<String>,
}

#[derive(Serialize)]
struct Stage {
    stage: &'static str,
    evidence: Vec<Value>,
    has_more: bool,
    note: &'static str,
}

impl Stage {
    fn new(stage: &'static str, mut evidence: Vec<Value>, note: &'static str) -> Self {
        let mut counts = std::collections::BTreeMap::new();
        let mut has_more = false;
        evidence.retain(|row| {
            let count = counts
                .entry(row["source"].as_str().unwrap_or("unknown").to_owned())
                .or_insert(0);
            *count += 1;
            if *count > LIMIT {
                has_more = true;
                false
            } else {
                true
            }
        });
        Self {
            stage,
            evidence,
            has_more,
            note,
        }
    }
}

pub(crate) fn run_memory_diagnostic(
    opts: MemoryDiagnosticOptions,
    json_output: bool,
    quiet: bool,
) -> Result<()> {
    crate::log::without_file_logging(|| {
        if opts.project.trim().is_empty()
            || opts.query.as_deref().is_some_and(|q| q.trim().is_empty())
        {
            bail!("doctor memory requires a non-blank project and phrase");
        }
        let conn = db::open_db_read_only_current().context("open memory evidence read-only")?;
        conn.execute_batch("BEGIN")?;
        let report = build_report(&conn, &opts)?;
        conn.execute_batch("ROLLBACK")?;
        if json_output {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if !quiet {
            println!("Why wasn't it remembered? project={}", safe(&opts.project));
            for stage in report["stages"].as_array().context("diagnostic stages")? {
                println!("\n{}:", stage["stage"].as_str().context("stage name")?);
                let rows = stage["evidence"].as_array().context("stage evidence")?;
                if rows.is_empty() {
                    println!("  unknown: no matching evidence in this scope");
                }
                let mut sources = std::collections::BTreeMap::<&str, Vec<&Value>>::new();
                for row in rows {
                    sources
                        .entry(row["source"].as_str().unwrap_or("unknown"))
                        .or_default()
                        .push(row);
                }
                for (source, evidence) in sources {
                    let mut statuses = std::collections::BTreeMap::new();
                    for row in &evidence {
                        if let Some(status) = row["status"].as_str() {
                            *statuses.entry(status).or_insert(0usize) += 1;
                        }
                    }
                    println!("  {source}: {} retained rows", evidence.len());
                    if !statuses.is_empty() {
                        println!("  stored statuses: {}", serde_json::to_string(&statuses)?);
                    }
                    for row in evidence.iter().take(3) {
                        println!("  {}", serde_json::to_string(row)?);
                    }
                    if evidence.len() > 3 {
                        println!("  {} more retained rows; use --json", evidence.len() - 3);
                    }
                }
                if stage["has_more"] == true {
                    println!("  more evidence omitted (20 rows per source)");
                }
                println!("  {}", stage["note"].as_str().context("stage note")?);
            }
        }
        Ok(())
    })
}

// SQL returns explicit, source-qualified objects. Bad JSON/SQL stays an error.
fn rows(conn: &Connection, sql: &str, binds: impl Params) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let values = stmt
        .query_map(binds, |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    values
        .into_iter()
        .map(|s| Ok(serde_json::from_str(&s)?))
        .collect()
}

fn safe(value: &str) -> String {
    db::redact_capture_content(value)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

fn redact(value: &mut Value) {
    match value {
        Value::String(s) => *s = safe(s),
        Value::Array(items) => items.iter_mut().for_each(redact),
        Value::Object(items) => {
            for (key, value) in items {
                redact(value);
                if matches!(key.as_str(), "preview" | "text" | "request" | "title") {
                    if let Value::String(s) = value {
                        *s = s.chars().take(300).collect();
                    }
                }
            }
        }
        _ => {}
    }
}

fn build_report(conn: &Connection, opts: &MemoryDiagnosticOptions) -> Result<Value> {
    let mut raw_capture = Vec::new();
    let mut linked_sessions = Vec::new();
    let mut raw_has_more = false;
    if let Some(session_id) = &opts.session_id {
        let raw = memory::raw_query::query_raw_session_messages(
            conn,
            &memory::raw_query::RawSessionMessagesRequest {
                host: opts
                    .host
                    .clone()
                    .context("source session requires --host")?,
                source_root: opts
                    .source_root
                    .clone()
                    .context("source session requires --source-root")?,
                project: opts.project.clone(),
                session_id: session_id.clone(),
                limit: LIMIT as i64,
                cursor: None,
            },
        )?;
        raw_has_more = raw.has_more;
        raw_capture.push(json!({"source":"raw_session", "host":raw.host,
            "source_root":raw.source_root,"session_id":raw.session_id,
            "content_hash":raw.content_hash,"sample_count":raw.count,"has_more":raw.has_more,
            "raw_ids":raw.messages.iter().map(|r|r.id).collect::<Vec<_>>(),
            "preview":raw.messages.first().map(|r|safe(&r.content).chars().take(300).collect::<String>())}));
    } else if let Some(query) = &opts.query {
        let raw = memory::raw_archive::search_raw_messages(
            conn,
            &memory::raw_archive::RawSearchRequest {
                query: query.clone(),
                project: Some(opts.project.clone()),
                branch: None,
                role: None,
                limit: 21,
                offset: 0,
                since_epoch: None,
                until_epoch: None,
            },
        )?;
        raw_has_more = raw.len() > LIMIT;
        for r in raw {
            let identity = conn.query_row(
                "SELECT r.source_root,CASE WHEN i.status='active' THEN i.host END FROM raw_messages r
                 LEFT JOIN raw_session_identities i ON i.id=r.transcript_identity_id
                 WHERE r.id=?1", [r.id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?)))?;
            if opts
                .host
                .as_ref()
                .is_some_and(|h| identity.1.as_ref() != Some(h))
            {
                continue;
            }
            if let Some(host) = &identity.1 {
                linked_sessions.push(json!({"host":host,"session_id":r.session_id}));
            }
            raw_capture.push(json!({"source":"raw_message","id":r.id,
                "session_id":r.session_id,"role":r.role,"capture_source":r.source,
                "source_root":identity.0,"host":identity.1,
                "preview":safe(&r.content).chars().take(300).collect::<String>()}));
        }
    }
    let linked_sessions = serde_json::to_string(&linked_sessions)?;
    let binds = params![
        opts.project,
        opts.query,
        opts.host,
        opts.session_id,
        linked_sessions
    ];
    // A raw hit can locate a Stop-ledger session even when the ledger stores only
    // transcript metadata. That association never proves sentence-level extraction.
    let matched_events = "WITH matched_events AS (
        SELECT e.* FROM captured_events e
        JOIN projects p ON p.id=e.project_id JOIN hosts h ON h.id=e.host_id
        WHERE p.project_path=?1 AND (?3 IS NULL OR h.name=?3)
          AND (?4 IS NULL OR e.session_id=?4)
          AND (?2 IS NULL OR instr(lower(e.content_text),lower(?2))>0
            OR EXISTS (SELECT 1 FROM json_each(?5) j
              WHERE json_extract(j.value,'$.host')=h.name
                AND json_extract(j.value,'$.session_id')=e.session_id))
    ) ";
    let mut capture = rows(conn, &format!("{matched_events}
        SELECT json_object('source','captured_event','id',e.id,'session_id',e.session_id,
          'host',h.name,'event_type',e.event_type,'role',e.role,
          'preview',e.content_text,'blob_id',e.content_blob_id,'created_at_epoch',e.created_at_epoch)
        FROM matched_events e JOIN hosts h ON h.id=e.host_id ORDER BY e.id DESC LIMIT 21"), binds)?;
    capture.extend(raw_capture);
    let mut capture = Stage::new("1. Capture", capture,
        "Raw hits and linked ledger sessions are source evidence, not sentence-level distillation proof. No hit is unknown; blobs and paraphrases can miss discovery.");
    capture.has_more |= raw_has_more;

    let mut extraction = rows(conn, &format!("{matched_events}
        SELECT json_object('source','extraction_task','id',t.id,'kind',t.task_kind,
          'status',t.status,'cursor_event_id',t.cursor_event_id,
          'high_watermark_event_id',t.high_watermark_event_id,'attempts',t.attempts,
          'failure_class',t.failure_class,'last_error',t.last_error,'next_retry_epoch',t.next_retry_epoch)
        FROM extraction_tasks t JOIN projects p ON p.id=t.project_id
        JOIN hosts h ON h.id=t.host_id LEFT JOIN sessions s ON s.id=t.session_row_id
        WHERE p.project_path=?1 AND (?3 IS NULL OR h.name=?3)
          AND (?4 IS NULL OR s.session_id=?4)
          AND (?2 IS NULL OR EXISTS (SELECT 1 FROM matched_events e WHERE e.session_row_id=t.session_row_id))
        ORDER BY t.id DESC LIMIT 21"), binds)?;
    extraction.extend(rows(
        conn,
        &format!(
            "{matched_events}
        SELECT json_object('source','observation','id',o.id,'status',o.status,
          'evidence_event_ids',o.evidence_event_ids,'text',o.text)
        FROM observations o WHERE o.project=?1 AND EXISTS (
          SELECT 1 FROM json_each(o.evidence_event_ids) j JOIN matched_events e ON e.id=j.value)
        ORDER BY o.id DESC LIMIT 21"
        ),
        binds,
    )?);
    let summaries = rows(conn, &format!("{matched_events}
        SELECT json_object('source','session_summary','id',s.id,
          'covered_from_event_id',s.covered_from_event_id,'covered_to_event_id',s.covered_to_event_id,
          'request',s.request)
        FROM session_summaries s WHERE s.project=?1 AND EXISTS (
          SELECT 1 FROM matched_events e WHERE e.session_row_id=s.session_row_id
            AND e.id BETWEEN s.covered_from_event_id AND s.covered_to_event_id)
        ORDER BY s.id DESC LIMIT 21"), binds)?;
    let summary_ids = serde_json::to_string(
        &summaries
            .iter()
            .filter_map(|s| s["id"].as_i64())
            .collect::<Vec<_>>(),
    )?;
    extraction.extend(summaries);
    let extraction = Stage::new("2. Extraction", extraction,
        "Task completion and range coverage do not prove this phrase was distilled. Stored failure/retry fields explain only that task.");

    let candidates = rows(conn, &format!("{matched_events}
        SELECT json_object('source','memory_candidate','id',c.id,'topic_key',c.topic_key,'text',c.text,
          'status',c.review_status,'block_reason',c.auto_promote_block_reason,
          'review_reason',c.review_reason,'review_actor',c.review_actor,
          'evidence_event_ids',c.evidence_event_ids,'source_kind',c.source_kind)
        FROM memory_candidates c JOIN projects p ON p.id=c.project_id WHERE p.project_path=?1
          AND ((?2 IS NOT NULL AND instr(lower(c.text),lower(?2))>0 AND ?3 IS NULL)
            OR EXISTS (SELECT 1 FROM json_each(c.evidence_event_ids) j JOIN matched_events e ON e.id=j.value))
        ORDER BY c.id DESC LIMIT 21"), binds)?;
    let candidate_ids = serde_json::to_string(
        &candidates
            .iter()
            .filter_map(|c| c["id"].as_i64())
            .collect::<Vec<_>>(),
    )?;
    let mut review = candidates;
    let user_candidates = rows(conn, &format!("{matched_events}
        SELECT json_object('source','user_context_candidate',
          'id',c.id,'status',c.review_status,'block_reason',c.auto_promote_block_reason,
          'review_note',c.review_note,'result_claim_id',c.result_claim_id,'source_refs',json(c.source_refs_json))
        FROM user_context_candidates c WHERE c.source_project=?1 AND (?3 IS NULL OR c.host=?3)
          AND (?4 IS NULL OR c.session_id=?4)
          AND (?2 IS NULL OR instr(lower(c.claim_text),lower(?2))>0
            OR EXISTS (SELECT 1 FROM json_each(c.source_refs_json) j JOIN matched_events e
              ON e.id=json_extract(j.value,'$.id') WHERE json_extract(j.value,'$.kind')='captured_event'))
        ORDER BY c.id DESC LIMIT 21"), binds)?;
    let claim_ids = serde_json::to_string(
        &user_candidates
            .iter()
            .filter_map(|c| c["result_claim_id"].as_i64())
            .collect::<Vec<_>>(),
    )?;
    review.extend(user_candidates);
    review.extend(rows(
        conn,
        "SELECT json_object('source','memory_operation','id',id,
        'candidate_id',source_candidate_id,'result_memory_id',result_memory_id,
        'operation',operation,'reason',reason,'noop_reason',noop_reason,'defer_reason',defer_reason)
        FROM memory_operation_log WHERE source_candidate_id IN (SELECT value FROM json_each(?1))
        ORDER BY id DESC LIMIT 21",
        [&candidate_ids],
    )?);
    let review = Stage::new("3. Review", review,
        "Only stored reasons are shown. A missing reason is unknown; source association is not proof that the candidate preserves the original meaning.");

    let memories = rows(conn, &format!("{matched_events}
        SELECT json_object('source','memory','id',m.id,'topic_key',m.topic_key,'title',m.title,'status',m.status,
          'expires_at_epoch',m.expires_at_epoch,'valid_from_epoch',m.valid_from_epoch,
          'valid_to_epoch',m.valid_to_epoch,'source_candidate_id',m.source_candidate_id)
        FROM memories m WHERE m.project=?1 AND (
          (?2 IS NOT NULL AND instr(lower(m.title || ' ' || m.content),lower(?2))>0 AND ?3 IS NULL)
          OR m.source_candidate_id IN (SELECT value FROM json_each(?6))
          OR EXISTS (SELECT 1 FROM json_each(m.evidence_event_ids) j JOIN matched_events e ON e.id=j.value))
        ORDER BY m.id DESC LIMIT 21"), params![opts.project,opts.query,opts.host,opts.session_id,linked_sessions,candidate_ids])?;
    let now = chrono::Utc::now().timestamp();
    let mut validity = Vec::new();
    let memory_ids = memories
        .iter()
        .filter_map(|m| m["id"].as_i64())
        .collect::<Vec<_>>();
    let classifications = truth::classify_memories(conn, &memory_ids, now)?;
    let projection = if memory_ids.is_empty() {
        None
    } else {
        Some(truth::project_current_truth(
            conn,
            &truth::TruthQuery {
                project: opts.project.clone(),
                branch: None,
                as_of_epoch: Some(now),
                subject_key: None,
            },
        )?)
    };
    for mut m in memories {
        let id = m["id"].as_i64().context("memory identity")?;
        m["classification"] =
            serde_json::to_value(classifications.get(&id).context("memory classification")?)?;
        let suppressions = memory::suppression::active_suppressions_for_memory(conn, id)?;
        m["suppressions"] = json!(suppressions
            .iter()
            .map(|s| json!({"id":s.id,"reason":s.reason}))
            .collect::<Vec<_>>());
        let canonical_ref = format!("memory:{id}");
        m["truth"] = json!(projection
            .iter()
            .flat_map(|p| &p.truths)
            .filter(|t| t
                .claim
                .as_ref()
                .is_some_and(|c| c.canonical_ref == canonical_ref)
                || t.rejected.contains(&canonical_ref)
                || t.conflicting_claims
                    .iter()
                    .any(|c| c.canonical_ref == canonical_ref))
            .map(|t| json!({"subject_key":t.subject_key,
                "validity":t.validity,"selected_reason":t.selected_reason,
                "selected_ref":t.claim.as_ref().map(|c|&c.canonical_ref),"rejected":t.rejected
            }))
            .collect::<Vec<_>>());
        validity.push(m);
    }
    let user_claims = rows(
        conn,
        "SELECT json_object('source','user_context_claim','id',id,
        'status',status,'valid_from_epoch',valid_from_epoch,'valid_to_epoch',valid_to_epoch,
        'supersedes_claim_id',supersedes_claim_id) FROM user_context_claims
        WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY id DESC LIMIT 21",
        [&claim_ids],
    )?;
    for mut claim in user_claims {
        let id = claim["id"].as_i64().context("user claim identity")?;
        claim["policy_suppressed"] = json!(memory::suppression::user_claim_is_policy_suppressed(
            conn, id
        )?);
        validity.push(claim);
    }
    let validity = Stage::new("4. Current validity", validity,
        "Memory classification and CurrentTruth use current evidence. Suppression is visibility, not falsity. User claims show stored lifecycle only.");
    let memory_ids = serde_json::to_string(&memory_ids)?;
    let latest_run: Option<String> = conn
        .query_row(
            "SELECT injection_run_id FROM context_injection_items WHERE project=?1
          AND (?2 IS NULL OR host=?2) AND (?3 IS NULL OR injection_run_id=?3)
         ORDER BY injected_at_epoch DESC,id DESC LIMIT 1",
            params![opts.project, opts.host, opts.injection_run_id],
            |r| r.get(0),
        )
        .optional()?;
    if opts.injection_run_id.is_some() && latest_run.is_none() {
        bail!("requested injection run has no retained audit in the selected project/host");
    }
    let mut context = rows(
        conn,
        "SELECT json_object('source','context_item','run_id',injection_run_id,
        'host',host,'session_id',session_id,'item_kind',item_kind,'item_id',item_id,
        'memory_id',memory_id,'channel',channel,'status',status,'drop_reason',drop_reason,
        'injected_at_epoch',injected_at_epoch)
        FROM context_injection_items WHERE project=?1 AND injection_run_id=?2
          AND ((memory_id IN (SELECT value FROM json_each(?3)))
            OR (item_kind='user_claim' AND item_id IN (SELECT value FROM json_each(?4)))
            OR (item_kind='session_summary' AND item_id IN (SELECT value FROM json_each(?5))))
        ORDER BY id DESC LIMIT 21",
        params![opts.project, latest_run, memory_ids, claim_ids, summary_ids],
    )?;
    if let Some(run_id) = latest_run {
        context.insert(
            0,
            rows(
                conn,
                "SELECT json_object('source','context_run','run_id',injection_run_id,
            'host',host,'session_id',session_id,'hook_source',hook_source,
            'injected_at_epoch',injected_at_epoch) FROM context_injection_items
            WHERE project=?1 AND injection_run_id=?2 ORDER BY id DESC LIMIT 1",
                params![opts.project, run_id],
            )?
            .into_iter()
            .next()
            .context("selected context run")?,
        );
    }
    let context = Stage::new("5. Context", context,
        "This is one recorded destination run, not a replay. No matching per-item audit means unknown, never a guessed ranking, budget or capture failure.");
    let mut report = json!({"schema_version":1,"project":opts.project,"query":opts.query,
        "source_session":{"host":opts.host,"source_root":opts.source_root,"session_id":opts.session_id},
        "as_of_epoch":now,"stages":[capture,extraction,review,validity,context]});
    redact(&mut report);
    Ok(report)
}

#[cfg(test)]
#[path = "memory_diagnostic_tests.rs"]
mod tests;
