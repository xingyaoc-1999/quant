use crate::{
    analyzer::{ContextKey, FinalSignal, MarketContext, Role},
    risk_manager::{RiskAssessment, RiskConfig, RiskManager, TradeDirection},
    types::*,
};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

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

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct AnalysisAudit {
    pub signal: FinalSignal,
    pub snapshot: MarketSnapshot,
    pub gravity_wells: Vec<PriceGravityWell>,
    pub risk_assessment: Option<RiskAssessment>,
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
        direction: Option<TradeDirection>,
    ) -> Option<&RiskAssessment> {
        let dir = direction?;

        // 1. 提取缓存数据
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

        let max_loss_pct = Some(0.02); // 2% 单笔最大亏损，可根据需要调整
        let leverage = 10.0; // 从配置或合约信息获取，此处示例为10倍

        // 尝试获取资金费率（假设已缓存到 ContextKey::FundingRate）
        let funding_rate = ctx.get_cached::<f64>(ContextKey::FundingRate);

        // 2. 创建风险管理器实例（使用默认配置）
        let risk_mgr = RiskManager::new(RiskConfig::default());

        // 3. 调用新版 assess 方法（注意参数顺序）
        let risk = risk_mgr.assess(
            Some(dir),             // direction
            &self.gravity_wells,   // wells
            self.snapshot.price,   // last_price
            atr_ratio,             // atr_ratio
            vol_p,                 // vol_p
            regime,                // regime
            is_tsunami,            // is_tsunami
            taker,                 // taker_ratio
            ma_dist,               // ma20_dist
            self.signal.net_score, // net_score
            max_loss_pct,          // max_loss_pct
            leverage,              // leverage
            funding_rate,          // funding_rate
        )?;

        self.risk_assessment = Some(risk);
        self.risk_assessment.as_ref()
    }

    pub fn to_markdown_v2(&self) -> String {
        debug!(?self);

        let signal = &self.signal;
        let snapshot = &self.snapshot;

        let mut fakeout_score = 0.0;
        let mut efficiency = None;

        for report in &signal.sub_reports {
            match report.kind {
                crate::analyzer::AnalyzerKind::Fakeout => {
                    fakeout_score = report.score;
                }
                crate::analyzer::AnalyzerKind::VolumeProfile => {
                    if let Some(eff_val) = report.debug_data.get("eff").and_then(|v| v.as_i64()) {
                        efficiency = Some(eff_val as f64 / 100.0);
                    }
                }
                _ => {}
            }
        }

        let fmt_raw = |val: f64, prec: usize| format!("{:.1$}", val, prec);
        let fmt_esc = |val: f64, prec: usize| escape_markdown_v2(&fmt_raw(val, prec));

        let dir_icon = if signal.net_score > 0.0 {
            "📈"
        } else if signal.net_score < 0.0 {
            "📉"
        } else {
            "➖"
        };
        let status_icon = if signal.is_rejected { "❌" } else { "✅" };

        let is_tsunami = self
            .risk_assessment
            .as_ref()
            .map_or(false, |r| r.is_tsunami);
        let tsunami_tag = if is_tsunami { " 🌊 *TSUNAMI*" } else { "" };

        let wells_str = self
            .gravity_wells
            .iter()
            .filter(|w| w.is_active)
            .take(4)
            .map(|w| {
                let icon = match w.side {
                    WellSide::Support => "🟢",
                    WellSide::Resistance => "🔴",
                    WellSide::Magnet => "🧲",
                };
                format!("{}{}·{}", icon, fmt_esc(w.level, 2), fmt_esc(w.strength, 1))
            })
            .collect::<Vec<_>>()
            .join("  ");

        let mut lines = Vec::new();

        lines.push(format!(
            "*{symbol}* {dir}{tsunami}",
            symbol = escape_markdown_v2(signal.symbol.as_str()),
            dir = dir_icon,
            tsunami = tsunami_tag,
        ));

        lines.push("━━━━━━━━━━━━━━━".to_string());
        lines.push(format!(
            "S: `{score}` {status}  │  OI: `{oi}%`",
            score = fmt_raw(signal.net_score, 0),
            status = status_icon,
            oi = format!("{:+}", fmt_raw(snapshot.entry_oi_change * 100.0, 2)),
        ));

        lines.push(format!(
            "Price: `${price}`",
            price = fmt_raw(snapshot.price, 2)
        ));

        if !wells_str.is_empty() {
            lines.push(format!("Wells: {wells}", wells = wells_str));
        }

        let mut msg = lines.join("\n");

        // 假突破显示
        if fakeout_score < -10.0 {
            let fakeout_icon = if fakeout_score < -30.0 {
                "🚨"
            } else {
                "⚠️"
            };
            msg.push_str(&format!(
                "\nFakeout: {icon} `{score:.0}`",
                icon = fakeout_icon,
                score = fakeout_score
            ));
        }

        // 效率显示
        if let Some(eff) = efficiency {
            let eff_disp = ((eff / 5.0).sqrt() * 100.0).round() as i32;
            let (eff_icon, eff_level) = if eff_disp >= 60 {
                ("⚡⚡", "高")
            } else if eff_disp >= 30 {
                ("⚡", "正常")
            } else {
                ("🐢", "低")
            };
            msg.push_str(&format!(
                "\nEfficiency: {icon} `{eff}%` \\({level}\\)",
                icon = eff_icon,
                eff = eff_disp,
                level = eff_level
            ));
        }

        if let Some(risk) = &self.risk_assessment {
            let dir_str = risk.direction.as_str();
            let conf_pct = (risk.confidence_mult * 100.0).round() as i32;

            let conf_val = ((conf_pct as f64) / 10.0).round().clamp(0.0, 10.0) as usize;
            let bar = if conf_val > 0 {
                "■".repeat(conf_val) + &"□".repeat(10 - conf_val)
            } else {
                "□".repeat(10)
            };

            let size_pct = fmt_raw(risk.position_size_pct * 100.0, 1);

            let sl_str = if risk.stop_loss_levels.len() >= 2 {
                format!(
                    "{}/{}",
                    fmt_raw(risk.stop_loss_levels[0], 1),
                    fmt_raw(risk.stop_loss_levels[1], 1)
                )
            } else {
                fmt_raw(risk.stop_loss_levels[0], 1)
            };

            let tp_line = |idx: usize| -> String {
                let tp = fmt_raw(risk.take_profit_levels[idx], 1);
                let rr = fmt_esc(risk.rr_levels[idx], 1);
                let alloc = fmt_esc(risk.allocation[idx] * 100.0, 0);
                format!("TP{}: `${}` \\(RR:{} \\| {}%\\)", idx + 1, tp, rr, alloc)
            };

            msg.push_str(&format!(
                "\n\n━━━━━━━━━━━━━━━\n\
                 *Risk Management*\n\
                 Dir: `{dir}`  │  Size: `{size}%`  │  SL: `{sl}`\n\
                 \n\
                 {tp1}\n\
                 {tp2}\n\
                 {tp3}\n\
                 \n\
                 WRR: `{wrr}`  │  Conf: `[{bar}]` `{conf}%`",
                dir = dir_str,
                size = size_pct,
                sl = sl_str,
                tp1 = tp_line(0),
                tp2 = tp_line(1),
                tp3 = tp_line(2),
                wrr = fmt_raw(risk.weighted_rr, 2),
                bar = bar,
                conf = conf_pct,
            ));

            msg.push_str(&format!(
                "\n`Est. Loss:` `{:.2}%` of capital",
                risk.estimated_loss_pct * 100.0
            ));
        }

        msg
    }
}

// ================= MarkdownV2 转义工具 =================

pub fn escape_markdown_v2(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
            | '{' | '}' | '.' | '!' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}
