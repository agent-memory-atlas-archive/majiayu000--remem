//! Shared host-profile selection for native readers, installers and diagnostics.

use anyhow::{ensure, Context, Result};
use std::path::PathBuf;

pub(crate) fn codex() -> Result<PathBuf> {
    let root = agent_sessions::Roots::from_env_for(agent_sessions::Agent::Codex)?
        .codex
        .context("cannot resolve Codex home: set an absolute CODEX_HOME")?;
    validate_codex_root(root)
}

fn validate_codex_root(root: PathBuf) -> Result<PathBuf> {
    ensure!(
        root.is_absolute(),
        "Codex home must be absolute; set CODEX_HOME to an absolute directory"
    );
    match std::fs::metadata(&root) {
        Ok(metadata) => ensure!(metadata.is_dir(), "Codex home is not a directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot inspect Codex home"),
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_root_requires_an_absolute_path() {
        for value in ["", " ", "profile", "./profile", "~/profile"] {
            assert!(validate_codex_root(PathBuf::from(value)).is_err());
        }
        let root = std::env::temp_dir().join("remem-codex-root");
        assert_eq!(validate_codex_root(root.clone()).unwrap(), root);
    }
}
