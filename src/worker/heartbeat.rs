//! Retire the heartbeat when a worker returns, errors or its future is dropped.

use crate::db;

pub(super) struct Registration {
    owner: String,
    pid: u32,
}

impl Registration {
    /// Arm only after the initial heartbeat has been persisted successfully.
    pub(super) fn new(owner: &str) -> Self {
        Self {
            owner: owner.to_owned(),
            pid: std::process::id(),
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(error) = db::open_db_no_migrate()
            .and_then(|conn| db::retire_worker_heartbeat(&conn, &self.owner, self.pid))
        {
            crate::log::error(
                "worker",
                &format!(
                    "heartbeat retirement failed owner={}: {error:#}",
                    self.owner
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_retires_heartbeat_on_early_error() -> anyhow::Result<()> {
        let _dir = db::test_support::ScopedTestDataDir::new("worker-heartbeat-error");
        let conn = db::open_db()?;
        let now = chrono::Utc::now().timestamp();
        let owner = db::current_worker_owner("once", std::process::id(), now * 1000);
        db::upsert_worker_heartbeat(&conn, &owner, i64::from(std::process::id()), now, now)?;
        let result: anyhow::Result<()> = (|| {
            let _registration = Registration::new(&owner);
            anyhow::bail!("fixture early worker error")
        })();
        assert!(result.is_err());
        assert!(db::healthy_worker_heartbeat(&conn, db::WORKER_HEARTBEAT_HEALTH_SECS)?.is_none());
        assert_eq!(db::latest_worker_heartbeat(&conn)?.unwrap().pid, None);
        Ok(())
    }
}
