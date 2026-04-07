use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{TrendStructure, VolumeState};
use serde_json::json;

pub struct VolumeStructureAnalyzer;

impl Analyzer for VolumeStructureAnalyzer {
    fn name(&self) -> &'static str {
        "volume_structure"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::VolumeProfile
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        // ========== 1. 数据提取阶段 (解开借用绑定) ==========
        let (vol_state, extreme_candle, vol_shrink_3, rsi_ready, dist_sup, dist_res, regime) = {
            let trend_role = ctx.get_role(Role::Trend)?;
            let filter_role = ctx
                .get_role(Role::Filter)
                .or_else(|_| ctx.get_role(Role::Trend))?;

            let f_filt = &filter_role.feature_set;
            let f_trend = &trend_role.feature_set;

            (
                f_filt.structure.volume_state.clone(),
                f_filt.signals.extreme_candle.unwrap_or(false),
                f_filt.signals.volume_shrink_3.unwrap_or(false),
                f_filt.signals.rsi_range_3.unwrap_or(false),
                f_filt.space.dist_to_support.unwrap_or(1.0),
                f_filt.space.dist_to_resistance.unwrap_or(1.0),
                f_trend
                    .structure
                    .trend_structure
                    .clone()
                    .unwrap_or(TrendStructure::Range),
            )
        };

        // ========== 2. 读取来自 VolatilityEnvironmentAnalyzer 的缓存 ==========
        let is_vol_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);

        let mut m_vol = 1.0;
        let mut res = AnalysisResult::new(self.kind(), "VOL_STRUCT".into());

        // ========== 3. 核心成交量与挤压共振逻辑 ==========
        if let Some(ref vs) = vol_state {
            ctx.set_cached(ContextKey::VolumeState, json!(vs));

            match vs {
                VolumeState::Expand => {
                    // 放量状态
                    if is_vol_compressed {
                        // 情景：低波挤压后的首次放量 —— 极强的突破信号
                        m_vol = 1.65;
                        res = res.because("🚀 爆发确认：成交量打破低波挤压，动能释放");
                    } else {
                        // 情景：常规趋势中的放量
                        m_vol = if vol_p > 65.0 { 1.25 } else { 1.1 };
                    }
                }
                VolumeState::Shrink => {
                    // 缩量状态
                    if is_vol_compressed
                        && dist_sup < 0.015
                        && matches!(
                            regime,
                            TrendStructure::Bullish | TrendStructure::StrongBullish
                        )
                    {
                        // 情景：多头趋势回调 + 缩量 + 波动收窄 + 支撑位 = VCP埋伏点
                        m_vol = 1.6;
                        res = res.because("💎 极致缩量挤压：多头趋势中抛压竭尽，关键支撑位蓄势");
                    } else if !is_vol_compressed && dist_res < 0.015 {
                        // 情景：阻力位附近的无量反弹
                        m_vol = 0.6;
                        res = res.because("⚠️ 弱势反弹：接近阻力位但缺乏资金跟进");
                    } else {
                        m_vol = 0.8; // 常规缩量，代表观望
                    }
                }
                VolumeState::Normal => {
                    // 成交量正常，但如果是高度压缩，也给予轻微关注
                    if is_vol_compressed {
                        m_vol = 1.1;
                        res = res.because("⚖️ 静态平衡：价格高度压缩，等待成交量入场打破僵局");
                    }
                }
            }
        }

        // ========== 4. 极端逻辑 (火药桶与力竭) ==========

        // 4.1 火药桶共振：多指标极致压缩
        if rsi_ready && vol_shrink_3 && is_vol_compressed {
            // 逻辑：RSI钝化 + 连续3根缩量 + 波动率挤压 = 即将发生的高胜率单边
            let bonus = 1.5 + ((100.0 - vol_p) / 200.0);
            m_vol *= bonus;
            res = res.because(format!("☢️ 火药桶共振：极致压缩后的爆发潜力 x{:.2}", bonus));
        }

        // 4.2 极端 K 线判定 (力竭或陷阱)
        if extreme_candle {
            if vol_p > 88.0 {
                // 逻辑：天量出现在极端波动中 = 筹码交换完毕，动力耗尽
                m_vol = 0.15;
                res = res
                    .violate()
                    .because("🚫 高位力竭：极端波动下的天量换手，趋势大概率反转");
            } else if dist_res < 0.012 {
                // 逻辑：阻力位放量大阳线，若不能迅速封死突破，通常是诱多扫损
                m_vol *= 0.4;
                res = res.because("🚩 阻力位异常放量：警惕流动性陷阱/假突破");
            }
        }

        // ========== 5. 结果持久化与输出 ==========
        ctx.set_multiplier(self.kind(), m_vol);

        // 计算成交量健康度，供 OI (持仓) 模块参考
        // 只有非极端的放量才被认为是健康的资金流入
        let vol_health = if matches!(vol_state, Some(VolumeState::Expand)) && !extreme_candle {
            1.25
        } else if matches!(vol_state, Some(VolumeState::Shrink)) {
            0.85
        } else {
            1.0
        };
        ctx.set_cached(ContextKey::MultOi, json!(vol_health));

        Ok(res.with_mult(m_vol))
    }
}
