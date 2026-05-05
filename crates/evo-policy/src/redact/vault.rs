use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::classify::{classify_secret, fingerprint_of};

/// Persistent file-backed vault. Always lives at `~/.evoclaw/secrets/vault.json`
/// (or whatever path the caller hands to `Vault::load`/`Vault::save`).
/// chmod 600 on Unix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vault {
    pub version: u32,
    pub entries: Vec<VaultEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

impl Vault {
    pub async fn load(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = tokio::fs::read_to_string(path).await?;
        let v: Self = serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(v)
    }

    pub async fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Atomic write: stage to `<path>.tmp` in the same directory (so the
        // rename is a same-filesystem op), apply secure perms BEFORE rename,
        // then rename — POSIX rename is atomic, so a kill mid-write can never
        // leave the canonical file truncated or empty.
        let tmp = tmp_path_for(path);
        tokio::fs::write(&tmp, json).await?;
        // Set 0600 on the temp file BEFORE rename so the final file inherits
        // secure perms even momentarily — we never want a 0644 vault on disk.
        Self::chmod_600(&tmp).await?;
        // Best-effort fsync: open the file and `sync_all` before the rename.
        if let Ok(file) = tokio::fs::OpenOptions::new().write(true).open(&tmp).await {
            let _ = file.sync_all().await;
        }
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    pub fn upsert(&mut self, name: &str, value: &str) -> &VaultEntry {
        let kind = classify_secret(value).label();
        let fingerprint = fingerprint_of(value);
        if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
            self.entries[idx].value = value.to_string();
            self.entries[idx].kind = kind.into();
            self.entries[idx].fingerprint = fingerprint;
            return &self.entries[idx];
        }
        self.entries.push(VaultEntry {
            name: name.into(),
            value: value.into(),
            kind: kind.into(),
            fingerprint,
            created_at: Utc::now(),
        });
        self.entries.last().unwrap()
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.name != name);
        before != self.entries.len()
    }

    pub fn get(&self, name: &str) -> Option<&VaultEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn list(&self) -> &[VaultEntry] {
        &self.entries
    }

    #[cfg(unix)]
    async fn chmod_600(path: &Path) -> Result<(), std::io::Error> {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(path, perms).await
    }

    #[cfg(not(unix))]
    async fn chmod_600(_path: &Path) -> Result<(), std::io::Error> {
        Ok(())
    }
}

pub fn default_vault_path(evoclaw_dir: &Path) -> PathBuf {
    evoclaw_dir.join("secrets").join("vault.json")
}

/// Same-directory `<path>.tmp` companion path used by `Vault::save` for the
/// atomic write-and-rename cycle. Kept private but `pub(crate)` so future
/// modules can reuse it.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}
