use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::TrendStructure;
use serde_json::json;

pub struct VolatilityEnvironmentAnalyzer;

impl Analyzer for VolatilityEnvironmentAnalyzer {
    fn name(&self) -> &'static str {
        "volatility_env"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Volatility
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        // ========== 1. 数据提取阶段 (解决借用冲突的核心) ==========
        // 使用块作用域确保不可变借用在计算和缓存操作前被释放
        let (vol_p, atr, regime) = {
            // 获取角色引用
            let trend_role = ctx.get_role(Role::Trend)?;
            let filter_role = ctx
                .get_role(Role::Filter)
                .or_else(|_| ctx.get_role(Role::Trend))?;

            let f_filter = &filter_role.feature_set;
            let f_trend = &trend_role.feature_set;

            // 提取我们需要的基础标量数据 (f64/bool) 和 克隆 Enum
            // 这样我们拿走的是副本，不再依赖 ctx 的借用
            let v_p = f_filter.price_action.volatility_percentile;
            let a = f_filter.indicators.atr_14.unwrap_or(last_price * 0.005);

            // 优先获取特征集中的结构，如果没有再尝试从缓存中获取（此时还在读操作内）
            let reg = f_trend
                .structure
                .trend_structure
                .clone()
                .unwrap_or_else(|| {
                    ctx.get_cached::<TrendStructure>(ContextKey::RegimeStructure)
                        .unwrap_or(TrendStructure::Range)
                });

            (v_p, a, reg)
        }; // <--- 不可变借用在这里被 Drop，ctx 变回自由状态

        // ========== 2. 计算与缓存阶段 ==========
        let atr_ratio = if last_price > f64::EPSILON {
            atr / last_price
        } else {
            0.005
        };

        // 现在可以安全地执行可变操作（Mutable Borrow）
        ctx.set_cached(ContextKey::VolAtrRatio, json!(atr_ratio));
        ctx.set_cached(ContextKey::VolPercentile, json!(vol_p));

        let mut res = AnalysisResult::new(self.kind(), "VOL_ENV".into());

        // 3. 深度熔断逻辑
        if vol_p < 8.0 {
            return Ok(res
                .with_mult(0.1)
                .because("Filter周期流动性极度匮乏，拒绝所有趋势交易")
                .violate());
        }

        // 4. 计算动态乘数
        let mut m_env = 1.0;
        let mut is_compressed = false;

        match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                if vol_p > 90.0 {
                    m_env = 1.15; // 加速阶段
                    res = res.because("强趋势加速，局部波动共振放大");
                } else if vol_p < 25.0 {
                    m_env = 0.65; // 动力不足
                    res = res.because("大趋势强但局部波动过低，警惕‘诱多/诱空’后撤");
                } else {
                    m_env = 1.3; // 黄金波段
                }
            }
            TrendStructure::Range => {
                if vol_p < 20.0 {
                    is_compressed = true;
                    m_env = 0.8; // 压缩态
                    res = res.because("局部进入极度压缩区间 (Squeeze)，等待突破信号");
                } else if vol_p > 85.0 {
                    m_env = 0.4; // 震荡市高波动 = 绞肉机
                    res = res
                        .because("震荡市局部极端高波动，属于无效噪音，规避风险")
                        .violate();
                }
            }
            _ => {
                m_env = if vol_p > 92.0 { 0.5 } else { 1.0 };
            }
        }

        // 5. 状态持久化与结果返回
        ctx.set_cached(ContextKey::VolIsCompressed, json!(is_compressed));
        ctx.set_multiplier(self.kind(), m_env);

        Ok(res.with_mult(m_env).debug(json!({
            "filter_vol_p": vol_p,
            "trend_regime": format!("{:?}", regime),
            "final_m": m_env,
            "is_compressed": is_compressed
        })))
    }
}
