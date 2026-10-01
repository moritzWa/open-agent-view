//! The harness the new-session composer starts on.
//!
//! The dashboard records the provider of every launch it dispatches, and the
//! next start opens the composer on it unless `--harness` says otherwise. A
//! missing or unreadable record falls back to the built-in default.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::domain::Provider;

const DOCUMENT_VERSION: u32 = 1;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize, Serialize)]
struct LastHarnessDocument {
    version: u32,
    provider: Provider,
}

#[derive(Clone, Debug)]
pub struct LastHarness {
    path: PathBuf,
}

impl LastHarness {
    pub fn load_default() -> Result<Self> {
        Ok(Self::at(default_last_harness_path()?))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn provider(&self) -> Option<Provider> {
        let input = fs::read_to_string(&self.path).ok()?;
        let document: LastHarnessDocument = serde_json::from_str(&input).ok()?;
        (document.version == DOCUMENT_VERSION).then_some(document.provider)
    }

    pub fn save(&self, provider: &Provider) -> Result<()> {
        if self.provider().as_ref() == Some(provider) {
            return Ok(());
        }
        let parent = self
            .path
            .parent()
            .context("last-harness path has no parent")?;
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        let document = LastHarnessDocument {
            version: DOCUMENT_VERSION,
            provider: provider.clone(),
        };
        let name = self
            .path
            .file_name()
            .context("last-harness path has no file name")?
            .to_string_lossy();
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = self
            .path
            .with_file_name(format!(".{name}.tmp-{}-{sequence}", std::process::id()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .with_context(|| format!("failed to create {}", temporary.display()))?;
            serde_json::to_writer_pretty(&mut file, &document)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            crate::fs_util::replace_file(&temporary, &self.path)
                .with_context(|| format!("failed to replace {}", self.path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

pub fn default_last_harness_path() -> Result<PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("open-agent-view")
            .join("last-harness.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/open-agent-view/last-harness.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_harness_survives_a_restart() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("last-harness.json");
        assert_eq!(LastHarness::at(path.clone()).provider(), None);
        LastHarness::at(path.clone())
            .save(&Provider::OpenCode)
            .unwrap();
        assert_eq!(
            LastHarness::at(path.clone()).provider(),
            Some(Provider::OpenCode)
        );
        LastHarness::at(path.clone())
            .save(&Provider::Codex)
            .unwrap();
        assert_eq!(LastHarness::at(path).provider(), Some(Provider::Codex));
    }

    #[test]
    fn unreadable_record_falls_back_to_the_default() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-harness.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(LastHarness::at(path).provider(), None);
    }
}
