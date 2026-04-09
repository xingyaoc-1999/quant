use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ContextKey, MarketContext, Role,
};
use crate::types::{PriceAction, PriceGravityWell, WellSide};
use serde_json::json;

pub struct VolumeStructureAnalyzer;

impl VolumeStructureAnalyzer {
    fn calculate_efficiency(p_action: &PriceAction, avg_volume: f64, last_price: f64) -> f64 {
        let epsilon = 1e-9;

        let rvol = p_action.volume / (avg_volume + epsilon);

        let spread_pct = (p_action.close - p_action.open).abs() / (last_price + epsilon);

        if rvol < 0.1 {
            return 0.0;
        }

        ((spread_pct * 100.0) / rvol).min(10.0)
    }
}

impl Analyzer for VolumeStructureAnalyzer {
    fn name(&self) -> &'static str {
        "volume_structure"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::VolumeProfile
    }

    fn analyze(&self, ctx: &mut MarketContext) -> Result<AnalysisResult, AnalysisError> {
        let last_price = ctx.global.last_price;

        let role_data = ctx
            .get_role(Role::Filter)
            .or_else(|_| ctx.get_role(Role::Trend))?;

        let p_action = &role_data.feature_set.price_action;
        let avg_volume = role_data
            .feature_set
            .indicators
            .volume_ma_20
            .unwrap_or_else(|| role_data.feature_set.price_action.volume);

        let sigma = ctx.get_cached::<f64>(ContextKey::Sigma).unwrap_or(0.005);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();

        let mut m_vol = 1.0;
        let mut score = 0.0;
        let mut res = AnalysisResult::new(self.kind(), "VSA_CORE".into());

        let is_up = p_action.close > p_action.open;
        let rvol = p_action.volume / (avg_volume + 1e-9);
        let efficiency = Self::calculate_efficiency(p_action, avg_volume, last_price);

        let active_well = wells.iter().filter(|w| w.is_active).min_by(|a, b| {
            let score_a = a.distance_pct.abs() / (a.strength + 0.1);
            let score_b = b.distance_pct.abs() / (b.strength + 0.1);
            score_a
                .partial_cmp(&score_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some(well) = active_well {
            let dist_to_well = well.distance_pct.abs();
            let in_critical_zone = dist_to_well < sigma * 1.5;

            if in_critical_zone {
                match well.side {
                    WellSide::Resistance => {
                        if is_up && rvol > 1.2 && efficiency > 1.5 {
                            score = 40.0;
                            m_vol = 1.6;
                            res = res
                                .because(format!("吸收突破: 巨量且高效击穿阻力 {}", well.source));
                        } else if is_up && rvol > 2.0 && efficiency < 0.4 {
                            score = -75.0; // 极强空头信号
                            m_vol = 0.5;
                            res = res
                                .violate()
                                .because(format!("派发陷阱: {} 处爆量滞涨，供应接管", well.source));
                        } else if !is_up && vol_p < 30.0 && efficiency < 0.5 {
                            score = -25.0;
                            res = res.because("上攻无力：阻力区缺乏买盘需求");
                        }
                    }
                    WellSide::Support => {
                        if !is_up && rvol > 1.5 && efficiency > 1.5 {
                            score = -40.0;
                            m_vol = 1.5;
                            res =
                                res.because(format!("恐慌破位: 卖盘失控贯穿支撑 {}", well.source));
                        } else if !is_up && rvol > 2.0 && efficiency < 0.4 {
                            score = 80.0;
                            m_vol = 2.0;
                            res = res
                                .because(format!("吸筹承接: {} 处爆量止跌，需求涌入", well.source));
                        } else if is_up && vol_p < 30.0 {
                            score = 30.0;
                            res = res.because("供应枯竭：缩量回测支撑有效");
                        }
                    }
                }
            }
        }

        if score == 0.0 {
            let trend_bias: f64 = if is_up { 15.0 } else { -15.0 };
            if vol_p > 50.0 && rvol > 1.0 {
                score = trend_bias;
                m_vol = 1.1;
            }
        }

        Ok(res.with_score(score).with_mult(m_vol).debug(json!({
            "eff": efficiency,
            "rvol": rvol,
            "vol_p": vol_p,
            "target_well": active_well.map(|w| &w.source)
        })))
    }
}
