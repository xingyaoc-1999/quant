use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::{MacdCross, MacdMomentum};
use common::Interval;
use serde_json::json;
use std::borrow::Cow;

pub struct ResonanceAnalyzer {
    default_aging_threshold: i32,
}

impl ResonanceAnalyzer {
    pub fn new() -> Self {
        Self {
            default_aging_threshold: 36,
        }
    }
}

impl Analyzer for ResonanceAnalyzer {
    fn name(&self) -> &'static str {
        "momentum_resonance"
    }
    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Signal
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Momentum
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let trend_data = ctx.get_role(Role::Trend);
        let feat = &trend_data.feature_set;

        // --- 1. 核心信号判定 (Trigger Identification) ---
        let is_reclaim = feat.signals.ma20_reclaim.unwrap_or(false);
        let is_breakdown = feat.signals.ma20_breakdown.unwrap_or(false);
        let macd_cross = feat.signals.macd_cross;

        // 确定基础方向
        let direction = if is_reclaim || macd_cross == Some(MacdCross::Golden) {
            1.0 // Long
        } else if is_breakdown || macd_cross == Some(MacdCross::Death) {
            -1.0 // Short
        } else {
            // 无基础触发信号，直接返回
            return Ok(AnalysisResult::default());
        };

        let mut description: Vec<Cow<'static, str>> = Vec::new();
        let mut m_resonance: f64 = 1.0; // 核心：共振质量乘数

        // --- 2. 基础得分分配 (Base Signal Strength) ---
        // 价格收复均线 (Reclaim) 的权重通常高于单纯的 MACD 交叉
        let base_score = if is_reclaim || is_breakdown {
            description.push(Cow::Borrowed("TRIGGER:MA20_STRUCT_CHANGE"));
            45.0
        } else {
            description.push(Cow::Borrowed("TRIGGER:MACD_CROSS"));
            30.0
        };

        // --- 3. 趋势阶段评估 (Aging & Early Bird Logic) ---
        let slope_bars = feat.structure.ma20_slope_bars.abs();
        let aging_threshold = match trend_data.interval {
            Interval::M15 => 24,
            Interval::H1 => 48,
            _ => self.default_aging_threshold,
        };

        if slope_bars < 12 {
            // 趋势初期：高爆发潜力
            m_resonance *= 1.3;
            description.push(Cow::Borrowed("QUALITY:EARLY_TREND_PREMIUM"));
        } else if slope_bars > aging_threshold {
            // 趋势老化：胜率衰减
            let penalty = 1.0 - ((slope_bars - aging_threshold) as f64 / 30.0).min(0.7);
            m_resonance *= penalty;
            description.push(Cow::Owned(format!(
                "QUALITY:AGING_TREND({:.0}%)",
                penalty * 100.0
            )));
        }

        // --- 4. 动能一致性确认 (Momentum Confirmation) ---
        if let Some(macd_mom) = feat.signals.macd_momentum {
            let mom_confirmed = (direction > 0.0 && macd_mom == MacdMomentum::Increasing)
                || (direction < 0.0 && macd_mom == MacdMomentum::Decreasing);

            if mom_confirmed {
                m_resonance *= 1.25;
                description.push(Cow::Borrowed("CONFIRM:MOMENTUM_ALIGNED"));
            } else {
                m_resonance *= 0.8;
                description.push(Cow::Borrowed("WARNING:MOMENTUM_DIVERGENT"));
            }
        }

        // --- 5. 市场 Beta 相关性过滤 ---
        if let Some(correlation) = feat.structure.correlation_with_global {
            if correlation > 0.90 {
                // 相关性过高说明只是随大盘波动，独立动能不足
                m_resonance *= 0.75;
                description.push(Cow::Borrowed("BETA:HIGH_MARKET_CORR"));
            }
        }

        // --- 6. 持久化核心 Action 和 维度乘数 ---
        let action_str = if direction > 0.0 { "BUY" } else { "SELL" };

        // 关键：写入全局 Action，供后续 DivergenceAnalyzer 等读取
        shared
            .data
            .insert("signal:action".into(), json!(action_str));

        // 关键：写入维度乘数 (使用 resonance_{action} 细分)
        shared.data.insert(
            format!("multiplier:signal:resonance_{}", action_str.to_lowercase()),
            json!(m_resonance),
        );

        // --- 7. 返回结果 ---
        Ok(AnalysisResult {
            // Score 代表信号原始冲力
            score: base_score * direction,
            // Multiplier 代表该信号的置信度/质量
            weight_multiplier: m_resonance,
            description: description.join(" | "),
            debug_data: json!({
                "direction": action_str,
                "m_resonance": m_resonance,
                "slope_bars": slope_bars,
                "base_score": base_score
            }),
            ..Default::default()
        })
    }
}
