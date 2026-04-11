use crate::{
    analyzer::{ContextKey, FinalSignal, MarketContext, Role},
    risk_manager::{RiskAssessment, RiskManager, TradeDirection},
    types::*,
};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct AnalysisAudit {
    pub signal: FinalSignal,
    pub snapshot: MarketSnapshot,
    pub gravity_wells: Vec<PriceGravityWell>,
    pub risk_assessment: Option<RiskAssessment>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct MarketSnapshot {
    pub timestamp: i64,
    pub price: f64,
    pub trend_price_change: f64,
    pub trend_taker_ratio: f64,
    pub filter_volume_ratio: f64,
    pub filter_vol_percentile: f64,
    pub entry_oi_change: f64,
    pub entry_taker_ratio: f64,
}

impl AnalysisAudit {
    pub fn build(ctx: &MarketContext, signal: FinalSignal) -> Self {
        let trend = ctx.get_role(Role::Trend).ok();
        let filter = ctx.get_role(Role::Filter).ok();
        let entry = ctx.get_role(Role::Entry).ok();

        let snapshot = MarketSnapshot {
            timestamp: Utc::now().timestamp_millis(),
            price: ctx.global.last_price,

            trend_price_change: trend
                .map(|r| {
                    let open = r.feature_set.price_action.open;
                    if open > f64::EPSILON {
                        (r.feature_set.price_action.close / open) - 1.0
                    } else {
                        0.0
                    }
                })
                .unwrap_or(0.0),

            trend_taker_ratio: trend
                .and_then(|r| r.taker_flow.taker_buy_ratio)
                .unwrap_or(0.5),

            filter_volume_ratio: filter
                .and_then(|r| {
                    let current_vol = r.feature_set.price_action.volume;
                    r.feature_set.indicators.volume_ma_20.map(|ma| {
                        if ma > f64::EPSILON {
                            current_vol / ma
                        } else {
                            1.0
                        }
                    })
                })
                .unwrap_or(1.0),

            filter_vol_percentile: ctx
                .get_cached::<f64>(ContextKey::VolPercentile)
                .unwrap_or(50.0),

            entry_oi_change: entry
                .and_then(|r| r.oi_data.as_ref())
                .map(|oi| oi.change_history.last().cloned().unwrap_or(0.0))
                .unwrap_or(0.0),

            entry_taker_ratio: entry
                .and_then(|r| r.taker_flow.taker_buy_ratio)
                .unwrap_or(0.5),
        };

        let gravity_wells = ctx
            .get_cached::<Vec<PriceGravityWell>>(ContextKey::SpaceGravityWells)
            .unwrap_or_default();

        Self {
            signal,
            snapshot,
            gravity_wells,
            risk_assessment: None,
        }
    }

    pub fn attach_risk(
        &mut self,
        ctx: &MarketContext,
        direction: TradeDirection,
    ) -> Option<&RiskAssessment> {
        if direction == TradeDirection::None {
            return None;
        }

        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .unwrap_or(0.005);
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .unwrap_or(50.0);
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .unwrap_or(TrendStructure::Range);
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .unwrap_or(false);
        let taker = ctx
            .get_role(Role::Entry)
            .ok()
            .and_then(|r| r.taker_flow.taker_buy_ratio)
            .unwrap_or(0.5);
        let ma_dist = ctx
            .get_role(Role::Trend)
            .ok()
            .and_then(|r| r.feature_set.space.ma20_dist_ratio);

        let risk = RiskManager::assess(
            direction,
            &self.gravity_wells,
            self.snapshot.price,
            atr_ratio,
            vol_p,
            regime,
            is_tsunami,
            taker,
            ma_dist,
        )?;

        self.risk_assessment = Some(risk);
        self.risk_assessment.as_ref()
    }
    pub fn to_markdown_v2(&self) -> String {
        let signal = &self.signal;
        let snapshot = &self.snapshot;

        let dir = if signal.net_score > 0.0 {
            "📈"
        } else {
            "📉"
        };
        let status = if signal.is_rejected { "❌" } else { "✅" };

        let active_wells: Vec<_> = self.gravity_wells.iter().filter(|w| w.is_active).collect();
        let wells_str = if active_wells.is_empty() {
            "None".to_string()
        } else {
            active_wells
                .iter()
                .take(3)
                .map(|w| {
                    let icon = match w.side {
                        WellSide::Support => "🟢",
                        WellSide::Resistance => "🔴",
                        WellSide::Magnet => "🧲",
                    };
                    format!("{}{:.0}", icon, w.level)
                })
                .collect::<Vec<_>>()
                .join(" ")
        };

        let mut msg = format!(
            "*{}* {}\n\
             ━━━━━━━━━━━━━━━\n\
             Score: `{:.1}` {}\n\
             Price: `${:.2}`\n\
             Vol: `{:.0}%` \\| OI: `{:+.2}%`\n\
             Wells: {}",
            escape_markdown_v2(&signal.symbol.to_string()),
            dir,
            signal.net_score,
            status,
            snapshot.price,
            snapshot.filter_vol_percentile,
            snapshot.entry_oi_change * 100.0,
            wells_str,
        );

        if let Some(risk) = &self.risk_assessment {
            let dir_str = match risk.direction {
                TradeDirection::Long => "LONG",
                TradeDirection::Short => "SHORT",
                TradeDirection::None => "NONE",
            };

            msg.push_str(&format!(
                "\n━━━━━━━━━━━━━━━\n\
                 *Risk*\n\
                 Dir: {}\n\
                 Size: `{:.0}%`\n\
                 SL: `${:.0}` \\| `${:.0}` \\| `${:.0}`\n\
                 TP: `${:.0}` \\| `${:.0}` \\| `${:.0}`\n\
                 RR: `{:.2}` \\| `{:.2}` \\| `{:.2}`\n\
                 Weighted RR: `{:.2}`\n\
                 Allocation: `{:.0}%` \\| `{:.0}%` \\| `{:.0}%`\n\
                 Confidence: `{:.0}%`\n\
                 Tags: {}",
                dir_str,
                risk.position_size_pct * 100.0,
                risk.stop_loss_levels[0],
                risk.stop_loss_levels[1],
                risk.stop_loss_levels[2],
                risk.take_profit_levels[0],
                risk.take_profit_levels[1],
                risk.take_profit_levels[2],
                risk.rr_levels[0],
                risk.rr_levels[1],
                risk.rr_levels[2],
                risk.weighted_rr,
                risk.allocation[0] * 100.0,
                risk.allocation[1] * 100.0,
                risk.allocation[2] * 100.0,
                risk.confidence_mult * 100.0,
                escape_markdown_v2(&risk.audit_tags.join(", ")),
            ));
        }

        msg
    }
}
fn escape_markdown_v2(s: &str) -> String {
    const SPECIAL_CHARS: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];

    let mut result = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if SPECIAL_CHARS.contains(&c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}
