use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceGravityWell, TrendStructure, VolumeState};
use serde_json::json;

pub struct LevelProximityAnalyzer;

impl LevelProximityAnalyzer {
    /// 计算引力强度：采用平方衰减模型，距离越近，强度呈指数级上升
    #[inline]
    fn calculate_intensity(dist: f64, threshold: f64) -> f64 {
        if dist >= threshold || threshold <= 0.0 {
            0.0
        } else {
            let ratio = 1.0 - (dist / threshold);
            ratio * ratio // 平滑曲线，边缘触碰感应弱，极近距离感应极强
        }
    }
}

impl Analyzer for LevelProximityAnalyzer {
    fn name(&self) -> &'static str {
        "level_proximity"
    }

    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::SupportResistance
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        // ========== 1. 数据提取 (不可变借用) ==========
        let trend_data = ctx.get_role(Role::Trend)?;
        let filter_data = ctx.get_role(Role::Filter)?;
        let last_price = ctx.global.last_price;

        // 提取 SpaceGeometry 空间几何特征
        let t_space = &trend_data.feature_set.space;
        let f_space = &filter_data.feature_set.space;

        // ========== 2. 环境状态联动 (从 Context 缓存读取) ==========
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .unwrap_or(0.005);
        let is_compressed = ctx
            .get_cached::<bool>(ContextKey::VolIsCompressed)
            .unwrap_or(false);

        // 获取成交量枚举状态
        let vol_state = ctx
            .get_cached::<serde_json::Value>(ContextKey::VolumeState)
            .and_then(|v| serde_json::from_value::<VolumeState>(v).ok());

        // 获取市场结构枚举
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);

        // ========== 3. 动态建模参数 ==========
        // 阈值逻辑：高波动下增加感应半径，低波动(压缩)下收紧半径
        let radius_mult = if is_compressed { 0.45 } else { 0.75 };
        let threshold = atr_ratio * (radius_mult + (vol_p / 250.0));
        let confluence_gate = threshold * 0.3; // 判定 MTF 共振的误差门限

        let mut m_long = 1.0;
        let mut m_short = 1.0;
        let mut gravity_wells = Vec::with_capacity(4);
        let mut res = AnalysisResult::new(self.kind(), "LEVEL_PROX".into());

        // ========== 4. 核心逻辑 A：支撑/阻力位博弈 ==========

        // --- 阻力位探测 ---
        if let Some(dist_res) = t_space.dist_to_resistance {
            let intensity = Self::calculate_intensity(dist_res, threshold);
            if intensity > 0.0 {
                let f_dist = f_space.dist_to_resistance.unwrap_or(f64::INFINITY);
                let is_confluent = (dist_res - f_dist).abs() < confluence_gate;
                let boost = if is_confluent { 1.5 } else { 1.0 };

                gravity_wells.push(PriceGravityWell {
                    level: last_price * (1.0 + dist_res),
                    source: if is_confluent {
                        "MTF_Confluence_Res"
                    } else {
                        "Trend_Res"
                    }
                    .into(),
                    distance_pct: dist_res,
                    strength: intensity * boost,
                });

                match (regime, vol_state) {
                    // 放量突破模式
                    (TrendStructure::StrongBullish, Some(VolumeState::Expand)) => {
                        m_long *= 1.0 + (0.4 * intensity);
                        m_short *= 0.15; // 严禁在放量突破强趋势时摸顶
                        res = res.because("阻力位警告：强趋势放量冲击，预期发生突破");
                    }
                    // 缩量撞墙模式
                    (TrendStructure::Range, Some(VolumeState::Shrink))
                    | (_, Some(VolumeState::Squeeze)) => {
                        m_short *= 1.0 + (1.2 * intensity * boost);
                        m_long *= 0.4;
                        res = res.because("阻力位确认：缩量摸顶，阻力引力生效，看空胜率增加");
                    }
                    _ => {
                        m_long *= 1.0 - (0.6 * intensity);
                    }
                }
            }
        }

        // --- 支撑位探测 ---
        if let Some(dist_sup) = t_space.dist_to_support {
            let intensity = Self::calculate_intensity(dist_sup, threshold);
            if intensity > 0.0 {
                let f_dist = f_space.dist_to_support.unwrap_or(f64::INFINITY);
                let is_confluent = (dist_sup - f_dist).abs() < confluence_gate;
                let boost = if is_confluent { 1.6 } else { 1.0 };

                gravity_wells.push(PriceGravityWell {
                    level: last_price * (1.0 - dist_sup),
                    source: if is_confluent {
                        "MTF_Confluence_Sup"
                    } else {
                        "Trend_Sup"
                    }
                    .into(),
                    distance_pct: -dist_sup,
                    strength: intensity * boost,
                });

                match (regime, vol_state) {
                    // 放量跌破模式
                    (TrendStructure::StrongBearish, Some(VolumeState::Expand)) => {
                        m_short *= 1.1;
                        m_long *= 0.1; // 严禁抄底
                        res = res.because("支撑位崩塌：强熊市放量砸盘，支撑预期失效");
                    }
                    // 缩量回踩模式
                    (TrendStructure::Bullish, _) | (TrendStructure::StrongBullish, _) => {
                        m_long *= 1.0 + (1.3 * intensity * boost);
                        m_short *= 0.3;
                        res = res.because("支撑位确认：顺势健康回踩，共振支撑提供极佳盈亏比");
                    }
                    _ => {
                        m_long *= 1.0 + (0.5 * intensity);
                        m_short *= 1.0 - (0.7 * intensity);
                    }
                }
            }
        }

        // ========== 5. 核心逻辑 B：均线空间几何 (Mean Reversion) ==========

        // 提取乖离率和收敛状态
        let ma_dist = t_space.ma20_dist_ratio.unwrap_or(0.0);
        let ma_converging = t_space.ma_converging.unwrap_or(false);

        // 乖离限制逻辑：橡皮筋拉得太紧必有回调
        let ma_limit = atr_ratio * 3.5;
        if ma_dist.abs() > ma_limit {
            if ma_dist > 0.0 {
                m_long *= 0.6; // 价格远超 MA20，追多风险极大
                res = res.because("空间预警：价格严重偏离 MA20，处于超买拉升末端");
            } else {
                m_short *= 0.6; // 价格远低于 MA20
                res = res.because("空间预警：价格严重下穿 MA20，警惕空头回补风险");
            }
        }

        // 均线收敛逻辑：变盘前奏
        if ma_converging && is_compressed {
            m_long *= 1.15;
            m_short *= 1.15; // 均线密集代表市场处于无方向坍缩，一旦启动就是大行情
            res = res.because("几何收敛：均线族高度密集，系统进入变盘临界点");
        }

        // ========== 6. 最终结果汇总与持久化 ==========

        // 限制范围，防止乘数过载
        m_long = m_long.clamp(0.05, 4.0);
        m_short = m_short.clamp(0.05, 4.0);

        ctx.set_cached(ContextKey::MultLongSpace, json!(m_long));
        ctx.set_cached(ContextKey::MultShortSpace, json!(m_short));
        ctx.set_cached(ContextKey::SpaceGravityWells, json!(gravity_wells));

        // 设定本分析器的最终影响权重
        let final_weight = m_long.max(m_short);
        ctx.set_multiplier(self.kind(), final_weight);

        Ok(res.with_mult(final_weight).debug(json!({
            "m_long": m_long,
            "m_short": m_short,
            "threshold": threshold,
            "ma_dist": ma_dist,
            "is_confluent": gravity_wells.iter().any(|w| w.source.contains("MTF")),
            "regime": format!("{:?}", regime)
        })))
    }
}
