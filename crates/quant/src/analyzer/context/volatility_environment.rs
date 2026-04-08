use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::TrendStructure;
use serde_json::json;

pub struct VolatilityEnvironmentAnalyzer;

const VOL_EXTREME_LOW: f64 = 8.0; // 极低波动熔断
const VOL_SQUEEZE_THRESHOLD: f64 = 20.0; // 进入压缩态阈值
const VOL_LOW_MOMENTUM: f64 = 25.0; // 趋势中动力不足
const VOL_MEAT_GRINDER: f64 = 85.0; // 震荡市绞肉机阈值
const VOL_ACCELERATION: f64 = 90.0; // 趋势加速阈值

impl Analyzer for VolatilityEnvironmentAnalyzer {
    fn name(&self) -> &'static str {
        "volatility_env"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Volatility
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        // ========== 1. 数据提取阶段 (保持高效的作用域设计) ==========
        let (vol_p, atr, regime) = {
            let trend_role = ctx.get_role(Role::Trend)?;
            let filter_role = ctx
                .get_role(Role::Filter)
                .or_else(|_| ctx.get_role(Role::Trend))?;

            let f_filter = &filter_role.feature_set;
            let f_trend = &trend_role.feature_set;

            let v_p = f_filter.price_action.volatility_percentile;
            let a = f_filter.indicators.atr_14.unwrap_or(last_price * 0.005);

            let reg = f_trend
                .structure
                .trend_structure
                .clone()
                .unwrap_or_else(|| {
                    ctx.get_cached::<TrendStructure>(ContextKey::RegimeStructure)
                        .unwrap_or(TrendStructure::Range)
                });

            (v_p, a, reg)
        };

        // ========== 2. 基础指标计算与状态预设 ==========
        let atr_ratio = if last_price > f64::EPSILON {
            atr / last_price
        } else {
            0.005
        };
        let mut m_env = 1.0;
        let mut is_compressed = false;
        let mut res = AnalysisResult::new(self.kind(), "VOL_ENV".into());

        // 核心缓存写入
        ctx.set_cached(ContextKey::VolAtrRatio, json!(atr_ratio));
        ctx.set_cached(ContextKey::VolPercentile, json!(vol_p));

        // ========== 3. 熔断逻辑：极低流动性保护 ==========
        if vol_p < VOL_EXTREME_LOW {
            return Ok(res
                .with_mult(0.1)
                .because("市场进入极度低频死寂期，暂停所有信号")
                .violate());
        }

        // ========== 4. 动态环境权重匹配 ==========
        match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                if vol_p > VOL_ACCELERATION {
                    m_env = 1.15; // 趋势末端加速或过热
                    res = res.because("强趋势进入放量加速段");
                } else if vol_p < VOL_LOW_MOMENTUM {
                    m_env = 0.65; // 趋势背离，缺乏波动支撑
                    res = res.because("趋势强但局部波动匮乏，预防虚假信号");
                } else {
                    m_env = 1.3; // 黄金波段 (Goldilocks Zone)
                    res = res.because("波动率与趋势强度完美匹配，环境极佳");
                }
            }
            TrendStructure::Range => {
                if vol_p < VOL_SQUEEZE_THRESHOLD {
                    is_compressed = true;
                    m_env = 0.85; // 压缩态，此时评分不宜过高，等待变盘
                    res = res.because("市场处于波动率极度压缩态 (Squeeze)，蓄势中");
                } else if vol_p > VOL_MEAT_GRINDER {
                    m_env = 0.3; // 震荡市高波动 = 散户绞肉机
                    res = res
                        .because("震荡市伴随极端高波动，属于无效噪音干扰")
                        .violate();
                } else {
                    m_env = 1.0; // 普通震荡
                    res = res.because("震荡环境波动适中");
                }
            }
            _ => {
                m_env = if vol_p > 92.0 { 0.5 } else { 1.0 };
            }
        }

        // ========== 5. 持久化与结果返回 ==========
        ctx.set_cached(ContextKey::VolIsCompressed, json!(is_compressed));
        ctx.set_multiplier(self.kind(), m_env);

        Ok(res.with_mult(m_env).debug(json!({
            "vol_p": vol_p,
            "regime": format!("{:?}", regime),
            "m_env": m_env,
            "is_compressed": is_compressed,
            "atr_ratio": atr_ratio
        })))
    }
}
