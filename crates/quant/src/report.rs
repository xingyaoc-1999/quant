use crate::{
    analyzer::{ContextKey, FinalSignal, MarketContext},
    config::AnalyzerConfig,
    risk_manager::{RiskAssessment, RiskManager},
    types::{
        futures::Role,
        gravity::{PriceGravityWell, WellSide},
        market::{TradeDirection, TrendStructure},
        session::TradingSession,
    },
    utils::math::dynamic_direction_threshold,
};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ==================== 报告格式化辅助器 ====================
struct ReportFormatter;

impl ReportFormatter {
    /// 根据价格绝对值动态选择小数位数
    fn price(val: f64) -> String {
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
    }

    /// 转义后的价格字符串
    fn price_esc(val: f64) -> String {
        escape_markdown_v2(&Self::price(val))
    }

    /// 普通数值格式化（指定精度）
    fn raw(val: f64, prec: usize) -> String {
        format!("{:.1$}", val, prec)
    }

    /// 转义后的普通数值
    fn raw_esc(val: f64, prec: usize) -> String {
        escape_markdown_v2(&Self::raw(val, prec))
    }

    /// 置信乘数转星级 (0.4~1.6 → ★☆☆☆☆)
    fn confidence_stars(mult: f64) -> String {
        let stars = ((mult - 0.4) / 0.3).clamp(0.0, 4.0).round() as usize + 1;
        "★".repeat(stars) + &"☆".repeat(5 - stars)
    }
}

// ==================== 主结构体 ====================
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

        let funding_rate = Some(ctx.global.funding_rate);

        let risk_mgr = RiskManager::new(config.clone());

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

        let direction = dynamic_direction_threshold(
            self.signal.net_score,
            vol_p,
            regime,
            estimated_confidence,
            config.risk.direction_base_threshold,
        );

        let max_loss_pct = Some(config.risk.max_loss_per_trade);

        let risk = risk_mgr.assess(
            direction,
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
            10.0,
        )?;

        self.risk_assessment = Some(risk);
        self.risk_assessment.as_ref()
    }

    pub fn to_markdown_v2(&self, ctx: &MarketContext) -> String {
        let header = self.build_header(ctx);
        let metrics = self.build_metrics();
        let wells = self.build_wells_section();
        let fakeout = self.build_fakeout_warning();
        let risk = self.build_risk_section();

        let mut parts = vec![header, metrics];
        if !wells.is_empty() {
            parts.push(wells);
        }
        let mut msg = parts.join("\n");
        if !fakeout.is_empty() {
            msg.push_str(&fakeout);
        }
        if !risk.is_empty() {
            msg.push_str(&risk);
        }

        msg
    }

    // ---------- 私有构建方法 ----------
    fn build_header(&self, ctx: &MarketContext) -> String {
        let signal = &self.signal;
        let session = TradingSession::from_timestamp(ctx.global.timestamp);
        let session_str = escape_markdown_v2(session.as_str());

        let dir_icon = if signal.net_score > 0.0 {
            "📈"
        } else if signal.net_score < 0.0 {
            "📉"
        } else {
            "➖"
        };

        let tsunami = self
            .risk_assessment
            .as_ref()
            .map_or(false, |r| r.is_tsunami);
        let tsunami_tag = if tsunami { " 🌊 *TSUNAMI*" } else { "" };

        format!(
            "*{symbol}*  {dir}  `{session}`{tsunami}",
            symbol = escape_markdown_v2(signal.symbol.as_str()),
            dir = dir_icon,
            session = session_str,
            tsunami = tsunami_tag,
        )
    }

    fn build_metrics(&self) -> String {
        let signal = &self.signal;
        let snapshot = &self.snapshot;

        let status_icon = if signal.is_rejected { "❌" } else { "✅" };
        let oi_str = format!(
            "{:+}",
            ReportFormatter::raw(snapshot.entry_oi_change * 100.0, 2)
        );

        let mut lines = Vec::new();
        lines.push("─────────".to_string());
        lines.push(format!(
            "🎯 Score: `{score}` {status}  💧 OI Δ: `{oi}%`",
            score = ReportFormatter::raw(signal.net_score, 0),
            status = status_icon,
            oi = oi_str,
        ));
        lines.push(format!(
            "💵 Price: `${}`",
            ReportFormatter::price(snapshot.price)
        ));
        lines.join("\n")
    }

    fn build_wells_section(&self) -> String {
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
                    ReportFormatter::price_esc(w.level),
                    ReportFormatter::raw_esc(w.strength, 1)
                )
            })
            .collect::<Vec<_>>()
            .join("  ");

        if wells_str.is_empty() {
            String::new()
        } else {
            format!("🧲 Wells: {}", wells_str)
        }
    }

    fn build_fakeout_warning(&self) -> String {
        let fakeout_score = self
            .signal
            .sub_reports
            .iter()
            .find(|r| r.kind == crate::analyzer::AnalyzerKind::Fakeout)
            .map(|r| r.score)
            .unwrap_or(0.0);

        if fakeout_score >= -10.0 {
            return String::new();
        }

        let icon = if fakeout_score < -30.0 {
            "🚨"
        } else {
            "⚠️"
        };
        format!(
            "\n⚠️ Fakeout: {icon} `{score:.0}`",
            icon = icon,
            score = fakeout_score
        )
    }
    fn build_risk_section(&self) -> String {
        let risk = match &self.risk_assessment {
            Some(r) => r,
            None => return String::new(),
        };

        // 方向字符串需要转义
        let dir_str = escape_markdown_v2(risk.direction.as_str());
        let conf_stars = ReportFormatter::confidence_stars(risk.confidence_mult);
        // 注意：size_pct 和 wrr 可能包含小数点，必须转义
        let size_pct = escape_markdown_v2(&ReportFormatter::raw(risk.position_size_pct * 100.0, 1));
        let wrr = escape_markdown_v2(&ReportFormatter::raw(risk.weighted_rr, 2));

        let sl_str = if risk.stop_loss_levels.len() >= 2 {
            format!(
                "{}/{}",
                ReportFormatter::price(risk.stop_loss_levels[0]),
                ReportFormatter::price(risk.stop_loss_levels[1])
            )
        } else {
            ReportFormatter::price(risk.stop_loss_levels[0])
        };
        // 止损价格可能包含小数点，也需要转义
        let sl_str = escape_markdown_v2(&sl_str);

        // 预转义固定特殊符号
        let sep = escape_markdown_v2("|");
        let lparen = escape_markdown_v2("(");
        let rparen = escape_markdown_v2(")");
        let lbrace = escape_markdown_v2("{");
        let rbrace = escape_markdown_v2("}");

        let entry_lines: Vec<String> = risk
            .entry_levels
            .iter()
            .zip(&risk.entry_allocations)
            .enumerate()
            .map(|(i, (&level, &alloc))| {
                let level_str = ReportFormatter::price_esc(level);
                let alloc_str = escape_markdown_v2(&format!("{:.0}", alloc * 100.0));
                format!(
                    "  ▸ ENTRY{}: `${}`  {}{}%{}",
                    i + 1,
                    level_str,
                    lbrace,
                    alloc_str,
                    rbrace,
                )
            })
            .collect();

        let tp_line = |idx: usize| -> String {
            let tp = ReportFormatter::price_esc(risk.take_profit_levels[idx]);
            let rr = escape_markdown_v2(&ReportFormatter::raw(risk.rr_levels[idx], 1));
            let alloc = escape_markdown_v2(&ReportFormatter::raw(risk.allocation[idx] * 100.0, 0));
            format!(
                "TP{}: `${}`  {}RR:{} {} {}%{}",
                idx + 1,
                tp,
                lparen,
                rr,
                sep,
                alloc,
                rparen
            )
        };

        let mut msg = String::new();
        msg.push_str("\n\n──────────\n");
        msg.push_str("*📊 风险管理*\n");
        msg.push_str(&format!(
            "🧭 方向: `{}`   {}   💰 仓位: `{}%`",
            dir_str, sep, size_pct
        ));
        msg.push_str(&format!("\n🛑 止损: `{}`", sl_str));
        msg.push_str("\n\n*🚪 入场计划*\n");
        msg.push_str(&entry_lines.join("\n"));
        msg.push_str("\n\n*🎯 止盈目标*\n");
        msg.push_str(&format!(
            "  {}\n  {}\n  {}\n",
            tp_line(0),
            tp_line(1),
            tp_line(2)
        ));
        msg.push_str(&format!(
            "\n⚖️ 加权盈亏比: `{}`   {}   ⭐ 置信度: `{}`",
            wrr, sep, conf_stars
        ));
        // 亏损百分比数值需要转义
        let total_loss_str = escape_markdown_v2(&format!("{:.2}", risk.estimated_loss_pct * 100.0));
        let margin_loss_str = escape_markdown_v2(&format!("{:.2}", risk.margin_loss_pct * 100.0));
        msg.push_str(&format!("\n\n💸 总资金亏损: `{}%`", total_loss_str));
        msg.push_str(&format!(
            "\n📉 保证金亏损 \\(10x\\): `{}%`",
            margin_loss_str
        ));

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
