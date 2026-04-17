use crate::{
    analyzer::{ContextKey, FinalSignal, MarketContext},
    config::AnalyzerConfig,
    risk_manager::{RiskAssessment, RiskManager},
    types::{
        futures::Role,
        gravity::{PriceGravityWell, WellSide},
        market::{TradeDirection, TrendStructure},
    },
};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;

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
                .copied()
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
            .cloned()
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
        config: &AnalyzerConfig,
    ) -> Option<&RiskAssessment> {
        // ---- 提取环境数据 ----
        let vol_p = ctx
            .get_cached::<f64>(ContextKey::VolPercentile)
            .copied()
            .unwrap_or(50.0);
        let regime = ctx
            .get_cached::<TrendStructure>(ContextKey::RegimeStructure)
            .copied()
            .unwrap_or(TrendStructure::Range);
        let is_tsunami = ctx
            .get_cached::<bool>(ContextKey::IsMomentumTsunami)
            .copied()
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

        let atr_ratio = ctx
            .get_cached::<f64>(ContextKey::VolAtrRatio)
            .copied()
            .unwrap_or(0.005);

        let funding_rate = ctx.get_cached::<f64>(ContextKey::FundingRate).copied();

        // 创建风险管理器
        let risk_mgr = RiskManager::new(config.clone());

        // ---- 预估计置信乘数（用于动态阈值） ----
        let is_long_hint = self.signal.net_score > 0.0;
        let estimated_confidence = risk_mgr.estimate_confidence(
            is_long_hint,
            regime,
            taker,
            vol_p,
            ma_dist,
            atr_ratio,
            self.signal.net_score,
            is_tsunami,
            funding_rate,
        );

        // ---- 动态方向判定（使用配置中的基础阈值） ----
        let direction = dynamic_direction_threshold(
            self.signal.net_score,
            vol_p,
            regime,
            estimated_confidence,
            config.risk.direction_base_threshold, // 从配置读取
        );

        let dir = direction?;

        // ---- 完整风险评估（使用配置中的最大亏损比例） ----
        let max_loss_pct = Some(config.risk.max_loss_per_trade);

        let risk = risk_mgr.assess(
            Some(dir),
            &self.gravity_wells,
            self.snapshot.price,
            atr_ratio,
            vol_p,
            regime,
            is_tsunami,
            taker,
            ma_dist,
            self.signal.net_score,
            max_loss_pct,
            funding_rate,
        )?;

        self.risk_assessment = Some(risk);
        self.risk_assessment.as_ref()
    }

    pub fn to_markdown_v2(&self) -> String {
        debug!(?self);

        let signal = &self.signal;
        let snapshot = &self.snapshot;

        let mut fakeout_score = 0.0;
        for report in &signal.sub_reports {
            if report.kind == crate::analyzer::AnalyzerKind::Fakeout {
                fakeout_score = report.score;
                break;
            }
        }

        // 动态价格格式化：根据绝对值决定小数位数
        let fmt_price = |val: f64| {
            let prec = if val.abs() < 1.0 {
                4
            } else if val.abs() < 10.0 {
                3
            } else if val.abs() < 1000.0 {
                2
            } else {
                1
            };
            format!("{:.1$}", val, prec)
        };
        let fmt_price_esc = |val: f64| escape_markdown_v2(&fmt_price(val));

        // 普通数值格式化
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
                format!(
                    "{}{}·{}",
                    icon,
                    fmt_price_esc(w.level),
                    fmt_esc(w.strength, 1)
                )
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

        lines.push(format!("Price: `${}`", fmt_price(snapshot.price)));

        if !wells_str.is_empty() {
            lines.push(format!("Wells: {wells}", wells = wells_str));
        }

        let mut msg = lines.join("\n");

        // 假突破警告
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

        if let Some(risk) = &self.risk_assessment {
            let dir_str = risk.direction.as_str();
            let conf_stars = self.confidence_stars(risk.confidence_mult);

            let size_pct = fmt_raw(risk.position_size_pct * 100.0, 1);

            let sl_str = if risk.stop_loss_levels.len() >= 2 {
                format!(
                    "{}/{}",
                    fmt_price(risk.stop_loss_levels[0]),
                    fmt_price(risk.stop_loss_levels[1])
                )
            } else {
                fmt_price(risk.stop_loss_levels[0])
            };

            let tp_line = |idx: usize| -> String {
                let tp = fmt_price(risk.take_profit_levels[idx]);
                let rr = fmt_esc(risk.rr_levels[idx], 1);
                let alloc = fmt_esc(risk.allocation[idx] * 100.0, 0);
                format!("TP{}: `${}` \\(RR:{} \\| {}%\\)", idx + 1, tp, rr, alloc)
            };

            let entry_lines: Vec<String> = risk
                .entry_levels
                .iter()
                .zip(&risk.entry_allocations)
                .enumerate()
                .map(|(i, (&level, &alloc))| {
                    format!(
                        "ENTRY{}: `${}` \\({}%\\)",
                        i + 1,
                        fmt_price_esc(level),
                        fmt_esc(alloc * 100.0, 0)
                    )
                })
                .collect();

            msg.push_str(&format!(
                "\n\n━━━━━━━━━━━━━━━\n\
                 *Risk Management*\n\
                 Dir: `{dir}`  │  Size: `{size}%`  │  SL: `{sl}`\n\
                 \n\
                 *Entry Plan*\n\
                 {entries}\n\
                 \n\
                 *Take Profit*\n\
                 {tp1}\n\
                 {tp2}\n\
                 {tp3}\n\
                 \n\
                 WRR: `{wrr}`  │  Conf: `{stars}` `{conf}%`",
                dir = dir_str,
                size = size_pct,
                sl = sl_str,
                entries = entry_lines.join("\n"),
                tp1 = tp_line(0),
                tp2 = tp_line(1),
                tp3 = tp_line(2),
                wrr = fmt_raw(risk.weighted_rr, 2),
                stars = conf_stars,
                conf = (risk.confidence_mult * 100.0).round() as i32,
            ));

            msg.push_str(&format!(
                "\n`Est. Loss:` `{:.2}%` of capital",
                risk.estimated_loss_pct * 100.0
            ));
        }

        msg
    }

    /// 将置信乘数 (0.4~1.6) 映射为 1~5 星级
    fn confidence_stars(&self, mult: f64) -> String {
        let stars = ((mult - 0.4) / 0.3).clamp(0.0, 4.0).round() as usize + 1;
        "★".repeat(stars) + &"☆".repeat(5 - stars)
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

/// 动态方向阈值计算（参数从配置传入）
fn dynamic_direction_threshold(
    net_score: f64,
    vol_p: f64,
    regime: TrendStructure,
    confidence_mult: f64,
    base_threshold: f64, // 新增：从配置读取的基础阈值
) -> Option<TradeDirection> {
    let vol_factor = if vol_p > 70.0 {
        1.3
    } else if vol_p < 30.0 {
        0.7
    } else {
        1.0
    };

    let regime_factor = match regime {
        TrendStructure::StrongBullish | TrendStructure::StrongBearish => 0.8,
        TrendStructure::Range => 1.2,
        _ => 1.0,
    };

    let confidence_factor = 1.0 / confidence_mult.clamp(0.5, 2.0);

    let threshold = base_threshold * vol_factor * regime_factor * confidence_factor;

    if net_score > threshold {
        Some(TradeDirection::Long)
    } else if net_score < -threshold {
        Some(TradeDirection::Short)
    } else {
        None
    }
}
