use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    SharedAnalysisState,
};
use chrono::{Datelike, Timelike};
use serde_json::json;
use std::borrow::Cow;

/// 时间窗口分析器 (Context 阶段)
/// 职责：根据交易时段、工作日、开收线波动率定义环境乘数
pub struct TemporalSessionAnalyzer;

impl TemporalSessionAnalyzer {
    /// 核心逻辑：根据 UTC 小时判断当前市场活跃度等级
    fn evaluate_session(&self, hour: u32) -> (f64, &'static str) {
        match hour {
            // 13:00 - 16:00 UTC: 欧美重叠黄金时段 (流动性最强)
            13..=16 => (1.25, "GOLDEN_OVERLAP"),
            // 08:00 - 12:00 UTC: 伦敦交易时段
            8..=12 => (1.1, "LONDON_SESSION"),
            // 17:00 - 21:00 UTC: 纽约后半场
            17..=21 => (1.0, "NEWYORK_LATE"),
            // 00:00 - 07:00 UTC: 亚盘时段 (波动通常较小)
            0..=7 => (0.8, "ASIA_SESSION"),
            // 22:00 - 23:59 UTC: 换线/结算死区 (点差大，滑点高)
            22..=23 => (0.6, "DAILY_SETTLEMENT"),
            _ => (1.0, "UNKNOWN_SESSION"),
        }
    }
}

impl Analyzer for TemporalSessionAnalyzer {
    fn name(&self) -> &'static str {
        "temporal_session"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }

    fn kind(&self) -> AnalyzerKind {
        // 时间窗口决定了预期的波动率环境
        AnalyzerKind::Volatility
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        // 1. 获取基础时间信息
        let datetime = ctx.timestamp;
        let hour = datetime.hour();
        let minute = datetime.minute();
        let weekday = datetime.weekday();

        let mut multiplier = 1.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        // --- 2. 交易时段评估 ---
        let (session_mult, session_name) = self.evaluate_session(hour);
        multiplier *= session_mult;
        description.push(Cow::Owned(format!("SESSION:{}", session_name)));

        // --- 3. 风险日过滤 (周末/周一逻辑) ---
        if weekday == chrono::Weekday::Sat {
            multiplier *= 0.5; // 周六流动性极差，显著降权
            description.push(Cow::Borrowed("WEEKEND_RISK:SAT"));
        } else if weekday == chrono::Weekday::Sun {
            if hour < 22 {
                multiplier *= 0.7;
                description.push(Cow::Borrowed("WEEKEND_RISK:SUN_EARLY"));
            } else {
                // 周日深夜（周一开盘前），波动开始放大
                multiplier *= 1.1;
                description.push(Cow::Borrowed("PRE_MONDAY_PUMP"));
            }
        }

        // --- 4. 极端时间微观审计 ---
        // 针对美股开盘瞬间 (UTC 14:30) 的剧烈扫盘
        if hour == 14 && (30..=35).contains(&minute) {
            multiplier *= 0.7;
            description.push(Cow::Borrowed("NYSE_OPEN_VOLATILITY"));
        }

        // 针对整点收线的前 2 分钟 (防止收线产生的假突破)
        if minute < 2 {
            multiplier *= 0.85;
            description.push(Cow::Borrowed("CANDLE_OPEN_NOISE"));
        }

        // --- 5. 广播环境因子 (核心修改) ---
        // 存入 shared，让后续的 Signal 分析器（如动量）能读到这个“时间权重”
        shared
            .data
            .insert("context:temporal_multiplier".into(), json!(multiplier));

        Ok(AnalysisResult {
            score: 0.0, // 时间本身不直接决定买卖（得分）
            is_violation: false,
            weight_multiplier: 1.0, // 自身权重保持 1.0，防止在 Aggregator 被重复连乘
            description: description.join(" | "),
            debug_data: json!({
                "utc_hour": hour,
                "utc_minute": minute,
                "weekday": weekday.to_string(),
                "final_temporal_multiplier": multiplier
            }),
            ..Default::default()
        })
    }
}
