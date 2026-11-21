//! Device-code polling.

use super::OAuthError;
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

pub enum Poll<T> {
    Pending,
    /// Adds 5 seconds to the delay.
    SlowDown,
    Complete(T),
    Failed(String),
}

const MIN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_INTERVAL: Duration = Duration::from_secs(60);
const MAX_LIFETIME: Duration = Duration::from_secs(3600);

/// A server's poll interval in seconds, clamped to 1..=60 so a huge or odd value cannot panic or stall.
pub fn interval_secs(secs: f64) -> Duration {
    Duration::from_secs_f64(secs.max(MIN_INTERVAL.as_secs_f64()).min(MAX_INTERVAL.as_secs_f64()))
}

/// Polls `request` until it completes, fails or `expires_in` passes. `OAuthError::Network` from the
/// closure counts as pending and is returned at the deadline if nothing else ends the wait; any other
/// error ends the wait at once. `interval` is clamped to 1..=60 seconds and `expires_in` to 1..=3600.
pub async fn poll<T, F, Fut>(interval: Duration, expires_in: Duration, mut request: F) -> Result<T, OAuthError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Poll<T>, OAuthError>>,
{
    let deadline = Instant::now() + expires_in.clamp(MIN_INTERVAL, MAX_LIFETIME);
    let mut delay = interval.clamp(MIN_INTERVAL, MAX_INTERVAL);
    let mut network_error = false;
    while Instant::now() < deadline {
        let result = match request().await {
            Ok(r) => {
                network_error = false;
                r
            }
            Err(OAuthError::Network) => {
                network_error = true;
                Poll::Pending
            }
            Err(e) => return Err(e),
        };
        match result {
            Poll::Pending => {}
            Poll::SlowDown => delay += Duration::from_secs(5),
            Poll::Complete(v) => return Ok(v),
            Poll::Failed(m) => return Err(OAuthError::Failed(m)),
        }
        tokio::time::sleep(delay.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
    Err(if network_error { OAuthError::Network } else { OAuthError::Failed("Device sign-in expired.".into()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[tokio::test(start_paused = true)]
    async fn handles_pending_slow_down_and_success() {
        let start = Instant::now();
        let calls = Cell::new(0);
        let token = poll(S(1), S(100), || {
            calls.set(calls.get() + 1);
            let r = match calls.get() {
                1 => Poll::Pending,
                2 => Poll::SlowDown,
                _ => Poll::Complete("token"),
            };
            async { Ok(r) }
        })
        .await
        .unwrap();
        assert_eq!(token, "token");
        assert_eq!(start.elapsed(), S(1 + 6)); // sleeps [1, 6]
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_polling_through_a_network_error() {
        let calls = Cell::new(0);
        let token = poll(S(1), S(100), || {
            calls.set(calls.get() + 1);
            let r = if calls.get() == 1 { Err(OAuthError::Network) } else { Ok(Poll::Complete("token")) };
            async { r }
        })
        .await
        .unwrap();
        assert_eq!(token, "token");
    }

    #[tokio::test(start_paused = true)]
    async fn reports_the_network_error_once_the_code_expires() {
        let r: Result<(), _> = poll(S(1), S(10), || async { Err(OAuthError::Network) }).await;
        assert!(matches!(r, Err(OAuthError::Network)));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_and_expiry_end_the_wait() {
        let r: Result<(), _> = poll(S(1), S(100), || async { Ok(Poll::Failed("The device code expired.".into())) }).await;
        assert_eq!(r.unwrap_err().to_string(), "The device code expired.");
        let r: Result<(), _> = poll(S(1), S(3), || async { Ok(Poll::Pending) }).await;
        assert_eq!(r.unwrap_err().to_string(), "Device sign-in expired.");
    }

    #[test]
    fn server_intervals_are_clamped() {
        assert_eq!(interval_secs(5.0), S(5));
        for (secs, want) in [(0.0, S(1)), (-3.0, S(1)), (f64::MAX, S(60)), (f64::INFINITY, S(60)), (f64::NAN, S(1))] {
            assert_eq!(interval_secs(secs), want, "{secs}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn huge_lifetimes_and_intervals_are_clamped() {
        let start = Instant::now();
        let r: Result<(), _> = poll(Duration::MAX, Duration::MAX, || async { Ok(Poll::Pending) }).await;
        assert_eq!(r.unwrap_err().to_string(), "Device sign-in expired.");
        assert_eq!(start.elapsed(), S(3600));
    }
}
