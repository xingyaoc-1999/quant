use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, AnalyzerStage, Config, MarketContext,
    Role, SharedAnalysisState,
};
use crate::types::{MacdCross, MacdMomentum};
use common::Interval;
use serde_json::json;
use std::borrow::Cow;

/// 动量共振分析器 (V3 增强版)
/// 核心职责：捕获均线收复/跌破与 MACD 共振，并根据市场空间和相关性进行动态评分缩放
pub struct ResonanceAnalyzer {
    /// 默认老化阈值：当趋势持续步数超过此值，分值开始衰减
    default_aging_threshold: i32,
}

impl ResonanceAnalyzer {
    pub fn new() -> Self {
        Self {
            default_aging_threshold: 36,
        }
    }
}

impl Analyzer for ResonanceAnalyzer {
    fn name(&self) -> &'static str {
        "momentum_resonance"
    }

    fn stage(&self) -> AnalyzerStage {
        AnalyzerStage::Signal
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Momentum
    }

    fn analyze(
        &self,
        ctx: &MarketContext,
        _config: &Config,
        shared: &SharedAnalysisState,
    ) -> Result<AnalysisResult, AnalysisError> {
        let trend_data = ctx.get_role(Role::Trend);
        let feat = &trend_data.feature_set;

        // --- 1. 获取外部环境因子 ---
        let correlation = feat.structure.correlation_with_global.unwrap_or(0.0);
        let aging_threshold = match trend_data.interval {
            Interval::M15 => 24, // 15m 线：约 6 小时后开始老化
            Interval::H1 => 48,  // 1h 线：约 2 天
            Interval::D1 => 20,  // 日线：约 1 个月
            _ => self.default_aging_threshold,
        };

        let mut abs_score = 0.0; // 存储信号绝对强度
        let mut description: Vec<Cow<'static, str>> = Vec::new();

        // --- 2. 核心触发逻辑与方向锁定 (Direction) ---
        let is_reclaim = feat.signals.ma20_reclaim.unwrap_or(false);
        let is_breakdown = feat.signals.ma20_breakdown.unwrap_or(false);
        let macd_cross = feat.signals.macd_cross;

        let direction = if is_reclaim || macd_cross == Some(MacdCross::Golden) {
            1.0 // 看多极性
        } else if is_breakdown || macd_cross == Some(MacdCross::Death) {
            -1.0 // 看空极性
        } else {
            // 无核心触发信号，直接返回默认值
            return Ok(AnalysisResult {
                description: format!("{}: NO_BASE_SIGNAL", self.name()),
                ..Default::default()
            });
        };

        // --- 3. 基础强度分配 (Base Power) ---
        if is_reclaim || is_breakdown {
            abs_score += 45.0; // 均线突破/收复是强信号
            description.push(if direction > 0.0 {
                Cow::Borrowed("LONG:MA20_RECLAIM")
            } else {
                Cow::Borrowed("SHORT:MA20_BREAKDOWN")
            });
        } else {
            abs_score += 30.0; // 单纯指标金叉/死叉分值稍低
            description.push(if direction > 0.0 {
                Cow::Borrowed("LONG:MACD_CROSS")
            } else {
                Cow::Borrowed("SHORT:MACD_CROSS")
            });
        }

        // --- 4. 动能确认奖励 (Momentum Bonus) ---
        let mom_confirmed = (direction > 0.0
            && feat.signals.macd_momentum == Some(MacdMomentum::Increasing))
            || (direction < 0.0 && feat.signals.macd_momentum == Some(MacdMomentum::Decreasing));

        if mom_confirmed {
            abs_score += 15.0;
            description.push(Cow::Borrowed("MOM_CONFIRMED"));
        }

        // --- 5. 空间压制检查 (Space Filtering) ---
        let space_dist = if direction > 0.0 {
            feat.space.dist_to_resistance
        } else {
            feat.space.dist_to_support
        };

        if let Some(dist) = space_dist {
            if dist < 0.003 {
                // 距离阻力/支撑不到 0.3%，视为“撞墙”，大幅扣分
                abs_score -= 30.0;
                description.push(Cow::Borrowed("SPACE_BLOCKED"));
            } else if dist > 0.02 {
                // 空间大于 2%，属于“海阔天空”，额外奖励
                abs_score += 10.0;
                description.push(Cow::Borrowed("SPACE_OPEN"));
            }
        }

        // --- 6. 相关性调整 (Alpha/Beta Scaling) ---
        if correlation > 0.9 {
            // 与大盘高度同步，属于 Beta 收益，分值打 8 折（追求独立行情）
            abs_score *= 0.8;
            description.push(Cow::Borrowed("HIGH_CORR_BETA"));
        } else if correlation < -0.3 {
            // 逆势或独立走势，属于高质量 Alpha，分值溢价 20%
            abs_score *= 1.2;
            description.push(Cow::Borrowed("INDEPENDENT_ALPHA"));
        }

        // --- 7. 趋势老化惩罚与早鸟奖励 (Aging Logic) ---
        let slope_bars = feat.structure.ma20_slope_bars.abs();
        if slope_bars > aging_threshold {
            // 趋势太老，容易发生均线回归，根据超时比例扣分
            let penalty_ratio = ((slope_bars - aging_threshold) as f64 / 20.0).min(1.0);
            abs_score -= 25.0 * penalty_ratio;
            description.push(Cow::Owned(format!(
                "TREND_AGING(-{:.0}%)",
                penalty_ratio * 100.0
            )));
        } else if slope_bars > 0 && slope_bars < 8 {
            // 趋势刚启动不久，属于早鸟阶段，额外奖励
            abs_score += 10.0;
            description.push(Cow::Borrowed("EARLY_BONUS"));
        }

        // --- 8. 最终安全性加固 ---
        // 确保各种惩罚不会让 abs_score 变成负数（防止多头信号变成空头信号）
        let final_strength = abs_score.max(0.0);

        // 将信号方向写入共享状态，供后续模块（如审计或下单）使用
        shared
            .data
            .insert("signal:direction".into(), json!(direction));

        Ok(AnalysisResult {
            // 最终分数 = 强度 * 方向
            score: final_strength * direction,
            description: description.join(" | "),
            debug_data: json!({
                "raw_abs_strength": abs_score,
                "final_abs_strength": final_strength,
                "direction": direction,
                "correlation": correlation,
                "slope_bars": slope_bars,
                "space_dist": space_dist,
            }),
            ..Default::default()
        })
    }
}
