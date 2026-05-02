//! JSONL session persistence: PRD §16 + §17.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionRecord {
    Task(TaskRecord),
    Turn(TurnRecord),
    End(EndRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub user_input: String,
    pub source: String,
    pub model: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub turn: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<RecordedToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<RecordedUsage>,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub result_truncated: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedUsage {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndRecord {
    pub state: String,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Session {
    path: PathBuf,
    file: File,
}

impl Session {
    pub async fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).await?;
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path { &self.path }

    pub async fn append(&mut self, record: &SessionRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.file.write_all(line.as_bytes()).await?;
        self.file.flush().await
    }

    pub async fn read_all(path: impl AsRef<Path>) -> std::io::Result<Vec<SessionRecord>> {
        let f = File::open(path).await?;
        let mut reader = BufReader::new(f).lines();
        let mut out = Vec::new();
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() { continue; }
            let r: SessionRecord = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            out.push(r);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn unique_log() -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        p.push(format!("evo-session-{stamp}.jsonl"));
        p
    }

    fn sample_task() -> SessionRecord {
        SessionRecord::Task(TaskRecord {
            task_id: "task-test-1".into(),
            user_input: "list files".into(),
            source: "cli".into(),
            model: "deepseek-chat".into(),
            started_at: "2026-05-02T11:39:00Z".parse().unwrap(),
        })
    }

    fn sample_turn() -> SessionRecord {
        SessionRecord::Turn(TurnRecord {
            turn: 1,
            summary: Some("listed 3 files".into()),
            tool_calls: vec![RecordedToolCall {
                name: "run_shell".into(),
                args: serde_json::json!({"cmd": "ls"}),
                result_truncated: "exit=0\n…".into(),
                is_error: false,
            }],
            usage: Some(RecordedUsage { input: 600, cached: 400, output: 50 }),
            ts: "2026-05-02T11:39:05Z".parse().unwrap(),
        })
    }

    #[tokio::test]
    async fn round_trip_task_and_turn() {
        let path = unique_log();
        let mut s = Session::open(&path).await.unwrap();
        s.append(&sample_task()).await.unwrap();
        s.append(&sample_turn()).await.unwrap();
        s.append(&SessionRecord::End(EndRecord {
            state: "COMPLETED".into(),
            finished_at: "2026-05-02T11:39:10Z".parse().unwrap(),
        })).await.unwrap();
        drop(s);
        let records = Session::read_all(&path).await.unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0], SessionRecord::Task(_)));
        assert!(matches!(records[1], SessionRecord::Turn(_)));
        assert!(matches!(records[2], SessionRecord::End(_)));
    }

    #[tokio::test]
    async fn jsonl_lines_are_one_per_record() {
        let path = unique_log();
        let mut s = Session::open(&path).await.unwrap();
        s.append(&sample_task()).await.unwrap();
        s.append(&sample_turn()).await.unwrap();
        drop(s);
        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(raw.lines().count(), 2);
    }
}
