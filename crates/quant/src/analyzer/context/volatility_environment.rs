use crate::analyzer::{
    AnalysisError,
    AnalysisResult,
    Analyzer,
    AnalyzerKind,
    AnalyzerStage,
    Config,
    ContextKey, // 确保引入了你定义的 ContextKey
    MarketContext,
    Role,
    SharedAnalysisState,
};
use serde_json::json;

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
        // 1. 获取币种作用域与基础特征
        let scope = shared.scope(&ctx.symbol);
        let trend_role = ctx.get_role(Role::Trend)?; // 使用 ? 处理 Result
        let feat = &trend_role.feature_set;
        let last_price = ctx.global.last_price;

        // --- A. 获取基础行情模式 (使用类型安全的 ContextKey) ---
        let regime_str = scope
            .get_val(ContextKey::RegimeStructure)
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "Range".to_string());

        let vol_p = feat.price_action.volatility_percentile;

        // --- B. 计算动态 ATR 比例并共享 (使用 ContextKey) ---
        let atr = feat.indicators.atr_14.unwrap_or(last_price * 0.005);
        let atr_ratio = if last_price > 0.0 {
            atr / last_price
        } else {
            0.005
        };

        scope.insert_ctx(ContextKey::VolAtrRatio, json!(atr_ratio));
        scope.insert_ctx(ContextKey::VolPercentile, json!(vol_p));

        // 初始化结果
        let mut res = AnalysisResult::new(self.kind(), "VOL_ENV".into()).with_desc(format!(
            "波动率分位: {:.1}% | ATR比例: {:.2}%",
            vol_p,
            atr_ratio * 100.0
        ));

        // --- C. 流动性枯竭检测 (一票否决) ---
        if vol_p < 8.0 {
            // 写入布尔值标识
            scope.insert_ctx(ContextKey::VolIsCompressed, json!(false));
            // 针对当前 AnalyzerKind 设置低倍数
            scope.set_multiplier(self.kind(), 0.1);

            return Ok(res
                .with_mult(0.1)
                .because("市场极度冻结 (Vol < 8%)，触发流动性熔断")
                .violate());
        }

        // --- D. 基于 Regime 的非线性逻辑 ---
        let mut m_vol_env: f64 = 1.0;
        let mut m_vol_squeeze: f64 = 1.0;

        // 使用模式匹配让逻辑更清晰
        match (regime_str.as_str(), vol_p) {
            // 1. 强趋势环境
            (s, p) if s.contains("Strong") => {
                if p > 90.0 {
                    m_vol_env = 1.1;
                    res = res.because("强趋势且波动率极高，趋势正在加速爆发");
                } else if p < 20.0 {
                    m_vol_env = 0.7;
                    res = res.because("强趋势但波动率萎缩，面临动力衰竭风险");
                } else {
                    m_vol_env = 1.25;
                    res = res.because("健康波动率支持强趋势持续");
                }
            }
            // 2. 震荡环境
            ("Range", p) => {
                if p > 85.0 {
                    m_vol_env = 0.4;
                    res = res.because("震荡市高波动，易发假突破，调低权重");
                } else if p < 20.0 {
                    m_vol_env = 0.7;
                    m_vol_squeeze = 1.25;
                    scope.insert_ctx(ContextKey::VolIsCompressed, json!(true));
                    res = res.because("震荡极度缩量 (Squeeze)，潜在大变盘预警");
                } else {
                    m_vol_env = 1.1;
                    res = res.because("标准震荡波动，适合区间交易");
                }
            }
            // 3. 默认/普通环境
            (_, p) => {
                if p > 92.0 {
                    m_vol_env = 0.6;
                    res = res.because("波动率处于极端高位，警惕均值回归反转");
                } else if p < 15.0 {
                    m_vol_env = 0.5;
                    res = res.because("波动率过低，缺乏获利空间");
                }
            }
        }

        // --- E. 状态持久化与最终计算 ---
        let final_mult = m_vol_env * m_vol_squeeze;

        // 使用框架提供的 set_multiplier
        scope.set_multiplier(self.kind(), final_mult);

        Ok(res.with_mult(final_mult).debug(json!({
            "vol_p": vol_p,
            "atr_ratio": atr_ratio,
            "regime": regime_str,
            "m_vol_env": m_vol_env,
            "m_vol_squeeze": m_vol_squeeze,
            "final_m": final_mult,
            "is_compressed": vol_p < 20.0 && regime_str == "Range"
        })))
    }
}
