use crate::{
    analyzer::{ContextKey, FinalSignal, MarketContext, Role},
    types::*,
};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct AnalysisAudit {
    /// 引擎给出的综合判定
    pub signal: FinalSignal,

    /// 市场当前“物理体温”
    pub snapshot: MarketSnapshot,

    /// 关键价格引力位 (支撑、压力、共振区)
    pub gravity_wells: Vec<PriceGravityWell>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct MarketSnapshot {
    pub timestamp: i64,

    pub price: f64,

    // --- Role::Trend (大趋势/日线) ---
    #[schemars(description = "Trend周期涨跌幅: (Close/Open)-1")]
    pub trend_price_change: f64,
    #[schemars(description = "Trend周期主买占比")]
    pub trend_taker_ratio: f64,

    // --- Role::Filter (环境/过滤) ---
    #[schemars(description = "Filter周期量比 (当前量/MA)")]
    pub filter_volume_ratio: f64,
    #[schemars(description = "全局波动率分位数")]
    pub filter_vol_percentile: f64,

    // --- Role::Entry (入场/执行) ---
    #[schemars(description = "Entry周期持仓变化")]
    pub entry_oi_change: f64,
    #[schemars(description = "Entry周期即时主买占比")]
    pub entry_taker_ratio: f64,
}

impl AnalysisAudit {
    pub fn build(ctx: &MarketContext, signal: FinalSignal) -> Self {
        // ========== 1. 预提取 Role (性能与借用优化) ==========
        // 这样后续代码中只通过 Option 访问，不再多次进入 ctx 查找
        let trend = ctx.get_role(Role::Trend).ok();
        let filter = ctx.get_role(Role::Filter).ok();
        let entry = ctx.get_role(Role::Entry).ok();
        // ========== 2. 构建市场快照 ==========
        let snapshot = MarketSnapshot {
            // 使用当前 UTC 时间，或者如果有回测需求，可从 ctx.global 获取逻辑时间
            timestamp: Utc::now().timestamp_millis(),
            price: ctx.global.last_price,

            // --- Trend 提取 ---
            trend_price_change: trend
                .map(|r| {
                    let open = r.feature_set.price_action.open;
                    // 防御：防止开盘价为 0 导致的崩溃或 NaN
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

            // --- Filter 提取 ---
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

            // --- Entry 提取 ---
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
        }
    }
}
