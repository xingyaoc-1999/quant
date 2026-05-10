use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::{
    analyzer::{AnalyzerKind, ErasedAnalysisResult},
    report::MarketSnapshot,
};

#[derive(Serialize)]
pub struct AuditRecord {
    pub timestamp: i64,
    pub event: AuditEvent,
    pub symbol: String,
    pub signal: Option<SignalSummary>,
    pub market_snapshot: Option<MarketSnapshot>,
    pub analysis: Vec<AnalyzerDetail>,
    pub reject_reason: Option<String>,
}

#[derive(Serialize)]
pub enum AuditEvent {
    Signal,
    Reject,
    Update,
}

#[derive(Serialize)]
pub struct SignalSummary {
    pub direction: String,
    pub entry_price: Option<f64>,
    pub stop_loss: Vec<f64>,
    pub take_profit: Vec<f64>,
    pub weighted_rr: f64,
    pub confidence: f64,
    pub tags: Vec<String>,
}

#[derive(Serialize)]
pub struct AnalyzerDetail {
    pub analyzer: AnalyzerKind,
    pub score: f64,
    pub desc: String,
}

pub async fn write_audit_log(record: &AuditRecord) {
    let line = serde_json::to_string(record).unwrap_or_default();
    if let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("audit.jsonl")
        .await
    {
        let _ = file.write_all(line.as_bytes()).await;
        let _ = file.write_all(b"\n").await;
    }
}

pub fn build_analysis_details(sub_reports: &[ErasedAnalysisResult]) -> Vec<AnalyzerDetail> {
    sub_reports
        .iter()
        .map(|r| AnalyzerDetail {
            analyzer: r.kind,
            score: r.score,
            desc: r.description.clone(),
        })
        .collect()
}
