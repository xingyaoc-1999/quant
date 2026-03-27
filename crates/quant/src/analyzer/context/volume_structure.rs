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
        // 归类为成交量分布/画像类
        AnalyzerKind::VolumeProfile
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        // 获取大周期/趋势周期的特征数据
        let trend_data = ctx.get_role(Role::Trend);
        let feat = &trend_data.feature_set;

        let mut env_multiplier = 1.0;
        let mut score = 0.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        if let Some(vol_state) = &feat.structure.volume_state {
            match vol_state {
                VolumeState::Expand => {
                    // 放量：代表真金白银的共识，显著提升后续信号可信度
                    env_multiplier *= 1.25;
                    score = 20.0;
                    description.push(Cow::Borrowed("VOL_EXPAND:CONVICTION"));
                }
                VolumeState::Shrink => {
                    // 缩量：市场兴趣匮乏，信号往往是诱多/诱空
                    env_multiplier *= 0.7;
                    score = -10.0;
                    description.push(Cow::Borrowed("VOL_SHRINK:LACK_OF_INTEREST"));
                }
                VolumeState::Squeeze => {
                    // 极端挤压：变盘信号，但当前缺乏方向感，小幅降权
                    env_multiplier *= 0.85;
                    score = 0.0;
                    description.push(Cow::Borrowed("VOL_SQUEEZE:POTENTIAL_BREAKOUT_SOON"));
                }
                VolumeState::Normal => {
                    description.push(Cow::Borrowed("VOL_NORMAL"));
                }
            }
        }

        // --- 2. 持续性分析：连续缩量检测 ---
        // 如果最近 3 根 K 线连续缩量，说明动能严重衰竭
        if feat.signals.volume_shrink_3.unwrap_or(false) {
            env_multiplier *= 0.8;
            description.push(Cow::Borrowed("CUMULATIVE_VOL_EXHAUSTION"));
        }

        // --- 3. 风险预警：巨量高潮 (Climax Volume) ---
        // 极端大柱子配合放量通常是趋势末端的标志
        if feat.signals.extreme_candle.unwrap_or(false)
            && matches!(feat.structure.volume_state, Some(VolumeState::Expand))
        {
            env_multiplier *= 0.9;
            description.push(Cow::Borrowed("CLIMAX_VOLUME:REVERSAL_RISK"));
        }

        // --- 4. 状态持久化：存入 Shared 状态供 Signal 阶段使用 ---
        // 我们将这个乘数存入 context:volume_multiplier
        shared
            .data
            .insert("context:volume_multiplier".into(), json!(env_multiplier));

      
        Ok(AnalysisResult {
            score, // 环境本身的得分
            description: description.join(" | "),
            debug_data: json!({
                "applied_multiplier": env_multiplier,
                "vol_state": feat.structure.volume_state,
                "vol_shrink_3": feat.signals.volume_shrink_3,
                "is_climax": feat.signals.extreme_candle,
            }),
            ..Default::default()
        })
    }
}
