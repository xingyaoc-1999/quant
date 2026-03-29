use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::PriceGravityWell;
use serde_json::json;
use std::borrow::Cow;

pub struct LevelProximityAnalyzer;

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
        let trend_data = ctx.get_role(Role::Trend);
        let filter_data = ctx.get_role(Role::Filter);
        let t_space = &trend_data.feature_set.space;
        let f_space = &filter_data.feature_set.space;

        // --- 1. 环境感知：获取 Regime 和 压缩状态 ---
        let regime = shared
            .data
            .get("ctx:regime:structure")
            .map(|s| s.as_str().unwrap_or("Range").to_string())
            .unwrap_or_else(|| "Range".to_string());

        let is_compressed = shared
            .data
            .get("ctx:volatility:is_compressed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // 动态调整“临近”定义的阈值：压缩市（窄幅）更敏感
        let proximity_threshold = if is_compressed { 0.003 } else { 0.007 };

        let mut m_space_long: f64 = 1.0;
        let mut m_space_short: f64 = 1.0;
        let mut description: Vec<Cow<'static, str>> = Vec::new();
        let mut gravity_wells: Vec<PriceGravityWell> = Vec::new();

        // --- 2. 压力位逻辑 (Resistance) ---
        if let Some(t_dist_res) = t_space.dist_to_resistance {
            // 填充重力井数据用于可视化或 Debug
            gravity_wells.push(PriceGravityWell {
                level: ctx.global.last_price * (1.0 + t_dist_res),
                source: "Trend_Resistance".into(),
                distance_pct: t_dist_res,
            });

            let f_dist_res = f_space.dist_to_resistance.unwrap_or(1.0);
            let is_confluence =
                t_dist_res < proximity_threshold && f_dist_res < proximity_threshold;

            if t_dist_res < proximity_threshold {
                match regime.as_str() {
                    "Range" => {
                        if is_compressed {
                            // 窄幅震荡边缘：防突破，削减双向信心
                            m_space_long *= 0.5;
                            m_space_short *= 0.7;
                            description.push(Cow::Borrowed("RESISTANCE_NEAR:COMPRESSED_ZONE"));
                        } else {
                            // 宽幅震荡边缘：理想的高抛点（大幅增加做空乘数）
                            m_space_long *= if is_confluence { 0.15 } else { 0.3 };
                            m_space_short *= if is_confluence { 1.6 } else { 1.3 };
                            description.push(Cow::Borrowed("RESISTANCE_NEAR:WIDE_RANGE_TOP"));
                        }
                    }
                    r if r.contains("Strong") => {
                        // 强趋势：临近压力位通常是蓄势突破，不轻易看空
                        m_space_long *= if is_confluence { 0.8 } else { 0.95 };
                        m_space_short *= 0.2;
                        description.push(Cow::Borrowed("RESISTANCE_NEAR:TREND_BREAKOUT_LOAD"));
                    }
                    _ => {
                        m_space_long *= 0.7;
                        m_space_short *= 0.9;
                        description.push(Cow::Borrowed("RESISTANCE_NEAR:NORMAL"));
                    }
                }
            }
        }

        // --- 3. 支撑位逻辑 (Support) ---
        if let Some(t_dist_sup) = t_space.dist_to_support {
            gravity_wells.push(PriceGravityWell {
                level: ctx.global.last_price * (1.0 - t_dist_sup),
                source: "Trend_Support".into(),
                distance_pct: -t_dist_sup,
            });

            let f_dist_sup = f_space.dist_to_support.unwrap_or(1.0);
            let is_confluence =
                t_dist_sup < proximity_threshold && f_dist_sup < proximity_threshold;

            if t_dist_sup < proximity_threshold {
                match regime.as_str() {
                    "Range" => {
                        if is_compressed {
                            // 窄幅震荡底：防下破，谨慎看多
                            m_space_long *= 0.8;
                            m_space_short *= 0.5;
                            description.push(Cow::Borrowed("SUPPORT_NEAR:COMPRESSED_WAIT"));
                        } else {
                            // 宽幅震荡底：黄金买点（大幅增加做多乘数）
                            m_space_long *= if is_confluence { 1.8 } else { 1.5 };
                            m_space_short *= if is_confluence { 0.15 } else { 0.3 };
                            description.push(Cow::Borrowed("SUPPORT_NEAR:WIDE_RANGE_BOTTOM"));
                        }
                    }
                    r if r.contains("Strong") => {
                        // 强趋势回调至支撑：优质买入点
                        m_space_long *= if is_confluence { 1.5 } else { 1.3 };
                        m_space_short *= 0.15;
                        description.push(Cow::Borrowed("SUPPORT_NEAR:TREND_BUY_DIP"));
                    }
                    _ => {
                        m_space_long *= 1.2;
                        m_space_short *= 0.8;
                        description.push(Cow::Borrowed("SUPPORT_NEAR:NORMAL"));
                    }
                }
            }
        }

        // --- 4. 乖离率逻辑 (MA20 Deviation) ---
        let mut m_deviation_long: f64 = 1.0;
        let mut m_deviation_short: f64 = 1.0;

        if let Some(dist_ratio) = t_space.ma20_dist_ratio {
            let limit = if regime.contains("Strong") {
                0.05
            } else {
                0.035
            };

            if dist_ratio > limit {
                m_deviation_long *= 0.4;
                m_deviation_short *= 1.2;
                description.push(Cow::Borrowed("DEVIATION:OVEREXTENDED_BULLISH"));
            } else if dist_ratio < -limit {
                m_deviation_short *= 0.4;
                m_deviation_long *= 1.2;
                description.push(Cow::Borrowed("DEVIATION:OVEREXTENDED_BEARISH"));
            }
        }

        // --- 5. 结构化持久化 ---
        shared.data.insert(
            "multiplier:space:proximity_long".into(),
            json!(m_space_long),
        );
        shared.data.insert(
            "multiplier:space:proximity_short".into(),
            json!(m_space_short),
        );
        shared.data.insert(
            "multiplier:space:deviation_long".into(),
            json!(m_deviation_long),
        );
        shared.data.insert(
            "multiplier:space:deviation_short".into(),
            json!(m_deviation_short),
        );

        shared
            .data
            .insert("ctx:space:gravity_wells".into(), json!(gravity_wells));
        shared.data.insert(
            "ctx:space:is_at_boundary".into(),
            json!(m_space_long > 1.4 || m_space_short > 1.4),
        );

        // --- 6. 构造返回结果 ---
        // 注意：Context 层分析器通常不直接给 score，而是通过 weight_multiplier 影响全局
        Ok(AnalysisResult {
            score: 0.0,
            weight_multiplier: 1.0,
            description: description.join(" | "),
            debug_data: json!({
                "long_combined": m_space_long * m_deviation_long,
                "short_combined": m_space_short * m_deviation_short,
                "is_compressed": is_compressed,
                "regime": regime,
                "wells": gravity_wells.len()
            }),
            ..Default::default()
        })
    }
}
