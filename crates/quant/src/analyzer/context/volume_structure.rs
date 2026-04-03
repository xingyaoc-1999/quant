use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, ContextKey,
    MarketContext, Role, SharedAnalysisState,
};
use crate::types::VolumeState;
use serde_json::json;

pub struct VolumeStructureAnalyzer;

impl Analyzer for VolumeStructureAnalyzer {
    fn name(&self) -> &'static str {
        "volume_structure"
    }

    fn stage(&self) -> AnalyzerStage {
        // 依然属于 Context 阶段，为 Signal 提供权重
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
        // 1. 获取币种作用域与特征
        let scope = shared.scope(&ctx.symbol);
        let trend_role = ctx.get_role(Role::Trend)?;
        let feat = &trend_role.feature_set;

        // --- A. 从上下文获取波动率状态 ---
        let is_vol_compressed = scope.get_bool(ContextKey::VolIsCompressed);

        let mut m_vol: f64 = 1.0;
        let mut res = AnalysisResult::new(self.kind(), "VOL_STRUCT".into());

        // --- B. 核心：成交量状态逻辑 ---
        if let Some(vol_state) = &feat.structure.volume_state {
            match vol_state {
                VolumeState::Expand => {
                    // 放量：动能强劲
                    m_vol = 1.35;
                    res = res
                        .with_score(15.0)
                        .because("成交量显著扩张，趋势动能得到燃料支撑");

                    // 【核心解锁】突破逻辑
                    // 如果波动率之前是压缩的，现在的放量就是“顶开天花板”的信号
                    if is_vol_compressed {
                        // 动态增加后续信号的权重上限
                        scope.insert_ctx(ContextKey::MultMomentum, json!(1.5));
                        res = res.because(
                            "检测到放量突破压缩区 (Volatility Breakout)，解锁更高动能权重",
                        );
                    }
                }
                VolumeState::Shrink => {
                    // 缩量：兴趣减退
                    m_vol = 0.7;
                    res = res
                        .with_score(-5.0)
                        .because("成交量萎缩，市场参与度下降，谨防虚假波动");
                }
                VolumeState::Squeeze => {
                    // 极致挤压：变盘前夜
                    // 如果波动率也压缩，那就是双重挤压，爆发潜力巨大
                    m_vol = if is_vol_compressed { 1.2 } else { 0.85 };
                    res = res.because("成交量进入极致挤压状态，市场正在高度蓄势");
                }
                _ => {
                    m_vol = 1.0;
                }
            }
        }

        // --- C. 智能检测：异常与风险 ---

        // 1. 高潮爆量检测 (Climax/Exhaustion)
        // 逻辑：如果价格波动极大（Extreme Candle）且成交量也是巨量，通常是反转信号而非持续信号
        if feat.signals.extreme_candle.unwrap_or(false) {
            m_vol *= 0.6;
            res = res
                .violate()
                .because("检测到放量力竭 (Volume Climax)，警惕流动性最后喷发后的反转");
        }

        // 2. “火药桶”模式 (Explosive Setup)
        // 条件：低波动 + 持续缩量 + RSI走平
        let rsi_ready = feat.signals.rsi_range_3.unwrap_or(false);
        let vol_shrink_3 = feat.signals.volume_shrink_3.unwrap_or(false);

        if rsi_ready && vol_shrink_3 && is_vol_compressed {
            m_vol *= 1.45;
            res = res.because("确认‘火药桶’蓄势模式：低波+缩量+RSI盘整，极易触发爆发性单边");
        }

    

        // --- E. 结果持久化 ---
        scope.set_multiplier(self.kind(), m_vol);

        // 额外标记：成交量是否强劲，供 Entry 阶段做最后的门槛检查
        let is_strong = matches!(feat.structure.volume_state, Some(VolumeState::Expand));
        scope.insert_ctx(ContextKey::MultOi, json!(if is_strong { 1.1 } else { 0.9 }));

        Ok(res.with_mult(m_vol).debug(json!({
            "final_m_vol": m_vol,
            "vol_state": format!("{:?}", feat.structure.volume_state),
            "is_vol_compressed": is_vol_compressed,
        })))
    }
}
