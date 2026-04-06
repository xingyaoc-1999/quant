use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::TrendStructure; // 假设你有这个枚举
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
        let trend_role = ctx.get_role(Role::Trend)?;
        let feat = &trend_role.feature_set;
        let last_price = ctx.global.last_price;

        let vol_p = feat.price_action.volatility_percentile;
        let atr = feat.indicators.atr_14.unwrap_or(last_price * 0.005);
        let atr_ratio = if last_price > 0.0 {
            atr / last_price
        } else {
            0.005
        };

        // 全局持久化，这步你做得很好
        ctx.set_cached(ContextKey::VolAtrRatio, json!(atr_ratio));
        ctx.set_cached(ContextKey::VolPercentile, json!(vol_p));

        let mut res = AnalysisResult::new(self.kind(), "VOL_ENV".into());

        // 2. 深度熔断逻辑：流动性枯竭
        if vol_p < 8.0 {
            return Ok(res
                .with_mult(0.1)
                .because("市场流动性极度匮乏，拒绝所有趋势交易")
                .violate());
        }

        // 3. 获取市场结构（增强鲁棒性）
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);

        // 4. 计算动态乘数
        let mut m_env = 1.0;
        let mut is_compressed = false;

        // 引入数学公式：波动率对趋势的“确认度”
        // 逻辑：波动率越接近 50%-70% (健康区)，乘数越高；两极化则压制
        match regime {
            TrendStructure::StrongBullish | TrendStructure::StrongBearish => {
                if vol_p > 90.0 {
                    m_env = 1.15; // 加速阶段
                    res = res.because("强趋势加速，波动率共振放大");
                } else if vol_p < 25.0 {
                    m_env = 0.65; // 动力不足
                    res = res.because("强趋势但波动过低，警惕‘诱多/诱空’后撤");
                } else {
                    m_env = 1.3; // 黄金波段
                }
            }
            TrendStructure::Range => {
                if vol_p < 20.0 {
                    is_compressed = true;
                    m_env = 0.8; // 压缩态压制当前波动，但开启 Squeeze 标志
                    res = res.because("进入极度压缩区间 (Squeeze)，等待突破信号");
                } else if vol_p > 85.0 {
                    m_env = 0.4; // 震荡市高波动 = 绞肉机
                    res = res
                        .because("震荡市极端高波动，属于无效噪音，规避风险")
                        .violate();
                }
            }
            _ => {
                // 普通趋势或未知
                m_env = if vol_p > 92.0 { 0.5 } else { 1.0 };
            }
        }

        ctx.set_cached(ContextKey::VolIsCompressed, json!(is_compressed));
        ctx.set_multiplier(self.kind(), m_env);

        Ok(res.with_mult(m_env).debug(json!({
            "vol_p": vol_p,
            "regime": format!("{:?}", regime),
            "final_m": m_env,
            "is_compressed": is_compressed
        })))
    }
}
