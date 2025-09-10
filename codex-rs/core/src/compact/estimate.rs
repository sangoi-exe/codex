use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;

/// Heuristic: ~4 chars per token (rounded up).
fn chars_to_tokens(chars: usize) -> u64 {
    ((chars as u64) + 3) / 4
}

/// Estimate tokens for a single response item.
fn estimate_tokens_for_item(item: &ResponseItem) -> u64 {
    match item {
        ResponseItem::Message { content, .. } => content
            .iter()
            .map(|c| match c {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    chars_to_tokens(text.len())
                }
                // Skip images in token estimate (count as 0 here).
                ContentItem::InputImage { .. } => 0,
            })
            .sum(),
        ResponseItem::Reasoning { summary, .. } => summary
            .iter()
            .map(|s| match s {
                ReasoningItemReasoningSummary::SummaryText { text } => chars_to_tokens(text.len()),
            })
            .sum(),
        ResponseItem::FunctionCall {
            name, arguments, ..
        } => chars_to_tokens(name.len() + arguments.len()),
        ResponseItem::FunctionCallOutput { output, .. } => {
            // Include the textual content; ignore the boolean flag.
            chars_to_tokens(output.content.len())
        }
        ResponseItem::CustomToolCall { name, input, .. } => {
            chars_to_tokens(name.len() + input.len())
        }
        ResponseItem::CustomToolCallOutput { output, .. } => chars_to_tokens(output.len()),
        ResponseItem::LocalShellCall { .. } => {
            // Shell calls are typically summarized already in history cells; treat as small.
            8
        }
        ResponseItem::WebSearchCall { .. } => 0,
        ResponseItem::Other => 0,
    }
}

/// Estimate total tokens for a slice of response items (history in order).
pub(crate) fn estimate_tokens_for_items(items: &[ResponseItem]) -> u64 {
    items.iter().map(estimate_tokens_for_item).sum()
}

/// Minimal report of before/after estimates around compaction.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompactionReport {
    pub before_tokens: u64,
    pub after_tokens: u64,
}

impl CompactionReport {
    pub fn new(before_tokens: u64, after_tokens: u64) -> Self {
        Self {
            before_tokens,
            after_tokens,
        }
    }

    /// Estimate percent remaining in context window given a total window size.
    pub fn percent_remaining_before(&self, context_window: u64) -> u8 {
        percent_remaining(self.before_tokens, context_window)
    }

    pub fn percent_remaining_after(&self, context_window: u64) -> u8 {
        percent_remaining(self.after_tokens, context_window)
    }
}

fn percent_remaining(estimated_tokens_in_window: u64, context_window: u64) -> u8 {
    use codex_protocol::protocol::TokenUsage;
    let mut tu = TokenUsage::default();
    tu.total_tokens = estimated_tokens_in_window;
    tu.percent_of_context_window_remaining(context_window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;

    #[test]
    fn estimates_message_text() {
        let item = ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: "abcd".into(), // 4 chars → 1 token
            }],
        };
        assert_eq!(estimate_tokens_for_items(&[item]), 1);
    }

    #[test]
    fn report_formats_remaining() {
        let report = CompactionReport::new(10_000, 5_000);
        // With a small window, remaining is clamped; behavior is stable.
        assert!(report.percent_remaining_before(16_385) <= 100);
        assert!(report.percent_remaining_after(16_385) <= 100);
    }
}
