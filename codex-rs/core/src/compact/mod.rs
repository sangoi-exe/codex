//! Compact module scaffolding (PR1): token estimation and report helpers.
//!
//! This module provides a lightweight, provider-agnostic token estimator and
//! a small `CompactionReport` type used by the current `/compact` flow to print
//! before/after deltas even when the provider omits `token_usage`.

mod estimate;
mod snapshot;

pub(crate) use estimate::CompactionReport;
pub(crate) use estimate::estimate_tokens_for_items;

/// Format a short, human-friendly completion message for the compaction step.
///
/// The numbers are estimates based on character counts (≈ 4 chars/token). When
/// a model context window is known, the message also includes the estimated
/// percentage of window remaining before/after.
pub(crate) fn format_completion_message(
    report: &CompactionReport,
    model_context_window: Option<u64>,
) -> String {
    match model_context_window {
        Some(ctx) if ctx > 0 => {
            let before_pct = report.percent_remaining_before(ctx);
            let after_pct = report.percent_remaining_after(ctx);
            format!(
                "Compaction complete: ~{} → ~{} tokens; saved ~{}; remaining ~{}% → ~{}%",
                report.before_tokens,
                report.after_tokens,
                report.before_tokens.saturating_sub(report.after_tokens),
                before_pct,
                after_pct
            )
        }
        _ => format!(
            "Compaction complete: ~{} → ~{} tokens; saved ~{}",
            report.before_tokens,
            report.after_tokens,
            report.before_tokens.saturating_sub(report.after_tokens)
        ),
    }
}
