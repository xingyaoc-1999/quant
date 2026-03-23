#[derive(Debug, Clone)]
pub struct PositionTracker {
    pub entry: AIAuditPayload,
    pub context: MarketContext,
    pub entry_time: DateTime<Utc>,
    pub last_eval_time: DateTime<Utc>,
    pub trailing_stop: Option<f64>,
    pub status: PositionStatus,
}

impl PositionTracker {
    pub fn new(entry: AIAuditPayload, initial_context: MarketContext) -> Self {
        Self {
            entry,
            context: initial_context,
            entry_time: Utc::now(),
            last_eval_time: Utc::now(),
            trailing_stop: None,
            status: PositionStatus::Open,
        }
    }

    /// 更新市场数据并重新评估
    pub fn update(
        &mut self,
        new_context: MarketContext,
        analyzers: &[Box<dyn Analyzer>],
        config: &Config,
    ) {
        self.context = new_context;
        self.last_eval_time = Utc::now();

        // 检查硬性失效
        if let Some(exit) = self.check_hard_stops() {
            self.status = PositionStatus::Closed {
                exit_reason: exit.reason,
                exit_price: exit.price,
                pnl: exit.pnl,
            };
            return;
        }

        // 重新运行部分分析器，获取当前评分和风险
        let mut shared = SharedAnalysisState::default(); // 可以复用入场时的状态
        let current_score = self.recalculate_score(analyzers, config, &mut shared);
        if current_score < self.entry.score_engine.final_score * 0.5 {
            // 例如分数下降超过50%
            self.status = PositionStatus::Closed {
                exit_reason: ExitReason::ScoreDeterioration,
                exit_price: self.context.current_price,
                pnl: self.calculate_pnl(self.context.current_price),
            };
            return;
        }

        // 更新移动止损
        self.update_trailing_stop(config);
    }

    fn check_hard_stops(&self) -> Option<ExitInfo> {
        let rules = &self.entry.trade_management_audit.invalidation_rules;
        let current = self.context.current_price;
        let direction = self.entry.header.direction;

        // 止损/止盈检查
        let stop = self.entry.trade_management_audit.stop_loss;
        let target = self.entry.trade_management_audit.take_profit;
        if (direction == Direction::Long && current <= stop)
            || (direction == Direction::Short && current >= stop)
        {
            return Some(ExitInfo {
                reason: ExitReason::StopLoss,
                price: stop,
                pnl: self.calculate_pnl(stop),
            });
        }
        if (direction == Direction::Long && current >= target)
            || (direction == Direction::Short && current <= target)
        {
            return Some(ExitInfo {
                reason: ExitReason::TakeProfit,
                price: target,
                pnl: self.calculate_pnl(target),
            });
        }

        // 时间止损
        if let Some(time_min) = rules.time_stop_minutes {
            if (Utc::now() - self.entry_time).num_minutes() >= time_min as i64 {
                return Some(ExitInfo {
                    reason: ExitReason::TimeStop,
                    price: current,
                    pnl: self.calculate_pnl(current),
                });
            }
        }

        // 结构失效
        if let Some(struct_stop) = rules.structure_stop_price {
            if (direction == Direction::Long && current <= struct_stop)
                || (direction == Direction::Short && current >= struct_stop)
            {
                return Some(ExitInfo {
                    reason: ExitReason::StructureInvalidation,
                    price: current,
                    pnl: self.calculate_pnl(current),
                });
            }
        }

        None
    }

    fn recalculate_score(
        &self,
        analyzers: &[Box<dyn Analyzer>],
        config: &Config,
        shared: &mut SharedAnalysisState,
    ) -> f64 {
        // 只运行与持仓相关的分析器，例如趋势、动量、背离等
        let relevant_kinds = vec![
            AnalyzerKind::TrendStrength,
            AnalyzerKind::Momentum,
            AnalyzerKind::Divergence,
        ];
        let mut total = 0.0;
        let mut weight_sum = 0.0;
        for analyzer in analyzers {
            if relevant_kinds.contains(&analyzer.kind()) {
                if let Ok(result) = analyzer.analyze(&self.context, config, shared) {
                    total += result.score * config.weights.get(&analyzer.kind()).unwrap_or(&0.0);
                    weight_sum += config.weights.get(&analyzer.kind()).unwrap_or(&0.0);
                }
            }
        }
        if weight_sum > 0.0 {
            total / weight_sum
        } else {
            0.0
        }
    }

    fn update_trailing_stop(&mut self, config: &Config) {
        // 示例：基于 ATR 的移动止损
        if let Some(atr) = self.context.get_role(Role::Entry).feature_set.atr {
            let multiplier = config.risk_config.atr_multiplier;
            let new_stop = if self.entry.header.direction == Direction::Long {
                self.context.current_price - atr * multiplier
            } else {
                self.context.current_price + atr * multiplier
            };
            // 只允许向有利方向移动止损
            if let Some(current) = self.trailing_stop {
                if (self.entry.header.direction == Direction::Long && new_stop > current)
                    || (self.entry.header.direction == Direction::Short && new_stop < current)
                {
                    self.trailing_stop = Some(new_stop);
                }
            } else {
                self.trailing_stop = Some(new_stop);
            }
        }
    }

    fn calculate_pnl(&self, exit_price: f64) -> f64 {
        // 根据方向计算盈亏（百分比或绝对值）
        let entry = self.entry.trade_management_audit.entry_price;
        match self.entry.header.direction {
            Direction::Long => (exit_price - entry) / entry,
            Direction::Short => (entry - exit_price) / entry,
        }
    }
}

struct ExitInfo {
    reason: ExitReason,
    price: f64,
    pnl: f64,
}
