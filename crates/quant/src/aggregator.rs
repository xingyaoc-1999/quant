use crate::analyzer::{
    AnalysisError, Analyzer, AnalyzerStage, Config, MarketContext, SharedAnalysisState,
};
use crate::types::{AIAuditPayload, AIAuditPayloadBuilder, AuditHeader, ScoreEngineSummary};
use chrono::Utc;

pub struct Aggregator {
    analyzers: Vec<Box<dyn Analyzer>>,
}

impl Aggregator {
    pub fn new(analyzers: Vec<Box<dyn Analyzer>>) -> Self {
        Self { analyzers }
    }

    pub fn assemble_audit_report(
        &self,
        ctx: &MarketContext,
        config: &Config,
    ) -> Result<AIAuditPayload, AnalysisError> {
        let shared = SharedAnalysisState::default();

        let mut builder = AIAuditPayloadBuilder::new();
        let mut score_summary = ScoreEngineSummary::default();

        builder = builder.header(AuditHeader {
            strategy_id: "alpha_resonance_v3".into(),
            symbol: ctx.symbol.clone(),
            direction: None,
            timestamp: Utc::now(),
            trigger_source: crate::types::TriggerSource::Auto,
            interval_setup: self.extract_interval_setup(ctx),
        });

        let stages = [
            AnalyzerStage::Context,
            AnalyzerStage::Signal,
            AnalyzerStage::Audit,
        ];
        // 定义阶段顺序
        let stages = [
            AnalyzerStage::Context,
            AnalyzerStage::Signal,
            AnalyzerStage::Audit,
        ];

        for stage in stages {
            // --- 关键改进 1：计算当前阶段生效的“总阀门” ---
            let current_gate = if stage == AnalyzerStage::Signal {
                // 如果进入了信号阶段，我们要融合所有环境因子
                let trend = shared
                    .data
                    .get("context:trend_multiplier")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);
                let vol = shared
                    .data
                    .get("context:volume_multiplier")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);
                let time = shared
                    .data
                    .get("context:temporal_multiplier")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);

                // 连乘得到最终环境系数
                let final_gate = trend * vol * time;

                // 存入 shared 方便 debug 和 AI 审计查看最终阀门是多少
                // 这里不再使用 shared.data.insert，因为 shared 此时可能被多个 analyzer 引用，
                // 建议在循环外或通过内部可变性处理，或者直接在这里使用。
                final_gate
            } else {
                // Context 阶段和 Audit 阶段通常不互乘，保持 1.0
                1.0
            };

            for analyzer in self.analyzers.iter().filter(|a| a.stage() == stage) {
                let res = analyzer.analyze(ctx, config, &shared)?;

                // --- 关键改进 2：获取静态权重 ---
                let weight = config.weights.get(&analyzer.kind()).unwrap_or(&1.0);

                // --- 关键改进 3：计算最终分数 ---
                // Context 阶段：weighted_score = 原始分 * 静态权重 (env_gate 是 1.0)
                // Signal 阶段：weighted_score = 原始分 * 静态权重 * 环境总阀门
                let weighted_score = res.score * weight * current_gate;

                if res.is_violation {
                    // 一票否决逻辑
                    score_summary.final_score = -100.0;
                    let mut component = res.to_component(analyzer.name());
                    component.score = -100.0;
                    score_summary.red_flags.push(component);

                    if stage == AnalyzerStage::Context || stage == AnalyzerStage::Audit {
                        return Ok(builder.score_engine(score_summary).build().unwrap());
                        // 提前熔断返回
                    }
                } else {
                    // 只有当还没被一票否决时，才累加分数
                    if score_summary.final_score > -100.0 {
                        score_summary.final_score += weighted_score;
                    }

                    // 构造组件数据，score 必须是加权后的，方便审计对照
                    let mut component = res.to_component(analyzer.name());
                    component.score = weighted_score;

                    // 根据加权后的分数归类
                    if weighted_score > 0.0 {
                        score_summary.positive_drivers.push(component);
                    } else if weighted_score < 0.0 {
                        score_summary.red_flags.push(component);
                    }
                }

                // B. 将分析器产出的特定领域数据喂给 Builder
                builder = self.apply_result_to_builder(builder, &res);
            }
        }
        // --- 4. 修正最终分数并封装 ---
        score_summary.final_score = score_summary.final_score.clamp(-100.0, 100.0);

        // 将汇总好的分数和剩下的上下文数据补全并构建
        builder
            .score_engine(score_summary)
            .global_context(self.collect_global_context(ctx)) // 假设的方法
            .build()
            .map_err(|e| AnalysisError::Calculation(format!("Builder failed: {}", e)))
    }

    /// 核心分发逻辑：根据 AnalysisResult 里的 Option 字段填充 Builder
    fn apply_result_to_builder(
        &self,
        mut builder: AIAuditPayloadBuilder,
        res: &crate::analyzer::AnalysisResult,
    ) -> AIAuditPayloadBuilder {
        // 提取博弈论数据
        // if let Some(gt) = &res.game_theory_data {
        //     builder = builder.game_theory(gt.clone());
        // }

        // // 提取市场统计
        // if let Some(stats) = &res.market_stats {
        //     builder = builder.market_statistics(stats.clone());
        // }

        // // 提取交易审计 (止损止盈等)
        // if let Some(trade) = &res.trade_audit {
        //     builder = builder.trade_audit(trade.clone());
        // }

        // // 提取动态上下文 (冲突、重力井)
        // if let Some(wells) = &res.gravity_wells {
        //     // 这里需要 Builder 内部逻辑来处理 Vec 的 extend，
        //     // 或者在此处先获取 builder 原有的 context 再修改
        // }

        builder
    }

    fn extract_interval_setup(&self, ctx: &MarketContext) -> crate::types::IntervalSetup {
        // 从 ctx.roles 提取
        crate::types::IntervalSetup::default()
    }

    fn collect_global_context(&self, ctx: &MarketContext) -> crate::types::GlobalContext {
        // 从 ctx 或外部状态获取全局环境
        crate::types::GlobalContext::default()
    }
}
