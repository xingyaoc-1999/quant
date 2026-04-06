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
        // 我们在一个独立的代码块中获取数据，执行完后 trend_role 和 feat 会被释放
        let (vol_state, extreme_candle, vol_shrink_3, rsi_ready, dist_sup, dist_res) = {
            let trend_role = ctx.get_role(Role::Trend)?;
            let f = &trend_role.feature_set;
            (
                f.structure.volume_state.clone(), // Clone 出来，断开引用
                f.signals.extreme_candle.unwrap_or(false),
                f.signals.volume_shrink_3.unwrap_or(false),
                f.signals.rsi_range_3.unwrap_or(false),
                f.space.dist_to_support.unwrap_or(1.0),
                f.space.dist_to_resistance.unwrap_or(1.0),
            )
        }; // <--- 借用在这里结束了，ctx 现在是自由的了

        // ========== 2. 缓存读取 (此时可以安全地操作 ctx) ==========
        let is_vol_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);

        let mut m_vol = 1.0;
        let mut res = AnalysisResult::new(self.kind(), "VOL_STRUCT".into());

        // ========== 3. 核心成交量逻辑 ==========
        if let Some(vs) = vol_state {
            // 这里现在可以安全地 set_cached 了，因为上面已经释放了 ctx
            ctx.set_cached(ContextKey::VolumeState, json!(vs));

            match vs {
                VolumeState::Expand => {
                    m_vol = if vol_p > 60.0 { 1.35 } else { 1.15 };
                    if is_vol_compressed && (dist_sup < 0.02 || dist_res < 0.02) {
                        m_vol *= 1.4;
                        res = res.because("🚀 爆发信号：放量突破低波压缩区");
                    }
                }
                VolumeState::Shrink => {
                    if vol_shrink_3
                        && dist_sup < 0.012
                        && matches!(
                            regime,
                            TrendStructure::Bullish | TrendStructure::StrongBullish
                        )
                    {
                        m_vol = 1.45;
                        res = res.because("支撑位缩量回踩：抛压枯竭点");
                    } else {
                        m_vol = 0.7;
                    }
                }
                VolumeState::Squeeze => {
                    m_vol = if is_vol_compressed { 1.2 } else { 0.85 };
                }
                _ => {}
            }
        }

        // ========== 4. 极端逻辑与熔断 ==========
        if rsi_ready && vol_shrink_3 && is_vol_compressed {
            let bonus = 1.5 + ((100.0 - vol_p) / 200.0);
            m_vol *= bonus;
            res = res.because(format!("☢️ 火药桶共振：爆发潜力 x{:.2}", bonus));
        }

        if extreme_candle {
            if vol_p > 85.0 {
                m_vol = 0.2;
                res = res.violate().because("高位力竭：天量换手见顶信号");
            } else if dist_res < 0.01 {
                m_vol *= 0.4;
                res = res.because("阻力位放量滞涨：警惕假突破");
            }
        }

        // ========== 5. 最终状态持久化 ==========
        ctx.set_multiplier(self.kind(), m_vol);

        let vol_health = if matches!(vol_state, Some(VolumeState::Expand)) && !extreme_candle {
            1.25
        } else {
            1.0
        };
        ctx.set_cached(ContextKey::MultOi, json!(vol_health));

        Ok(res.with_mult(m_vol))
    }
}
