use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, WellSide};

// ================= 物理与磨损常量 =================
const SIGMA_ATR_MULT: f64 = 0.8;
const CONFLUENCE_GATE_MULT: f64 = 0.4;
const ACTIVE_WELL_THRESHOLD: f64 = 0.08;
const MAX_STRENGTH_CAP: f64 = 3.5;
const SECONDARY_WELL_WEIGHT: f64 = 0.3;
const CONVERGENCE_BOOST: f64 = 1.35;

// 磨损配置
const WEAR_DECAY_PER_HIT: f64 = 0.18;
const MAX_WEAR_CAP: f64 = 0.75;
const RECOVERY_RATE_PER_HOUR: f64 = 0.05;

pub struct LevelProximityAnalyzer;

impl LevelProximityAnalyzer {
    /// 高斯引力核心算法
    #[inline]
    fn calculate_intensity(dist: f64, sigma: f64) -> f64 {
        if sigma <= 0.0 {
            return 0.0;
        }
        let gauss = (-(dist * dist) / (2.0 * sigma * sigma)).exp();
        let long_range = 0.05 * (-(dist) / (10.0 * sigma)).exp();
        gauss.max(long_range)
    }

    /// 能量折旧计算
    fn calculate_wear_multiplier(hit_count: u32, last_hit_ts: i64, now: i64) -> f64 {
        if hit_count == 0 {
            return 1.0;
        }
        let diff_ms = (now - last_hit_ts).max(0);
        let hours_passed = diff_ms as f64 / 3_600_000.0;
        let recovery = hours_passed * RECOVERY_RATE_PER_HOUR;
        let wear = ((hit_count as f64 * WEAR_DECAY_PER_HIT) - recovery).clamp(0.0, MAX_WEAR_CAP);
        1.0 - wear
    }

    /// 复合强度：处理多个引力源共振
    fn calculate_composite_strength(side_wells: Vec<&PriceGravityWell>) -> f64 {
        if side_wells.is_empty() {
            return 0.0;
        }
        let mut strengths: Vec<f64> = side_wells.iter().map(|w| w.strength).collect();
        strengths.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

        let primary = strengths[0];
        let secondary_sum: f64 = strengths.iter().skip(1).sum();
        (primary + secondary_sum * SECONDARY_WELL_WEIGHT).min(MAX_STRENGTH_CAP)
    }
}

impl Analyzer for LevelProximityAnalyzer {
    fn name(&self) -> &'static str {
        "level_proximity_pro"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::SupportResistance
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;
        let now = ctx.global.timestamp;

        // 1. 动态感知参数
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .unwrap_or(0.005);
        let sigma = atr_ratio * (SIGMA_ATR_MULT + (vol_p / 120.0));
        let confluence_gate = sigma * CONFLUENCE_GATE_MULT;

        // 2. 提取数据
        let trend_role = ctx.get_role(Role::Trend)?;
        let filter_role = ctx.get_role(Role::Filter).unwrap_or(trend_role);

        let t_space = &trend_role.feature_set.space;
        let f_space = &filter_role.feature_set.space;
        let ma_converging = t_space.ma_converging.unwrap_or(false);

        let mut wells: Vec<PriceGravityWell> = Vec::new();

        // 3. 核心注入闭包 (修复方向逻辑)
        let mut process_source = |dist_opt: Option<f64>,
                                  side_hint: Option<WellSide>,
                                  label: &str,
                                  weight: f64,
                                  hits: u32,
                                  last_ts: i64| {
            let dist_raw = match dist_opt {
                Some(d) => d,
                None => return,
            };

            // 计算强度：使用绝对值，因为引力双向作用
            let wear_mult = Self::calculate_wear_multiplier(hits, last_ts, now);
            let final_strength =
                Self::calculate_intensity(dist_raw.abs(), sigma) * weight * wear_mult;

            if final_strength < 0.02 {
                return;
            }

            // 计算该水平位的绝对价格：Price * (1 + 偏移%)
            // 支撑位偏移应为负 (如 -0.02)，阻力位偏移应为正 (如 0.02)
            let current_level = last_price * (1.0 + dist_raw);

            // 判定 Side：优先使用 hint，否则根据偏移正负自动判定
            let side = side_hint.unwrap_or(if dist_raw >= 0.0 {
                WellSide::Resistance
            } else {
                WellSide::Support
            });

            // 检查共振合并
            let mut merged = false;
            for existing in wells.iter_mut() {
                let diff = (existing.level - current_level).abs() / last_price;
                if existing.side == side && diff < confluence_gate {
                    existing.strength += final_strength * 0.6;

                    // 优化：只有不包含时才拼接字符串，减少分配
                    if !existing.source.contains(label) {
                        if !existing.source.is_empty() {
                            existing.source.push('+');
                        }
                        existing.source.push_str(label);
                    }
                    if final_strength > ACTIVE_WELL_THRESHOLD {
                        existing.is_active = true;
                    }
                    merged = true;
                    break;
                }
            }

            if !merged {
                wells.push(PriceGravityWell {
                    level: current_level,
                    side,
                    source: label.to_string(),
                    distance_pct: dist_raw,
                    strength: final_strength,
                    is_active: final_strength > ACTIVE_WELL_THRESHOLD,
                    hit_count: hits,
                    last_hit_ts: last_ts,
                });
            }
        };

        // 4. 多维度数据喂入 (修复支撑位负号)

        // 阻力位：dist_to_resistance 在 FeatureSet 中是正数，保持正号
        process_source(
            t_space.dist_to_resistance,
            Some(WellSide::Resistance),
            "1D_R",
            1.2,
            t_space.res_hit_count,
            t_space.res_last_hit,
        );
        process_source(
            f_space.dist_to_resistance,
            Some(WellSide::Resistance),
            "4H_R",
            0.8,
            f_space.res_hit_count,
            f_space.res_last_hit,
        );

        // 支撑位：dist_to_support 在 FeatureSet 中是正数，必须取反传给闭包
        process_source(
            t_space.dist_to_support.map(|d| -d),
            Some(WellSide::Support),
            "1D_S",
            1.2,
            t_space.sup_hit_count,
            t_space.sup_last_hit,
        );
        process_source(
            f_space.dist_to_support.map(|d| -d),
            Some(WellSide::Support),
            "4H_S",
            0.8,
            f_space.sup_hit_count,
            f_space.sup_last_hit,
        );

        // MA 距离：FeatureSet 提供的是 (Price - MA)/MA，需要转换为 (MA - Price)/Price
        if let Some(ratio) = t_space.ma20_dist_ratio {
            // 换算公式: dist_raw = (MA/Price) - 1 = 1/(ratio + 1) - 1
            let ma_dist_raw = 1.0 / (ratio + 1.0) - 1.0;
            process_source(Some(ma_dist_raw), None, "1D_MA20", 1.0, 0, 0);
        }

        // 5. 均线聚合增强
        if ma_converging {
            for w in wells.iter_mut() {
                w.strength *= CONVERGENCE_BOOST;
            }
        }

        // 6. 计算净场强
        let total_res = Self::calculate_composite_strength(
            wells
                .iter()
                .filter(|w| w.side == WellSide::Resistance && w.is_active)
                .collect(),
        );
        let total_sup = Self::calculate_composite_strength(
            wells
                .iter()
                .filter(|w| w.side == WellSide::Support && w.is_active)
                .collect(),
        );

        // 7. 推土机自适应评分
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .unwrap_or(false);
        let regime = ctx.get_cached::<TrendStructure>(ContextKey::RegimeStructure);

        let raw_score = if is_tsunami {
            match regime {
                Some(TrendStructure::StrongBullish) | Some(TrendStructure::Bullish) => {
                    (total_sup + total_res * 0.75) * 40.0
                }
                Some(TrendStructure::StrongBearish) | Some(TrendStructure::Bearish) => {
                    (-total_res - total_sup * 0.75) * 40.0
                }
                _ => (total_sup * 0.3 - total_res * 0.3) * 40.0,
            }
        } else {
            (total_sup - total_res) * 40.0
        };

        let sensitivity_mult = if is_tsunami { 0.6 } else { 1.0 };
        let final_score = (raw_score * sensitivity_mult).clamp(-100.0, 100.0);

        ctx.set_cached(ContextKey::SpaceGravityWells, wells);
        ctx.set_cached(ContextKey::GravitySigma, sigma);

        Ok(
            AnalysisResult::new(self.kind(), "LEVEL_PROX_ADAPTIVE".into())
                .with_score(final_score)
                .because(format!(
                    "模式:{} | 净场强 S:{:.2} R:{:.2}",
                    if is_tsunami {
                        "突破/推土"
                    } else {
                        "标准/拦截"
                    },
                    total_sup,
                    total_res
                )),
        )
    }
}
