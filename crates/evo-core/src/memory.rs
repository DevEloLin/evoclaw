//! Memory L0..L5 + grep search + redaction. PRD §17.3 + §33 + PROMPTS §9.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

static MEM_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum MemoryLayer {
    L0,
    L1,
    L2,
    L3,
    L4,
    L5,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub layer: MemoryLayer,
    pub content: String,
    pub source: String,
    pub confidence: f32,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl MemoryRecord {
    pub fn new(
        layer: MemoryLayer,
        content: impl Into<String>,
        source: impl Into<String>,
        confidence: f32,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: format!(
                "mem-{}-{}",
                now.format("%Y%m%dT%H%M%S%.6f"),
                MEM_SEQ.fetch_add(1, Ordering::Relaxed)
            ),
            layer,
            content: content.into(),
            source: source.into(),
            confidence,
            tags: Vec::new(),
            created_at: now,
            expires_at: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Memory {
    root: PathBuf,
}

impl Memory {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn layer_path(&self, layer: MemoryLayer) -> PathBuf {
        self.root.join(format!("{}.jsonl", layer_name(layer)))
    }

    pub async fn write(&self, mut record: MemoryRecord) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.root).await?;
        // Redact BEFORE serialising so the on-disk JSONL never sees raw secrets.
        record.content = redact(&record.content);
        let path = self.layer_path(record.layer);
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        // POSIX O_APPEND writes are atomic up to PIPE_BUF (4096 bytes), which
        // covers the typical MemoryRecord. Two concurrent gateway writers no
        // longer race-clobber the file the way the previous read-modify-write
        // path did. TODO: extremely large reflection records (>PIPE_BUF) may
        // still interleave; acceptable for now until we add per-layer locks.
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.flush().await
    }

    pub async fn read_all(&self, layer: MemoryLayer) -> std::io::Result<Vec<MemoryRecord>> {
        let path = self.layer_path(layer);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(rec) = serde_json::from_str::<MemoryRecord>(line) {
                out.push(rec);
            }
        }
        Ok(out)
    }

    pub async fn search(
        &self,
        query: &str,
        layers: &[MemoryLayer],
        limit: usize,
    ) -> std::io::Result<Vec<MemoryRecord>> {
        let mut hits = Vec::new();
        let q = query.to_lowercase();
        for layer in layers {
            for rec in self.read_all(*layer).await? {
                if rec.content.to_lowercase().contains(&q)
                    || rec.tags.iter().any(|t| t.to_lowercase().contains(&q))
                {
                    hits.push(rec);
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
            if hits.len() >= limit {
                break;
            }
        }
        hits.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(hits)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn layer_name(l: MemoryLayer) -> &'static str {
    match l {
        MemoryLayer::L0 => "L0",
        MemoryLayer::L1 => "L1",
        MemoryLayer::L2 => "L2",
        MemoryLayer::L3 => "L3",
        MemoryLayer::L4 => "L4",
        MemoryLayer::L5 => "L5",
    }
}

/// PROMPTS §9: redact secrets/cookies/PII before persistence. String-based.
pub fn redact(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        out.push_str(&redact_line(line));
    }
    out
}

fn redact_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    for marker in ["set-cookie:", "sessionid=", "csrftoken=", "authorization:"] {
        if lower.contains(marker) {
            return "[REDACTED:cookie/auth]\n".into();
        }
    }
    if lower.contains("bearer ") {
        return redact_token_segments(line, "bearer ");
    }
    redact_key_prefixes(line)
}

/// Find every occurrence of an API-key-like prefix and replace from the prefix
/// through the next whitespace/quote/comma boundary.
fn redact_key_prefixes(line: &str) -> String {
    const PREFIXES: &[(&str, usize)] =
        &[("sk-", 6), ("ghp_", 6), ("sk_live_", 10), ("sk_test_", 10)];
    let mut out = String::with_capacity(line.len());
    let mut idx = 0;
    while idx < line.len() {
        // Find the earliest prefix occurrence at or after idx.
        let mut earliest: Option<(usize, &str, usize)> = None;
        for (prefix, min_len) in PREFIXES {
            if let Some(pos_rel) = line[idx..].find(prefix) {
                let abs = idx + pos_rel;
                if earliest.map(|(p, _, _)| abs < p).unwrap_or(true) {
                    earliest = Some((abs, prefix, *min_len));
                }
            }
        }
        let Some((abs, prefix, min_len)) = earliest else {
            out.push_str(&line[idx..]);
            break;
        };
        // Tail of the prefix until whitespace / quote / comma / newline.
        let after = &line[abs + prefix.len()..];
        let tail_end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
            .unwrap_or(after.len());
        let total_len = prefix.len() + tail_end;
        if total_len >= min_len {
            out.push_str(&line[idx..abs]);
            out.push_str("[REDACTED:key]");
            idx = abs + total_len;
        } else {
            // Too short to be a credential — keep verbatim, advance past the prefix.
            out.push_str(&line[idx..abs + prefix.len()]);
            idx = abs + prefix.len();
        }
    }
    out
}

fn redact_token_segments(line: &str, marker_lower: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut idx = 0;
    let lower = line.to_ascii_lowercase();
    while let Some(pos) = lower[idx..].find(marker_lower) {
        let abs = idx + pos;
        out.push_str(&line[idx..abs + marker_lower.len()]);
        let after = &line[abs + marker_lower.len()..];
        let token_end = after
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after.len());
        out.push_str("[REDACTED:bearer]");
        idx = abs + marker_lower.len() + token_end;
    }
    out.push_str(&line[idx..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_sk_key() {
        assert!(redact("key=sk-abcdef12345 in env").contains("[REDACTED:key]"));
    }
    #[test]
    fn redact_ghp_token() {
        assert!(redact("token ghp_xxxxxxxxxxxxxxxxx").contains("[REDACTED:key]"));
    }
    #[test]
    fn redact_bearer() {
        let out = redact("Authorization: Bearer abc123\n");
        assert!(out.contains("[REDACTED"));
    }
    #[test]
    fn redact_set_cookie() {
        let out = redact("Set-Cookie: sessionid=12345; HttpOnly\n");
        assert!(out.contains("[REDACTED:cookie/auth]"));
    }
    #[test]
    fn redact_keeps_innocent_text() {
        assert_eq!(redact("hello world"), "hello world");
    }

    fn unique_root(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        p.push(format!(
            "evo-mem-{name}-{}-{stamp}-{seq}",
            std::process::id()
        ));
        p
    }

    #[tokio::test]
    async fn write_then_search_finds_record() {
        let m = Memory::at(unique_root("search"));
        let mut r = MemoryRecord::new(MemoryLayer::L3, "user prefers fish shell", "doctor", 0.9);
        r.tags = vec!["shell".into(), "preference".into()];
        m.write(r).await.unwrap();
        let hits = m.search("fish", &[MemoryLayer::L3], 10).await.unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn search_redacts_at_write() {
        let m = Memory::at(unique_root("redact"));
        let r = MemoryRecord::new(MemoryLayer::L3, "key=sk-abc999 found", "task", 0.7);
        m.write(r).await.unwrap();
        let hits = m.search("REDACTED", &[MemoryLayer::L3], 10).await.unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn search_empty_when_no_match() {
        let m = Memory::at(unique_root("none"));
        m.write(MemoryRecord::new(MemoryLayer::L3, "alpha", "task", 0.5))
            .await
            .unwrap();
        let hits = m.search("zeta", &[MemoryLayer::L3], 10).await.unwrap();
        assert!(hits.is_empty());
    }
}
