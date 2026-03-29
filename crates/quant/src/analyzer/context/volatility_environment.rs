use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use serde_json::json;
use std::borrow::Cow;

pub struct VolatilityEnvironmentAnalyzer;

impl Analyzer for VolatilityEnvironmentAnalyzer {
    fn name(&self) -> &'static str {
        "volatility_env"
    }
    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Volatility
    }
    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let feat = &ctx.get_role(Role::Trend).feature_set;

        // --- 1. 获取行情模式 ---
        let regime = shared
            .data
            .get("ctx:regime:structure")
            .map(|s| s.as_str().unwrap_or("Range").to_string())
            .unwrap_or_else(|| "Range".to_string());

        let vol_p = feat.price_action.volatility_percentile;
        let m_vol_env: f64;
        let mut m_vol_squeeze: f64 = 1.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        // --- 2. 流动性枯竭熔断检测 (Optimization B) ---
        // 如果波动率极低，市场处于僵尸状态，通常伴随流动性风险
        let is_frozen = vol_p < 0.08;
        if is_frozen {
            shared
                .data
                .insert("ctx:volatility:is_frozen".into(), json!(true));
            // 存入一个全局性的一票否决因子
            shared
                .data
                .insert("multiplier:volatility:circuit_breaker".into(), json!(0.1));

            return Ok(AnalysisResult {
                weight_multiplier: 0.1,
                description: "VOL:MARKET_FROZEN_LIQUIDITY_RISK".into(),
                ..Default::default()
            });
        }

        // --- 3. 基于 Regime 的非线性逻辑 ---
        match regime.as_str() {
            // A. 强趋势模式
            r if r.contains("Strong") => {
                if vol_p > 0.90 {
                    m_vol_env = 1.1; // 趋势加速
                    description.push(Cow::Borrowed("VOL:TREND_ACCELERATION"));
                } else if vol_p < 0.20 {
                    m_vol_env = 0.7; // 动能缺失
                    description.push(Cow::Borrowed("VOL:TREND_LOSING_MOMENTUM"));
                } else {
                    m_vol_env = 1.25;
                    description.push(Cow::Borrowed("VOL:TREND_OPTIMAL"));
                }
            }

            // B. 震荡模式
            "Range" => {
                if vol_p > 0.85 {
                    m_vol_env = 0.4; // 假突破高发区
                    description.push(Cow::Borrowed("VOL:RANGE_CHAOS"));
                } else if vol_p < 0.20 {
                    // 蓄势逻辑 (Optimization A)
                    m_vol_env = 0.7; // 当前无动能，降低基础分
                    m_vol_squeeze = 1.15; // 提高蓄势潜力分

                    shared
                        .data
                        .insert("ctx:volatility:is_compressed".into(), json!(true));

                    // 核心修改：设置一个“天花板”，防止后续 Volume 分析器由于 Double Counting 把分带飞
                    // 意味着即便蓄势再好，在没突破前，总权重上限锁定在 0.9
                    shared
                        .data
                        .insert("ctx:volatility:max_env_multiplier".into(), json!(0.9));

                    description.push(Cow::Borrowed("VOL:RANGE_PHYSICAL_SQUEEZE"));
                } else {
                    m_vol_env = 1.1;
                    description.push(Cow::Borrowed("VOL:RANGE_NORMAL"));
                }
            }

            // C. 普通趋势
            _ => {
                if vol_p > 0.92 {
                    m_vol_env = 0.6; // 强弩之末
                    description.push(Cow::Borrowed("VOL:EXTREME_EXPANSION"));
                } else if vol_p < 0.15 {
                    m_vol_env = 0.5; // 冷清
                    description.push(Cow::Borrowed("VOL:INACTIVE_MARKET"));
                } else {
                    m_vol_env = 1.0;
                    description.push(Cow::Borrowed("VOL:NORMAL"));
                }
            }
        }

        // --- 4. 结构化持久化 ---
        shared
            .data
            .insert("multiplier:volatility:env".into(), json!(m_vol_env));
        shared.data.insert(
            "multiplier:volatility:squeeze_logic".into(),
            json!(m_vol_squeeze),
        );
        shared
            .data
            .insert("ctx:volatility:percentile".into(), json!(vol_p));

        Ok(AnalysisResult {
            weight_multiplier: m_vol_env * m_vol_squeeze,
            description: description.join(" | "),
            debug_data: json!({
                "vol_p": vol_p,
                "regime": regime,
                "m_vol_env": m_vol_env,
                "m_vol_squeeze": m_vol_squeeze,
                "is_compressed": vol_p < 0.20
            }),
            ..Default::default()
        })
    }
}
