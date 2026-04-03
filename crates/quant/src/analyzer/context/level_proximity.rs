use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, ContextKey,
    MarketContext, Role, SharedAnalysisState,
};
use crate::types::PriceGravityWell;
use serde_json::json;

pub struct LevelProximityAnalyzer;

impl LevelProximityAnalyzer {
    /// 核心算法：计算非线性引力强度 (0.0 -> 1.0)
    /// 优化：增加内联标记，使用直接乘法代替 powi(2) 减少函数调用开销
    #[inline]
    fn calculate_intensity(dist: f64, threshold: f64) -> f64 {
        // 防御性编程：避免 threshold <= 0 导致的除以零或无穷大问题
        if dist >= threshold || threshold <= 0.0 {
            0.0
        } else {
            let ratio = 1.0 - (dist / threshold);
            ratio * ratio
        }
    }
}

impl Analyzer for LevelProximityAnalyzer {
    fn name(&self) -> &'static str {
        "level_proximity"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Context
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::SupportResistance
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let scope = shared.scope(&ctx.symbol);
        let trend_data = ctx.get_role(Role::Trend)?;
        let filter_data = ctx.get_role(Role::Filter)?;
        let last_price = ctx.global.last_price;

        // --- 1. 获取标准化上下文 ---
        let is_compressed = scope.get_bool(ContextKey::VolIsCompressed);
        let atr_ratio = scope.get_f64(ContextKey::VolAtrRatio).unwrap_or(0.005);
        let regime = scope
            .get_val(ContextKey::RegimeStructure)
            .and_then(|v| v.as_str().map(|s| s.to_string())) // 转化为 String
            .unwrap_or_else(|| "Range".to_string());

        // --- 2. 动态参数计算 ---
        let radius_mult = if is_compressed { 0.4 } else { 0.8 };
        let threshold = atr_ratio * radius_mult;
        let confluence_gate = threshold * 0.25;

        let mut m_long: f64 = 1.0;
        let mut m_short: f64 = 1.0;

        // 优化：明确知道最多只会产生 2 个引力井（支撑1个，阻力1个），直接预分配内存
        let mut gravity_wells = Vec::with_capacity(2);
        let mut res = AnalysisResult::new(self.kind(), "LEVEL_PROX".into());

        let t_space = &trend_data.feature_set.space;
        let f_space = &filter_data.feature_set.space;

        // --- 3. 阻力位检测与引力计算 (Resistance) ---
        if let Some(dist_res) = t_space.dist_to_resistance {
            let intensity = Self::calculate_intensity(dist_res, threshold);

            // 优化：仅在引力强度 > 0 时才将引力井推入 Vec，避免无用数据污染可视化和内存
            if intensity > 0.0 {
                gravity_wells.push(PriceGravityWell {
                    level: last_price * (1.0 + dist_res),
                    source: "Trend_Resistance".into(),
                    distance_pct: dist_res,
                    strength: intensity,
                });

                // 优化：使用 f64::INFINITY 代替 999.0 魔法数字，计算更严谨
                let f_dist = f_space.dist_to_resistance.unwrap_or(f64::INFINITY);
                let is_confluent = (dist_res - f_dist).abs() < confluence_gate;
                let boost = if is_confluent { 1.4 } else { 1.0 };

                match regime.as_str() {
                    "Range" => {
                        m_long *= 1.0 - (0.85 * intensity);
                        m_short *= 1.0 + (0.8 * intensity * boost);
                        res = res.because("震荡区间顶部：阻力引力增强，抑制多头逻辑");
                    }
                    s if s.contains("StrongBullish") => {
                        m_short *= 0.15;
                        res = res.because("强多头环境：阻力位预期将被突破，禁止摸顶");
                    }
                    _ => {
                        m_long *= 1.0 - (0.5 * intensity);
                    }
                }
            }
        }

        // --- 4. 支撑位检测与引力计算 (Support) ---
        if let Some(dist_sup) = t_space.dist_to_support {
            let intensity = Self::calculate_intensity(dist_sup, threshold);

            if intensity > 0.0 {
                gravity_wells.push(PriceGravityWell {
                    level: last_price * (1.0 - dist_sup),
                    source: "Trend_Support".into(),
                    distance_pct: -dist_sup,
                    strength: intensity,
                });

                let f_dist = f_space.dist_to_support.unwrap_or(f64::INFINITY);
                let is_confluent = (dist_sup - f_dist).abs() < confluence_gate;
                let boost = if is_confluent { 1.6 } else { 1.0 };

                match regime.as_str() {
                    "Range" => {
                        m_long *= 1.0 + (1.2 * intensity * boost);
                        m_short *= 1.0 - (0.9 * intensity);
                        res = res.because("震荡区间底部：支撑引力增强，抑制空头逻辑");
                    }
                    s if s.contains("StrongBearish") => {
                        m_long *= 0.1;
                        res = res.because("强空头环境：支撑位有效性极低，严禁抄底");
                    }
                    s if s.contains("Bullish") => {
                        m_long *= 1.0 + (0.6 * intensity * boost);
                        res = res.because("趋势回调：触及关键支撑共振区，视为顺势补票");
                    }
                    _ => {
                        m_short *= 1.0 - (0.4 * intensity);
                    }
                }
            }
        }

        // --- 5. 数据持久化与维度输出 ---
        m_long = m_long.clamp(0.1, 3.5);
        m_short = m_short.clamp(0.1, 3.5);

        scope.insert_ctx(ContextKey::MultLongSpace, json!(m_long));
        scope.insert_ctx(ContextKey::MultShortSpace, json!(m_short));
        scope.insert_ctx(ContextKey::SpaceGravityWells, json!(gravity_wells));

        let final_weight = m_long.max(m_short);
        scope.set_multiplier(self.kind(), final_weight);

        Ok(res.with_mult(final_weight).debug(json!({
            "m_long": m_long,
            "m_short": m_short,
            "detected_wells": gravity_wells.len(),
            "active_regime": regime,
            "dynamic_threshold": threshold
        })))
    }
}
