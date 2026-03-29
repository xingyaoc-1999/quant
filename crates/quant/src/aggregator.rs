use std::collections::HashMap;

use dashmap::DashMap;
use serde_json::Value;

use crate::analyzer::{AnalysisResult, SharedAnalysisState};

pub struct SignalAggregator;

impl SignalAggregator {
    pub fn aggregate(&self, results: &[AnalysisResult], shared: &SharedAnalysisState) {
        let total_raw_score: f64 = results.iter().map(|r| r.score).sum();

        let multipliers = self.extract_multipliers(&shared.data);

        let is_long = total_raw_score >= 0.0;

        let mut m_env = multipliers.get("regime:base").unwrap_or(&1.0)
            * multipliers.get("regime:momentum").unwrap_or(&1.0)
            * multipliers.get("regime:oi").unwrap_or(&1.0)
            * multipliers.get("volatility:env").unwrap_or(&1.0)
            * multipliers.get("volatility:squeeze_logic").unwrap_or(&1.0)
            * multipliers.get("volume:structure").unwrap_or(&1.0);
        if let Some(max_limit) = shared
            .data
            .get("ctx:volatility:max_env_multiplier")
            .and_then(|v| v.as_f64())
        {
            m_env = m_env.min(max_limit);
        }

        let m_space = if is_long {
            multipliers.get("space:proximity_long").unwrap_or(&1.0)
                * multipliers.get("space:deviation_long").unwrap_or(&1.0)
        } else {
            multipliers.get("space:proximity_short").unwrap_or(&1.0)
                * multipliers.get("space:deviation_short").unwrap_or(&1.0)
        };

        let total_multiplier = m_env * m_space;

        let final_multiplier = if multipliers.contains_key("volatility:circuit_breaker") {
            0.1 // 极低波动强制熄火
        } else {
            total_multiplier
        };

        let normalized_base = (total_raw_score / 60.0).tanh();
        let final_score = normalized_base * 100.0 * final_multiplier;
    }

    fn extract_multipliers(&self, data: &DashMap<String, Value>) -> HashMap<String, f64> {
        data.iter()
            .filter(|r| r.key().starts_with("multiplier:"))
            .map(|r| {
                // DashMap 的迭代器成员通过 key() 和 value() 访问
                let clean_key = r.key().trim_start_matches("multiplier:");
                let val = r.value().as_f64().unwrap_or(1.0);
                (clean_key.to_string(), val)
            })
            .collect()
    }
}
