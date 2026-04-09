use cc_core::Usage;

/// Cumulative usage across a session.
#[derive(Debug, Default)]
pub struct UsageTracker {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub turn_count: u32,
    /// Input tokens from the most recent API response (for auto-compact threshold).
    last_input: u32,
}

impl UsageTracker {
    pub fn record(&mut self, usage: &Usage) {
        self.total_input_tokens += usage.input_tokens as u64;
        self.total_output_tokens += usage.output_tokens as u64;
        if let Some(c) = usage.cache_creation_input_tokens {
            self.total_cache_creation_tokens += c as u64;
        }
        if let Some(c) = usage.cache_read_input_tokens {
            self.total_cache_read_tokens += c as u64;
        }
        self.last_input = usage.input_tokens;
        self.turn_count += 1;
    }

    /// Total "effective" input tokens for auto-compact threshold check.
    pub fn last_input_tokens(&self) -> u32 {
        self.last_input
    }

    /// Estimate cost in USD (rough, based on public pricing).
    pub fn estimated_cost_usd(&self, model: &str) -> f64 {
        let (input_rate, output_rate) = match model {
            m if m.contains("opus") => (15.0, 75.0),
            m if m.contains("sonnet") => (3.0, 15.0),
            m if m.contains("haiku") => (0.25, 1.25),
            _ => (3.0, 15.0), // default to sonnet pricing
        };
        let input_cost = self.total_input_tokens as f64 / 1_000_000.0 * input_rate;
        let output_cost = self.total_output_tokens as f64 / 1_000_000.0 * output_rate;
        input_cost + output_cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_cumulative_usage() {
        let mut tracker = UsageTracker::default();
        tracker.record(&Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(5),
        });
        tracker.record(&Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        assert_eq!(tracker.total_input_tokens, 300);
        assert_eq!(tracker.total_output_tokens, 150);
        assert_eq!(tracker.total_cache_creation_tokens, 10);
        assert_eq!(tracker.total_cache_read_tokens, 5);
        assert_eq!(tracker.turn_count, 2);
        assert_eq!(tracker.last_input_tokens(), 200);
    }

    #[test]
    fn cost_estimate_uses_model_pricing() {
        let mut tracker = UsageTracker::default();
        tracker.total_input_tokens = 1_000_000;
        tracker.total_output_tokens = 1_000_000;

        let opus_cost = tracker.estimated_cost_usd("claude-opus-4-6");
        assert!((opus_cost - 90.0).abs() < 0.01); // 15 + 75

        let sonnet_cost = tracker.estimated_cost_usd("claude-sonnet-4-6");
        assert!((sonnet_cost - 18.0).abs() < 0.01); // 3 + 15
    }
}
