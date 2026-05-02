//! Cost Engine — 3-tier budget. PRD §35 + §42.6 + PROMPTS §10.3.

use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetLevel {
    Task,
    Day,
    Month,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetCheck {
    Ok,
    SoftWarn(BudgetLevel),
    HardStop(BudgetLevel),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetCfg {
    pub per_task_usd: f64,
    pub per_day_soft_usd: f64,
    pub per_day_hard_usd: f64,
    pub per_month_usd: f64,
}

impl Default for BudgetCfg {
    fn default() -> Self {
        Self {
            per_task_usd: 0.50,
            per_day_soft_usd: 5.0,
            per_day_hard_usd: 20.0,
            per_month_usd: 100.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEvent {
    pub ts: DateTime<Utc>,
    pub task_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub output_tokens: u64,
    pub usd: f64,
}

#[derive(Debug, Clone, Default)]
pub struct CostSummary {
    pub task_usd: f64,
    pub day_usd: f64,
    pub month_usd: f64,
    pub events: u64,
    pub avg_cache_hit_rate: f64,
}

pub struct CostEngine {
    log_path: PathBuf,
    cfg: BudgetCfg,
}

impl CostEngine {
    pub fn at(log_path: impl Into<PathBuf>, cfg: BudgetCfg) -> Self {
        Self {
            log_path: log_path.into(),
            cfg,
        }
    }

    pub fn cfg(&self) -> &BudgetCfg {
        &self.cfg
    }
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub async fn record(&self, event: &CostEvent) -> std::io::Result<()> {
        if let Some(parent) = self.log_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut existing = tokio::fs::read_to_string(&self.log_path)
            .await
            .unwrap_or_default();
        existing.push_str(&serde_json::to_string(event)?);
        existing.push('\n');
        tokio::fs::write(&self.log_path, existing).await
    }

    pub async fn check_for_task(&self, task_id: &str) -> std::io::Result<BudgetCheck> {
        let s = self.summary_for_task(task_id).await?;
        if s.month_usd > self.cfg.per_month_usd {
            return Ok(BudgetCheck::HardStop(BudgetLevel::Month));
        }
        if s.day_usd > self.cfg.per_day_hard_usd {
            return Ok(BudgetCheck::HardStop(BudgetLevel::Day));
        }
        if s.task_usd > self.cfg.per_task_usd {
            return Ok(BudgetCheck::HardStop(BudgetLevel::Task));
        }
        if s.day_usd > self.cfg.per_day_soft_usd {
            return Ok(BudgetCheck::SoftWarn(BudgetLevel::Day));
        }
        Ok(BudgetCheck::Ok)
    }

    pub async fn summary_for_task(&self, task_id: &str) -> std::io::Result<CostSummary> {
        let now = Utc::now();
        let day_start = now - Duration::days(1);
        let month_start = first_of_month(now);
        let mut s = CostSummary::default();
        let mut total_input = 0u64;
        let mut total_cached = 0u64;
        for ev in self.read_events().await? {
            if ev.task_id == task_id {
                s.task_usd += ev.usd;
            }
            if ev.ts >= day_start {
                s.day_usd += ev.usd;
            }
            if ev.ts >= month_start {
                s.month_usd += ev.usd;
            }
            s.events += 1;
            total_input += ev.input_tokens;
            total_cached += ev.cached_tokens;
        }
        s.avg_cache_hit_rate = if total_input == 0 {
            0.0
        } else {
            total_cached as f64 / total_input as f64
        };
        Ok(s)
    }

    pub async fn read_events(&self) -> std::io::Result<Vec<CostEvent>> {
        let text = match tokio::fs::read_to_string(&self.log_path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<CostEvent>(line) {
                out.push(ev);
            }
        }
        Ok(out)
    }
}

fn first_of_month(now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// Estimate USD cost from token counts when provider does not return USD.
/// $1 / 1M input, $3 / 1M output, cached billed at 1/10 input rate.
pub fn estimate_usd(input_tokens: u64, cached_tokens: u64, output_tokens: u64) -> f64 {
    let billable_input = input_tokens.saturating_sub(cached_tokens);
    let in_usd = billable_input as f64 * 1.0 / 1_000_000.0;
    let cached_usd = cached_tokens as f64 * 0.1 / 1_000_000.0;
    let out_usd = output_tokens as f64 * 3.0 / 1_000_000.0;
    in_usd + cached_usd + out_usd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_log() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        p.push(format!("evo-cost-{pid}-{stamp}-{seq}.jsonl"));
        p
    }

    fn ev(usd: f64, task: &str) -> CostEvent {
        CostEvent {
            ts: Utc::now(),
            task_id: task.into(),
            model: "m".into(),
            input_tokens: 1000,
            cached_tokens: 600,
            output_tokens: 50,
            usd,
        }
    }

    #[tokio::test]
    async fn ok_when_under_budget() {
        let e = CostEngine::at(unique_log(), BudgetCfg::default());
        e.record(&ev(0.10, "t1")).await.unwrap();
        assert_eq!(e.check_for_task("t1").await.unwrap(), BudgetCheck::Ok);
    }

    #[tokio::test]
    async fn task_hard_stop() {
        let e = CostEngine::at(unique_log(), BudgetCfg::default());
        e.record(&ev(0.60, "t1")).await.unwrap();
        let c = e.check_for_task("t1").await.unwrap();
        assert!(matches!(c, BudgetCheck::HardStop(BudgetLevel::Task)));
    }

    #[tokio::test]
    async fn day_soft_warn() {
        let e = CostEngine::at(unique_log(), BudgetCfg::default());
        for i in 0..12 {
            e.record(&ev(0.49, &format!("t{i}"))).await.unwrap();
        }
        let c = e.check_for_task("t0").await.unwrap();
        assert!(matches!(c, BudgetCheck::SoftWarn(BudgetLevel::Day)));
    }

    #[test]
    fn estimate_default_rates() {
        let usd = estimate_usd(1_000_000, 0, 1_000_000);
        assert!((usd - 4.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_cached_discount() {
        let usd = estimate_usd(1_000_000, 1_000_000, 0);
        assert!((usd - 0.1).abs() < 1e-9);
    }
}
