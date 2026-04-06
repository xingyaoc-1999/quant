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
        // 核心技巧：通过 Clone 和 Copy 将数据从 ctx 的生命周期中剥离出来
        let (vol_state, extreme_candle, vol_shrink_3, rsi_ready, dist_sup, dist_res, regime) = {
            // 获取 Trend 角色 (用于判断大趋势背景)
            let trend_role = ctx.get_role(Role::Trend)?;

            // 获取 Filter 角色 (用于捕捉局部成交量信号)，若未配置则回退到 Trend
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
                // 从 Trend 角色提取市场结构
                f_trend
                    .structure
                    .trend_structure
                    .clone()
                    .unwrap_or(TrendStructure::Range),
            )
        }; // <--- 借用结束，ctx 现在可以安全地进行写操作 (set_cached/set_multiplier)

        // ========== 2. 缓存读取 (此时 ctx 是自由的) ==========
        let is_vol_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);

        let mut m_vol = 1.0;
        let mut res = AnalysisResult::new(self.kind(), "VOL_STRUCT".into());

        // ========== 3. 核心成交量逻辑 ==========
        if let Some(ref vs) = vol_state {
            // 安全地持久化局部成交量状态
            ctx.set_cached(ContextKey::VolumeState, json!(vs));

            match vs {
                VolumeState::Expand => {
                    // 放量阶段：波动率越高，确认度越高
                    m_vol = if vol_p > 60.0 { 1.35 } else { 1.15 };

                    // 逻辑：如果之前是压缩态 (Squeeze)，且现在在关键位置放量，属于爆发信号
                    if is_vol_compressed && (dist_sup < 0.02 || dist_res < 0.02) {
                        m_vol *= 1.4;
                        res = res.because("🚀 爆发信号：放量突破低波压缩区");
                    }
                }
                VolumeState::Shrink => {
                    // 逻辑：在强多头趋势的支撑位缩量，意味着卖盘枯竭
                    if vol_shrink_3
                        && dist_sup < 0.012
                        && matches!(
                            regime,
                            TrendStructure::Bullish | TrendStructure::StrongBullish
                        )
                    {
                        m_vol = 1.45;
                        res = res.because("支撑位缩量回踩：抛压枯竭点，高胜率入场区");
                    } else {
                        m_vol = 0.7; // 趋势中无意义的缩量视为动能不足
                    }
                }
                VolumeState::Squeeze => {
                    // 缩量到极致通常是变盘前兆
                    m_vol = if is_vol_compressed { 1.2 } else { 0.85 };
                }
                _ => {}
            }
        }

        // ========== 4. 极端逻辑与熔断 (火药桶共振) ==========
        if rsi_ready && vol_shrink_3 && is_vol_compressed {
            // 联动：指标钝化 + 缩量 + 波动压缩 = 即将发生剧烈单边
            let bonus = 1.5 + ((100.0 - vol_p) / 200.0);
            m_vol *= bonus;
            res = res.because(format!("☢️ 火药桶共振：极致压缩后的爆发潜力 x{:.2}", bonus));
        }

        if extreme_candle {
            if vol_p > 85.0 {
                // 逻辑：高波动+天量天价 = 经典的力竭信号
                m_vol = 0.2;
                res = res.violate().because("高位力竭：极端波动下的天量换手信号");
            } else if dist_res < 0.01 {
                // 阻力位出现异常巨大的 K 线通常是诱多
                m_vol *= 0.4;
                res = res.because("阻力位放量滞涨：警惕假突破/流动性陷阱");
            }
        }

        // ========== 5. 最终状态持久化 ==========
        ctx.set_multiplier(self.kind(), m_vol);

        // 为 OI (持仓) 分析器计算“成交量健康度”
        let vol_health = if matches!(vol_state, Some(VolumeState::Expand)) && !extreme_candle {
            1.25
        } else {
            1.0
        };
        ctx.set_cached(ContextKey::MultOi, json!(vol_health));

        Ok(res.with_mult(m_vol))
    }
}
