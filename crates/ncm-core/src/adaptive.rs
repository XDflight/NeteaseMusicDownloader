//! Adaptive concurrency: a dynamic connection gate plus a hill-climbing AIMD controller.
//!
//! The controller watches aggregate throughput once per window. It adds one connection at a
//! time while that keeps paying off (`GAIN_THRESHOLD`), undoes an increase that did not, and
//! halves the budget on errors or a throughput collapse. When it settles it re-probes after
//! `HOLD_WINDOWS`, so it follows changes of the network.

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::tuning as t;

// ------------------------------------------------------------------------------------ gate

/// A semaphore whose capacity can change at runtime. Lowering it never interrupts running
/// holders; the excess simply drains as permits are returned.
pub struct Gate {
    state: Mutex<GateState>,
    notify: Notify,
}

struct GateState {
    capacity: usize,
    in_use: usize,
}

pub struct Permit {
    gate: Arc<Gate>,
}

impl Gate {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(GateState { capacity, in_use: 0 }), notify: Notify::new() })
    }

    pub async fn acquire(self: &Arc<Self>) -> Permit {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(p) = self.try_acquire() {
                return p;
            }
            notified.await;
        }
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut s = self.state.lock().unwrap();
        if s.in_use < s.capacity {
            s.in_use += 1;
            Some(Permit { gate: self.clone() })
        } else {
            None
        }
    }

    pub fn set_capacity(&self, capacity: usize) {
        self.state.lock().unwrap().capacity = capacity;
        self.notify.notify_waiters();
    }

    pub fn capacity(&self) -> usize {
        self.state.lock().unwrap().capacity
    }

    pub fn in_use(&self) -> usize {
        self.state.lock().unwrap().in_use
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap().in_use -= 1;
        self.gate.notify.notify_one();
    }
}

// ---------------------------------------------------------------------------------- policy

#[derive(Debug, Clone)]
pub struct PolicyParams {
    pub min: usize,
    pub initial: usize,
    pub max: usize,
    pub gain: f64,
    pub step: usize,
    pub decrease: f64,
    pub hold_windows: u32,
    pub drop_tolerance: f64,
    pub saturation: f64,
    pub idle_bps: f64,
}

impl Default for PolicyParams {
    fn default() -> Self {
        Self {
            min: t::MIN_CONNECTIONS,
            initial: t::INITIAL_CONNECTIONS,
            max: t::MAX_CONNECTIONS,
            gain: t::GAIN_THRESHOLD,
            step: t::INCREASE_STEP,
            decrease: t::DECREASE_FACTOR,
            hold_windows: t::HOLD_WINDOWS,
            drop_tolerance: t::DROP_TOLERANCE,
            saturation: t::SATURATION_RATIO,
            idle_bps: t::IDLE_BYTES_PER_SEC,
        }
    }
}

/// What happened during one measurement window.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// Aggregate bytes per second.
    pub throughput: f64,
    /// Hard failures (timeouts, resets, throttling responses).
    pub errors: u32,
    /// Mean number of connections that were busy.
    pub avg_in_flight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Idle,
    Increased,
    Reverted,
    Held,
    BackedOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Growing,
    Holding(u32),
}

/// Pure state machine; all timing lives outside so it can be unit-tested with a model network.
#[derive(Debug, Clone)]
pub struct Policy {
    p: PolicyParams,
    limit: usize,
    phase: Phase,
    /// Throughput at the previous limit (Growing) or a smoothed reference (Holding).
    reference: Option<f64>,
}

impl Policy {
    pub fn new(p: PolicyParams) -> Self {
        let limit = p.initial.clamp(p.min, p.max);
        Self { p, limit, phase: Phase::Growing, reference: None }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn on_window(&mut self, w: Window) -> Decision {
        if w.errors > 0 {
            self.back_off();
            self.reference = Some(w.throughput);
            return Decision::BackedOff;
        }
        let saturated = w.avg_in_flight >= self.p.saturation * self.limit as f64;
        if w.throughput < self.p.idle_bps || !saturated {
            // Not enough demand to say anything about the network.
            return Decision::Idle;
        }

        match (self.phase, self.reference) {
            (Phase::Growing, None) => {
                self.reference = Some(w.throughput);
                self.increase()
            }
            (Phase::Growing, Some(prev)) => {
                if w.throughput >= prev * (1.0 + self.p.gain) {
                    self.reference = Some(w.throughput);
                    self.increase()
                } else if w.throughput < prev * self.p.drop_tolerance {
                    self.back_off();
                    self.reference = Some(w.throughput);
                    self.phase = Phase::Holding(self.p.hold_windows);
                    Decision::BackedOff
                } else {
                    // The extra connection was not worth it: take it back and settle.
                    self.limit = self.limit.saturating_sub(self.p.step).max(self.p.min);
                    self.reference = Some(w.throughput);
                    self.phase = Phase::Holding(self.p.hold_windows);
                    Decision::Reverted
                }
            }
            (Phase::Holding(left), reference) => {
                let reference = reference.unwrap_or(w.throughput);
                if w.throughput < reference * self.p.drop_tolerance {
                    self.back_off();
                    self.reference = Some(w.throughput);
                    self.phase = Phase::Holding(self.p.hold_windows);
                    return Decision::BackedOff;
                }
                self.reference = Some(0.7 * reference + 0.3 * w.throughput);
                if left <= 1 {
                    // Periodic probe: is there more bandwidth than before?
                    self.phase = Phase::Growing;
                    self.reference = Some(w.throughput);
                    self.increase()
                } else {
                    self.phase = Phase::Holding(left - 1);
                    Decision::Held
                }
            }
        }
    }

    fn increase(&mut self) -> Decision {
        if self.limit >= self.p.max {
            self.phase = Phase::Holding(self.p.hold_windows);
            return Decision::Held;
        }
        self.limit = (self.limit + self.p.step).min(self.p.max);
        Decision::Increased
    }

    fn back_off(&mut self) {
        let halved = (self.limit as f64 * self.p.decrease).floor() as usize;
        self.limit = halved.max(self.p.min);
        self.phase = Phase::Holding(self.p.hold_windows);
    }
}

// --------------------------------------------------------------------------------- limiter

/// Live numbers for the UI.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetSnapshot {
    pub bytes_per_sec: f64,
    pub in_flight: usize,
    pub limit: usize,
}

/// Shared by the fetch workers (which report bytes/errors) and the controller task.
pub struct Limiter {
    /// Requests in flight.
    pub gate: Arc<Gate>,
    bytes: AtomicU64,
    errors: AtomicU32,
    limit: AtomicUsize,
    speed_bits: AtomicU64,
}

impl Limiter {
    pub fn new() -> Arc<Self> {
        let params = PolicyParams::default();
        let initial = params.initial.clamp(params.min, params.max);
        Arc::new(Self {
            gate: Gate::new(initial),
            bytes: AtomicU64::new(0),
            errors: AtomicU32::new(0),
            limit: AtomicUsize::new(initial),
            speed_bits: AtomicU64::new(0f64.to_bits()),
        })
    }

    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Report a hard failure (timeout, reset, 403/429/5xx). The next window will back off.
    pub fn report_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> NetSnapshot {
        NetSnapshot {
            bytes_per_sec: f64::from_bits(self.speed_bits.load(Ordering::Relaxed)),
            in_flight: self.gate.in_use(),
            limit: self.limit.load(Ordering::Relaxed),
        }
    }

    /// Run the controller until `cancel` fires.
    pub async fn run_controller(self: Arc<Self>, cancel: CancellationToken) {
        let mut policy = Policy::new(PolicyParams::default());
        let mut window_start = Instant::now();
        let mut window_bytes = self.bytes.load(Ordering::Relaxed);
        let (mut samples, mut in_flight_sum) = (0u32, 0.0f64);
        let mut last_tick = (Instant::now(), window_bytes);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(t::SAMPLE_INTERVAL) => {}
            }
            samples += 1;
            in_flight_sum += self.gate.in_use() as f64;

            // ~1 s smoothed speed for the UI.
            let now = Instant::now();
            let total = self.bytes.load(Ordering::Relaxed);
            let dt = now.duration_since(last_tick.0);
            if dt >= Duration::from_millis(500) {
                let inst = (total - last_tick.1) as f64 / dt.as_secs_f64();
                let prev = f64::from_bits(self.speed_bits.load(Ordering::Relaxed));
                self.speed_bits.store((0.6 * prev + 0.4 * inst).to_bits(), Ordering::Relaxed);
                last_tick = (now, total);
            }

            let elapsed = now.duration_since(window_start);
            if elapsed < t::PROBE_WINDOW {
                continue;
            }
            let w = Window {
                throughput: (total - window_bytes) as f64 / elapsed.as_secs_f64(),
                errors: self.errors.swap(0, Ordering::Relaxed),
                avg_in_flight: in_flight_sum / f64::from(samples.max(1)),
            };
            let before = policy.limit();
            let decision = policy.on_window(w);
            let after = policy.limit();
            if after != before {
                tracing::info!(
                    "adaptive: {decision:?} {before} -> {after} connections ({:.2} MiB/s, {} errors)",
                    w.throughput / 1_048_576.0,
                    w.errors
                );
                self.gate.set_capacity(after);
                self.limit.store(after, Ordering::Relaxed);
            }
            window_start = now;
            window_bytes = total;
            samples = 0;
            in_flight_sum = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the policy against a model network for `windows` windows.
    fn simulate(model: impl Fn(usize, usize) -> f64, windows: usize, params: PolicyParams) -> Vec<usize> {
        let mut policy = Policy::new(params);
        let mut trace = Vec::new();
        for i in 0..windows {
            let limit = policy.limit();
            let w = Window { throughput: model(limit, i), errors: 0, avg_in_flight: limit as f64 };
            policy.on_window(w);
            trace.push(policy.limit());
        }
        trace
    }

    const MIB: f64 = 1_048_576.0;

    #[test]
    fn climbs_until_link_is_saturated_then_settles() {
        // Each connection gets 1 MiB/s, the link tops out at 5 MiB/s.
        let trace = simulate(|n, _| (n as f64).min(5.0) * MIB, 40, PolicyParams::default());
        let settled = *trace.last().unwrap();
        assert!((5..=6).contains(&settled), "settled at {settled}: {trace:?}");
        assert!(trace.iter().all(|&l| l <= 8));
    }

    #[test]
    fn climbs_on_a_fat_pipe_until_relative_gain_drops_below_threshold() {
        // Perfectly linear scaling: the n-th connection adds 1/n. With the 15% threshold the
        // policy stops at 7 (1/7 = 14.3%); it must never exceed the configured maximum.
        let trace = simulate(|n, _| n as f64 * 3.0 * MIB, 40, PolicyParams::default());
        let settled = *trace.last().unwrap();
        assert!((6..=8).contains(&settled), "settled at {settled}: {trace:?}");
    }

    #[test]
    fn never_exceeds_max() {
        let p = PolicyParams { gain: 0.0, ..PolicyParams::default() };
        let trace = simulate(|n, _| n as f64 * 3.0 * MIB, 40, p);
        assert!(trace.iter().all(|&l| l <= 8), "{trace:?}");
        assert_eq!(*trace.last().unwrap(), 8);
    }

    #[test]
    fn backs_off_on_errors_and_never_drops_below_min() {
        let mut policy = Policy::new(PolicyParams::default());
        for _ in 0..10 {
            policy.on_window(Window { throughput: 2.0 * MIB, errors: 3, avg_in_flight: 2.0 });
        }
        assert_eq!(policy.limit(), 1);
    }

    #[test]
    fn halves_on_throughput_collapse() {
        let p = PolicyParams { initial: 8, ..PolicyParams::default() };
        let mut policy = Policy::new(p);
        // Establish a reference, then collapse.
        policy.on_window(Window { throughput: 8.0 * MIB, errors: 0, avg_in_flight: 8.0 });
        let d = policy.on_window(Window { throughput: 2.0 * MIB, errors: 0, avg_in_flight: 8.0 });
        assert_eq!(d, Decision::BackedOff);
        assert_eq!(policy.limit(), 4);
    }

    #[test]
    fn follows_a_bandwidth_change() {
        // 3 MiB/s link that jumps to 7 MiB/s after 30 windows.
        let model = |n: usize, i: usize| (n as f64).min(if i < 30 { 3.0 } else { 7.0 }) * MIB;
        let trace = simulate(model, 120, PolicyParams::default());
        assert!(trace[29] <= 4, "before the change: {}", trace[29]);
        assert!(*trace.last().unwrap() >= 6, "after the change: {:?}", &trace[30..]);
    }

    #[test]
    fn idle_windows_do_not_change_anything() {
        let mut policy = Policy::new(PolicyParams::default());
        let d = policy.on_window(Window { throughput: 0.0, errors: 0, avg_in_flight: 0.0 });
        assert_eq!(d, Decision::Idle);
        assert_eq!(policy.limit(), 2);
    }

    #[tokio::test]
    async fn gate_capacity_can_shrink_and_grow() {
        let gate = Gate::new(2);
        let a = gate.acquire().await;
        let b = gate.acquire().await;
        assert!(gate.try_acquire().is_none());
        gate.set_capacity(1);
        drop(a);
        assert!(gate.try_acquire().is_none(), "still one holder over the reduced capacity");
        drop(b);
        let c = gate.try_acquire().expect("capacity 1 free again");
        gate.set_capacity(3);
        assert!(gate.try_acquire().is_some());
        drop(c);
    }
}
