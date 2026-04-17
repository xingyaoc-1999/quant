use crate::analyzer::{
    AnalysisError, AnalysisResult, Analyzer, AnalyzerKind, ConfigurableAnalyzer, ContextKey,
    MarketContext, Role,
};
use crate::config::AnalyzerConfig;
use crate::types::gravity::PriceGravityWell;
use crate::types::market::{RsiState, VolumeState};
use crate::types::session::TradingSession;
use std::collections::HashMap;

// 假突破状态： (连续确认次数, 冷却剩余K线数)
type FakeoutState = HashMap<String, (usize, usize)>;

// ==================== FakeoutExtra ====================
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

// ==================== FakeoutDetector ====================
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
        let entry_role = ctx.get_role(Role::Entry)?;
        let fs = &entry_role.feature_set;
        let price = &fs.price_action;

        let wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .cloned()
            .unwrap_or_default();

        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .copied()
            .unwrap_or(0.005);
        let atr = atr_ratio * ctx.global.last_price;

        if atr <= f64::EPSILON || wells.is_empty() {
            return Ok(AnalysisResult::new(self.kind())
                .with_score(0.0)
                .because("ATR 无效或无引力井，跳过检测"));
        }

        let slope = fs.structure.ma20_slope.unwrap_or(0.0);
        let rsi_state = fs.structure.rsi_state.unwrap_or(RsiState::Neutral);
        let efficiency = ctx
            .get_cached::<f64>(ContextKey::LastEfficiency)
            .copied()
            .unwrap_or(0.5);
        let rvol = ctx
            .get_cached::<f64>(ContextKey::LastRVol)
            .copied()
            .unwrap_or(1.0);
        let volume_state = ctx
            .get_cached::<VolumeState>(ContextKey::VolumeState)
            .copied()
            .unwrap_or(VolumeState::Normal);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);

        let session = TradingSession::from_timestamp(ctx.global.timestamp);
        let session_adj = session.factor(&self.config.session);
        let vol_bias = Self::compute_vol_bias(vol_p);

        let state: FakeoutState = ctx
            .get_cached::<FakeoutState>(ContextKey::FakeoutState)
            .cloned()
            .unwrap_or_default();

        let cfg = &self.config.fakeout;

        // 处理每个活跃井，收集惩罚结果
        let (total_penalty, reasons, new_state) = wells.iter().filter(|w| w.is_active).fold(
            (0.0, Vec::new(), state.clone()),
            |(penalty, mut reasons, mut st), well| {
                if let Some((p, reason)) = self.process_well(
                    well,
                    price,
                    atr,
                    slope,
                    rsi_state,
                    efficiency,
                    rvol,
                    volume_state,
                    session_adj,
                    vol_bias,
                    cfg,
                    &mut st,
                ) {
                    reasons.push(reason);
                    (penalty + p, reasons, st)
                } else {
                    (penalty, reasons, st)
                }
            },
        );

        ctx.set_cached(ContextKey::FakeoutState, new_state);

        let final_score = total_penalty.clamp(-100.0, 100.0);
        let mult = match final_score {
            s if s < -30.0 => cfg.fakeout_mult_penalty(),
            s if s < -10.0 => cfg.minor_fakeout_mult(),
            _ => 1.0,
        };

        let description = match final_score {
            s if s < -30.0 => "强烈假突破信号",
            s if s < 0.0 => "疑似假突破",
            _ => "未发现假突破",
        };

        let extra = FakeoutExtra {
            total_penalty,
            wells_scanned: wells.iter().filter(|w| w.is_active).count(),
            efficiency,
            rvol,
            session: format!("{:?}", session),
            session_adj,
            vol_adapt: vol_bias,
        };

        Ok(AnalysisResult::new(self.kind())
            .with_score(final_score)
            .with_mult(mult)
            .because(description)
            .because(reasons.join("; "))
            .with_extra(extra))
    }
}

impl FakeoutDetector {
    fn compute_vol_bias(vol_p: f64) -> f64 {
        match vol_p {
            v if v > 80.0 => 0.8,
            v if v < 20.0 => 1.2,
            _ => 1.0,
        }
    }

    fn process_well(
        &self,
        well: &PriceGravityWell,
        price: &crate::types::market::PriceAction,
        atr: f64,
        slope: f64,
        rsi_state: RsiState,
        efficiency: f64,
        rvol: f64,
        volume_state: VolumeState,
        session_adj: f64,
        vol_bias: f64,
        cfg: &crate::config::FakeoutConfig,
        state: &mut FakeoutState,
    ) -> Option<(f64, String)> {
        let level = well.level;
        let strength = well.strength.clamp(0.2, 3.0);
        let well_key = format!("{:.2}_{:?}", level, well.side);

        let breach_up = Self::is_breach(
            price.high,
            price.low,
            level,
            atr,
            cfg.breach_atr_mult(),
            true,
        );
        let breach_down = Self::is_breach(
            price.high,
            price.low,
            level,
            atr,
            cfg.breach_atr_mult(),
            false,
        );

        // 未突破：更新冷却/重置计数
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

        // 突破但未收盘收回：清除状态
        if !Self::is_closed_inside(price.close, level, cfg.close_return_threshold()) {
            state.remove(&well_key);
            return None;
        }

        let (count, cooldown) = state.entry(well_key.clone()).or_insert((0, 0));
        if *cooldown > 0 {
            *cooldown -= 1;
            return None;
        }

        *count += 1;
        if *count < cfg.fakeout_confirm_bars() {
            return None;
        }

        // 计算惩罚
        let mut penalty = cfg.fakeout_base_penalty() * strength;
        let mut adjustment = Self::slope_adjustment(breach_up, slope, cfg);
        adjustment *= Self::rsi_adjustment(breach_up, breach_down, rsi_state, cfg);
        adjustment *= Self::volume_efficiency_penalty(efficiency, rvol, volume_state, cfg);
        adjustment *= vol_bias * session_adj;

        penalty *= adjustment;

        let reason = format!(
            "{}: {}突破后收盘收回 (adj={:.2}, eff={:.2}, rvol={:.2})",
            well.source_string(),
            if breach_up { "向上" } else { "向下" },
            adjustment,
            efficiency,
            rvol
        );

        // 触发后重置计数并进入冷却
        *count = 0;
        *cooldown = cfg.fakeout_cooldown_bars();

        Some((-penalty, reason))
    }

    fn is_breach(high: f64, low: f64, level: f64, atr: f64, mult: f64, is_above: bool) -> bool {
        let threshold = atr * mult;
        if is_above {
            high > level + threshold
        } else {
            low < level - threshold
        }
    }

    fn is_closed_inside(close: f64, level: f64, threshold: f64) -> bool {
        (close - level).abs() / level < threshold
    }

    fn slope_adjustment(breach_up: bool, slope: f64, cfg: &crate::config::FakeoutConfig) -> f64 {
        let strong_threshold = cfg.slope_strong_threshold();
        if breach_up && slope > strong_threshold {
            cfg.slope_strong_factor()
        } else if !breach_up && slope < -strong_threshold {
            cfg.slope_strong_factor()
        } else if breach_up && slope < -strong_threshold {
            cfg.slope_weak_factor()
        } else if !breach_up && slope > strong_threshold {
            cfg.slope_weak_factor()
        } else {
            1.0
        }
    }

    fn rsi_adjustment(
        breach_up: bool,
        breach_down: bool,
        rsi_state: RsiState,
        cfg: &crate::config::FakeoutConfig,
    ) -> f64 {
        let mut adj = 1.0;
        if breach_up && rsi_state == RsiState::Overbought {
            adj *= cfg.rsi_overbought_factor();
        }
        if breach_down && rsi_state == RsiState::Oversold {
            adj *= cfg.rsi_oversold_factor();
        }
        adj
    }

    fn volume_efficiency_penalty(
        efficiency: f64,
        rvol: f64,
        volume_state: VolumeState,
        cfg: &crate::config::FakeoutConfig,
    ) -> f64 {
        let mut factor: f64 = 1.0;
        let low_eff = efficiency < cfg.vol_eff_low_threshold();
        let high_eff = efficiency > cfg.vol_eff_high_threshold();
        let surge = rvol > cfg.vol_surge_mult();
        let shrink = rvol < cfg.vol_shrink_mult();

        if low_eff && surge {
            factor *= 1.4;
        } else if high_eff && surge {
            factor *= 0.7;
        } else if shrink {
            factor *= 0.9;
        }

        if volume_state == VolumeState::Expand && low_eff {
            factor *= 1.2;
        } else if volume_state == VolumeState::Shrink {
            factor *= 0.9;
        }

        factor.clamp(0.5, 2.0)
    }
}
