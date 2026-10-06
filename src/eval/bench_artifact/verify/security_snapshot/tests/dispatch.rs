use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::super::{
    expected_events, image, inventory, validate_full_snapshot_identity, VerificationContext,
};
use crate::eval::memory_bench::{
    production_security_snapshot_for_test,
    types::{MemoryBenchSuiteFixture, MemoryBenchTask},
};

const SUITE_BYTES: &[u8] =
    include_bytes!("../../../../../../eval/public/memory/suites/adversarial-policy/suite.json");

struct FreshSnapshot {
    root: PathBuf,
    task: MemoryBenchTask,
    retrieved_event_ids: Vec<String>,
    suite_sha256: String,
}

impl Drop for FreshSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl FreshSnapshot {
    async fn new(task_id: &str) -> Result<Self> {
        let suite: MemoryBenchSuiteFixture = serde_json::from_slice(SUITE_BYTES)?;
        let task = suite
            .tasks
            .into_iter()
            .find(|task| task.id == task_id)
            .context("typed security task")?;
        // Generate current-schema state through production capture and real
        // task claims. No old committed SQLite artifact supplies the baseline.
        let (snapshot, retrieved_event_ids) = production_security_snapshot_for_test(&task).await?;
        image::validate_canonical(&snapshot)?;
        let root = std::env::temp_dir().join(format!(
            "remem-dispatch-snapshot-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&root)?;
        let fixture = Self {
            root,
            task,
            retrieved_event_ids,
            suite_sha256: format!("{:x}", Sha256::digest(SUITE_BYTES)),
        };
        fs::write(fixture.root.join("snapshot.sqlite3"), snapshot)?;
        Ok(fixture)
    }

    fn open(&self) -> Result<Connection> {
        Ok(Connection::open(self.root.join("snapshot.sqlite3"))?)
    }

    fn validate_inventory(&self, connection: &Connection) -> Result<()> {
        inventory::validate_closed_world(connection, &self.task, expected_events(&self.task)?.len())
    }

    fn validate_replay(
        &self,
        connection: &Connection,
        context: &mut VerificationContext,
    ) -> Result<()> {
        validate_full_snapshot_identity(
            connection,
            &self.task,
            &self.retrieved_event_ids,
            &self.suite_sha256,
            std::env::consts::OS,
            std::env::consts::ARCH,
            context,
        )
    }

    fn validate(&self, connection: &Connection, context: &mut VerificationContext) -> Result<()> {
        self.validate_inventory(connection)?;
        self.validate_replay(connection, context)
    }
}

#[tokio::test]
async fn dispatch_snapshots_accept_actual_production_claim_history() -> Result<()> {
    for (task_id, expected_rows) in [
        ("secrets-api-key-001", 2),
        ("approved-external-source-001", 3),
        ("instruction-injection-001", 3),
    ] {
        let fixture = FreshSnapshot::new(task_id).await?;
        let connection = fixture.open()?;
        let mut context = VerificationContext::new();
        fixture.validate(&connection, &mut context)?;
        let actual_rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM worker_dispatch_state", [], |row| {
                row.get(0)
            })?;
        assert_eq!(actual_rows, expected_rows, "{task_id}");
        if task_id == "approved-external-source-001" {
            let graph_status: String = connection.query_row(
                "SELECT status FROM extraction_tasks WHERE task_kind = 'graph_candidate'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(graph_status, "pending");
        }
    }
    Ok(())
}

fn assert_dispatch_tampering_rejected(fixture: &FreshSnapshot, mutations: &[&str]) -> Result<()> {
    let connection = fixture.open()?;
    let mut context = VerificationContext::new();
    fixture.validate(&connection, &mut context)?;
    for sql in mutations {
        connection.execute_batch("SAVEPOINT dispatch_tamper")?;
        connection.execute_batch(sql)?;
        let inventory_error = fixture.validate_inventory(&connection).unwrap_err();
        assert!(
            format!("{inventory_error:#}").contains("worker_dispatch_state"),
            "{inventory_error:#}"
        );
        let replay_error = fixture
            .validate_replay(&connection, &mut context)
            .unwrap_err();
        assert!(
            format!("{replay_error:#}").contains("worker_dispatch_state"),
            "{replay_error:#}"
        );
        connection.execute_batch("ROLLBACK TO dispatch_tamper; RELEASE dispatch_tamper")?;
        fixture.validate(&connection, &mut context)?;
    }
    Ok(())
}

#[tokio::test]
async fn dispatch_snapshots_reject_cross_scope_and_unclaimed_groups() -> Result<()> {
    let fixture = FreshSnapshot::new("approved-external-source-001").await?;
    assert_dispatch_tampering_rejected(
        &fixture,
        &[
            "UPDATE worker_dispatch_state SET scope = 'job'
             WHERE scope = 'extraction' AND stage = 'observation_extract'",
            "UPDATE worker_dispatch_state SET host = 'foreign-host'
             WHERE scope = 'extraction' AND stage = 'observation_extract'",
            "UPDATE worker_dispatch_state SET project = 'foreign-project'
             WHERE scope = 'extraction' AND stage = 'memory_candidate'",
            "INSERT INTO worker_dispatch_state
                 (scope, stage, host, project, ready_sequence, last_claim_sequence)
             SELECT scope, 'graph_candidate', host, project, 2, 3
             FROM worker_dispatch_state
             WHERE scope = 'extraction' AND stage = 'memory_candidate'",
            "INSERT INTO worker_dispatch_state VALUES ('queue', 'job', '', '', 0, NULL)",
        ],
    )
}

#[tokio::test]
async fn dispatch_snapshots_reject_changed_sequences_and_missing_rows() -> Result<()> {
    let fixture = FreshSnapshot::new("approved-external-source-001").await?;
    assert_dispatch_tampering_rejected(
        &fixture,
        &[
            "UPDATE worker_dispatch_state SET ready_sequence = 0
             WHERE scope = 'extraction' AND stage = 'memory_candidate'",
            "UPDATE worker_dispatch_state SET last_claim_sequence = 3
             WHERE scope = 'extraction' AND stage = 'observation_extract'",
            "UPDATE worker_dispatch_state SET last_claim_sequence = 3 WHERE scope = 'queue'",
            "UPDATE worker_dispatch_state SET last_claim_sequence = NULL WHERE scope = 'queue'",
            "UPDATE worker_dispatch_state SET ready_sequence = 0.5
             WHERE scope = 'extraction' AND stage = 'memory_candidate'",
            "DELETE FROM worker_dispatch_state WHERE scope = 'queue'",
        ],
    )
}

#[tokio::test]
async fn dispatch_snapshot_full_identity_rejects_hidden_ledger_columns() -> Result<()> {
    let fixture = FreshSnapshot::new("approved-external-source-001").await?;
    let connection = fixture.open()?;
    let mut context = VerificationContext::new();
    fixture.validate(&connection, &mut context)?;
    connection.execute_batch(
        "ALTER TABLE worker_dispatch_state ADD COLUMN hidden_payload TEXT;
         UPDATE worker_dispatch_state SET hidden_payload = 'undeclared task payload'
         WHERE scope = 'queue'",
    )?;
    // The six declared ledger fields are unchanged. Full schema/cell identity
    // must remain a separate required gate and catch the new hidden column.
    fixture.validate_inventory(&connection)?;
    let error = fixture.validate(&connection, &mut context).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("complete typed snapshot identity differs"),
        "{message}"
    );
    assert!(message.contains("worker_dispatch_state"), "{message}");
    assert!(message.contains("sqlite_schema"), "{message}");
    Ok(())
}
