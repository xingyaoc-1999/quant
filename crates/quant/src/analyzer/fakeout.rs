use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::{AnalyzerConfig, FakeoutConfig};
use crate::types::gravity::{PriceGravityWell, WellSide, WellSource};
use crate::types::market::{PriceAction, RsiState, VolumeState};
use crate::types::session::TradingSession;
use std::collections::HashMap;

type WellKey = (WellSource, WellSide);
type FakeoutState = HashMap<WellKey, (usize, usize)>;

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct FakeoutExtra {
    pub total_penalty: f64,
    pub wells_scanned: usize,
    pub efficiency: f64,
    pub rvol: f64,
    pub session: String,
    pub session_adj: f64,
    pub vol_adapt: f64,
}

pub struct FakeoutDetector {
    config: AnalyzerConfig,
}

impl ConfigurableAnalyzer for FakeoutDetector {
    fn with_config(config: AnalyzerConfig) -> Self {
        Self { config }
    }
    fn config(&self) -> &AnalyzerConfig {
        &self.config
    }
}

impl Analyzer for FakeoutDetector {
    type Extra = FakeoutExtra;

    fn name(&self) -> &'static str {
        "fakeout_detector_v4"
    }
    fn kind(&self) -> AnalyzerKind {
        AnalyzerKind::Fakeout
    }

    fn dependencies(&self) -> Vec<ContextKey> {
        vec![
            ContextKey::SpaceGravityWells,
            ContextKey::VolAtrRatio,
            ContextKey::LastEfficiency,
            ContextKey::LastRVol,
            ContextKey::VolumeState,
            ContextKey::VolPercentile,
        ]
    }

    fn analyze(
        &self,
        ctx: &mut MarketContext,
    ) -> Result<AnalysisResult<Self::Extra>, AnalysisError> {
        let last_price = ctx.global.last_price;
        if !last_price.is_finite() || last_price <= 0.0 {
            return Ok(AnalysisResult::new(self.kind()).with_score(0.0));
        }

        // 提取数据
        let (price, wells, atr, slope, rsi_state, efficiency, rvol, volume_state, vol_p) = {
            let role_data = ctx
                .get_role(Role::Entry)
                .or_else(|_| ctx.get_role(Role::Trend))?;
            let fs = &role_data.feature_set;

            let atr = ctx
                .get_cached::<f64>(ContextKey::VolAtrRatio)
                .copied()
                .unwrap_or(0.005)
                * last_price;

            (
                fs.price_action.clone(),
                ctx.get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
                    .cloned()
                    .unwrap_or_default(),
                atr,
                fs.structure.ma20_slope.unwrap_or(0.0),
                fs.structure.rsi_state.unwrap_or(RsiState::Neutral),
                ctx.get_cached::<f64>(ContextKey::LastEfficiency)
                    .copied()
                    .unwrap_or(0.5),
                ctx.get_cached::<f64>(ContextKey::LastRVol)
                    .copied()
                    .unwrap_or(1.0),
                ctx.get_cached::<VolumeState>(ContextKey::VolumeState)
                    .copied()
                    .unwrap_or(VolumeState::Normal),
                ctx.get_cached::<f64>(ContextKey::VolPercentile)
                    .copied()
                    .unwrap_or(50.0),
            )
        };

        if atr <= f64::EPSILON || wells.is_empty() {
            return Ok(AnalysisResult::new(self.kind())
                .with_score(0.0)
                .because("数据不足"));
        }

        let session = TradingSession::from_timestamp(ctx.global.timestamp);
        let session_adj = session.factor(&self.config.session);
        let vol_bias = self.compute_vol_bias(vol_p);

        // 状态提取（排除 Magnet 井）
        let mut state = ctx
            .cache
            .remove(&ContextKey::FakeoutState)
            .and_then(|boxed| boxed.downcast::<FakeoutState>().ok())
            .map(|boxed| *boxed)
            .unwrap_or_default();

        let cfg = &self.config.fakeout;
        let mut total_score = 0.0;
        let mut reasons = Vec::with_capacity(4);

        for well in wells
            .iter()
            .filter(|w| w.is_active && w.side != WellSide::Magnet)
        {
            if let Some((score, reason)) = self.process_well(
                well,
                &price,
                atr,
                slope,
                rsi_state,
                efficiency,
                rvol,
                volume_state,
                session_adj,
                vol_bias,
                cfg,
                &mut state,
            ) {
                total_score += score;
                reasons.push(reason);
            }
        }

        ctx.set_cached(ContextKey::FakeoutState, state);

        let final_score = total_score.clamp(-100.0, 100.0);
        let mult = if final_score.abs() > 30.0 {
            cfg.fakeout_mult_penalty()
        } else if final_score.abs() > 10.0 {
            cfg.minor_fakeout_mult()
        } else {
            1.0
        };

        let description = match final_score {
            s if s > 30.0 => "强烈支撑假突破（看涨）",
            s if s > 0.0 => "疑似支撑假突破（偏多）",
            s if s < -30.0 => "强烈阻力假突破（看跌）",
            s if s < 0.0 => "疑似阻力假突破（偏空）",
            _ => "未发现假突破",
        };

        Ok(AnalysisResult::new(self.kind())
            .with_score(final_score)
            .with_mult(mult)
            .because(description)
            .because(reasons.join("; "))
            .with_extra(FakeoutExtra {
                total_penalty: total_score,
                wells_scanned: wells.iter().filter(|w| w.side != WellSide::Magnet).count(),
                efficiency,
                rvol,
                session: format!("{:?}", session),
                session_adj,
                vol_adapt: vol_bias,
            }))
    }
}

impl FakeoutDetector {
    #[inline]
    fn compute_vol_bias(&self, vol_p: f64) -> f64 {
        if vol_p > 80.0 {
            0.8
        } else if vol_p < 20.0 {
            1.2
        } else {
            1.0
        }
    }

    fn process_well(
        &self,
        well: &PriceGravityWell,
        price: &PriceAction,
        atr: f64,
        slope: f64,
        rsi_state: RsiState,
        efficiency: f64,
        rvol: f64,
        volume_state: VolumeState,
        session_adj: f64,
        vol_bias: f64,
        cfg: &FakeoutConfig,
        state: &mut FakeoutState,
    ) -> Option<(f64, String)> {
        // 新键：使用 WellSource + WellSide，不再依赖当前价格
        let well_key = if let Some(source) = well.sources.first() {
            (*source, well.side)
        } else {
            // fallback，理论不会发生
            (WellSource::Ma20, well.side)
        };

        let check_up = well.side == WellSide::Resistance;
        let check_down = well.side == WellSide::Support;

        let breach_up = check_up
            && self.is_breach(
                price.high,
                price.low,
                well.level,
                atr,
                cfg.breach_atr_mult(),
                true,
            );
        let breach_down = check_down
            && self.is_breach(
                price.high,
                price.low,
                well.level,
                atr,
                cfg.breach_atr_mult(),
                false,
            );

        // 未突破：处理冷却
        if !breach_up && !breach_down {
            if let Some((count, cooldown)) = state.get_mut(&well_key) {
                if *cooldown > 0 {
                    *cooldown -= 1;
                } else {
                    *count = 0;
                }
            }
            return None;
        }

        // 收盘必须回到井内
        if !self.is_closed_inside(price.close, well.level, cfg.close_return_threshold()) {
            state.remove(&well_key);
            return None;
        }

        let (count, cooldown) = state.entry(well_key).or_insert((0, 0));
        if *cooldown > 0 {
            *cooldown -= 1;
            return None;
        }

        *count += 1;
        if *count < cfg.fakeout_confirm_bars() {
            return None;
        }

        let strength = well.strength.clamp(0.2, 3.0);
        let adjustment = self.calculate_total_adjustment(
            breach_up,
            breach_down,
            slope,
            rsi_state,
            efficiency,
            rvol,
            volume_state,
            vol_bias,
            session_adj,
            cfg,
        );

        let penalty = cfg.fakeout_base_penalty() * strength * adjustment;

        let score = if breach_up { -penalty } else { penalty };

        let direction_str = if breach_up { "向上" } else { "向下" };
        let bias_str = if breach_up { "看跌" } else { "看涨" };
        let reason = format!(
            "{}: {}突破假突破 → {} (adj={:.2})",
            well.source_string(),
            direction_str,
            bias_str,
            adjustment
        );

        *count = 0;
        *cooldown = cfg.fakeout_cooldown_bars();

        Some((score, reason))
    }

    fn calculate_total_adjustment(
        &self,
        up: bool,
        down: bool,
        slope: f64,
        rsi: RsiState,
        eff: f64,
        rvol: f64,
        vol_s: VolumeState,
        v_bias: f64,
        session_adj: f64,
        cfg: &FakeoutConfig,
    ) -> f64 {
        let slope_adj = self.slope_adjustment(up, slope, cfg);
        let rsi_adj = self.rsi_adjustment(up, down, rsi, cfg);
        let vol_adj = self.volume_efficiency_penalty(eff, rvol, vol_s, cfg);
        slope_adj * rsi_adj * vol_adj * v_bias * session_adj
    }

    #[inline]
    fn is_breach(
        &self,
        high: f64,
        low: f64,
        level: f64,
        atr: f64,
        mult: f64,
        is_above: bool,
    ) -> bool {
        let limit = atr * mult;
        if is_above {
            high > level + limit
        } else {
            low < level - limit
        }
    }

    #[inline]
    fn is_closed_inside(&self, close: f64, level: f64, threshold: f64) -> bool {
        (close - level).abs() / level < threshold
    }

    fn slope_adjustment(&self, up: bool, slope: f64, cfg: &FakeoutConfig) -> f64 {
        let thresh = cfg.slope_strong_threshold();
        match (up, slope) {
            (true, s) if s > thresh => cfg.slope_strong_factor(),
            (false, s) if s < -thresh => cfg.slope_strong_factor(),
            (true, s) if s < -thresh => cfg.slope_weak_factor(),
            (false, s) if s > thresh => cfg.slope_weak_factor(),
            _ => 1.0,
        }
    }

    fn rsi_adjustment(&self, up: bool, down: bool, rsi: RsiState, cfg: &FakeoutConfig) -> f64 {
        match (up, down, rsi) {
            (true, _, RsiState::Overbought) => cfg.rsi_overbought_factor(),
            (_, true, RsiState::Oversold) => cfg.rsi_oversold_factor(),
            _ => 1.0,
        }
    }

    fn volume_efficiency_penalty(
        &self,
        eff: f64,
        rvol: f64,
        vol_s: VolumeState,
        cfg: &FakeoutConfig,
    ) -> f64 {
        let mut factor: f64 = 1.0;
        let is_low_eff = eff < cfg.vol_eff_low_threshold();
        let is_high_eff = eff > cfg.vol_eff_high_threshold();
        let is_surge = rvol > cfg.vol_surge_mult();

        factor *= match (is_low_eff, is_high_eff, is_surge) {
            (true, _, true) => 1.4,
            (_, true, true) => 0.7,
            _ if rvol < cfg.vol_shrink_mult() => 0.9,
            _ => 1.0,
        };

        if vol_s == VolumeState::Expand && is_low_eff {
            factor *= 1.2;
        }
        if vol_s == VolumeState::Shrink {
            factor *= 0.9;
        }

        factor.clamp(0.5, 2.0)
    }
}
