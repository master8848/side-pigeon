//! Heartbeat scheduling for the Discord Gateway.
//!
//! Per the Gateway spec, a client must send a HEARTBEAT **immediately** after
//! receiving HELLO, then every `heartbeat_interval` ms. `Heartbeat::new`
//! models exactly that: the first `tick()` resolves immediately, subsequent
//! ticks fire every `interval`. The server may additionally request a beat out
//! of band (op 1), which the gateway loop handles separately.

use std::time::Duration;

/// Schedules gateway heartbeats (immediate first beat, then every `interval`).
pub struct Heartbeat {
    interval: Duration,
    ticker: tokio::time::Interval,
    first: bool,
}

impl Heartbeat {
    /// Create a heartbeat schedule for the given `heartbeat_interval`.
    pub fn new(interval: Duration) -> Self {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.reset(); // arm the first *scheduled* tick at now + interval
        Self {
            interval,
            ticker,
            first: true,
        }
    }

    /// Wait until the next heartbeat should be sent.
    ///
    /// The first call returns immediately (spec: beat right after HELLO); every
    /// later call waits one full `interval`.
    pub async fn tick(&mut self) {
        if self.first {
            self.first = false;
            return;
        }
        self.ticker.tick().await;
    }

    /// The configured heartbeat interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{advance, timeout, Duration as TokioDuration};

    #[tokio::test(start_paused = true)]
    async fn first_beat_immediate_then_every_interval() {
        let mut hb = Heartbeat::new(TokioDuration::from_millis(1000));

        // First beat: immediate (well within 100 ms of paused time).
        timeout(TokioDuration::from_millis(100), hb.tick())
            .await
            .expect("first heartbeat should be immediate");

        // Second beat must wait the full interval.
        assert!(
            timeout(TokioDuration::from_millis(300), hb.tick())
                .await
                .is_err(),
            "second beat fired too early"
        );
        advance(TokioDuration::from_millis(700)).await; // 700 + 300 = 1000 total
        timeout(TokioDuration::from_millis(100), hb.tick())
            .await
            .expect("second beat should fire at interval");

        // Third beat: full interval again.
        assert!(
            timeout(TokioDuration::from_millis(300), hb.tick())
                .await
                .is_err(),
            "third beat fired too early"
        );
        advance(TokioDuration::from_millis(700)).await;
        timeout(TokioDuration::from_millis(100), hb.tick())
            .await
            .expect("third beat should fire at interval");
    }

    #[tokio::test]
    async fn interval_is_exposed() {
        let hb = Heartbeat::new(Duration::from_millis(41250));
        assert_eq!(hb.interval(), Duration::from_millis(41250));
    }
}
