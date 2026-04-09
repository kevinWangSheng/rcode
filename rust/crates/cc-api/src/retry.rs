use cc_core::CcError;
use std::time::Duration;

/// Retry configuration for API requests.
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Determine whether and when to retry after an error.
    /// Returns `Some(delay)` if the request should be retried after `delay`,
    /// or `None` if the error is not retryable or max retries reached.
    pub fn should_retry(&self, error: &CcError, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_retries {
            return None;
        }
        match error {
            CcError::RateLimited { retry_after } => {
                Some(Duration::from_secs(retry_after.unwrap_or(5)))
            }
            CcError::Api {
                retryable: true, ..
            } => {
                let backoff = self
                    .initial_backoff
                    .mul_f64(self.backoff_multiplier.powi(attempt as i32));
                Some(backoff.min(self.max_backoff))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_rate_limited() {
        let policy = RetryPolicy::default();
        let err = CcError::RateLimited {
            retry_after: Some(10),
        };
        assert_eq!(policy.should_retry(&err, 0), Some(Duration::from_secs(10)));
        assert_eq!(policy.should_retry(&err, 1), Some(Duration::from_secs(10)));
        assert_eq!(policy.should_retry(&err, 2), None); // max_retries = 2
    }

    #[test]
    fn retries_retryable_api_error_with_backoff() {
        let policy = RetryPolicy::default();
        let err = CcError::api_retryable("server error", 500);
        assert_eq!(policy.should_retry(&err, 0), Some(Duration::from_secs(1)));
        assert_eq!(policy.should_retry(&err, 1), Some(Duration::from_secs(2)));
        assert_eq!(policy.should_retry(&err, 2), None);
    }

    #[test]
    fn no_retry_for_non_retryable() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.should_retry(&CcError::Cancelled, 0), None);
        assert_eq!(policy.should_retry(&CcError::api("bad request"), 0), None);
    }
}
