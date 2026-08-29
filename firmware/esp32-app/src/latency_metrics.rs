//! Lightweight, allocation-free latency / WCET metrics (REQ-SCHED-001 / P3).
//!
//! Uses atomics so any thread can record a timing with no locks. For each
//! named channel we track the sample count plus running **min / avg / max**;
//! the running max is the *worst-case execution time* (WCET) observation that
//! matters for deterministic-latency characterisation, and the avg gives a
//! sense of the steady-state. Values are microseconds (from `esp_timer`).
//!
//! NOTE: these targets (RISC-V / Xtensa 32-bit) have no 64-bit atomics, so we
//! store timings in `u32` microseconds — ample here (u32 wraps only after
//! ~71 minutes) — and keep the average as a signed running mean.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::LazyLock;

/// A single named timing channel.
pub struct TimingChannel {
    count: AtomicU32,
    min_us: AtomicU32,
    max_us: AtomicU32,
    avg_us: AtomicI32,
}

impl TimingChannel {
    const fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            min_us: AtomicU32::new(u32::MAX),
            max_us: AtomicU32::new(0),
            avg_us: AtomicI32::new(0),
        }
    }

    /// Record a duration in microseconds.
    pub fn record(&self, us: u64) {
        let us = us as u32;
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        // Running max (relaxed CAS loop — lossy under contention is fine for
        // telemetry; we only need the observed worst case).
        let mut cur = self.max_us.load(Ordering::Relaxed);
        while us > cur {
            match self
                .max_us
                .compare_exchange_weak(cur, us, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
        let mut cur = self.min_us.load(Ordering::Relaxed);
        while us < cur {
            match self
                .min_us
                .compare_exchange_weak(cur, us, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
        // Running average (signed so a below-average sample stays correct):
        // avg += (x - avg) / n.
        let x = us as i32;
        let n = n as i32;
        let mut avg = self.avg_us.load(Ordering::Relaxed);
        loop {
            let new_avg = avg + (x - avg) / n;
            match self
                .avg_us
                .compare_exchange_weak(avg, new_avg, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => avg = actual,
            }
        }
    }

    /// One-line summary: `name: n=.. min=..ms avg=..ms max(WCET)=..ms`.
    pub fn report(&self, name: &str) -> String {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return format!("{name}: -");
        }
        let min = self.min_us.load(Ordering::Relaxed);
        let max = self.max_us.load(Ordering::Relaxed);
        let avg = self.avg_us.load(Ordering::Relaxed) as i64;
        format!(
            "{name}: n={count} min={}ms avg={}ms wcet={}ms",
            min / 1000,
            avg.max(0) / 1000,
            max / 1000
        )
    }

    /// Reset the channel (e.g. after a batch run / before a benchmark).
    /// Unused in the shipped firmware but kept as the public API for an
    /// external benchmark harness to bracket a measurement window.
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.min_us.store(u32::MAX, Ordering::Relaxed);
        self.max_us.store(0, Ordering::Relaxed);
        self.avg_us.store(0, Ordering::Relaxed);
    }
}

/// The set of latency channels we expose.
pub struct Metrics {
    /// DeepSeek LLM round-trip (agent → Core-0 worker → agent) — the dominant
    /// variable cost; isolated on Core 0 by the P1 channel.
    pub llm_rt_us: TimingChannel,
    /// Ingress AT parse + dispatch + render (the real-time command path on Core 1).
    pub at_dispatch_us: TimingChannel,
    /// End-to-end: command received at the UART → reply placed in the outbox.
    pub e2e_reply_us: TimingChannel,
    /// One ReAct task execution on the agent thread (tool calls + LLM + decision).
    pub agent_task_us: TimingChannel,
}

impl Metrics {
    const fn new() -> Self {
        Self {
            llm_rt_us: TimingChannel::new(),
            at_dispatch_us: TimingChannel::new(),
            e2e_reply_us: TimingChannel::new(),
            agent_task_us: TimingChannel::new(),
        }
    }
}

static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

/// Monotonic time in microseconds (ESP-IDF `esp_timer`, shared across cores).
pub fn now_us() -> u64 {
    unsafe { esp_idf_sys::esp_timer_get_time() as u64 }
}

pub fn llm_rt() -> &'static TimingChannel {
    &METRICS.llm_rt_us
}
pub fn at_dispatch() -> &'static TimingChannel {
    &METRICS.at_dispatch_us
}
pub fn e2e_reply() -> &'static TimingChannel {
    &METRICS.e2e_reply_us
}
pub fn agent_task() -> &'static TimingChannel {
    &METRICS.agent_task_us
}

/// One-line report of all measured latencies (for the periodic health log).
pub fn report() -> String {
    [
        llm_rt().report("llm_rt"),
        at_dispatch().report("at_dispatch"),
        e2e_reply().report("e2e_reply"),
        agent_task().report("agent_task"),
    ]
    .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_sample_tracks_min_max_avg() {
        let ch = TimingChannel::new();
        ch.record(1_000_000); // 1 s in µs
        assert_eq!(ch.count.load(Ordering::Relaxed), 1);
        assert_eq!(ch.min_us.load(Ordering::Relaxed), 1_000_000);
        assert_eq!(ch.max_us.load(Ordering::Relaxed), 1_000_000);
        assert_eq!(ch.avg_us.load(Ordering::Relaxed), 1_000_000);
    }

    #[test]
    fn min_and_max_track_the_extremes() {
        let ch = TimingChannel::new();
        ch.record(100);
        ch.record(5_000);
        ch.record(3_000);
        assert_eq!(ch.min_us.load(Ordering::Relaxed), 100);
        assert_eq!(ch.max_us.load(Ordering::Relaxed), 5_000);
        // Running mean of {100, 5000, 3000} = 8100/3 = 2700 exactly.
        assert_eq!(ch.avg_us.load(Ordering::Relaxed), 2_700);
    }

    #[test]
    fn running_average_converges_on_constant_input() {
        let ch = TimingChannel::new();
        for _ in 0..1000 {
            ch.record(1_000); // constant 1 ms
        }
        assert_eq!(ch.avg_us.load(Ordering::Relaxed), 1_000);
        assert_eq!(ch.min_us.load(Ordering::Relaxed), 1_000);
        assert_eq!(ch.max_us.load(Ordering::Relaxed), 1_000);
    }

    #[test]
    fn reset_clears_all_state() {
        let ch = TimingChannel::new();
        ch.record(123);
        ch.reset();
        assert_eq!(ch.count.load(Ordering::Relaxed), 0);
        assert_eq!(ch.min_us.load(Ordering::Relaxed), u32::MAX);
        assert_eq!(ch.max_us.load(Ordering::Relaxed), 0);
        assert_eq!(ch.avg_us.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn empty_channel_reports_dash() {
        let ch = TimingChannel::new();
        assert_eq!(ch.report("llm_rt"), "llm_rt: -");
    }
}

