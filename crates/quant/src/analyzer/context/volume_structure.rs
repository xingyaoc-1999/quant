use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::VolumeState;
use serde_json::json;
use std::borrow::Cow;

pub struct VolumeStructureAnalyzer;

impl Analyzer for VolumeStructureAnalyzer {
    fn name(&self) -> &'static str {
        "volume_structure"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::VolumeProfile
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let trend_data = ctx.get_role(Role::Trend);
        let feat = &trend_data.feature_set;

        let mut m_vol: f64 = 1.0;
        let mut score = 0.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        // --- 1. 获取波动率上下文 ---
        let is_compressed = shared
            .data
            .get("ctx:volatility:is_compressed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // --- 2. 基础成交量逻辑 & 解锁机制 ---
        if let Some(vol_state) = &feat.structure.volume_state {
            match vol_state {
                VolumeState::Expand => {
                    m_vol = 1.3;
                    score = 15.0;
                    description.push(Cow::Borrowed("VOL:EXPANSION_CONFIRMED"));

                    // 【核心解锁】如果是放量，说明正在尝试突破压缩区，顶开波动率设置的天花板
                    if is_compressed {
                        shared
                            .data
                            .insert("ctx:volatility:max_env_multiplier".into(), json!(1.5));
                        description.push(Cow::Borrowed("VOL:BREAKOUT_UNLOCK"));
                    }
                }
                VolumeState::Shrink => {
                    m_vol = 0.75;
                    score = -5.0;
                    description.push(Cow::Borrowed("VOL:SHRINKING_INTEREST"));
                }
                VolumeState::Squeeze => {
                    // 如果处于波动率压缩+成交量挤压，这是极高潜力的蓄势
                    m_vol = if is_compressed { 1.15 } else { 0.85 };
                    description.push(Cow::Borrowed("VOL:SQUEEZE_ACCUMULATING"));
                }
                _ => {}
            }
        }

        // --- 3. 异常与风险校验 ---

        // A. 高潮爆量反转 (Climax) - 此时通常是流动性最后的喷发，不是好的入场点
        if feat.signals.extreme_candle.unwrap_or(false) {
            m_vol *= 0.65; // 惩罚性降权
            description.push(Cow::Borrowed("VOL:CLIMAX_EXHAUSTION_RISK"));
        }

        // B. 蓄势爆发准备 (结合 RSI 窄幅震荡)
        let rsi_ready = feat.signals.rsi_range_3.unwrap_or(false);
        let vol_shrink_3 = feat.signals.volume_shrink_3.unwrap_or(false);

        if rsi_ready && vol_shrink_3 && is_compressed {
            // 这是一个“火药桶”模式：低波、缩量、RSI走平
            m_vol *= 1.4;
            description.push(Cow::Borrowed("VOL:EXPLOSIVE_SETUP"));
        }

        // --- 4. 结构化持久化 ---
        // 使用符合 Aggregator 自动提取的键名
        shared
            .data
            .insert("multiplier:volume:structure".into(), json!(m_vol));

        // 记录成交量强度供其他 Analyzer 参考
        shared.data.insert(
            "ctx:volume:is_strong".into(),
            json!(matches!(
                feat.structure.volume_state,
                Some(VolumeState::Expand)
            )),
        );

        Ok(AnalysisResult {
            score,
            weight_multiplier: m_vol,
            description: description.join(" | "),
            debug_data: json!({
                "vol_m": m_vol,
                "is_compressed": is_compressed,
                "vol_state": format!("{:?}", feat.structure.volume_state)
            }),
            ..Default::default()
        })
    }
}
